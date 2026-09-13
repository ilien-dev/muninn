//! `muninn compile` and `muninn apply`.
//!
//! compile: parse + classify, store rules with source hashes, write artefacts to
//! `.muninn/compiled/`. Never touches harness configuration.
//! apply: show the diff against `.claude/settings.json`, ask, write only the exact
//! strings we own (tracked in `compiled/applied.json`), and enable hook rules.

use crate::output;
use anyhow::{bail, Context, Result};
use muninn_compile::{compile_project, emit, Class, CompiledRule};
use muninn_core::db::now_ms;
use muninn_core::sanitize::fts_term;
use muninn_core::{Db, Mode, ProjectPaths};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::{BufRead, Write};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Applied {
    pub version: u32,
    pub applied_at: i64,
    pub targets: Vec<String>,
    pub deny: Vec<String>,
    pub ask: Vec<String>,
    pub hooks_enabled: bool,
}

pub fn applied_path(paths: &ProjectPaths) -> std::path::PathBuf {
    paths.compiled_dir().join("applied.json")
}

pub fn load_applied(paths: &ProjectPaths) -> Option<Applied> {
    std::fs::read_to_string(applied_path(paths))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

/// True when every compiled source file still has the hash recorded in `rule`.
pub fn is_up_to_date(paths: &ProjectPaths, db: &Db) -> bool {
    let mut stmt = match db
        .conn
        .prepare("SELECT DISTINCT source_file, source_hash FROM rule")
    {
        Ok(s) => s,
        Err(_) => return false,
    };
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    if rows.is_empty() {
        // never compiled: up to date only if there are no instruction files at all
        return muninn_compile::parse::discover(&paths.root).is_empty();
    }
    let current: BTreeSet<String> = muninn_compile::parse::discover(&paths.root)
        .iter()
        .filter_map(|f| {
            f.strip_prefix(&paths.root)
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        })
        .collect();
    let recorded: BTreeSet<String> = rows.iter().map(|(f, _)| f.clone()).collect();
    if current != recorded {
        return false;
    }
    rows.iter().all(|(f, h)| {
        std::fs::read(paths.root.join(f))
            .map(|b| blake3::hash(&b).to_hex().as_str() == h)
            .unwrap_or(false)
    }) && paths.compiled_dir().join("report.md").exists()
}

/// Try to link a rule to a `decision` record by shared terms (FTS on decisions).
fn rationale_for(db: &Db, text: &str) -> Option<i64> {
    let mut terms: Vec<String> = text
        .split_whitespace()
        .filter_map(fts_term)
        .filter(|t| t.len() >= 4)
        .collect();
    terms.sort_by_key(|t| std::cmp::Reverse(t.len()));
    terms.dedup();
    terms.truncate(6);
    if terms.len() < 2 {
        return None;
    }
    let q = terms
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(" AND ");
    db.conn
        .query_row(
            "SELECT r.id FROM record_fts f JOIN record r ON r.id = f.rowid WHERE r.kind='decision' AND r.invalid=0 AND record_fts MATCH ?1 ORDER BY bm25(record_fts) LIMIT 1",
            [q],
            |row| row.get::<_, i64>(0),
        )
        .ok()
}

pub struct CompileOutcome {
    pub rules: Vec<CompiledRule>,
    pub up_to_date: bool,
}

/// Compile and store. Returns the compiled rules (empty when up to date and not forced).
pub fn compile(paths: &ProjectPaths, db: &Db, force: bool) -> Result<CompileOutcome> {
    if !force && is_up_to_date(paths, db) {
        return Ok(CompileOutcome {
            rules: vec![],
            up_to_date: true,
        });
    }
    let rules = compile_project(&paths.root);
    std::fs::create_dir_all(paths.compiled_dir())?;

    let now = now_ms();
    let tx = db.write_tx()?;
    tx.execute("DELETE FROM rule", [])?;
    let mut without: Vec<&CompiledRule> = Vec::new();
    {
        let mut ins = tx.prepare(
            "INSERT INTO rule(source_file, source_line, source_hash, text, class, pattern_id, emitted, rationale_ref, compiled_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )?;
        for r in &rules {
            let emitted = if r.classification.class == Class::InterpretiveOnly {
                None
            } else {
                Some(serde_json::to_string(&serde_json::json!({
                    "permissions": r.classification.permissions.iter().map(|p| p.render()).collect::<Vec<_>>(),
                    "hooks": r.classification.hooks,
                }))?)
            };
            let rationale = rationale_for(db, &r.candidate.text);
            if rationale.is_none() {
                without.push(r);
            }
            ins.execute(rusqlite::params![
                r.candidate.source_file,
                r.candidate.source_line as i64,
                r.candidate.source_hash,
                r.candidate.text,
                r.classification.class.as_str(),
                r.classification.pattern_id,
                emitted,
                rationale,
                now
            ])?;
        }
    }
    tx.commit()?;

    let dir = paths.compiled_dir();
    std::fs::write(
        dir.join("permissions.json"),
        serde_json::to_string_pretty(&emit::permissions(&rules))? + "\n",
    )?;
    std::fs::write(
        dir.join("pretooluse.json"),
        serde_json::to_string_pretty(&emit::hooks(&rules))? + "\n",
    )?;
    std::fs::write(
        dir.join("rules.json"),
        serde_json::to_string_pretty(&rules)? + "\n",
    )?;
    std::fs::write(dir.join("report.md"), emit::report(&rules, &without))?;
    Ok(CompileOutcome {
        rules,
        up_to_date: false,
    })
}

pub fn run_compile(paths: &ProjectPaths, force: bool, json: bool) -> Result<()> {
    let db =
        Db::open(&paths.db_path(), Mode::ReadWrite).context("opening store (run `muninn init`)")?;
    let out = compile(paths, &db, force)?;
    if out.up_to_date {
        if json {
            output::json(&serde_json::json!({ "up_to_date": true }));
        } else {
            output::out("muninn compile: up to date (use --force to recompile)");
        }
        return Ok(());
    }
    let cov = emit::coverage(&out.rules, 0);
    if json {
        output::json(
            &serde_json::json!({ "up_to_date": false, "coverage": cov, "report": paths.compiled_dir().join("report.md") }),
        );
    } else {
        let enforceable = cov.total - cov.by_class.get("interpretive_only").copied().unwrap_or(0);
        output::out(&format!(
            "muninn compile: {} rule candidates → {} enforceable ({} permission rules, {} PreToolUse rules), {} interpretive only ({:.0}%)",
            cov.total,
            enforceable,
            cov.permission_rules,
            cov.hook_rules,
            cov.total - enforceable,
            cov.interpretive_fraction * 100.0
        ));
        output::out(&format!(
            "  report: {}",
            paths.compiled_dir().join("report.md").display()
        ));
        output::out("  nothing applied; run `muninn apply` to review the diff");
    }
    Ok(())
}

fn settings_path(paths: &ProjectPaths) -> std::path::PathBuf {
    paths.root.join(".claude/settings.json")
}

fn read_settings(paths: &ProjectPaths) -> Result<serde_json::Value> {
    match std::fs::read_to_string(settings_path(paths)) {
        Ok(s) if !s.trim().is_empty() => {
            Ok(serde_json::from_str(&s).context("parsing .claude/settings.json")?)
        }
        _ => Ok(serde_json::json!({})),
    }
}

fn list_at(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get("permissions")
        .and_then(|p| p.get(key))
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn set_list(v: &mut serde_json::Value, key: &str, items: Vec<String>) {
    let obj = v.as_object_mut().expect("settings is an object");
    let perms = obj
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}));
    if !perms.is_object() {
        *perms = serde_json::json!({});
    }
    let arr: Vec<serde_json::Value> = items.into_iter().map(serde_json::Value::String).collect();
    if arr.is_empty() {
        perms.as_object_mut().unwrap().remove(key);
    } else {
        perms
            .as_object_mut()
            .unwrap()
            .insert(key.into(), serde_json::Value::Array(arr));
    }
}

