//! PreToolUse enforcement: evaluate compiled hook rules against a tool call.
//! Read-only; logs every decision (and every silence) to `.muninn/log/enforce.jsonl`.

use crate::compile_cmd::load_applied;
use crate::hook::HookInput;
use muninn_core::db::now_ms;
use muninn_core::ProjectPaths;
use regex::Regex;
use serde::Deserialize;
use std::io::Write;

#[derive(Debug, Deserialize)]
struct HookFile {
    rules: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    id: String,
    tool_regex: String,
    #[serde(default)]
    command_regex: Option<String>,
    #[serde(default)]
    path_regex: Option<String>,
    #[serde(default)]
    condition: Option<String>,
    decision: String,
    reason: String,
}

fn current_branch(root: &std::path::Path) -> Option<String> {
    let git = root.join(".git");
    let head = if git.is_dir() {
        git.join("HEAD")
    } else {
        let s = std::fs::read_to_string(&git).ok()?;
        let dir = s.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
        let p = std::path::PathBuf::from(dir);
        let p = if p.is_absolute() { p } else { root.join(p) };
        p.join("HEAD")
    };
    let s = std::fs::read_to_string(head).ok()?;
    s.trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string)
}

fn input_str<'a>(v: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(k).and_then(|x| x.as_str()))
}

/// Every path a call touches: `file_paths` for a Codex patch (one call, several
/// files; see `hook::normalize_apply_patch`), otherwise the single path field.
fn touched_paths<'a>(v: &'a serde_json::Value, keys: &[&str]) -> Vec<&'a str> {
    let many: Vec<&str> = v
        .get("file_paths")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    if many.is_empty() {
        vec![input_str(v, keys).unwrap_or("")]
    } else {
        many
    }
}

