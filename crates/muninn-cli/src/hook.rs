//! Hook entry points. One binary, one `hook <event>` subcommand, stdin JSON in,
//! JSON out. Read hooks open the database read-only; only Stop/SessionEnd/
//! PostToolUse may write.

use crate::output;
use muninn_core::db::now_ms;
use muninn_core::heartbeat::{self, Heartbeat};
use muninn_core::sanitize::from_bytes_lossy;
use muninn_core::{health, Db, Mode, ProjectPaths};
use serde::Deserialize;
use std::io::Read;
use std::path::PathBuf;

/// Superset of the fields Claude Code and Codex send. Unknown fields are ignored;
/// missing ones are tolerated. Nothing here is trusted beyond being a string.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct HookInput {
    pub session_id: String,
    pub transcript_path: Option<String>,
    pub cwd: Option<String>,
    pub hook_event_name: Option<String>,
    pub prompt: Option<String>,
    pub trigger: Option<String>,
    pub source: Option<String>,
    pub compact_summary: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    pub tool_response: Option<serde_json::Value>,
    pub tool_use_id: Option<String>,
    /// Codex sends a turn id; Claude Code does not. Used to shape PreToolUse output.
    pub turn_id: Option<String>,
    pub permission_mode: Option<String>,
    pub model: Option<String>,
}

const MAX_STDIN: u64 = 32 * 1024 * 1024;

pub fn read_input() -> HookInput {
    let mut buf = Vec::new();
    let _ = std::io::stdin()
        .lock()
        .take(MAX_STDIN)
        .read_to_end(&mut buf);
    let text = from_bytes_lossy(&buf);
    let mut input: HookInput = serde_json::from_str(&text).unwrap_or_default();
    normalize_apply_patch(&mut input);
    input
}

/// Codex edits files through one `apply_patch` tool whose input is the patch text.
/// Every hook reads its input here, so the call is reshaped once into the form the
/// engine matches on: `Write` when the patch creates a path, `Edit` otherwise, with
/// `file_path` (the first file) and `file_paths` (every file the patch touches).
fn normalize_apply_patch(input: &mut HookInput) {
    if input.tool_name.as_deref() != Some("apply_patch") {
        return;
    }
    let Some(patch) = input.tool_input.as_ref().and_then(|v| {
        v.get("command")
            .and_then(|c| c.as_str())
            .or_else(|| v.as_str())
            .map(str::to_string)
    }) else {
        return;
    };
    let (files, creates) = patch_files(&patch);
    let Some(first) = files.first().cloned() else {
        return;
    };
    input.tool_name = Some(if creates { "Write" } else { "Edit" }.into());
    input.tool_input = Some(serde_json::json!({
        "file_path": first,
        "file_paths": files,
        "patch": patch,
    }));
}

/// The paths named by an `apply_patch` envelope, in order, without repeats, and
/// whether any of them is created (`Add File`, or the target of a `Move to`).
fn patch_files(patch: &str) -> (Vec<String>, bool) {
    let mut files: Vec<String> = Vec::new();
    let mut creates = false;
    for line in patch.lines() {
        let line = line.trim_end();
        let path = if let Some(p) = line
            .strip_prefix("*** Add File: ")
            .or_else(|| line.strip_prefix("*** Move to: "))
        {
            creates = true;
            p
        } else if let Some(p) = line
            .strip_prefix("*** Update File: ")
            .or_else(|| line.strip_prefix("*** Delete File: "))
        {
            p
        } else {
            continue;
        };
        let path = path.trim();
        if !path.is_empty() && !files.iter().any(|f| f == path) {
            files.push(path.to_string());
        }
    }
    (files, creates)
}

fn additional_context(event: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "hookSpecificOutput": { "hookEventName": event, "additionalContext": text }
    })
}

