//! `muninn init` / `muninn clean`: everything that touches files outside `.muninn/`
//! is recorded in `.muninn/init.json` so `clean` only undoes what `init` did.

use crate::output;
use anyhow::{bail, Context, Result};
use muninn_core::{caps, tokens, Db, Mode, ProjectPaths};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const BOOT_BLOCK: &str = include_str!("../../../plugin/templates/CLAUDE.muninn.md");
/// Compact summary injected by the SessionStart hook (`boot = "hook"`, the default).
pub const BOOT_HOOK: &str = include_str!("../../../plugin/templates/BOOT.hook.md");
const BEGIN: &str = "<!-- muninn:begin -->";
const END: &str = "<!-- muninn:end -->";
/// The whole store stays out of version control. Muninn is a personal memory: each person's store
/// is built from their own sessions, so nothing in it is meant to travel with the repository.
/// Earlier versions ignored only the database, logs and state, which left the Markdown mirror
/// (`records/`, `index.md`) and `compiled/` to be committed by a `git add .`: every record's
/// words, retired ones included, published with the code, and a merge conflict on every
/// numbered file two people wrote.
const GITIGNORE_LINES: [&str; 1] = [".muninn/"];

#[derive(Debug, Default, Serialize, Deserialize)]
struct InitState {
    set_auto_memory_false: bool,
    #[serde(default)]
    allow_rules_added: Vec<String>,
    boot_block_files: Vec<String>,
    gitignore_lines_added: Vec<String>,
    /// What `autoMemoryEnabled` held before `init` set it, so `--undo` restores that
    /// rather than assuming the key was absent.
    #[serde(default)]
    auto_memory_was: Option<bool>,
    codex_hooks_written: bool,
}

pub struct InitOpts {
    pub keep_native: bool,
    pub refresh: bool,
    pub codex: bool,
    /// Write the long boot block into CLAUDE.md / AGENTS.md (opt-in; the default
    /// injects the compact summary from the SessionStart hook and touches no file).
    pub boot_file: bool,
}

pub struct BudgetReport {
    pub hook_chars: usize,
    pub hook_tokens: usize,
    pub chars: usize,
    pub est_tokens: usize,
    #[allow(dead_code)]
    pub exact_tokens: Option<usize>,
    pub ok: bool,
}

pub fn check_budget() -> BudgetReport {
    let chars = BOOT_BLOCK.chars().count();
    let est_tokens = tokens::estimate(BOOT_BLOCK);
    let exact_tokens: Option<usize> = exact_count(BOOT_BLOCK);
    let tok = exact_tokens.unwrap_or(est_tokens);
    let hook_chars = BOOT_HOOK.chars().count();
    let hook_tokens = exact_count(BOOT_HOOK).unwrap_or(tokens::estimate(BOOT_HOOK));
    BudgetReport {
        chars,
        est_tokens,
        exact_tokens,
        hook_chars,
        hook_tokens,
        ok: chars <= caps::BOOT_BLOCK_MAX_CHARS
            && tok <= caps::BOOT_BLOCK_MAX_TOKENS
            && hook_chars <= caps::BOOT_HOOK_MAX_CHARS
            && hook_tokens <= caps::BOOT_HOOK_MAX_TOKENS,
    }
}

#[cfg(feature = "exact-tokens")]
fn exact_count(s: &str) -> Option<usize> {
    Some(tokens::count_cl100k(s))
}
#[cfg(not(feature = "exact-tokens"))]
fn exact_count(_s: &str) -> Option<usize> {
    None
}