/// `a/b/../c` → `a/c` without touching the disk. A file that does not exist yet
/// cannot be canonicalised, and `root/../x` still starts with `root` component-wise.
fn lexical(p: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

pub struct Verdict {
    pub decision: String,
    pub reason: String,
    pub rule_id: String,
}

/// The cell's world is its checkout. A shell command or a read that names the store
/// (`MUNINN_ROOT`, `muninn.db`, `.muninn/`, `sqlite3`), walks `..`, or takes an absolute
/// path outside the checkout and outside the system directories is denied.
fn store_access(input: &HookInput, root: &str) -> Option<Verdict> {
    let tool = input.tool_name.as_deref().unwrap_or("");
    let ti = input.tool_input.clone().unwrap_or(serde_json::Value::Null);
    let text: String = match tool {
        "Bash" => input_str(&ti, &["command"]).unwrap_or("").to_string(),
        "Read" | "Grep" | "Glob" => touched_paths(&ti, &["file_path", "path", "pattern"]).join(" "),
        _ => return None,
    };
    if text.is_empty() {
        return None;
    }
    let store = std::env::var("MUNINN_ROOT").unwrap_or_default();
    let rootp = std::path::Path::new(root);
    let rootc = rootp.canonicalize().unwrap_or(rootp.to_path_buf());
    let named = (!store.is_empty() && text.contains(&store))
        || text.contains("muninn.db")
        || text.contains("MUNINN_ROOT")
        || text.contains(".muninn/")
        || text.contains("sqlite3");
    let escapes = text.contains("../")
        || text
            .split(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == '=' || c == ':')
            .any(|tok| {
                if !tok.starts_with('/') || tok.len() < 2 {
                    return false;
                }
                let p = std::path::Path::new(tok);
                let system = [
                    "/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", "/dev", "/proc", "/sys",
                    "/opt", "/run", "/nix",
                ];
                !(p.starts_with(&rootc)
                    || p.starts_with(rootp)
                    || system.iter().any(|s| p.starts_with(s)))
            });
    if !(named || escapes) {
        return None;
    }
    Some(Verdict {
        decision: "deny".into(),
        reason: format!(
            "Muninn: this checkout ({}) is the whole project; memory arrives through the hooks — use what was delivered or say nothing is recorded",
            rootc.display()
        ),
        rule_id: if named { "store-access".into() } else { "escape".into() },
    })
}

/// Evaluate. `None` = no rule matched (silence, still logged).
pub fn evaluate(paths: &ProjectPaths, input: &HookInput) -> Option<Verdict> {
    // Confinement: with MUNINN_CONFINE_ROOT set (experiment cells), an edit outside that
    // directory is denied. The seeded memory carries the real repository's absolute
    // paths, and an agent that follows them would write into the wrong tree.
    if let Ok(root) = std::env::var("MUNINN_CONFINE_ROOT") {
        let tool = input.tool_name.as_deref().unwrap_or("");
        if matches!(tool, "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
            let ti = input.tool_input.clone().unwrap_or(serde_json::Value::Null);
            let root = std::path::Path::new(&root);
            let rootc = root.canonicalize().unwrap_or(root.to_path_buf());
            let root_lex = lexical(root);
            for path in touched_paths(&ti, &["file_path", "notebook_path"]) {
                let p = std::path::Path::new(path);
                let abs = lexical(&if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    root.join(p)
                });
                // resolve symlinks through the file, or its directory for a new file;
                // a path under directories that do not exist yet is judged lexically
                let resolved = abs.canonicalize().ok().or_else(|| {
                    let dir = abs.parent()?.canonicalize().ok()?;
                    Some(dir.join(abs.file_name()?))
                });
                let inside = match resolved {
                    Some(r) => r.starts_with(&rootc),
                    None => abs.starts_with(&root_lex) || abs.starts_with(&rootc),
                };
                if !path.is_empty() && !inside {
                    return Some(Verdict {
                        decision: "deny".into(),
                        reason: format!(
                            "Muninn: write only inside {} (this checkout); {} is outside it",
                            rootc.display(),
                            path
                        ),
                        rule_id: "confine".into(),
                    });
                }
            }
        }
        // Instrument integrity (cells only): memory reaches the agent through the hooks
        // and nothing else. Gate 3 on Codex found a no-memory cell reading the engine's
        // source to locate the store and then the store itself (MUNINN_ROOT was in its
        // environment). A command or read that names the store, the database, the
        // variable, sqlite, or a path outside the checkout is denied and counted.
        if let Some(v) = store_access(input, &root) {
            crate::delivery::append(
                paths,
                &crate::delivery::Line {
                    at: muninn_core::db::now_ms(),
                    session: input.session_id.clone(),
                    arm: std::env::var("MUNINN_ARM").unwrap_or_else(|_| "literal".into()),
                    ids: vec![],
                    tokens: 0,
                    reason: format!("deny:{}", v.rule_id),
                },
            );
            return Some(v);
        }
    }
    let applied = load_applied(paths)?;
    if !applied.hooks_enabled {
        return None;
    }
    let file = std::fs::read_to_string(paths.compiled_dir().join("pretooluse.json")).ok()?;
    let hf: HookFile = serde_json::from_str(&file).ok()?;
    let tool = input.tool_name.as_deref().unwrap_or("");
    let ti = input.tool_input.clone().unwrap_or(serde_json::Value::Null);
    let command = input_str(&ti, &["command"]).unwrap_or("");
    let touched = touched_paths(&ti, &["file_path", "path", "notebook_path"]);
    let is_codex = input.turn_id.is_some();

    let mut verdict = None;
    for e in &hf.rules {
        let Ok(tr) = Regex::new(&e.tool_regex) else {
            continue;
        };
        if !tr.is_match(tool) {
            continue;
        }
        if let Some(cr) = &e.command_regex {
            match Regex::new(cr) {
                Ok(r) if r.is_match(command) => {}
                _ => continue,
            }
        }
        let path_re = match e.path_regex.as_deref().map(Regex::new) {
            None => None,
            Some(Ok(r)) => Some(r),
            Some(Err(_)) => continue,
        };
        let cond = e.condition.as_deref();
        if let Some(list) = cond.and_then(|c| c.strip_prefix("branch_in:")) {
            if !current_branch(&paths.root).is_some_and(|b| list.split(',').any(|x| x.trim() == b))
            {
                continue;
            }
        } else if cond.is_some_and(|c| c != "new_file" && c != "root_file") {
            continue;
        }
        // path regex and path condition hold for the same path; a patch matches when
        // any one of its files does
        // a relative tool path is relative to the project, never to wherever this process
        // happens to have been started: resolving it against the process directory judged
        // `README.md` by whichever README the caller's cwd held
        let absolute = |path: &str| {
            let p = std::path::Path::new(path);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                paths.root.join(p)
            }
        };
        let path_ok = |path: &str| {
            if path_re.as_ref().is_some_and(|r| !r.is_match(path)) {
                return false;
            }
            match cond {
                Some("new_file") => !path.is_empty() && !absolute(path).exists(),
                Some("root_file") => {
                    let abs = absolute(path);
                    !path.is_empty() && abs.parent() == Some(paths.root.as_path()) && !abs.exists()
                }
                _ => true,
            }
        };
        if !touched.iter().any(|p| path_ok(p)) {
            continue;
        }
        verdict = Some(Verdict {
            decision: e.decision.clone(),
            reason: e.reason.clone(),
            rule_id: e.id.clone(),
        });
        break;
    }

    // denominator: every evaluation leaves a line
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_dir().join("enforce.jsonl"))
    {
        let line = serde_json::json!({
            "at": now_ms(), "session": input.session_id, "tool": tool, "harness": if is_codex { "codex" } else { "claude-code" },
            "rule": verdict.as_ref().map(|v| v.rule_id.clone()), "decision": verdict.as_ref().map(|v| v.decision.clone())
        });
        let _ = writeln!(f, "{line}");
    }
    verdict
}