pub fn run_apply(paths: &ProjectPaths, revert: bool, yes: bool, json: bool) -> Result<()> {
    if revert {
        return run_revert(paths, json);
    }
    let frag_path = paths.compiled_dir().join("permissions.json");
    let frag: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&frag_path)
            .context("no compiled output; run `muninn compile` first")?,
    )?;
    let want_deny = list_at(&frag, "deny");
    let want_ask = list_at(&frag, "ask");
    let hooks_file = paths.compiled_dir().join("pretooluse.json");
    let hook_count = std::fs::read_to_string(&hooks_file)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("rules").and_then(|r| r.as_array()).map(|a| a.len()))
        .unwrap_or(0);

    let prev = load_applied(paths).unwrap_or_default();
    let mut settings = read_settings(paths)?;
    let cur_deny = list_at(&settings, "deny");
    let cur_ask = list_at(&settings, "ask");

    // ours = previously applied strings; base = everything else the user has
    let base_deny: Vec<String> = cur_deny
        .iter()
        .filter(|s| !prev.deny.contains(s))
        .cloned()
        .collect();
    let base_ask: Vec<String> = cur_ask
        .iter()
        .filter(|s| !prev.ask.contains(s))
        .cloned()
        .collect();
    let mut next_deny = base_deny.clone();
    for d in &want_deny {
        if !next_deny.contains(d) {
            next_deny.push(d.clone());
        }
    }
    let mut next_ask = base_ask.clone();
    for a in &want_ask {
        if !next_ask.contains(a) && !next_deny.contains(a) {
            next_ask.push(a.clone());
        }
    }

    let added_deny: Vec<&String> = next_deny.iter().filter(|s| !cur_deny.contains(s)).collect();
    let removed_deny: Vec<&String> = cur_deny.iter().filter(|s| !next_deny.contains(s)).collect();
    let added_ask: Vec<&String> = next_ask.iter().filter(|s| !cur_ask.contains(s)).collect();
    let removed_ask: Vec<&String> = cur_ask.iter().filter(|s| !next_ask.contains(s)).collect();
    let changes = added_deny.len() + removed_deny.len() + added_ask.len() + removed_ask.len();
    let hooks_change = !prev.hooks_enabled && hook_count > 0;

    if json {
        output::json(&serde_json::json!({
            "target": settings_path(paths), "add_deny": added_deny, "remove_deny": removed_deny, "add_ask": added_ask, "remove_ask": removed_ask, "hook_rules": hook_count, "applied": yes
        }));
        if !yes {
            return Ok(());
        }
    } else {
        output::out(&format!("--- {}", settings_path(paths).display()));
        for s in &removed_deny {
            output::out(&format!("- deny  {s}"));
        }
        for s in &added_deny {
            output::out(&format!("+ deny  {s}"));
        }
        for s in &removed_ask {
            output::out(&format!("- ask   {s}"));
        }
        for s in &added_ask {
            output::out(&format!("+ ask   {s}"));
        }
        if changes == 0 {
            output::out("  (no permission changes)");
        }
        output::out(&format!(
            "--- PreToolUse: {} rule(s) from .muninn/compiled/pretooluse.json will be {}",
            hook_count,
            if prev.hooks_enabled {
                "kept enabled"
            } else {
                "enabled"
            }
        ));
        if changes == 0 && !hooks_change {
            output::out("nothing to apply");
            return Ok(());
        }
        if !yes {
            let stdin = std::io::stdin();
            if !stdin.is_terminal_like() {
                output::out("Re-run with --yes to apply (no interactive terminal).");
                return Ok(());
            }
            print!("Apply? [y/N] ");
            std::io::stdout().flush().ok();
            let mut line = String::new();
            stdin.lock().read_line(&mut line).ok();
            if !matches!(line.trim().to_lowercase().as_str(), "y" | "yes") {
                output::out("not applied");
                return Ok(());
            }
        }
    }

    set_list(&mut settings, "deny", next_deny);
    set_list(&mut settings, "ask", next_ask);
    std::fs::create_dir_all(paths.root.join(".claude"))?;
    std::fs::write(
        settings_path(paths),
        serde_json::to_string_pretty(&settings)? + "\n",
    )?;
    let applied = Applied {
        version: 1,
        applied_at: now_ms(),
        targets: vec![".claude/settings.json".into()],
        deny: want_deny,
        ask: want_ask,
        hooks_enabled: true,
    };
    std::fs::write(
        applied_path(paths),
        serde_json::to_string_pretty(&applied)? + "\n",
    )?;
    if !json {
        output::out("applied");
    }
    Ok(())
}

