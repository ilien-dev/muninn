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
    serde_json::from_str(&text).unwrap_or_default()
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
    let (hb, hb_err) = Heartbeat::start(&paths, event, &session);
    if let Some(e) = hb_err {
        output::err(&format!("muninn: heartbeat: {e}"));
    }
    let result = match event {
        "SessionStart" => session_start(&paths, &input),
        "PreToolUse" => Ok(crate::pretooluse::evaluate(&paths, &input)
            .map(|v| crate::pretooluse::render(&v, input.turn_id.is_some()))),
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
    let mut text = report.summary();
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
    }
    Ok(Some(additional_context("SessionStart", &text)))
}

/// Experiment arms are selected by `MUNINN_ARM`: `literal` (default), `off`
/// (no delivery), `control` (length-matched irrelevant episodes from the store at
/// `MUNINN_CONTROL_DB`). Everything else about the hook is identical across arms.
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
    let db = Db::open(&paths.db_path(), Mode::ReadOnly)?;
    let exclude = crate::delivery::delivered_ids(paths, session);
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
        let mut stmt = cdb.conn.prepare("SELECT id, kind, subject, object, body, origin, trust, created_at, session_id FROM record WHERE invalid=0 AND kind='episode' ORDER BY (id + ?1) % 101, id LIMIT 40")?;
        let hits: Vec<recall::Hit> = stmt
            .query_map([seed], |r| {
                Ok(recall::Hit {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    subject: r.get(2)?,
                    object: r.get(3)?,
                    body: r.get(4)?,
                    origin: r.get(5)?,
                    trust: r.get(6)?,
                    created_at: r.get(7)?,
                    session_id: r.get(8)?,
                    score: 0.0,
                })
            })?
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
    for id in &literal.gated {
        log(vec![*id], 0, "gated:budget");
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
    let cue_hits = if std::env::var_os("MUNINN_NO_CUES").is_some() {
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
        crate::delivery::delivered_ids(paths, session)
    };
    let ctx = cue::TurnContext {
        event: event.to_string(),
        ..Default::default()
    };
    let hits = cue::evaluate(db, &ctx, &exclude).ok()?;
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
    for id in &m.gated {
        crate::delivery::append(
            paths,
            &crate::delivery::Line {
                at: now_ms(),
                session: session.to_string(),
                arm: arm(),
                ids: vec![*id],
                tokens: 0,
                reason: "gated:budget".into(),
            },
        );
    }
    Some(m.text)
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
            files,
            symbols: syms,
        },
    );
    // dead ends anchored to a directory fire before an edit under it
    if matches!(tool, "Edit" | "Write" | "MultiEdit") {
        return Ok(None);
    }
    Ok(None)
}

/// PreCompact: snapshot of the active invariants and recent corrections, so what the
/// summary loses is reinjected from the store, not from the summary.
fn pre_compact(
    paths: &ProjectPaths,
    _input: &HookInput,
    session: &str,
) -> muninn_core::Result<Option<serde_json::Value>> {
    let db = Db::open(&paths.db_path(), Mode::ReadOnly)?;
    let rows = muninn_core::project::load(
        &db,
        "invalid = 0 AND kind IN ('invariant','correction') ORDER BY created_at DESC LIMIT 80",
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