pub fn set_config(dir: &Path, key: &str, value: serde_json::Value) -> Result<()> {
    let p = dir.join("config.json");
    let mut v: serde_json::Value = std::fs::read_to_string(&p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    v[key] = value;
    std::fs::create_dir_all(dir)?;
    std::fs::write(&p, serde_json::to_string_pretty(&v)? + "\n")?;
    Ok(())
}

fn state_path(paths: &ProjectPaths) -> std::path::PathBuf {
    paths.muninn_dir.join("init.json")
}

fn load_state(paths: &ProjectPaths) -> InitState {
    std::fs::read_to_string(state_path(paths))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(paths: &ProjectPaths, st: &InitState) -> Result<()> {
    std::fs::write(state_path(paths), serde_json::to_string_pretty(st)?)?;
    Ok(())
}

/// Insert or replace the boot block. Only text between the markers is ever touched.
pub fn upsert_block(existing: &str, block: &str) -> String {
    let block = block.trim_end();
    if let (Some(b), Some(e)) = (existing.find(BEGIN), existing.find(END)) {
        if b < e {
            let end = e + END.len();
            let mut s = String::with_capacity(existing.len() + block.len());
            s.push_str(&existing[..b]);
            s.push_str(block);
            s.push_str(&existing[end..]);
            return s;
        }
    }
    let mut s = existing.to_string();
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    if !s.is_empty() {
        s.push('\n');
    }
    s.push_str(block);
    s.push('\n');
    s
}

pub fn remove_block(existing: &str) -> String {
    if let (Some(b), Some(e)) = (existing.find(BEGIN), existing.find(END)) {
        if b < e {
            let mut end = e + END.len();
            if existing[end..].starts_with('\n') {
                end += 1;
            }
            let mut start = b;
            // drop the blank line we added before the block
            if existing[..b].ends_with("\n\n") {
                start -= 1;
            }
            let mut s = existing[..start].to_string();
            s.push_str(&existing[end..]);
            return s;
        }
    }
    existing.to_string()
}

/// The two commands the boot block asks the agent to run must not stop it at a
/// permission prompt. Narrow patterns; `muninn scan-config` accepts them.
// `show` is named by the boot summary — the catalogue tells the agent to pull an entry with
// it — so an install that does not allow it sends the agent into a permission prompt for the
// one command the memory asked it to run.
const ALLOW_RULES: [&str; 3] = [
    "Bash(muninn why:*)",
    "Bash(muninn status:*)",
    "Bash(muninn show:*)",
];

fn set_allow_rules(root: &Path, add: bool) -> Result<Vec<String>> {
    let dir = root.join(".claude");
    let file = dir.join("settings.json");
    let mut v: serde_json::Value = match std::fs::read_to_string(&file) {
        Ok(s) if !s.trim().is_empty() => {
            serde_json::from_str(&s).context("parsing .claude/settings.json")?
        }
        _ => serde_json::json!({}),
    };
    let perms = v
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!(".claude/settings.json is not a JSON object"))?
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}));
    let allow = perms
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("permissions is not an object"))?
        .entry("allow")
        .or_insert_with(|| serde_json::json!([]));
    let arr = allow
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("permissions.allow is not an array"))?;
    let mut changed = Vec::new();
    for r in ALLOW_RULES {
        let present = arr.iter().any(|x| x.as_str() == Some(r));
        if add && !present {
            arr.push(serde_json::Value::String(r.into()));
            changed.push(r.to_string());
        } else if !add && present {
            arr.retain(|x| x.as_str() != Some(r));
            changed.push(r.to_string());
        }
    }
    if !changed.is_empty() {
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&file, serde_json::to_string_pretty(&v)? + "\n")?;
    }
    Ok(changed)
}

/// Set, or with `None` remove, `autoMemoryEnabled` in `.claude/settings.json`.
///
/// Returns what the key held before, and whether the file was written — which it is
/// only when the value actually changes. `init` is run again on every upgrade, and
/// this file belongs to the user: rewriting it to the value it already holds costs
/// them a reformatted file and a "touched" line for work that did not happen.
fn set_auto_memory(root: &Path, value: Option<bool>) -> Result<(Option<bool>, bool)> {
    let dir = root.join(".claude");
    let file = dir.join("settings.json");
    let mut v: serde_json::Value = match std::fs::read_to_string(&file) {
        Ok(s) if !s.trim().is_empty() => {
            serde_json::from_str(&s).context("parsing .claude/settings.json")?
        }
        _ => serde_json::json!({}),
    };
    let Some(obj) = v.as_object_mut() else {
        bail!(".claude/settings.json is not a JSON object")
    };
    let before = obj.get("autoMemoryEnabled").and_then(|x| x.as_bool());
    let unchanged = match value {
        Some(b) => before == Some(b),
        None => !obj.contains_key("autoMemoryEnabled"),
    };
    if unchanged {
        return Ok((before, false));
    }
    match value {
        Some(b) => {
            obj.insert("autoMemoryEnabled".into(), serde_json::Value::Bool(b));
        }
        None => {
            obj.remove("autoMemoryEnabled");
        }
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&file, serde_json::to_string_pretty(&v)? + "\n")?;
    Ok((before, true))
}

