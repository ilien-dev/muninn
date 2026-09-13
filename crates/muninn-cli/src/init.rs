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
const GITIGNORE_LINES: [&str; 4] = [
    ".muninn/muninn.db*",
    ".muninn/log/",
    ".muninn/compact/",
    ".muninn/init.json",
];

#[derive(Debug, Default, Serialize, Deserialize)]
struct InitState {
    set_auto_memory_false: bool,
    #[serde(default)]
    allow_rules_added: Vec<String>,
    boot_block_files: Vec<String>,
    gitignore_lines_added: Vec<String>,
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

fn set_config(dir: &Path, key: &str, value: serde_json::Value) -> Result<()> {
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
const ALLOW_RULES: [&str; 2] = ["Bash(muninn why:*)", "Bash(muninn status:*)"];

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

fn set_auto_memory(root: &Path, value: Option<bool>) -> Result<bool> {
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
    Ok(true)
}

fn ensure_gitignore(root: &Path) -> Result<Vec<String>> {
    let file = root.join(".gitignore");
    let existing = std::fs::read_to_string(&file).unwrap_or_default();
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

pub fn codex_hooks_json(binary: &Path) -> serde_json::Value {
    let bin = binary.display().to_string();
    let cmd = |ev: &str| serde_json::json!({ "type": "command", "command": format!("{bin} hook {ev}"), "timeout": 5 });
    let cmd_async = |ev: &str| serde_json::json!({ "type": "command", "command": format!("{bin} hook {ev}"), "timeout": 30, "async": true });
    serde_json::json!({
        "description": "Muninn memory engine hooks (Codex). Same binary as the Claude Code plugin.",
        "hooks": {
            "SessionStart":     [{ "matcher": "startup|resume|compact", "hooks": [cmd("SessionStart")] }],
            "UserPromptSubmit": [{ "hooks": [cmd("UserPromptSubmit")] }],
            "PreToolUse":       [{ "matcher": "^(Bash|Edit|Write|MultiEdit|Read)$", "hooks": [cmd("PreToolUse")] }],
            "PostToolUse":      [{ "matcher": "^(Bash|Read|Edit|Write|Grep|Glob|MultiEdit)$", "hooks": [cmd("PostToolUse")] }],
            "PreCompact":       [{ "hooks": [cmd("PreCompact")] }],
            "PostCompact":      [{ "hooks": [cmd("PostCompact")] }],
            "Stop":             [{ "hooks": [cmd_async("Stop")] }],
            "SessionEnd":       [{ "hooks": [{ "type": "command", "command": format!("{bin} hook SessionEnd"), "timeout": 1 }] }]
        }
    })
}

pub fn run(paths: &ProjectPaths, opts: InitOpts, json: bool) -> Result<()> {
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
        set_auto_memory(&paths.root, Some(false))?;
        st.set_auto_memory_false = true;
        touched.push(".claude/settings.json (autoMemoryEnabled=false)".into());
    }
    let added_rules = set_allow_rules(&paths.root, true)?;
    if !added_rules.is_empty() {
        st.allow_rules_added = added_rules;
        touched.push(".claude/settings.json (permissions.allow: muninn why, muninn status)".into());
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
            serde_json::to_string_pretty(&codex_hooks_json(&bin))? + "\n",
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
        set_auto_memory(&paths.root, None)?;
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
            if next.trim().is_empty() {
                std::fs::remove_file(&file)?;
            } else {
                std::fs::write(&file, next)?;
            }
            undone.push(".gitignore".into());
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
}
