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
        "PostToolUse" => Ok(None),
        "PreCompact" => Ok(None),
        "PostCompact" => Ok(None),
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
    Ok(Some(additional_context("SessionStart", &report.summary())))
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
    let literal = recall::deliver(&db, prompt, &exclude)?;
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
    log(literal.ids.clone(), literal.tokens, "literal");
    Ok(Some(additional_context("UserPromptSubmit", &literal.text)))
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
            match muninn_capture::ingest::ingest_transcript(&db, p, &_input.session_id) {
                Ok(st) => {
                    if st.inserted > 0 {
                        output::err(&format!(
                            "muninn: ingested {} episode(s) from {} turn(s)",
                            st.inserted, st.turns
                        ));
                    }
                }
                Err(e) => output::err(&format!("muninn: ingest: {e}")),
            }
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