/// Whether the file already ignores the whole store. Every line this tool would add
/// lives under `.muninn/`, so a rule on the directory makes all four dead text — and
/// this repository ignores `/.muninn/` on purpose, so appending them put back, on
/// every `init`, a list a commit had deliberately replaced with one line.
fn covers_store(existing: &str) -> bool {
    existing
        .lines()
        .any(|l| matches!(l.trim(), ".muninn" | ".muninn/" | "/.muninn" | "/.muninn/"))
}

fn ensure_gitignore(root: &Path) -> Result<Vec<String>> {
    let file = root.join(".gitignore");
    let existing = std::fs::read_to_string(&file).unwrap_or_default();
    if covers_store(&existing) {
        return Ok(Vec::new());
    }
    let mut added = Vec::new();
    let mut out = existing.clone();
    for line in GITIGNORE_LINES {
        if !existing.lines().any(|l| l.trim() == line) {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(line);
            out.push('\n');
            added.push(line.to_string());
        }
    }
    if !added.is_empty() {
        std::fs::write(&file, out)?;
    }
    Ok(added)
}

/// Quote the binary path for a Codex hook command string.
///
/// Codex hooks are shell-form by the harness's own design [X2], so this path is
/// parsed by a shell. Single quotes make every metacharacter literal. What they
/// cannot make safe is a single quote itself, and a control character would split
/// or truncate the command line — those are refused rather than escaped, because a
/// path is not worth a second escaping layer at a trust boundary.
fn shell_quote_binary(binary: &Path) -> Result<String> {
    let s = binary.to_str().with_context(|| {
        format!(
            "the muninn binary path is not valid UTF-8: {}",
            binary.display()
        )
    })?;
    if let Some(bad) = s.chars().find(|c| *c == '\'' || c.is_control()) {
        bail!(
            "refusing to write .codex/hooks.json: the muninn binary path contains {bad:?}, \
             which cannot be quoted safely for a shell-form hook ({s}). Install the binary \
             somewhere without it (plugin/scripts/install.sh) and re-run `muninn init --codex`."
        );
    }
    Ok(format!("'{s}'"))
}

pub fn codex_hooks_json(binary: &Path) -> Result<serde_json::Value> {
    let bin = shell_quote_binary(binary)?;
    // The per-turn hooks get 2 seconds and the per-session ones 5, the same split the Claude
    // plugin ships and the same the committed `codex/hooks.json` template describes. This
    // generator gave every synchronous hook 5, so a `muninn init --codex` install let a
    // prompt hook block the user for five seconds where the plugin lets it block for two —
    // and the contracts say these run in under a millisecond.
    let cmd = |ev: &str, timeout: u32| serde_json::json!({ "type": "command", "command": format!("{bin} hook {ev}"), "timeout": timeout });
    let cmd_async = |ev: &str| serde_json::json!({ "type": "command", "command": format!("{bin} hook {ev}"), "timeout": 30, "async": true });
    Ok(serde_json::json!({
        "description": "Muninn memory engine hooks (Codex). Same binary as the Claude Code plugin.",
        "hooks": {
            "SessionStart":     [{ "matcher": "startup|resume|compact", "hooks": [cmd("SessionStart", 5)] }],
            "UserPromptSubmit": [{ "hooks": [cmd("UserPromptSubmit", 2)] }],
            // Codex edits files through `apply_patch`; the hook reshapes it into Edit/Write
            "PreToolUse":       [{ "matcher": "^(Bash|Edit|Write|MultiEdit|Read|apply_patch)$", "hooks": [cmd("PreToolUse", 2)] }],
            "PostToolUse":      [{ "matcher": "^(Bash|Read|Edit|Write|Grep|Glob|MultiEdit|apply_patch)$", "hooks": [cmd("PostToolUse", 2)] }],
            "PreCompact":       [{ "hooks": [cmd("PreCompact", 5)] }],
            "PostCompact":      [{ "hooks": [cmd("PostCompact", 5)] }],
            "Stop":             [{ "hooks": [cmd_async("Stop")] }],
            "SessionEnd":       [{ "hooks": [{ "type": "command", "command": format!("{bin} hook SessionEnd"), "timeout": 1 }] }]
        }
    }))
}

