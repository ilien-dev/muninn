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

pub struct Verdict {
    pub decision: String,
    pub reason: String,
    pub rule_id: String,
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
            let path = input_str(&ti, &["file_path", "notebook_path"]).unwrap_or("");
            let root = std::path::Path::new(&root);
            let p = std::path::Path::new(path);
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                root.join(p)
            };
            let canon = abs.canonicalize().unwrap_or(abs.clone());
            let rootc = root.canonicalize().unwrap_or(root.to_path_buf());
            if !path.is_empty() && !canon.starts_with(&rootc) {
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
    let applied = load_applied(paths)?;
    if !applied.hooks_enabled {
        return None;
    }
    let file = std::fs::read_to_string(paths.compiled_dir().join("pretooluse.json")).ok()?;
    let hf: HookFile = serde_json::from_str(&file).ok()?;
    let tool = input.tool_name.as_deref().unwrap_or("");
    let ti = input.tool_input.clone().unwrap_or(serde_json::Value::Null);
    let command = input_str(&ti, &["command"]).unwrap_or("");
    let path = input_str(&ti, &["file_path", "path", "notebook_path"]).unwrap_or("");
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
        if let Some(pr) = &e.path_regex {
            match Regex::new(pr) {
                Ok(r) if r.is_match(path) => {}
                _ => continue,
            }
        }
        if let Some(cond) = &e.condition {
            let ok = if let Some(list) = cond.strip_prefix("branch_in:") {
                current_branch(&paths.root).is_some_and(|b| list.split(',').any(|x| x.trim() == b))
            } else if cond == "new_file" {
                !path.is_empty() && !std::path::Path::new(path).exists()
            } else if cond == "root_file" {
                let p = std::path::Path::new(path);
                let abs = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    paths.root.join(p)
                };
                !path.is_empty() && abs.parent() == Some(paths.root.as_path()) && !abs.exists()
            } else {
                false
            };
            if !ok {
                continue;
            }
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