/// Shape the harness expects. Codex has no `ask`: it becomes a reminder.
pub fn render(v: &Verdict, is_codex: bool) -> serde_json::Value {
    if v.decision == "ask" && is_codex {
        serde_json::json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "additionalContext": format!("Muninn: this action needs the user's confirmation. {}", v.reason) } })
    } else {
        serde_json::json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": v.decision, "permissionDecisionReason": v.reason } })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touched_paths_prefers_the_patch_list() {
        let patch = serde_json::json!({ "file_path": "a.rs", "file_paths": ["a.rs", "b/c.rs"] });
        assert_eq!(
            touched_paths(&patch, &["file_path"]),
            vec!["a.rs", "b/c.rs"]
        );
        let edit = serde_json::json!({ "file_path": "a.rs" });
        assert_eq!(touched_paths(&edit, &["file_path"]), vec!["a.rs"]);
        assert_eq!(
            touched_paths(&serde_json::Value::Null, &["file_path"]),
            vec![""]
        );
    }

    /// A relative path to a new file that climbs out of the root is denied (Codex
    /// sends relative patch paths; `root/../x` used to pass the component check).
    #[test]
    fn store_reads_and_escapes_are_denied_in_a_cell() {
        let root = std::env::temp_dir().join("muninn-cell-root");
        std::fs::create_dir_all(&root).unwrap();
        let r = root.to_string_lossy().to_string();
        let bash = |cmd: &str| HookInput {
            tool_name: Some("Bash".into()),
            tool_input: Some(serde_json::json!({ "command": cmd })),
            ..Default::default()
        };
        assert_eq!(
            store_access(&bash("sqlite3 $MUNINN_ROOT/muninn.db .tables"), &r).map(|v| v.rule_id),
            Some("store-access".into())
        );
        assert_eq!(
            store_access(&bash("cat ../../GATE3.md"), &r).map(|v| v.rule_id),
            Some("escape".into())
        );
        assert_eq!(
            store_access(&bash("grep -rn zstd /home/someone/other"), &r).map(|v| v.rule_id),
            Some("escape".into())
        );
        assert!(store_access(&bash("git log --oneline"), &r).is_none());
        assert!(store_access(&bash(&format!("ls {}/docs", r)), &r).is_none());
        assert!(store_access(&bash("/usr/bin/grep -rn codec docs"), &r).is_none());
        let read = HookInput {
            tool_name: Some("Read".into()),
            tool_input: Some(serde_json::json!({ "file_path": "/tmp/elsewhere/muninn.db" })),
            ..Default::default()
        };
        assert!(store_access(&read, &r).is_some());
    }

    #[test]
    fn confine_denies_a_new_file_outside_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cell");
        std::fs::create_dir_all(&root).unwrap();
        let paths = ProjectPaths::from_root(&root);
        let call = |file_paths: serde_json::Value| HookInput {
            tool_name: Some("Write".into()),
            tool_input: Some(serde_json::json!({ "file_path": "x", "file_paths": file_paths })),
            ..Default::default()
        };
        std::env::set_var("MUNINN_CONFINE_ROOT", &root);
        let out = evaluate(
            &paths,
            &call(serde_json::json!(["ok.txt", "../outside.txt"])),
        );
        let inside = evaluate(
            &paths,
            &call(serde_json::json!(["a/../ok.txt", "./b/c.rs"])),
        );
        std::env::remove_var("MUNINN_CONFINE_ROOT");
        assert_eq!(out.map(|v| v.rule_id).as_deref(), Some("confine"));
        assert!(inside.is_none());
    }
}