pub fn run(paths: &ProjectPaths, opts: InitOpts, json: bool) -> Result<()> {
    if opts.codex {
        let bin = std::env::current_exe().context("locating muninn binary")?;
        // A plugin's binary lives in a directory named after its version, which the next
        // update replaces: hooks pinned to it stop at the first upgrade, and the Codex plugin
        // already runs the same hooks, so the project file would fire every one twice.
        if let Some(root) = bin.parent().and_then(Path::parent) {
            if root.join(".codex-plugin").is_dir() || root.join(".claude-plugin").is_dir() {
                bail!(
                    "--codex is for a binary installed with install.sh; this one belongs to the \
                     plugin ({}). The Codex plugin already installs these hooks: `codex plugin \
                     marketplace add ilien-dev/muninn`, then `codex plugin add muninn@muninn`.",
                    bin.display()
                );
            }
        }
    }
    let mut touched: Vec<String> = Vec::new();
    let mut st = load_state(paths);
    for d in paths.all_dirs() {
        std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
    }
    let db = Db::open(&paths.db_path(), Mode::ReadWrite).context("creating database")?;
    db.meta_set("initialised_at", &muninn_core::db::now_ms().to_string())?;
    db.meta_set("muninn_version", muninn_core::VERSION)?;
    touched.push(paths.db_path().display().to_string());

    let added = ensure_gitignore(&paths.root)?;
    if !added.is_empty() {
        st.gitignore_lines_added.extend(added.iter().cloned());
        touched.push(".gitignore".into());
    }

    if !opts.keep_native {
        let (before, wrote) = set_auto_memory(&paths.root, Some(false))?;
        if wrote {
            st.set_auto_memory_false = true;
            st.auto_memory_was = before;
            touched.push(".claude/settings.json (autoMemoryEnabled=false)".into());
        }
    }
    let added_rules = set_allow_rules(&paths.root, true)?;
    if !added_rules.is_empty() {
        st.allow_rules_added = added_rules;
        touched.push(
            ".claude/settings.json (permissions.allow: muninn why, muninn status, muninn show)"
                .into(),
        );
    }

    // default: no file is touched; the SessionStart hook injects the compact summary.
    // `--boot-file` writes the long block and records `boot = "file"` so the hook
    // does not inject it twice.
    if opts.boot_file {
        set_config(
            &paths.muninn_dir,
            "boot",
            serde_json::Value::String("file".into()),
        )?;
        let budget = check_budget();
        if !budget.ok {
            bail!(
                "boot block over budget: {} chars / ~{} tokens (max {} / {})",
                budget.chars,
                budget.est_tokens,
                caps::BOOT_BLOCK_MAX_CHARS,
                caps::BOOT_BLOCK_MAX_TOKENS
            );
        }
        for name in ["CLAUDE.md", "AGENTS.md"] {
            let file = paths.root.join(name);
            let existing = std::fs::read_to_string(&file).unwrap_or_default();
            let has = existing.contains(BEGIN);
            if has && !opts.refresh {
                continue;
            }
            let next = upsert_block(&existing, BOOT_BLOCK);
            if next != existing {
                std::fs::write(&file, next)?;
                touched.push(format!(
                    "{name} (boot block{})",
                    if has { " refreshed" } else { "" }
                ));
            }
            if !st.boot_block_files.iter().any(|f| f == name) {
                st.boot_block_files.push(name.to_string());
            }
        }
    }

    if opts.codex {
        let bin = std::env::current_exe().context("locating muninn binary")?;
        let dir = paths.root.join(".codex");
        std::fs::create_dir_all(&dir)?;
        let file = dir.join("hooks.json");
        std::fs::write(
            &file,
            serde_json::to_string_pretty(&codex_hooks_json(&bin)?)? + "\n",
        )?;
        st.codex_hooks_written = true;
        touched.push(".codex/hooks.json".into());
    }

    save_state(paths, &st)?;
    if json {
        output::json(&serde_json::json!({ "root": paths.root, "touched": touched }));
    } else {
        output::out(&format!("muninn initialised in {}", paths.root.display()));
        for t in &touched {
            output::out(&format!("  touched {t}"));
        }
        if opts.keep_native {
            output::out("  native memory left enabled (--keep-native)");
        }
    }
    Ok(())
}