fn run_revert(paths: &ProjectPaths, json: bool) -> Result<()> {
    let Some(prev) = load_applied(paths) else {
        bail!("nothing applied")
    };
    let mut settings = read_settings(paths)?;
    let deny: Vec<String> = list_at(&settings, "deny")
        .into_iter()
        .filter(|s| !prev.deny.contains(s))
        .collect();
    let ask: Vec<String> = list_at(&settings, "ask")
        .into_iter()
        .filter(|s| !prev.ask.contains(s))
        .collect();
    set_list(&mut settings, "deny", deny);
    set_list(&mut settings, "ask", ask);
    if settings
        .get("permissions")
        .and_then(|p| p.as_object())
        .is_some_and(|o| o.is_empty())
    {
        settings.as_object_mut().unwrap().remove("permissions");
    }
    std::fs::write(
        settings_path(paths),
        serde_json::to_string_pretty(&settings)? + "\n",
    )?;
    std::fs::remove_file(applied_path(paths)).ok();
    if json {
        output::json(&serde_json::json!({ "reverted": true, "deny": prev.deny, "ask": prev.ask }));
    } else {
        output::out(&format!(
            "reverted {} deny and {} ask rule(s); PreToolUse rules disabled",
            prev.deny.len(),
            prev.ask.len()
        ));
    }
    Ok(())
}

trait TerminalLike {
    fn is_terminal_like(&self) -> bool;
}
impl TerminalLike for std::io::Stdin {
    fn is_terminal_like(&self) -> bool {
        use std::io::IsTerminal;
        self.is_terminal()
    }
}