/// Run one hook. Always returns exit code 0: a memory layer must never block the
/// agent because of its own failure. Failures go to the heartbeat and stderr.
pub fn run(event: &str, cwd_override: Option<PathBuf>) -> i32 {
    let input = read_input();
    let cwd = cwd_override
        .or_else(|| input.cwd.as_ref().map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let paths = ProjectPaths::resolve(&cwd);
    if !paths.is_initialised() {
        // Not opted in for this project: silence, no files, no heartbeat.
        return 0;
    }
    let session = if input.session_id.is_empty() {
        "unknown".to_string()
    } else {
        input.session_id.clone()
    };
    if matches!(
        event,
        "SessionStart" | "UserPromptSubmit" | "Stop" | "SessionEnd" | "PreCompact"
    ) {
        crate::sessions::note(&paths, &session, input.transcript_path.as_deref());
    }
    let (hb, hb_err) = Heartbeat::start(&paths, event, &session);
    if let Some(e) = hb_err {
        output::err(&format!("muninn: heartbeat: {e}"));
    }
    let result = match event {
        "SessionStart" => session_start(&paths, &input),
        "PreToolUse" => pre_tool_use(&paths, &input, &session),
        "UserPromptSubmit" => user_prompt(&paths, &input, &session),
        "PostToolUse" => post_tool_use(&paths, &input, &session),
        "PreCompact" => pre_compact(&paths, &input, &session),
        "PostCompact" => post_compact(&paths, &input, &session),
        "InstructionsLoaded" => Ok(None),
        "Stop" => write_path(&paths, &input, false),
        "SessionEnd" => write_path(&paths, &input, true),
        other => Err(muninn_core::Error::Other(format!(
            "unknown hook event {other}"
        ))),
    };
    match result {
        Ok(Some(v)) => {
            output::json(&v);
            hb.finish(true, None);
        }
        Ok(None) => {
            hb.finish(true, None);
        }
        Err(e) => {
            output::err(&format!("muninn: {event}: {e}"));
            hb.finish(false, Some(e.to_string()));
        }
    }
    0
}

fn session_start(
    paths: &ProjectPaths,
    _input: &HookInput,
) -> muninn_core::Result<Option<serde_json::Value>> {
    let db = Db::open(&paths.db_path(), Mode::ReadOnly);
    let report = match &db {
        Ok(db) => health::run(paths, Some(db), None, false),
        Err(e) => health::run(paths, None, Some(e), false),
    };
    // The write path (resume, git capture, projection, sidecar) runs detached; this
    // hook stays read-only and returns at once. At most one spawn per two minutes:
    // a burst of SessionStarts must not fan out into a burst of writers.
    crate::maintain::spawn_detached_throttled(paths, 120);
    let mut text = String::new();
    // The boot summary: how to read blocks and when to ask. Injected here by default
    // so the user's CLAUDE.md / AGENTS.md stay untouched (`muninn init --boot-file`
    // puts the long block in the file instead and sets `boot = "file"`).
    if boot_mode(paths) == "hook" {
        text.push_str(crate::init::BOOT_HOOK.trim_end());
        text.push_str("\n\n");
    }
    text.push_str(&report.summary());
    // F3 event cues: invariants and corrections resurface at every session start; after
    // a compaction they are reinjected without a gate and the ledger epoch moves on
    if let Ok(db) = &db {
        let compact = _input.source.as_deref() == Some("compact");
        let event = if compact {
            "post_compact"
        } else {
            "session_start"
        };
        if compact {
            crate::delivery::bump_epoch(paths, &_input.session_id);
        }
        if let Some(d) = event_delivery(paths, db, &_input.session_id, event) {
            text.push('\n');
            text.push_str(&d);
        }
        // The catalogue: one line per decision on record, once per session. Every other
        // delivery answers a question; this answers the one an agent cannot ask, because
        // asking it means already knowing what is there. On the v4 and v5 grids the engine
        // delivered the current decision in 27 cells of 27 and the agent wrote "no current
        // recorded decision" in most of them — it had a filtered selection and no way to tell
        // a memory that holds nothing from a query that missed, and it went to the checkout
        // two to three times as often as the competitor's agent, which is handed a complete
        // catalogue every session and pulls what it wants by id.
        if let Ok(c) = muninn_core::recall::catalog(db, muninn_core::caps::BUDGET_CATALOG_TOKENS) {
            if !c.text.is_empty() {
                text.push('\n');
                text.push_str(&c.text);
                crate::delivery::append(
                    paths,
                    &crate::delivery::Line {
                        at: now_ms(),
                        session: _input.session_id.clone(),
                        arm: arm(),
                        ids: c.ids.clone(),
                        tokens: c.tokens,
                        reason: "catalog".into(),
                    },
                );
            }
        }
    }
    Ok(Some(additional_context("SessionStart", &text)))
}

/// Experiment arms are selected by `MUNINN_ARM`: `literal` (default), `off`
/// (no delivery), `control` (length-matched irrelevant episodes from the store at
/// `MUNINN_CONTROL_DB`). Everything else about the hook is identical across arms.
/// Dir/symbol cue delivery (prompt time and tool time) is opt-in: the Gate 4 grid did
/// not distinguish its gain from lexical recall (+0.083 [−0.083, +0.250]), so the shipped
/// default is lexical recall plus event reinjection (session start, compaction), which
/// the decay probe measured at 100/100. `MUNINN_CUES=1|0` overrides `.muninn/config.json`.
/// Where the boot summary travels: `hook` (default; SessionStart additionalContext),
/// `file` (written by `muninn init --boot-file`), `off`. `MUNINN_BOOT` overrides.
fn boot_mode(paths: &ProjectPaths) -> String {
    if let Ok(v) = std::env::var("MUNINN_BOOT") {
        return v.to_ascii_lowercase();
    }
    std::fs::read_to_string(paths.muninn_dir.join("config.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.get("boot")
                .and_then(|c| c.as_str().map(|s| s.to_ascii_lowercase()))
        })
        .unwrap_or_else(|| "hook".into())
}

fn cues_enabled(paths: &ProjectPaths) -> bool {
    if let Ok(v) = std::env::var("MUNINN_CUES") {
        return v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on");
    }
    std::fs::read_to_string(paths.muninn_dir.join("config.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("cues").and_then(|c| c.as_bool()))
        .unwrap_or(false)
}

/// Whether the prompt hook delivers a block at all.
///
/// Muninn injects on every prompt; claude-mem injects once at session start. That difference
/// is most of why Muninn occupies 2.6 times the window (head-to-head v7). Since the session
/// catalogue arrived the agent is told what is on record up front and can pull any of it by
/// id, so per-prompt delivery may no longer be carrying its cost — which is a question for a
/// grid, not for a preference. Default on: nothing changes until a measurement says it should.
fn prompt_delivery_enabled(paths: &ProjectPaths) -> bool {
    if let Ok(v) = std::env::var("MUNINN_PROMPT_DELIVERY") {
        return !(v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off"));
    }
    std::fs::read_to_string(paths.muninn_dir.join("config.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("prompt_delivery").and_then(|c| c.as_bool()))
        .unwrap_or(true)
}

fn arm() -> String {
    std::env::var("MUNINN_ARM").unwrap_or_else(|_| "literal".into())
}

fn user_prompt(
    paths: &ProjectPaths,
    input: &HookInput,
    session: &str,
) -> muninn_core::Result<Option<serde_json::Value>> {
    use muninn_core::recall;
    let arm = arm();
    let prompt = input.prompt.as_deref().unwrap_or("");
    let log = |ids: Vec<i64>, tokens: usize, reason: &str| {
        crate::delivery::append(
            paths,
            &crate::delivery::Line {
                at: now_ms(),
                session: session.to_string(),
                arm: arm.clone(),
                ids,
                tokens,
                reason: reason.into(),
            },
        );
    };
    if arm == "off" {
        log(vec![], 0, "arm_off");
        return Ok(None);
    }
    if !recall::intent_gate(prompt) {
        log(vec![], 0, "gated:intent");
        return Ok(None);
    }
    if !prompt_delivery_enabled(paths) {
        log(vec![], 0, "gated:prompt_delivery_off");
        return Ok(None);
    }
    let db = Db::open(&paths.db_path(), Mode::ReadOnly)?;
    let exclude = crate::delivery::delivered_ids(paths, &db, session);
    let literal = deliver_fused(paths, &db, prompt, session, &exclude)?;
    if arm == "control" {
        // same token budget as the literal delivery would have used, filled from an unrelated store
        let Some(cpath) = std::env::var_os("MUNINN_CONTROL_DB") else {
            log(vec![], 0, "control:no_db");
            return Ok(None);
        };
        let cdb = Db::open(std::path::Path::new(&cpath), Mode::ReadOnly)?;
        let target = literal.tokens;
        if target == 0 {
            log(vec![], 0, "control:literal_empty");
            return Ok(None);
        }
        let seed = (blake3::hash(prompt.as_bytes()).as_bytes()[0] as i64) % 97;
        let mut stmt = cdb.conn.prepare(&format!(
            "SELECT {} FROM served_record r WHERE r.kind='episode' ORDER BY (r.id + ?1) % 101, r.id LIMIT 40",
            recall::SERVED_COLS
        ))?;
        let hits: Vec<recall::Hit> = stmt
            .query_map([seed], |r| recall::Hit::from_served_row(r, 0.0))?
            .filter_map(|r| r.ok())
            .collect();
        let d = recall::render(&hits, target, &[]);
        if d.text.is_empty() {
            log(vec![], 0, "control:empty");
            return Ok(None);
        }
        log(d.ids.iter().map(|i| -i).collect(), d.tokens, "control");
        return Ok(Some(additional_context("UserPromptSubmit", &d.text)));
    }
    if literal.text.is_empty() {
        log(vec![], 0, "silence:no_match");
        return Ok(None);
    }
    for (id, r) in &literal.reasons {
        if r.starts_with("cue:") {
            log(vec![*id], 0, r);
        }
    }
    if !literal.gated.is_empty() {
        // One line, not one per record. A store at the schema's cap gates thousands of
        // candidates on every prompt, and a line each grew the ledger by 4 200 rows a prompt
        // — which `delivered_ids` then re-read on the next one. The count is what the
        // denominator needs; the ids are kept for the top of the ranking only.
        const KEPT: usize = 32;
        let n = literal.gated.len();
        log(
            literal.gated.iter().take(KEPT).copied().collect(),
            0,
            &format!("gated:budget:{n}"),
        );
    }
    log(literal.ids.clone(), literal.tokens, "literal");
    Ok(Some(additional_context("UserPromptSubmit", &literal.text)))
}

/// Cues (turn context) and lexical recall fused by RRF under the turn budget.
fn deliver_fused(
    paths: &ProjectPaths,
    db: &Db,
    prompt: &str,
    session: &str,
    exclude: &std::collections::HashSet<i64>,
) -> muninn_core::Result<muninn_core::cue::Merged> {
    use muninn_core::{cue, recall};
    // query expansion through the symbol graph was measured (+0.125 on three runs) and
    // withdrawn when the five-run replication gave −0.125 [−0.275, −0.025] (GATE4.md §1)
    let terms = recall::select_terms(db, prompt, 8)?;
    let mut lexical = recall::recall(db, &terms, 8, exclude)?;
    // F1: conflicts are served as conflicts (the render-matched control arm skips this)
    let mark = std::env::var("MUNINN_ARM")
        .map(|a| a != "unfiltered")
        .unwrap_or(true);
    for h in lexical.iter_mut() {
        if mark && h.kind != "episode" {
            if let Ok(c) = muninn_core::filter::conflicts_of(db, h.id) {
                if !c.is_empty() {
                    let ids: Vec<String> = c.iter().map(|i| format!("#{i}")).collect();
                    h.kind = format!("{}:conflict with {}", h.kind, ids.join(","));
                }
            }
        }
    }
    let mut ctx = cue::load_context(paths, session, "prompt");
    // symbols the prompt itself names
    ctx.symbols.extend(cue::lexical_symbols(prompt, 6));
    // the lexical-only arm of the cue experiment switches the trigger conditions off
    let cue_hits = if std::env::var_os("MUNINN_NO_CUES").is_some() || !cues_enabled(paths) {
        vec![]
    } else {
        cue::evaluate(db, &ctx, exclude)?
    };
    let ids: Vec<i64> = cue_hits.iter().map(|h| h.record_id).collect();
    let cue_records = cue::hits_for(db, &ids)?;
    Ok(cue::merge(
        &cue_hits,
        &cue_records,
        &lexical,
        &terms,
        muninn_core::caps::BUDGET_TURN_TOKENS,
        &[],
    ))
}

/// Event-cue delivery (session_start / post_compact): invariants and corrections,
/// ungated, under the turn budget; logged like any other delivery.
fn event_delivery(paths: &ProjectPaths, db: &Db, session: &str, event: &str) -> Option<String> {
    use muninn_core::cue;
    if arm() == "off" || std::env::var_os("MUNINN_NO_CUES").is_some() {
        return None;
    }
    let exclude = if event == "post_compact" {
        std::collections::HashSet::new()
    } else {
        crate::delivery::delivered_ids(paths, db, session)
    };
    let ctx = cue::TurnContext {
        event: event.to_string(),
        ..Default::default()
    };
    // four times what a 700-token budget can render: what surfaces is unchanged, the
    // conjunction check and the record load never run for the rest, and `dropped` keeps the
    // disclosure line honest.
    const EVENT_CANDIDATES: usize = 64;
    let (hits, dropped) = cue::evaluate_capped(db, &ctx, &exclude, EVENT_CANDIDATES).ok()?;
    if hits.is_empty() && dropped == 0 {
        return None;
    }
    let ids: Vec<i64> = hits.iter().map(|h| h.record_id).collect();
    let recs = cue::hits_for(db, &ids).ok()?;
    let m = cue::merge(
        &hits,
        &recs,
        &[],
        &[],
        muninn_core::caps::BUDGET_TURN_TOKENS,
        &ids,
    );
    if m.text.is_empty() {
        return None;
    }
    crate::delivery::append(
        paths,
        &crate::delivery::Line {
            at: now_ms(),
            session: session.to_string(),
            arm: arm(),
            ids: m.ids.clone(),
            tokens: m.tokens,
            reason: format!("cue:event:{event}"),
        },
    );
    if !m.gated.is_empty() {
        // one line, not one per record: see the same cap on the prompt path
        const KEPT: usize = 32;
        let n = m.gated.len() + dropped;
        crate::delivery::append(
            paths,
            &crate::delivery::Line {
                at: now_ms(),
                session: session.to_string(),
                arm: arm(),
                ids: m.gated.iter().take(KEPT).copied().collect(),
                tokens: 0,
                reason: format!("gated:budget:{n}"),
            },
        );
    }
    if m.gated.is_empty() && dropped == 0 {
        Some(m.text)
    } else {
        // the budget is hard; silence about what it cut is not. The decay probe found
        // that with more invariants than fit (16 of ~30 tokens under 700), the rest
        // vanished without a trace at every compaction.
        let mut text = m.text;
        text.push_str(&format!(
            "[muninn:gated] {} more invariant/correction record(s) exist but did not fit the {}-token turn budget; run `muninn why <topic>` before assuming a rule is absent\n",
            m.gated.len() + dropped,
            muninn_core::caps::BUDGET_TURN_TOKENS
        ));
        Some(text)
    }
}

/// PostToolUse: the turn context — files touched and symbols referenced — goes to
/// an append-only log; a read hook never opens the store for writing.
fn post_tool_use(
    paths: &ProjectPaths,
    input: &HookInput,
    session: &str,
) -> muninn_core::Result<Option<serde_json::Value>> {
    use muninn_core::cue;
    let Some(tool) = input.tool_name.as_deref() else {
        return Ok(None);
    };
    let empty = serde_json::Value::Null;
    let base = input
        .cwd
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| paths.source_root());
    let (files, mut syms) =
        cue::context_of_tool(tool, input.tool_input.as_ref().unwrap_or(&empty), &base);
    if files.is_empty() && syms.is_empty() {
        return Ok(None);
    }
    // definitions of the touched files, when the graph knows them (indexed lookup)
    if let Ok(db) = Db::open(&paths.db_path(), Mode::ReadOnly) {
        for f in &files {
            if let Ok(mut st) = db.conn.prepare_cached(
                "SELECT short_name FROM symbol WHERE path = ?1 ORDER BY line LIMIT 12",
            ) {
                if let Ok(rows) = st.query_map([f.as_str()], |r| r.get::<_, String>(0)) {
                    syms.extend(rows.flatten());
                }
            }
        }
    }
    cue::append_context(
        paths,
        &cue::ContextLine {
            at: now_ms(),
            session: session.to_string(),
            files: files.clone(),
            symbols: syms.clone(),
        },
    );
    // cue-anchored delivery at the moment the file is touched: a decision anchored to
    // this directory or to one of these symbols arrives now, not at the next prompt
    if matches!(tool, "Read" | "Grep" | "Glob" | "Bash") {
        if let Ok(db) = Db::open(&paths.db_path(), Mode::ReadOnly) {
            let ctx = cue::TurnContext {
                files,
                symbols: syms,
                event: "post_tool".into(),
                keywords: Vec::new(),
            };
            if let Some(text) = cue_delivery(paths, &db, session, &ctx, "post_tool") {
                return Ok(Some(additional_context("PostToolUse", &text)));
            }
        }
    }
    Ok(None)
}

/// PreToolUse: the F2 verdict (deny/ask) and, for an edit, the cues anchored to the
/// file about to change (`pre_edit`: dead ends and decisions under that directory).
fn pre_tool_use(
    paths: &ProjectPaths,
    input: &HookInput,
    session: &str,
) -> muninn_core::Result<Option<serde_json::Value>> {
    use muninn_core::cue;
    let verdict = crate::pretooluse::evaluate(paths, input);
    let mut out = verdict
        .as_ref()
        .map(|v| crate::pretooluse::render(v, input.turn_id.is_some()));
    let tool = input.tool_name.as_deref().unwrap_or("");
    if matches!(tool, "Edit" | "Write" | "MultiEdit" | "NotebookEdit") && arm() != "off" {
        let empty = serde_json::Value::Null;
        let base = input
            .cwd
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| paths.source_root());
        let (files, mut syms) =
            cue::context_of_tool(tool, input.tool_input.as_ref().unwrap_or(&empty), &base);
        if let Ok(db) = Db::open(&paths.db_path(), Mode::ReadOnly) {
            for f in &files {
                if let Ok(mut st) = db.conn.prepare_cached(
                    "SELECT short_name FROM symbol WHERE path = ?1 ORDER BY line LIMIT 12",
                ) {
                    if let Ok(rows) = st.query_map([f.as_str()], |r| r.get::<_, String>(0)) {
                        syms.extend(rows.flatten());
                    }
                }
            }
            let ctx = cue::TurnContext {
                files,
                symbols: syms,
                event: "pre_edit".into(),
                keywords: Vec::new(),
            };
            if let Some(text) = cue_delivery(paths, &db, session, &ctx, "pre_edit") {
                match out.as_mut() {
                    Some(v) => {
                        v["hookSpecificOutput"]["additionalContext"] =
                            serde_json::Value::String(text);
                    }
                    None => out = Some(additional_context("PreToolUse", &text)),
                }
            }
        }
    }
    Ok(out)
}

/// Cue delivery for a tool-time context (post_tool / pre_edit): gated by the ledger,
/// under the turn budget, logged with its reasons.
fn cue_delivery(
    paths: &ProjectPaths,
    db: &Db,
    session: &str,
    ctx: &muninn_core::cue::TurnContext,
    when: &str,
) -> Option<String> {
    use muninn_core::cue;
    if std::env::var_os("MUNINN_NO_CUES").is_some()
        || arm() == "off"
        || arm() == "control"
        || !cues_enabled(paths)
    {
        return None;
    }
    let exclude = crate::delivery::delivered_ids(paths, db, session);
    let hits = cue::evaluate(db, ctx, &exclude).ok()?;
    if hits.is_empty() {
        return None;
    }
    let ids: Vec<i64> = hits.iter().map(|h| h.record_id).collect();
    let recs = cue::hits_for(db, &ids).ok()?;
    let m = cue::merge(
        &hits,
        &recs,
        &[],
        &[],
        muninn_core::caps::BUDGET_TURN_TOKENS,
        &[],
    );
    if m.text.is_empty() {
        return None;
    }
    for (id, r) in &m.reasons {
        crate::delivery::append(
            paths,
            &crate::delivery::Line {
                at: now_ms(),
                session: session.to_string(),
                arm: arm(),
                ids: vec![*id],
                tokens: 0,
                reason: format!("{r}@{when}"),
            },
        );
    }
    crate::delivery::append(
        paths,
        &crate::delivery::Line {
            at: now_ms(),
            session: session.to_string(),
            arm: arm(),
            ids: m.ids.clone(),
            tokens: m.tokens,
            reason: format!("literal:{when}"),
        },
    );
    Some(m.text)
}

/// PreCompact: snapshot of the active invariants and recent corrections, so what the
/// summary loses is reinjected from the store, not from the summary.
fn pre_compact(
    paths: &ProjectPaths,
    _input: &HookInput,
    session: &str,
) -> muninn_core::Result<Option<serde_json::Value>> {
    let db = Db::open(&paths.db_path(), Mode::ReadOnly)?;
    let rows = muninn_core::project::load_served(
        &db,
        "kind IN ('invariant','correction') ORDER BY created_at DESC LIMIT 80",
    )?;
    let epoch = crate::delivery::epoch(paths, session);
    let _ = std::fs::create_dir_all(paths.compact_dir());
    let p = paths.compact_dir().join(format!("{session}-{epoch}.json"));
    let _ = std::fs::write(&p, serde_json::to_string_pretty(&rows).unwrap_or_default());
    Ok(None)
}

/// PostCompact: the epoch moves on (the ledger restarts), invariants come back without
/// a gate, and the compaction summary's claims of success are checked against the
/// exit codes the transcript actually holds [W4].
fn post_compact(
    paths: &ProjectPaths,
    input: &HookInput,
    session: &str,
) -> muninn_core::Result<Option<serde_json::Value>> {
    crate::delivery::bump_epoch(paths, session);
    let db = Db::open(&paths.db_path(), Mode::ReadOnly)?;
    let mut text = String::new();
    if let Some(d) = event_delivery(paths, &db, session, "post_compact") {
        text.push_str(&d);
    }
    if let (Some(summary), Some(tp)) = (
        input.compact_summary.as_deref(),
        input.transcript_path.as_deref(),
    ) {
        for claim in unverified_claims(summary, std::path::Path::new(tp)) {
            text.push_str(&format!("[muninn:unverified] the compaction summary says \"{claim}\" but no command in the transcript ended with exit 0 for it; re-verify before relying on it\n"));
        }
    }
    if text.is_empty() {
        Ok(None)
    } else {
        Ok(Some(additional_context("PostCompact", &text)))
    }
}

/// Sentences of the summary that claim a result ("confirmed", "passes", "works",
/// "verified", "all tests pass") when the transcript's last test/build command did not
/// end with exit 0.
fn unverified_claims(summary: &str, transcript: &std::path::Path) -> Vec<String> {
    const CLAIMS: [&str; 10] = [
        "confirmed",
        "confirmado",
        "tests pass",
        "pasan los tests",
        "all green",
        "works",
        "funciona",
        "verified",
        "verificado",
        "build succeeds",
    ];
    let claims: Vec<String> = summary
        .split(['.', '\n'])
        .map(str::trim)
        .filter(|s| {
            let l = s.to_lowercase();
            CLAIMS.iter().any(|c| l.contains(c))
        })
        .map(|s| s.chars().take(160).collect())
        .collect();
    if claims.is_empty() {
        return vec![];
    }
    let Ok(session) = muninn_capture::parse_any(transcript, 0) else {
        return claims;
    };
    let mut last_ok = false;
    let mut any = false;
    for t in &session.turns {
        for c in &t.tools {
            let cmd = c.target.to_lowercase();
            if cmd.contains("test")
                || cmd.contains("build")
                || cmd.contains("clippy")
                || cmd.contains("check")
            {
                any = true;
                last_ok = c.exit_code == Some(0);
            }
        }
    }
    if any && last_ok {
        vec![]
    } else {
        claims
    }
}

/// Stop and SessionEnd: the only hooks that write. Fold heartbeats and
/// deliveries, ingest the transcript from its watermark, verify integrity.
fn write_path(
    paths: &ProjectPaths,
    _input: &HookInput,
    session_end: bool,
) -> muninn_core::Result<Option<serde_json::Value>> {
    let db = Db::open(&paths.db_path(), Mode::ReadWrite)?;
    heartbeat::fold_into_db(paths, &db)?;
    crate::delivery::fold_into_db(paths, &db)?;
    if let Some(t) = _input.transcript_path.as_deref() {
        let p = std::path::Path::new(t);
        if p.is_file() {
            match muninn_capture::ingest::ingest_transcript_with(
                &db,
                p,
                &_input.session_id,
                Some(paths),
            ) {
                Ok(st) => {
                    if st.inserted > 0 {
                        output::err(&format!(
                            "muninn: ingested {} record(s) from {} turn(s)",
                            st.inserted, st.turns
                        ));
                    }
                }
                Err(e) => output::err(&format!("muninn: ingest: {e}")),
            }
        }
    }
    // sessions whose own write hook was cut short (headless sessions end first)
    crate::sessions::ingest_pending(paths, &db);
    if let Err(e) = crate::maintain::capture_git(paths, &db) {
        output::err(&format!("muninn: git capture: {e}"));
    }
    // embeddings for the new records, on this asynchronous path only
    if let Ok(emb) = muninn_embed::Embedder::load_default() {
        if let Err(e) = muninn_embed::embed_pending(&db, &emb, false) {
            output::err(&format!("muninn: embed: {e}"));
        }
    }
    // Integrity is verified here, off the read path, at most once an hour.
    db.record_quick_check(3_600_000)?;
    // Rules are recompiled here when their source files changed; never applied.
    if !crate::compile_cmd::is_up_to_date(paths, &db) {
        if let Err(e) = crate::compile_cmd::compile(paths, &db, false) {
            output::err(&format!("muninn: compile: {e:#}"));
        }
    }
    let now = now_ms();
    if session_end {
        db.meta_set("last_session_end_ms", &now.to_string())?;
        db.meta_set("ingest_watermark_ms", &now.to_string())?;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload Codex 0.154 sends for a patch (captured from a live session).
    #[test]
    fn codex_apply_patch_becomes_edit_or_write() {
        let mut add = HookInput {
            tool_name: Some("apply_patch".into()),
            tool_input: Some(serde_json::json!({
                "command": "*** Begin Patch\n*** Add File: probe.txt\n+hello\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** End Patch"
            })),
            ..Default::default()
        };
        normalize_apply_patch(&mut add);
        assert_eq!(add.tool_name.as_deref(), Some("Write"));
        let ti = add.tool_input.unwrap();
        assert_eq!(ti["file_path"], "probe.txt");
        assert_eq!(
            ti["file_paths"],
            serde_json::json!(["probe.txt", "src/a.rs"])
        );

        let (files, creates) = patch_files(
            "*** Begin Patch\n*** Update File: a.rs\n*** Delete File: b.rs\n*** End Patch",
        );
        assert_eq!(files, vec!["a.rs", "b.rs"]);
        assert!(!creates);

        let mut shell = HookInput {
            tool_name: Some("Bash".into()),
            tool_input: Some(serde_json::json!({ "command": "*** Add File: x" })),
            ..Default::default()
        };
        normalize_apply_patch(&mut shell);
        assert_eq!(shell.tool_name.as_deref(), Some("Bash"));
    }
}