/// Undo everything `init` recorded. Never touches content outside our markers.
pub fn clean(paths: &ProjectPaths, yes: bool, json: bool) -> Result<()> {
    if !paths.muninn_dir.exists() {
        bail!(
            "nothing to clean: {} does not exist",
            paths.muninn_dir.display()
        );
    }
    if !yes {
        output::out(&format!(
            "This removes {} and undoes what `muninn init` changed. Re-run with --yes to confirm.",
            paths.muninn_dir.display()
        ));
        return Ok(());
    }
    let st = load_state(paths);
    let mut undone = Vec::new();
    for name in &st.boot_block_files {
        let file = paths.root.join(name);
        if let Ok(existing) = std::fs::read_to_string(&file) {
            let next = remove_block(&existing);
            if next != existing {
                if next.trim().is_empty() {
                    std::fs::remove_file(&file)?;
                } else {
                    std::fs::write(&file, next)?;
                }
                undone.push(format!("{name} (boot block removed)"));
            }
        }
    }
    if st.set_auto_memory_false {
        // restore what was there, which is not always "absent": a project that had
        // native memory on explicitly gets its own value back, not a deleted key.
        set_auto_memory(&paths.root, st.auto_memory_was)?;
        undone.push(".claude/settings.json (autoMemoryEnabled restored)".into());
    }
    if !st.allow_rules_added.is_empty() {
        let _ = set_allow_rules(&paths.root, false)?;
        undone.push(".claude/settings.json (permissions.allow rules removed)".into());
    }
    if !st.gitignore_lines_added.is_empty() {
        let file = paths.root.join(".gitignore");
        if let Ok(existing) = std::fs::read_to_string(&file) {
            let next: String = existing
                .lines()
                .filter(|l| !st.gitignore_lines_added.iter().any(|a| a == l.trim()))
                .map(|l| format!("{l}\n"))
                .collect();
            // only the lines this tool added: a project's own `.gitignore` is not ours to
            // delete, and the line printed said "removed .gitignore" when it was not
            if next.trim().is_empty() {
                std::fs::remove_file(&file)?;
                undone.push(".gitignore".into());
            } else {
                std::fs::write(&file, next)?;
                undone.push(".gitignore (its own lines only)".into());
            }
        }
    }
    if st.codex_hooks_written {
        let _ = std::fs::remove_file(paths.root.join(".codex/hooks.json"));
        undone.push(".codex/hooks.json".into());
    }
    std::fs::remove_dir_all(&paths.muninn_dir)?;
    undone.push(paths.muninn_dir.display().to_string());
    if json {
        output::json(&serde_json::json!({ "undone": undone }));
    } else {
        for u in &undone {
            output::out(&format!("  removed {u}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `init` runs on every install and must be idempotent against a `.gitignore`
    /// that is broader than its own list, not only against one that repeats it.
    /// The generator and the committed `codex/hooks.json` template are two descriptions of
    /// one install, and they disagreed on three timeouts: every synchronous hook got 5
    /// seconds here where the plugin and the template give the per-turn ones 2.
    #[test]
    fn the_codex_hooks_match_the_template_that_documents_them() {
        let generated = codex_hooks_json(std::path::Path::new("/usr/local/bin/muninn")).unwrap();
        let template: serde_json::Value =
            serde_json::from_str(include_str!("../../../codex/hooks.json")).unwrap();
        let (g, t) = (&generated["hooks"], &template["hooks"]);
        let names: Vec<&String> = t.as_object().unwrap().keys().collect();
        assert!(!names.is_empty());
        for k in names {
            let (a, b) = (&g[k][0], &t[k][0]);
            assert_eq!(a["matcher"], b["matcher"], "{k}: matcher");
            assert_eq!(
                a["hooks"][0]["timeout"], b["hooks"][0]["timeout"],
                "{k}: timeout"
            );
            assert_eq!(
                a["hooks"][0]["command"], b["hooks"][0]["command"],
                "{k}: command"
            );
        }
    }

    /// The Codex plugin ships its own hooks file, which Codex reads instead of the Claude
    /// Code one. It must run the same events, matchers and timeouts as `init --codex`, with
    /// the binary under `${PLUGIN_ROOT}` (Codex substitutes it before the shell runs) and
    /// SessionStart through the script that downloads the binary on first use.
    #[test]
    fn the_codex_plugin_hooks_match_the_generator() {
        let mut generated = codex_hooks_json(Path::new("${PLUGIN_ROOT}/bin/muninn")).unwrap();
        generated["hooks"]["SessionStart"][0]["hooks"][0] = serde_json::json!({
            "type": "command", "command": "'${PLUGIN_ROOT}/scripts/session-start'", "timeout": 120
        });
        let shipped: serde_json::Value =
            serde_json::from_str(include_str!("../../../plugin/hooks/codex.json")).unwrap();
        assert_eq!(generated["hooks"], shipped["hooks"]);
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../../plugin/.codex-plugin/plugin.json"))
                .unwrap();
        assert_eq!(manifest["hooks"], "./hooks/codex.json");
    }

    /// A marketplace install reads the version from the manifests in the repository, and the
    /// plugin downloads the release binary tagged with that version. A manifest that names a
    /// different version than the binary it is built with downloads another release, or none.
    #[test]
    fn every_manifest_carries_the_binary_version() {
        let v = muninn_core::VERSION;
        let read = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        let claude = read(include_str!("../../../plugin/.claude-plugin/plugin.json"));
        let codex = read(include_str!("../../../plugin/.codex-plugin/plugin.json"));
        let market = read(include_str!("../../../.claude-plugin/marketplace.json"));
        assert_eq!(claude["version"], v, "plugin/.claude-plugin/plugin.json");
        assert_eq!(codex["version"], v, "plugin/.codex-plugin/plugin.json");
        assert_eq!(market["metadata"]["version"], v, "marketplace metadata");
        for p in market["plugins"].as_array().unwrap() {
            assert_eq!(p["version"], v, "marketplace entry {}", p["name"]);
        }
    }

    #[test]
    fn gitignore_untouched_when_the_store_is_already_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        for rule in ["/.muninn/", ".muninn/", ".muninn", "/.muninn"] {
            let before = format!("/target\n{rule}\n");
            std::fs::write(tmp.path().join(".gitignore"), &before).unwrap();
            assert!(ensure_gitignore(tmp.path()).unwrap().is_empty(), "{rule}");
            let after = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
            assert_eq!(after, before, "{rule}");
        }
        // a file that does not cover the store gets the one line, once
        std::fs::write(tmp.path().join(".gitignore"), "/target\n").unwrap();
        assert_eq!(
            ensure_gitignore(tmp.path()).unwrap(),
            vec![".muninn/".to_string()]
        );
        assert!(ensure_gitignore(tmp.path()).unwrap().is_empty());
    }

    /// A project set up by an earlier version holds the four narrow lines, which leave the
    /// Markdown mirror committable. Running `init` again (every upgrade does) must close that.
    #[test]
    fn an_upgrade_ignores_the_whole_store_where_the_old_lines_did_not() {
        let tmp = tempfile::tempdir().unwrap();
        let old = ".muninn/muninn.db*\n.muninn/log/\n.muninn/compact/\n.muninn/init.json\n";
        std::fs::write(tmp.path().join(".gitignore"), old).unwrap();
        assert_eq!(
            ensure_gitignore(tmp.path()).unwrap(),
            vec![".muninn/".to_string()]
        );
        let after = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(covers_store(&after), "{after}");
        assert!(
            after.starts_with(old),
            "the project's existing lines are kept: {after}"
        );
    }

    /// `init` is run again on every upgrade. It may not rewrite a file it is not
    /// changing, and `--undo` may not delete a value the project set itself.
    #[test]
    fn auto_memory_is_written_once_and_restored_to_what_was_there() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join(".claude/settings.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "{\n  \"autoMemoryEnabled\": true\n}\n").unwrap();

        let (before, wrote) = set_auto_memory(tmp.path(), Some(false)).unwrap();
        assert_eq!((before, wrote), (Some(true), true));
        let (before, wrote) = set_auto_memory(tmp.path(), Some(false)).unwrap();
        assert_eq!(
            (before, wrote),
            (Some(false), false),
            "second run must not write"
        );

        set_auto_memory(tmp.path(), before).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(v["autoMemoryEnabled"], serde_json::json!(false));

        // a project that never had the key gets it removed, not set back to false
        std::fs::write(&file, "{}\n").unwrap();
        let (was, wrote) = set_auto_memory(tmp.path(), Some(false)).unwrap();
        assert_eq!((was, wrote), (None, true));
        set_auto_memory(tmp.path(), was).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(v.get("autoMemoryEnabled").is_none());
        assert!(!set_auto_memory(tmp.path(), None).unwrap().1);
    }

    #[test]
    fn block_roundtrip_preserves_foreign_content() {
        let before = "# My project\n\nRules here.\n";
        let with = upsert_block(before, BOOT_BLOCK);
        assert!(with.starts_with(before));
        assert!(with.contains(BEGIN) && with.contains(END));
        let again = upsert_block(&with, "<!-- muninn:begin -->\nnew\n<!-- muninn:end -->");
        assert_eq!(again.matches(BEGIN).count(), 1);
        assert!(again.contains("\nnew\n"));
        assert_eq!(remove_block(&again), before);
    }

    /// The repository's own CLAUDE.md keeps its rule: under 1 000 cl100k tokens.
    #[test]
    fn claude_md_within_budget() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../CLAUDE.md");
        let text = std::fs::read_to_string(&p).expect("CLAUDE.md at the repository root");
        let tokens = exact_count(&text).unwrap_or_else(|| tokens::estimate(&text));
        println!(
            "CLAUDE.md: {} chars, {} tokens",
            text.chars().count(),
            tokens
        );
        assert!(
            tokens <= 1_000,
            "CLAUDE.md is {tokens} tokens; the file's own rule is 1 000"
        );
    }

    #[test]
    fn boot_block_within_budget() {
        let b = check_budget();
        assert!(
            b.ok,
            "chars={} est_tokens={} exact={:?}",
            b.chars, b.est_tokens, b.exact_tokens
        );
    }

    /// Codex hooks are shell-form, so the binary path reaches a shell. A path with a
    /// space or a metacharacter is quoted, not refused; one that quoting cannot make
    /// safe is refused, and the command never carries the raw character.
    #[test]
    fn codex_hook_path_is_quoted_and_dangerous_paths_refused() {
        let quoted = codex_hooks_json(Path::new("/opt/Application Support/muninn"))
            .expect("a space is quotable");
        let cmd = quoted["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .expect("a command string");
        assert_eq!(cmd, "'/opt/Application Support/muninn' hook SessionStart");

        for bad in [
            "/tmp/a;rm -rf ~/muninn",
            "/tmp/$(id)/muninn",
            "/tmp/a`id`/muninn",
            "/tmp/a|id/muninn",
        ] {
            let cmd = codex_hooks_json(Path::new(bad))
                .unwrap_or_else(|e| panic!("{bad} is quotable, not refused: {e}"))["hooks"]
                ["SessionStart"][0]["hooks"][0]["command"]
                .as_str()
                .expect("a command string")
                .to_string();
            assert!(
                cmd.starts_with(&format!("'{bad}' hook")),
                "{bad} must reach the shell inside quotes, got {cmd}"
            );
        }

        for bad in ["/tmp/it's/muninn", "/tmp/a\nrm -rf ~/muninn"] {
            assert!(
                codex_hooks_json(Path::new(bad)).is_err(),
                "{bad:?} cannot be quoted safely and must be refused"
            );
        }
    }
}
