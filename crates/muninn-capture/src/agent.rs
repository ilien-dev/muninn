//! Prompts an agent wrote. A coordinating agent that starts a worker, or sends one a message,
//! types into the worker's session, and the harness logs that text in the user's role with the
//! same marks a person's prompt carries (`origin: human`, `promptSource: typed`). Nothing in
//! the worker's transcript tells the two apart. The coordinator's transcript does: the text is
//! the input of one of its tool calls.
//!
//! So the evidence is structural and lives across transcripts. Every tool call that is not a
//! file or shell tool leaves the hashes of the long strings in its input; a prompt whose
//! normalised text hashes to one of them, in another session of the same store, was written
//! by an agent. The match is the whole text, never a word in it: trust derives from who wrote
//! a turn, not from what it says. Ingest (`ingest.rs`) and the `extract_dump` measurement
//! example both go through [`mark_agent_written`].

use crate::model::{Session, Turn};
use serde_json::Value;
use std::collections::HashSet;

mod undo;

/// A string shorter than this is a name, a path or a flag, not a task handed to an agent.
const MIN_TASK_CHARS: usize = 40;

/// The hash a text is matched by: whitespace collapsed, case folded. `None` for a text too
/// short to be a task.
pub fn text_hash(s: &str) -> Option<String> {
    let norm = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if norm.chars().count() < MIN_TASK_CHARS {
        return None;
    }
    Some(blake3::hash(norm.as_bytes()).to_hex()[..32].to_string())
}

/// Tools whose input is a file, a pattern or a command line: never a task for an agent.
fn is_file_or_shell_tool(name: &str) -> bool {
    matches!(
        name,
        "Bash"
            | "shell"
            | "PowerShell"
            | "Edit"
            | "Write"
            | "MultiEdit"
            | "Read"
            | "NotebookEdit"
            | "Grep"
            | "Glob"
            | "apply_patch"
            | "AskUserQuestion"
            | "TodoWrite"
            | "ExitPlanMode"
    )
}

/// The hashes of the long strings in a tool call's input, at any depth: the agent start, the
/// message to a worker, the built-in Agent/Task tool's prompt, whatever the field is called.
pub fn task_hashes(name: &str, input: &Value) -> Vec<String> {
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => {
                let clean = muninn_core::sanitize::clean_text(
                    &muninn_core::sanitize::strip_harness_blocks(s),
                );
                if let Some(h) = text_hash(&clean) {
                    if !out.contains(&h) {
                        out.push(h);
                    }
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            Value::Object(o) => o.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    if !is_file_or_shell_tool(name) {
        walk(input, &mut out);
    }
    out
}

/// The hash of a turn's prompt, as [`task_hashes`] would have taken it from a tool input.
pub fn prompt_hash(t: &Turn) -> Option<String> {
    text_hash(&t.user_prompt)
}

/// Every task hash the tool calls of this session carry.
pub fn session_task_hashes(s: &Session) -> HashSet<String> {
    s.turns
        .iter()
        .flat_map(|t| t.tools.iter())
        .flat_map(|c| c.task_hashes.iter().cloned())
        .collect()
}

/// Mark the turns whose prompt is a task another session's tool call handed over.
/// `handed_over(hash)` answers for the other sessions of the store, never this one: an agent
/// that quotes the user's prompt back into a tool of its own session does not make the
/// prompt an agent's. Returns how many turns were marked.
pub fn mark_agent_written(s: &mut Session, mut handed_over: impl FnMut(&str) -> bool) -> usize {
    let mut n = 0;
    for t in &mut s.turns {
        if t.automated || t.agent_written {
            continue;
        }
        if prompt_hash(t).is_some_and(|h| handed_over(&h)) {
            t.agent_written = true;
            n += 1;
        }
    }
    n
}

/// Ingest's side of [`mark_agent_written`]: a prompt is an agent's when another session of
/// this store has handed it over already.
pub(crate) fn mark_from_store(
    tx: &rusqlite::Connection,
    s: &mut Session,
    sid: &str,
) -> muninn_core::Result<usize> {
    let mut q = tx.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM agent_task WHERE hash = ?1 AND session_id <> ?2)",
    )?;
    let mut err = None;
    let n = mark_agent_written(s, |h| {
        q.query_row(rusqlite::params![h, sid], |r| r.get::<_, bool>(0))
            .map_err(|e| err = Some(e))
            .unwrap_or(false)
    });
    match err {
        Some(e) => Err(e.into()),
        None => Ok(n),
    }
}

/// What this session leaves for the others to match a task against: the hash of each prompt
/// typed in the user's role. Inside the caller's write transaction.
pub(crate) fn record_prompts(
    tx: &rusqlite::Connection,
    s: &Session,
    sid: &str,
    transcript: &std::path::Path,
) -> muninn_core::Result<()> {
    let mut prompt = tx.prepare_cached(
        "INSERT OR IGNORE INTO turn_prompt(hash, session_id, transcript_ref) VALUES (?1, ?2, ?3)",
    )?;
    for t in &s.turns {
        if t.automated || t.agent_written {
            continue;
        }
        if let Some(h) = prompt_hash(t) {
            let tref = format!("{}:{}", transcript.display(), t.end_offset);
            prompt.execute(rusqlite::params![h, sid, tref])?;
        }
    }
    Ok(())
}

/// The tasks this session's tool calls handed over, and what follows from them. A task that
/// arrives after the worker's turn was ingested shows that turn's prompt was an agent's: what
/// the turn typed at trust 3 is revoked (`invalidated_via = 'agent_prompt'`) and what it did
/// to the user's records is undone (`undo`); retired, never deleted.
///
/// Called before anything of this session's batch is written. The prompt that opens the turn
/// which hands a task over was typed before the worker ran: its statements have to meet the
/// store as it was then, with the worker's prompt already undone, and not be taken for
/// something said after it. Inside the caller's write transaction. Returns how many records
/// were retired.
pub(crate) fn find_out(
    tx: &rusqlite::Connection,
    s: &Session,
    sid: &str,
) -> muninn_core::Result<usize> {
    let mut task =
        tx.prepare_cached("INSERT OR IGNORE INTO agent_task(hash, session_id) VALUES (?1, ?2)")?;
    let mut seen = tx.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM turn_prompt WHERE hash = ?1 AND session_id <> ?2)",
    )?;
    let mut hashes: Vec<String> = session_task_hashes(s).into_iter().collect();
    hashes.sort();
    // only a task some other session's prompt carries reaches the record scans below; the
    // probe is on the primary key's index, the scans are not
    let mut found = Vec::new();
    for h in hashes {
        if task.execute(rusqlite::params![h, sid])? == 1
            && seen.query_row(rusqlite::params![h, sid], |r| r.get::<_, bool>(0))?
        {
            found.push(h);
        }
    }
    if found.is_empty() {
        return Ok(0);
    }
    undo_found(tx, &found, sid)
}

/// What `find_out` does once tasks are found: the turns that carried them (`turn_prompt` rows
/// of sessions other than `sid`) are undone as an agent's.
fn undo_found(
    tx: &rusqlite::Connection,
    found: &[String],
    sid: &str,
) -> muninn_core::Result<usize> {
    // every restatement of every task found here is undone together, newest first, before
    // anything of those turns is retired: the order the hashes sort in decides nothing
    let mut retired = undo::undo_restatements(tx, found, sid)?;
    for h in found {
        // the rows of the turns that carried the task, active and retired apart, so that each
        // half is read through its own index on the turn
        let scope = "id IN (SELECT id FROM record WHERE invalid = 0 AND transcript_ref IN                          (SELECT transcript_ref FROM turn_prompt WHERE hash = ?1 AND session_id <> ?2)                      UNION ALL SELECT id FROM record WHERE invalid = 1 AND transcript_ref IN                          (SELECT transcript_ref FROM turn_prompt WHERE hash = ?1 AND session_id <> ?2))";
        let (n, back) = undo::revoke(tx, scope, rusqlite::params![h, sid])?;
        retired += n + relabel_episodes(tx, scope, rusqlite::params![h, sid])?;
        retired += undo::settle(tx, back)?;
        // A kind the undo of a prompt's statements does not cover (`undo`'s typed kinds) is
        // written as the person's only by `muninn remember`: a dead end said in words. It is
        // retired as they are, with the same cause, here where both ingest orders pass.
        retired += tx.execute(
            "UPDATE record SET invalid = 1, invalid_reason = 'revoked', invalidated_by = NULL, \
             invalidated_via = 'agent_prompt' WHERE invalid = 0 AND origin = 'user_said' \
             AND kind NOT IN ('decision', 'invariant', 'correction', 'episode') AND transcript_ref IN \
             (SELECT transcript_ref FROM turn_prompt WHERE hash = ?1 AND session_id <> ?2)",
            rusqlite::params![h, sid],
        )?;
    }
    // a statement that stood in the way of giving a record back may have been one of these
    // prompts' own, revoked now
    retired += undo::reconsider(tx)?;
    Ok(retired)
}

/// A statement `muninn remember` kept from turn `t` of session `sid` while nothing showed the
/// turn's prompt was an agent's, and the turn found to be one at its own ingest: the
/// coordinator's task was on record first. That is the order in which an extracted record is
/// never typed, so nothing undoes it otherwise. The turn's prompt is put on record as the
/// worker-first order puts it, and the turn is undone as `find_out` undoes it then, on behalf of
/// the session that handed the task over. Inside the ingest transaction. Returns how many
/// records were retired.
pub(crate) fn undo_kept_turn(
    tx: &rusqlite::Connection,
    t: &Turn,
    sid: &str,
    tref: &str,
) -> muninn_core::Result<usize> {
    let Some(h) = prompt_hash(t) else {
        return Ok(0);
    };
    let coordinator: Option<String> = match tx.query_row(
        "SELECT session_id FROM agent_task WHERE hash = ?1 AND session_id <> ?2 ORDER BY session_id LIMIT 1",
        rusqlite::params![h, sid],
        |r| r.get(0),
    ) {
        Ok(c) => Some(c),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.into()),
    };
    let Some(coordinator) = coordinator else {
        return Ok(0);
    };
    tx.execute(
        "INSERT OR IGNORE INTO turn_prompt(hash, session_id, transcript_ref) VALUES (?1, ?2, ?3)",
        rusqlite::params![h, sid, tref],
    )?;
    undo_found(tx, &[h], &coordinator)
}

/// The episodes of a worker's turn ingested before the coordinator's were written as the
/// user's (`user:`, `tool_observed`). Each is retired with a cause and replaced by the same
/// literal text labelled `agent:`, `agent_inferred` at trust 0, as the coordinator-first order
/// writes it; the new row is the old one's heir. Nothing stored is edited, and a task already
/// on record never reaches here again, so a second `maintain` writes nothing.
///
/// An episode of such a turn that was left retired with the mark of a record not given back
/// (`undo`) is replaced the same way: it is the agent's, and the mark is for the user's.
pub(super) fn relabel_episodes(
    tx: &rusqlite::Connection,
    scope: &str,
    params: &[&dyn rusqlite::ToSql],
) -> muninn_core::Result<usize> {
    type Ep = (
        i64,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        i64,
    );
    let rows: Vec<Ep> = tx
        .prepare(&format!(
            "SELECT id, subject, object, body, anchor_path, anchor_hash, session_id, transcript_ref, created_at \
             FROM record WHERE (invalid = 0 OR invalidated_via IS '{MARK}') \
             AND kind = 'episode' AND origin = 'tool_observed' AND {scope}",
            MARK = muninn_core::db::AGENT_UNRESTORED
        ))?
        .query_map(params, |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut n = 0;
    for (id, subject, object, body, anchor, anchor_hash, session, tref, created) in rows {
        let Some(rest) = body.strip_prefix("user") else {
            continue;
        };
        let new_body = format!("agent{rest}");
        let hash = crate::ingest::dedup("episode", &subject, "happened", &object, &new_body);
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO record(kind, subject, relation, object, body, origin, trust, anchor_path, anchor_hash, session_id, transcript_ref, dedup_hash, created_at) \
             VALUES('episode', ?1, 'happened', ?2, ?3, 'agent_inferred', ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                subject,
                object,
                new_body,
                crate::extract::trust_of("agent_inferred"),
                anchor,
                anchor_hash,
                session,
                tref,
                hash,
                created
            ],
        )?;
        if inserted == 1 {
            let heir = tx.last_insert_rowid();
            n += tx.execute(
                "UPDATE record SET invalid = 1, invalid_reason = 'superseded', invalidated_by = ?1, \
                 invalidated_via = 'agent_prompt' WHERE id = ?2",
                rusqlite::params![heir, id],
            )?;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_task_is_matched_by_its_whole_text_and_not_from_a_file_tool() {
        let task = "Revisa el módulo de pagos y escribe las pruebas que faltan, sin tocar main.";
        let h = task_hashes("mcp__team__start", &json!({"worktree": "x", "task": task}));
        assert_eq!(h, vec![text_hash(task).unwrap()]);
        // spacing and case do not change the text
        assert_eq!(
            text_hash(
                "revisa el  módulo de pagos y escribe las pruebas que faltan,\nsin tocar main."
            ),
            text_hash(task)
        );
        assert!(task_hashes("Write", &json!({"file_path": "a", "content": task})).is_empty());
        assert!(task_hashes("Bash", &json!({"command": task})).is_empty());
        // a short value is not a task
        assert!(task_hashes("SendMessage", &json!({"to": "worker-1"})).is_empty());
        // nested, as the built-in tools and MCP servers lay them out
        assert_eq!(
            task_hashes("Agent", &json!({"spec": {"prompt": task}})).len(),
            1
        );
        // a question put to the user is not a task for an agent
        assert!(task_hashes(
            "AskUserQuestion",
            &json!({"questions": [{"question": task}]})
        )
        .is_empty());
    }
}

#[cfg(test)]
mod ingest_tests {
    use crate::ingest::ingest_transcript;
    use muninn_core::db::{Db, Mode};
    use serde_json::json;
    use std::path::{Path, PathBuf};

    const TASK: &str =
        "Revisa el módulo de facturas. Nunca hagas push a main desde la rama de pruebas.";

    fn write(dir: &Path, name: &str, lines: &[serde_json::Value]) -> PathBuf {
        let p = dir.join(name);
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&p, text).unwrap();
        p
    }

    /// The coordinator: one turn that starts a worker with `TASK`.
    fn coordinator(dir: &Path) -> PathBuf {
        write(
            dir,
            "coord.jsonl",
            &[
                json!({"type":"user","sessionId":"coord","message":{"role":"user","content":"Lanza un worker para las facturas."}}),
                json!({"type":"assistant","sessionId":"coord","message":{"role":"assistant","content":[
                    {"type":"tool_use","id":"t1","name":"mcp__team__start","input":{"name":"w1","task":TASK}}]}}),
                json!({"type":"user","sessionId":"coord","message":{"role":"user","content":[
                    {"type":"tool_result","tool_use_id":"t1","content":"started"}]}}),
                json!({"type":"assistant","sessionId":"coord","message":{"role":"assistant","content":[{"type":"text","text":"Worker started."}]}}),
            ],
        )
    }

    /// The worker: the same text arrives in the user's role, marked as typed by a human.
    fn worker(dir: &Path) -> PathBuf {
        write(
            dir,
            "worker.jsonl",
            &[
                json!({"type":"user","sessionId":"worker","origin":{"kind":"human"},"promptSource":"typed","message":{"role":"user","content":TASK}}),
                json!({"type":"assistant","sessionId":"worker","message":{"role":"assistant","content":[{"type":"text","text":"On it."}]}}),
            ],
        )
    }

    fn typed_active(db: &Db) -> i64 {
        db.count("SELECT count(*) FROM record WHERE invalid=0 AND kind<>'episode'")
            .unwrap()
    }

    /// Only the write path pays for the hashes: a read hook's parse leaves them empty.
    #[test]
    fn only_the_write_path_parse_hashes_tasks() {
        let tmp = tempfile::tempdir().unwrap();
        let p = coordinator(tmp.path());
        let read = crate::parse_any(&p, 0).unwrap();
        assert!(read
            .turns
            .iter()
            .flat_map(|t| &t.tools)
            .all(|c| c.task_hashes.is_empty()));
        let write = crate::parse_any_with_tasks(&p, 0).unwrap();
        assert_eq!(super::session_task_hashes(&write).len(), 1);
    }

    /// The coordinator's turn is on record first: the worker's prompt types nothing at trust
    /// 3, and its episode says an agent wrote it.
    #[test]
    fn a_prompt_another_session_handed_over_is_not_the_user_speaking() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        ingest_transcript(&db, &coordinator(tmp.path()), "coord").unwrap();
        ingest_transcript(&db, &worker(tmp.path()), "worker").unwrap();
        assert_eq!(typed_active(&db), 0, "no rule typed from an agent's prompt");
        let (origin, trust): (String, i64) = db
            .conn
            .query_row(
                "SELECT origin, trust FROM record WHERE kind='episode' AND session_id='worker'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((origin.as_str(), trust), ("agent_inferred", 0));
    }

    /// The worker's turn is on record first, as it usually is (the coordinator's turn ends
    /// later): what it typed is retired, with a cause, when the coordinator's turn arrives.
    #[test]
    fn a_rule_typed_from_a_prompt_later_found_handed_over_is_retired() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        ingest_transcript(&db, &worker(tmp.path()), "worker").unwrap();
        assert!(typed_active(&db) > 0, "the fixture types a rule at first");
        ingest_transcript(&db, &coordinator(tmp.path()), "coord").unwrap();
        assert_eq!(typed_active(&db), 0);
        let via: Vec<String> = db
            .conn
            .prepare("SELECT invalidated_via FROM record WHERE kind<>'episode'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(
            !via.is_empty() && via.iter().all(|v| v == "agent_prompt"),
            "{via:?}"
        );
        // a rule a person types in a third session is still theirs
        let own = write(
            tmp.path(),
            "own.jsonl",
            &[
                json!({"type":"user","sessionId":"own","message":{"role":"user","content":"Nunca hagas push a main desde la rama de pruebas."}}),
                json!({"type":"assistant","sessionId":"own","message":{"role":"assistant","content":[{"type":"text","text":"Ok."}]}}),
            ],
        );
        ingest_transcript(&db, &own, "own").unwrap();
        assert_eq!(typed_active(&db), 1);
    }
}

#[cfg(test)]
mod episode_handover_tests {
    use crate::ingest::ingest_transcript;
    use muninn_core::db::{Db, Mode};
    use serde_json::json;
    use std::path::{Path, PathBuf};

    const TASK: &str =
        "Ordena el inventario de repuestos por proveedor y nunca toques la tabla de precios.";

    fn write(dir: &Path, name: &str, lines: &[serde_json::Value]) -> PathBuf {
        let p = dir.join(name);
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&p, text).unwrap();
        p
    }

    fn coordinator(dir: &Path) -> PathBuf {
        write(
            dir,
            "coord.jsonl",
            &[
                json!({"type":"user","sessionId":"coord","message":{"role":"user","content":"Reparte el trabajo del inventario."}}),
                json!({"type":"assistant","sessionId":"coord","message":{"role":"assistant","content":[
                    {"type":"tool_use","id":"t1","name":"Agent","input":{"description":"inventario","prompt":TASK}}]}}),
                json!({"type":"user","sessionId":"coord","message":{"role":"user","content":[
                    {"type":"tool_result","tool_use_id":"t1","content":"done"}]}}),
                json!({"type":"assistant","sessionId":"coord","message":{"role":"assistant","content":[{"type":"text","text":"Hecho."}]}}),
            ],
        )
    }

    fn worker(dir: &Path) -> PathBuf {
        write(
            dir,
            "worker.jsonl",
            &[
                json!({"type":"user","sessionId":"worker","message":{"role":"user","content":TASK}}),
                json!({"type":"assistant","sessionId":"worker","message":{"role":"assistant","content":[{"type":"text","text":"Ordenado por proveedor."}]}}),
            ],
        )
    }

    /// (id, invalid, origin, trust, body, invalidated_by, invalidated_via) of the worker's episodes
    type Row = (i64, i64, String, i64, String, Option<i64>, Option<String>);

    fn episodes(db: &Db) -> Vec<Row> {
        db.conn
            .prepare(
                "SELECT id, invalid, origin, trust, body, invalidated_by, invalidated_via FROM record \
                 WHERE kind='episode' AND session_id='worker' ORDER BY id",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })
            .unwrap()
            .flatten()
            .collect()
    }

    /// The worker's turn first, the usual order: its episode was written as the user's. When
    /// the coordinator's turn shows an agent wrote the prompt, that episode is retired with a
    /// cause and an agent's episode replaces it, linked as its heir; nothing stored is edited.
    #[test]
    fn a_worker_episode_found_to_be_an_agents_prompt_is_replaced_by_one() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let w = worker(tmp.path());
        let c = coordinator(tmp.path());
        ingest_transcript(&db, &w, "worker").unwrap();
        let before = episodes(&db);
        assert_eq!(before.len(), 1);
        assert!(before[0].4.starts_with("user: "), "{before:?}");
        ingest_transcript(&db, &c, "coord").unwrap();
        let after = episodes(&db);
        assert_eq!(after.len(), 2, "{after:?}");
        let (old, new) = (&after[0], &after[1]);
        assert_eq!(old.4, before[0].4, "the stored body is not edited");
        assert_eq!(old.2, "tool_observed", "nor its origin");
        assert_eq!(old.1, 1);
        assert_eq!(old.5, Some(new.0), "the agent's episode is its heir");
        assert_eq!(old.6.as_deref(), Some("agent_prompt"));
        assert_eq!((new.1, new.2.as_str(), new.3), (0, "agent_inferred", 0));
        assert!(new.4.starts_with("agent: "), "{}", new.4);

        // maintain, twice: the coordinator read again changes nothing
        for _ in 0..2 {
            db.conn
                .execute("DELETE FROM meta WHERE key LIKE 'wm:%'", [])
                .unwrap();
            ingest_transcript(&db, &c, "coord").unwrap();
            crate::ingest::ingest_transcript_released(&db, &w, "worker", None).unwrap();
            assert_eq!(episodes(&db), after, "idempotent");
        }

        // and the retired one is not what a question about the inventory gets back
        let terms: Vec<String> = ["inventario", "repuestos", "proveedor"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let hits = muninn_core::recall::recall(&db, &terms, 10, &std::collections::HashSet::new())
            .unwrap();
        assert!(
            hits.iter().all(|h| h.id != old.0),
            "{:?}",
            hits.iter().map(|h| h.id).collect::<Vec<_>>()
        );
        assert!(hits.iter().any(|h| h.id == new.0));
    }

    /// The coordinator's turn first: the agent's episode is written once, and nothing is
    /// retired or written twice.
    #[test]
    fn a_prompt_known_to_be_an_agents_is_written_once() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        ingest_transcript(&db, &coordinator(tmp.path()), "coord").unwrap();
        ingest_transcript(&db, &worker(tmp.path()), "worker").unwrap();
        let e = episodes(&db);
        assert_eq!(e.len(), 1, "{e:?}");
        assert_eq!((e[0].1, e[0].2.as_str(), e[0].3), (0, "agent_inferred", 0));
        assert!(e[0].4.starts_with("agent: "), "{}", e[0].4);
    }
}

#[cfg(test)]
mod own_rule_tests {
    use crate::ingest::ingest_transcript;
    use muninn_core::db::{Db, Mode};
    use serde_json::json;
    use std::path::{Path, PathBuf};

    const RULE: &str = "Nunca subas los archivos de configuración local al repositorio.";

    fn write(dir: &Path, name: &str, lines: &[serde_json::Value]) -> PathBuf {
        let p = dir.join(name);
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&p, text).unwrap();
        p
    }

    fn task() -> String {
        format!("Prepara la rama de la versión 2 del catálogo. {RULE}")
    }

    /// The user states a rule in their own session; a coordinator later copies it into a
    /// worker's task. The worker is ingested first, its sentence restates the user's record
    /// (moving it to the worker's turn), and then the coordinator's call shows the worker's
    /// prompt was an agent's. The user's rule must stay on record, back in the user's turn,
    /// as it does when the coordinator comes first.
    #[test]
    fn a_users_rule_an_agent_copied_survives_the_late_coordinator() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let own = write(
            tmp.path(),
            "own.jsonl",
            &[
                json!({"type":"user","sessionId":"own","timestamp":"2026-11-01T10:00:00Z","message":{"role":"user","content":RULE}}),
                json!({"type":"assistant","sessionId":"own","message":{"role":"assistant","content":[{"type":"text","text":"Entendido."}]}}),
            ],
        );
        let worker = write(
            tmp.path(),
            "worker.jsonl",
            &[
                json!({"type":"user","sessionId":"worker","timestamp":"2026-11-02T10:00:00Z","message":{"role":"user","content":task()}}),
                json!({"type":"assistant","sessionId":"worker","message":{"role":"assistant","content":[{"type":"text","text":"Rama lista."}]}}),
            ],
        );
        let coord = write(
            tmp.path(),
            "coord.jsonl",
            &[
                json!({"type":"user","sessionId":"coord","timestamp":"2026-11-02T09:59:00Z","message":{"role":"user","content":"Lanza la preparación del catálogo."}}),
                json!({"type":"assistant","sessionId":"coord","message":{"role":"assistant","content":[
                    {"type":"tool_use","id":"t1","name":"mcp__team__start","input":{"task":task()}}]}}),
                json!({"type":"user","sessionId":"coord","message":{"role":"user","content":[
                    {"type":"tool_result","tool_use_id":"t1","content":"started"}]}}),
                json!({"type":"assistant","sessionId":"coord","message":{"role":"assistant","content":[{"type":"text","text":"Lanzado."}]}}),
            ],
        );
        ingest_transcript(&db, &own, "own").unwrap();
        let (id, own_ref): (i64, String) = db
            .conn
            .query_row(
                "SELECT id, transcript_ref FROM record WHERE kind='invariant' AND invalid=0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        ingest_transcript(&db, &worker, "worker").unwrap();
        ingest_transcript(&db, &coord, "coord").unwrap();
        let (invalid, tref, session): (i64, String, String) = db
            .conn
            .query_row(
                "SELECT invalid, transcript_ref, session_id FROM record WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(invalid, 0, "the user's rule is still on record");
        assert_eq!((tref.as_str(), session.as_str()), (own_ref.as_str(), "own"));
        assert_eq!(
            db.count("SELECT count(*) FROM record_turn WHERE record_id IN (SELECT id FROM record WHERE invalid=0)")
                .unwrap(),
            0,
            "the worker's turn no longer counts as one that stated it"
        );
    }
}

#[cfg(test)]
mod replaced_rule_copied_tests {
    use crate::ingest::ingest_transcript;
    use muninn_core::db::{Db, Mode};
    use serde_json::json;
    use std::path::{Path, PathBuf};

    const A: &str = "We go with Postgres for the job queue.";
    const B: &str = "Actually, switch the job queue to Redis instead of Postgres.";

    fn task() -> String {
        format!("Write the migration notes for the queue. {A}")
    }

    fn write(dir: &Path, name: &str, lines: &[serde_json::Value]) -> PathBuf {
        let p = dir.join(name);
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&p, text).unwrap();
        p
    }

    fn turn(sid: &str, ts: &str, prompt: &str, reply: &str) -> Vec<serde_json::Value> {
        vec![
            json!({"type":"user","sessionId":sid,"timestamp":ts,"message":{"role":"user","content":prompt}}),
            json!({"type":"assistant","sessionId":sid,"message":{"role":"assistant","content":[{"type":"text","text":reply}]}}),
        ]
    }

    fn state(db: &Db, text: &str) -> (i64, i64, Option<i64>) {
        db.conn
            .query_row(
                "SELECT id, invalid, invalidated_by FROM record WHERE kind='decision' AND instr(object, ?1) > 0 \
                 ORDER BY id LIMIT 1",
                [text.trim_end_matches('.')],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }

    /// The user said A, then B replaced it. An agent copies A into a worker's task. Whatever
    /// the order in which the worker and the coordinator are ingested, B is the current word
    /// once both are on record, and A is retired with B as its heir.
    #[test]
    fn a_replaced_rule_an_agent_copied_does_not_come_back() {
        for worker_first in [true, false] {
            let tmp = tempfile::tempdir().unwrap();
            let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
            let mut own = turn("own", "2026-11-05T10:00:00Z", A, "Understood.");
            let p = write(tmp.path(), "own.jsonl", &own);
            ingest_transcript(&db, &p, "own").unwrap();
            own.extend(turn(
                "own",
                "2026-11-05T10:01:00Z",
                B,
                "Understood, switching the job queue from Postgres to Redis.",
            ));
            write(tmp.path(), "own.jsonl", &own);
            ingest_transcript(&db, &p, "own").unwrap();
            let (b_id, b_inv, _) = state(&db, B);
            assert_eq!((state(&db, A).1, b_inv), (1, 0), "B replaced A");

            let worker = write(
                tmp.path(),
                "worker.jsonl",
                &turn("worker", "2026-11-06T09:00:00Z", &task(), "Notes written."),
            );
            let mut c = turn(
                "coord",
                "2026-11-06T08:59:00Z",
                "Start the queue notes.",
                "Starting.",
            );
            c.insert(
                1,
                json!({"type":"assistant","sessionId":"coord","message":{"role":"assistant","content":[
                    {"type":"tool_use","id":"t1","name":"Agent","input":{"prompt":task()}}]}}),
            );
            c.insert(
                2,
                json!({"type":"user","sessionId":"coord","message":{"role":"user","content":[
                    {"type":"tool_result","tool_use_id":"t1","content":"done"}]}}),
            );
            let coord = write(tmp.path(), "coord.jsonl", &c);
            if worker_first {
                ingest_transcript(&db, &worker, "worker").unwrap();
                ingest_transcript(&db, &coord, "coord").unwrap();
            } else {
                ingest_transcript(&db, &coord, "coord").unwrap();
                ingest_transcript(&db, &worker, "worker").unwrap();
            }
            let (_, a_inv, a_heir) = state(&db, A);
            let (_, b_inv, _) = state(&db, B);
            assert_eq!(b_inv, 0, "B is current (worker first: {worker_first})");
            assert_eq!(
                (a_inv, a_heir),
                (1, Some(b_id)),
                "A is retired by B (worker first: {worker_first})"
            );
            assert_eq!(
                db.count("SELECT count(*) FROM record WHERE invalid=0 AND kind='episode' AND body LIKE '%Redis%'")
                    .unwrap(),
                1,
                "B's episode is served again (worker first: {worker_first})"
            );
        }
    }
}

#[cfg(test)]
mod older_statement_under_a_workers_row_tests {
    use crate::ingest::ingest_transcript;
    use muninn_core::db::{Db, Mode};
    use serde_json::json;
    use std::path::{Path, PathBuf};

    fn write(dir: &Path, name: &str, lines: &[serde_json::Value]) -> PathBuf {
        let p = dir.join(name);
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&p, text).unwrap();
        p
    }

    fn turn(sid: &str, ts: &str, prompt: &str, reply: &str) -> Vec<serde_json::Value> {
        vec![
            json!({"type":"user","sessionId":sid,"timestamp":ts,"message":{"role":"user","content":prompt}}),
            json!({"type":"assistant","sessionId":sid,"message":{"role":"assistant","content":[{"type":"text","text":reply}]}}),
        ]
    }

    /// A worker's session is read while nothing says it is an agent's, then an older session
    /// of the person's, then the coordinator. The person's statement, read after the worker's
    /// row that would replace it, is not retired under it: it stands beside the row, marked
    /// (`ingest::mark_against_later`), and the row is revoked as an agent's copy when the
    /// coordinator is read. In every read order of the three the person's statement is
    /// current at trust 3 and nothing of the worker's is, for each rule read from the older
    /// side: a bare quantity, the key, a correction said again.
    #[test]
    fn the_persons_older_statement_is_current_whatever_order_a_workers_copy_is_read_in() {
        // (rule, kind, the person's sentence, what the task carries, a word only the first
        //  holds, a word only the second holds, the two session ids)
        let cases = [
            (
                "quantity",
                "decision",
                "We use 3 attempts for the payment worker.",
                "Write the retry notes for the payment worker. bump to 7 attempts",
                "3 attempts",
                "7 attempts",
                ("own", "worker"),
            ),
            (
                "key",
                "invariant",
                "Never commit the local settings files of the staging environment to the shared repository before two reviews.",
                "Write the release notes. Never commit the local settings files of the staging environment to the shared repository before one review.",
                "two reviews",
                "one review",
                ("own", "worker"),
            ),
            // a correction replaces one of its own session: the two ids share their head
            (
                "correction",
                "correction",
                "No, the server port is 8080, not 9090.",
                "No, the server port is 8443 behind the proxy.",
                "8080",
                "8443",
                ("sess0001-own", "sess0001-worker"),
            ),
        ];
        let mut wrong = Vec::new();
        for (rule, kind, mine, task, old, new, (own_sid, worker_sid)) in cases {
            for order in [
                ["O", "W", "C"],
                ["O", "C", "W"],
                ["W", "O", "C"],
                ["W", "C", "O"],
                ["C", "O", "W"],
                ["C", "W", "O"],
            ] {
                let tmp = tempfile::tempdir().unwrap();
                let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
                let own = write(
                    tmp.path(),
                    "own.jsonl",
                    &turn(own_sid, "2026-11-01T10:00:00Z", mine, "Understood."),
                );
                let worker = write(
                    tmp.path(),
                    "worker.jsonl",
                    &turn(worker_sid, "2026-11-03T10:00:00Z", task, "Done."),
                );
                let mut c = turn(
                    "coord",
                    "2026-11-03T09:59:00Z",
                    "Start the notes for the release.",
                    "Started.",
                );
                c.insert(
                    1,
                    json!({"type":"assistant","sessionId":"coord","message":{"role":"assistant","content":[
                        {"type":"tool_use","id":"t1","name":"Agent","input":{"prompt":task}}]}}),
                );
                c.insert(
                    2,
                    json!({"type":"user","sessionId":"coord","message":{"role":"user","content":[
                        {"type":"tool_result","tool_use_id":"t1","content":"done"}]}}),
                );
                let coord = write(tmp.path(), "coord.jsonl", &c);
                for step in order {
                    match step {
                        "O" => ingest_transcript(&db, &own, own_sid).unwrap(),
                        "W" => ingest_transcript(&db, &worker, worker_sid).unwrap(),
                        _ => ingest_transcript(&db, &coord, "coord").unwrap(),
                    };
                }
                let active = |word: &str| {
                    db.count(&format!(
                        "SELECT count(*) FROM record WHERE invalid = 0 AND kind = '{kind}' \
                         AND origin = 'user_said' AND instr(object, '{word}') > 0"
                    ))
                    .unwrap()
                };
                let marked = db
                    .count("SELECT count(*) FROM record WHERE invalidated_via LIKE 'agent_unrestored%'")
                    .unwrap();
                // (the person's statement current, the worker's copy current, retired and marked)
                let end = (active(old), active(new), marked);
                if end != (1, 0, 0) {
                    wrong.push(format!("{rule} {}: {end:?}", order.concat()));
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "the person's statement is current, the worker's copy is not, nothing is marked: {wrong:?}"
        );
    }
    /// Two statements of the person's, the second replacing the first, both dated before a
    /// worker's row that would replace either, and the coordinator. In all 24 read orders the
    /// person's later statement is current at trust 3 and nothing of the worker's is, but
    /// one: second, worker, first, coordinator. There the worker's row retired the second in
    /// date order and the first was read before the coordinator, so the second stays retired
    /// and marked for `doctor`, the rule since Task 7 for a word read in between. The first
    /// statement is current only where it was read after the second (beside it, marked as in
    /// conflict). Every order ends as on the build that never retired an older record under a
    /// later one. Doing that lost statements here: nothing current in the order worker,
    /// second, first, coordinator, and both current, unmarked, in the order worker, first,
    /// second, coordinator.
    #[test]
    fn two_older_statements_under_a_workers_row_end_with_the_later_one_current() {
        // (rule, kind, first, second, what the task carries, a word of each, session ids)
        let cases = [
            (
                "quantity",
                "decision",
                "We use 3 attempts for the payment worker.",
                "bump to 5 attempts",
                "Write the retry notes for the payment worker. bump to 7 attempts",
                ["3 attempts", "5 attempts", "7 attempts"],
                ["own", "own2", "worker"],
            ),
            (
                "key",
                "invariant",
                "Never commit the local settings files of the staging environment to the shared repository before two reviews.",
                "Never commit the local settings files of the staging environment to the shared repository before three reviews.",
                "Write the release notes. Never commit the local settings files of the staging environment to the shared repository before one review.",
                ["two reviews", "three reviews", "one review"],
                ["own", "own2", "worker"],
            ),
            (
                "correction",
                "correction",
                "No, the server port is 8080, not 9090.",
                "No, the server port is 8081, not 9090.",
                "No, the server port is 8443 behind the proxy.",
                ["8080", "8081", "8443"],
                ["sess0001-own", "sess0001-own2", "sess0001-worker"],
            ),
        ];
        let mut orders = Vec::new();
        for a in 0..4 {
            for b in 0..4 {
                for c in 0..4 {
                    for d in 0..4 {
                        let o = [a, b, c, d];
                        if (0..4).all(|k| o.contains(&k)) {
                            orders.push(o);
                        }
                    }
                }
            }
        }
        let mut wrong = Vec::new();
        for (rule, kind, first, second, task, words, sids) in cases {
            for order in &orders {
                let tmp = tempfile::tempdir().unwrap();
                let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
                let files = [
                    write(
                        tmp.path(),
                        "own.jsonl",
                        &turn(sids[0], "2026-11-01T10:00:00Z", first, "Understood."),
                    ),
                    write(
                        tmp.path(),
                        "own2.jsonl",
                        &turn(sids[1], "2026-11-02T10:00:00Z", second, "Understood."),
                    ),
                    write(
                        tmp.path(),
                        "worker.jsonl",
                        &turn(sids[2], "2026-11-03T10:00:00Z", task, "Done."),
                    ),
                    {
                        let mut c = turn(
                            "coord",
                            "2026-11-03T09:59:00Z",
                            "Start the notes for the release.",
                            "Started.",
                        );
                        c.insert(
                            1,
                            json!({"type":"assistant","sessionId":"coord","message":{"role":"assistant","content":[
                                {"type":"tool_use","id":"t1","name":"Agent","input":{"prompt":task}}]}}),
                        );
                        c.insert(
                            2,
                            json!({"type":"user","sessionId":"coord","message":{"role":"user","content":[
                                {"type":"tool_result","tool_use_id":"t1","content":"done"}]}}),
                        );
                        write(tmp.path(), "coord.jsonl", &c)
                    },
                ];
                let names = [sids[0], sids[1], sids[2], "coord"];
                for i in order {
                    ingest_transcript(&db, &files[*i], names[*i]).unwrap();
                }
                let active = |word: &str| {
                    db.count(&format!(
                        "SELECT count(*) FROM record WHERE invalid = 0 AND kind = '{kind}' \
                         AND origin = 'user_said' AND instr(object, '{word}') > 0"
                    ))
                    .unwrap()
                };
                let at = |k: usize| order.iter().position(|i| *i == k).unwrap();
                // the first statement is current only where it was read after the second
                let first_stands = i64::from(at(0) > at(1));
                // second, worker, first, coordinator: a word read between the two
                let held = at(1) < at(2) && at(2) < at(0) && at(0) < at(3);
                let unrestored = db
                    .count(&format!(
                        "SELECT count(*) FROM record WHERE invalidated_via LIKE 'agent_unrestored%' \
                         AND kind = '{kind}' AND instr(object, '{}') > 0",
                        words[1]
                    ))
                    .unwrap();
                let end = (active(words[0]), active(words[1]), active(words[2]));
                if end != (first_stands, i64::from(!held), 0) || unrestored != i64::from(held) {
                    let tag: String = order.iter().map(|i| ["A", "E", "W", "C"][*i]).collect();
                    wrong.push(format!("{rule} {tag}: {end:?}"));
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "(first, second, the worker's) active; want (1 if the first was read after the second, 1, 0), \
             and the second retired and marked in the one order: {wrong:?}"
        );
    }
}

#[cfg(test)]
mod restatement_journal_tests {
    use crate::ingest::ingest_transcript;
    use muninn_core::db::{Db, Mode};
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    const A: &str = "We go with Postgres for the job queue.";
    const B: &str = "Actually, switch the job queue to Redis instead of Postgres.";
    const C: &str = "Actually, switch the job queue to SQS instead of Redis.";
    const X: &str = "x other";
    /// A second change only an agent's task states.
    const D: &str = "Actually, switch the job queue to Kafka instead of SQS.";
    /// A later word of the user's.
    const K: &str = "Actually, switch the job queue to NATS instead of Redis.";
    // Two subjects, so that a later word can replace one statement and not another.
    /// Both subjects.
    const Q: &str = "We go with Redis for the job queue and the session cache.";
    /// The job queue only.
    const S: &str = "Actually, switch the job queue to SQS.";
    /// The session cache only.
    const M: &str = "Actually, move the session cache over to Memcached cluster nodes.";
    /// The job queue only, in other words.
    const T: &str = "Actually, switch the job queue over to SQS instead.";
    /// The session cache only, in other words.
    const N: &str = "Actually, switch the session cache to Memcached instead of Redis.";
    /// Replaces A (the job queue) and brings in the session cache.
    const R: &str =
        "Actually, switch the job queue to Redis and keep the session cache on Redis too.";
    /// The job queue and the page cache.
    const F: &str = "We go with Postgres for the job queue and Varnish for the page cache.";
    /// The job queue only: replaces F.
    const G: &str = "Actually, switch the job queue to RabbitMQ.";
    /// The page cache only.
    const H: &str = "Actually, move the page cache over to Fastly edge nodes.";
    /// The job queue, and it brings in the page cache.
    const J: &str = "Actually, switch the job queue to Kafka and put the page cache on Varnish.";

    /// The TLS backend.
    const O: &str = "We go with openssl for the TLS backend.";
    /// A change of it that only a task states.
    const P: &str = "Actually, switch the TLS backend to boringssl.";
    /// The same change, in words that name the value the user had chosen.
    const P2: &str = "Actually, move the TLS backend off openssl onto boringssl.";
    /// The user's later word. It shares no word with O; the reply to it names what it replaces.
    const W: &str = "Let's go with rustls from now on.";
    /// The same word, to a reply that names the agent's value.
    const W2: &str = "Let's go with rustls for the proxy.";

    /// A quantity.
    const Q3: &str = "We use 3 attempts for the payment worker.";
    /// A change of it.
    const Q5: &str = "Actually, bump the payment worker to 5 attempts.";
    /// The user's later word: it names the unit and no subject.
    const Q7: &str = "bump to 7 attempts";

    const NAMES: [(&str, &str); 24] = [
        ("W2", W2),
        ("Q3", Q3),
        ("Q5", Q5),
        ("Q7", Q7),
        ("P2", P2),
        ("O", O),
        ("P", P),
        ("W", W),
        ("A", A),
        ("B", B),
        ("C", C),
        ("D", D),
        ("K", K),
        ("X", X),
        ("Q", Q),
        ("S", S),
        ("M", M),
        ("T", T),
        ("N", N),
        ("R", R),
        ("F", F),
        ("G", G),
        ("H", H),
        ("J", J),
    ];

    /// (invalid, heir's name) of the first decision record holding each statement.
    type State = BTreeMap<&'static str, (i64, Option<&'static str>)>;

    #[derive(Clone, Copy, Debug)]
    enum Step {
        /// Worker `n` ends its turn; its prompt is task `n`.
        Worker(usize),
        /// A coordinator that handed over these tasks ends its turn.
        Coord(&'static [usize]),
        /// The user says this in their own session, after the workers' turns were written.
        Say(&'static str),
        /// Worker `n`'s session gains one turn per task listed, and is read once.
        Turns(usize, &'static [usize]),
        /// Worker `n` is given the first task and hands the others on: its prompt is an
        /// agent's, and it is a coordinator too.
        Relay(usize, usize, &'static [usize]),
        /// A coordinator whose own prompt, in the turn that hands the tasks over, is a statement
        /// of the user's.
        Asks(&'static [usize], &'static str),
        /// Something outside ingest happens to the store: see `Scene::done`.
        Done(Outside),
    }
    use Step::{Asks, Coord, Done, Relay, Say, Turns, Worker};

    #[derive(Clone, Copy, Debug)]
    enum Outside {
        /// The person revokes this statement of theirs (`muninn revoke`, the real function).
        Revoke(&'static str),
        /// What maintain's commit pass does to this statement when a commit took its value
        /// out: the pass's event, and its `UPDATE ... WHERE id = ? AND invalid = 0`.
        Gone(&'static str),
        /// A pass that retires records ran and found nothing of its own to retire.
        Pass,
        /// Everything is exported, with the retired, and imported into a new store, which
        /// takes the place of this one. The transcripts read so far are not read again.
        Moved,
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Extra {
        None,
        /// B also replaced a statement from another turn.
        OtherReplaced,
        /// Every transcript is read twice more with the watermarks cleared.
        ReadTwice,
    }

    struct Shape {
        name: String,
        /// The user's first statement.
        first: &'static str,
        /// What the user said after A, before any worker ran.
        said: &'static [&'static str],
        /// Task `n` is worker `n`'s prompt.
        tasks: Vec<String>,
        /// Ingest orders that must agree. The last one has every coordinator first.
        orders: Vec<Vec<Step>>,
        /// The one statement active at the end.
        current: &'static str,
        /// Journal rows the first order writes at least: the shape restates what it claims to.
        restated: i64,
        extra: Extra,
    }

    /// A task that carries one of the user's statements.
    fn task(n: usize, carried: &str) -> String {
        let lead = [
            "Write the migration notes for the queue.",
            "Review the deployment scripts for the queue.",
            "List the open tickets about the queue.",
        ][n];
        format!("{lead} {carried}")
    }

    /// A task long enough that its turn's episode is not a short one: a rule that takes "the most
    /// recent" record of the user's then passes over the episode and meets the statement.
    fn long_task(carried: &str) -> String {
        let filler = "Read the export module and list each place the retry loop is set up, then \
                      write the upgrade notes in the docs folder, one paragraph per call site, and \
                      leave the headings there as they are. ";
        format!(
            "Context for the worker job follows. {}{carried}",
            filler.repeat(5)
        )
    }

    /// The tasks by the hash the undo sees, lowest first, so a shape can put either one first
    /// without relying on how two texts happen to sort.
    fn by_hash(mut tasks: Vec<String>) -> Vec<String> {
        tasks.sort_by_key(|t| super::task_hashes("Agent", &json!({ "prompt": t }))[0].clone());
        tasks
    }

    struct Scene {
        dir: tempfile::TempDir,
        db: Db,
        own: Vec<serde_json::Value>,
        workers: BTreeMap<usize, Vec<serde_json::Value>>,
        read: Vec<(PathBuf, String)>,
        /// How many times the store was exported and imported into a new one.
        moves: usize,
    }

    impl Scene {
        fn new() -> Scene {
            let dir = tempfile::tempdir().unwrap();
            let db = Db::open(&dir.path().join("m.db"), Mode::ReadWrite).unwrap();
            Scene {
                dir,
                db,
                own: Vec::new(),
                workers: BTreeMap::new(),
                read: Vec::new(),
                moves: 0,
            }
        }

        fn step(&mut self, step: Step, tasks: &[String]) {
            match step {
                Worker(n) => self.turns(n, &[n], tasks),
                Turns(n, given) => self.turns(n, given, tasks),
                Relay(n, given, hands) => self.relay(n, given, hands, tasks),
                Coord(hands) => self.coordinator(hands, tasks, "Start the queue work."),
                Asks(hands, text) => self.coordinator(hands, tasks, text),
                Say(text) => self.say("06", text),
                Done(what) => self.done(what),
            }
        }

        fn file(&mut self, sid: &str, lines: &[serde_json::Value]) {
            let p = self.dir.path().join(format!("{sid}.jsonl"));
            let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
            std::fs::write(&p, text).unwrap();
            ingest_transcript(&self.db, &p, sid).unwrap();
            if !self.read.iter().any(|(_, s)| s == sid) {
                self.read.push((p, sid.to_string()));
            }
        }

        fn turn(sid: &str, ts: &str, prompt: &str, reply: &str) -> Vec<serde_json::Value> {
            vec![
                json!({"type":"user","sessionId":sid,"timestamp":ts,"message":{"role":"user","content":prompt}}),
                json!({"type":"assistant","sessionId":sid,"message":{"role":"assistant","content":[{"type":"text","text":reply}]}}),
            ]
        }

        fn say(&mut self, day: &str, text: &str) {
            let n = self.own.len() / 2;
            let reply = match text {
                A if n > 0 => "Back to Postgres then.",
                B => "Understood, switching the job queue from Postgres to Redis.",
                W => "Got it, switching the TLS backend from openssl to rustls.",
                W2 => "Got it, switching the TLS backend from boringssl to rustls.",
                C => "Understood, switching the job queue from Redis to SQS.",
                _ => "Understood.",
            };
            let ts = format!("2026-11-{day}T10:{n:02}:00Z");
            self.own.extend(Self::turn("own", &ts, text, reply));
            let own = self.own.clone();
            self.file("own", &own);
        }

        fn turns(&mut self, n: usize, given: &[usize], tasks: &[String]) {
            let sid = format!("w{n}");
            let mut lines = self.workers.remove(&n).unwrap_or_default();
            for t in given {
                let ts = format!("2026-11-06T09:{:02}:00Z", (*t).max(n));
                lines.extend(Self::turn(&sid, &ts, &tasks[*t], "Done."));
            }
            self.file(&sid, &lines);
            self.workers.insert(n, lines);
        }

        /// One turn: `prompt`, then one tool call per task handed over.
        fn handing(
            sid: &str,
            ts: &str,
            prompt: &str,
            hands: &[usize],
            tasks: &[String],
        ) -> Vec<serde_json::Value> {
            let mut c = Self::turn(sid, ts, prompt, "Starting.");
            for (i, t) in hands.iter().enumerate() {
                let id = format!("t{i}");
                c.insert(
                    1 + 2 * i,
                    json!({"type":"assistant","sessionId":sid,"message":{"role":"assistant","content":[
                        {"type":"tool_use","id":id,"name":"Agent","input":{"prompt":tasks[*t]}}]}}),
                );
                c.insert(
                    2 + 2 * i,
                    json!({"type":"user","sessionId":sid,"message":{"role":"user","content":[
                        {"type":"tool_result","tool_use_id":id,"content":"done"}]}}),
                );
            }
            c
        }

        fn relay(&mut self, n: usize, given: usize, hands: &[usize], tasks: &[String]) {
            let sid = format!("w{n}");
            let ts = format!("2026-11-06T09:{n:02}:00Z");
            self.file(&sid, &Self::handing(&sid, &ts, &tasks[given], hands, tasks));
        }

        fn coordinator(&mut self, hands: &[usize], tasks: &[String], prompt: &str) {
            let sid = format!(
                "coord{}",
                hands.iter().map(usize::to_string).collect::<String>()
            );
            let c = Self::handing(&sid, "2026-11-06T08:59:00Z", prompt, hands, tasks);
            self.file(&sid, &c);
        }

        fn done(&mut self, what: Outside) {
            let id_of = |db: &Db, text: &str| -> i64 {
                db.conn
                    .query_row(
                        "SELECT id FROM record WHERE kind='decision' AND (object = ?1 OR object = ?2) ORDER BY id LIMIT 1",
                        [text, text.trim_end_matches('.')],
                        |r| r.get(0),
                    )
                    .unwrap()
            };
            match what {
                Outside::Revoke(text) => {
                    muninn_core::filter::revoke(&self.db, id_of(&self.db, text), "test").unwrap();
                }
                Outside::Gone(text) => {
                    let id = id_of(&self.db, text);
                    muninn_core::db::note_event(&self.db.conn, "commits").unwrap();
                    self.db
                        .conn
                        .execute(
                            "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_via='gone:x' WHERE id=?1 AND invalid=0",
                            [id],
                        )
                        .unwrap();
                }
                Outside::Pass => muninn_core::db::note_event(&self.db.conn, "pass").unwrap(),
                Outside::Moved => {
                    let out = self.dir.path().join(format!("export{}.jsonl", self.moves));
                    muninn_core::project::export_jsonl(&self.db, &out, true).unwrap();
                    self.moves += 1;
                    let next = self.dir.path().join(format!("m{}.db", self.moves));
                    let db = Db::open(&next, Mode::ReadWrite).unwrap();
                    muninn_core::project::import_with(&db, &out, true).unwrap();
                    // what the new store has to know to read the same transcripts on: where
                    // each was read to, and who wrote which prompt
                    db.conn
                        .execute("ATTACH DATABASE ?1 AS old", [self.old_path()])
                        .unwrap();
                    db.conn
                        .execute_batch(
                            "INSERT OR REPLACE INTO meta SELECT * FROM old.meta WHERE key LIKE 'wm:%' OR key LIKE 'released:%'; \
                             INSERT OR IGNORE INTO agent_task SELECT * FROM old.agent_task; \
                             INSERT OR IGNORE INTO turn_prompt SELECT * FROM old.turn_prompt; \
                             DETACH DATABASE old;",
                        )
                        .unwrap();
                    self.db = db;
                }
            }
        }

        fn old_path(&self) -> String {
            let name = match self.moves {
                1 => "m.db".to_string(),
                n => format!("m{}.db", n - 1),
            };
            self.dir.path().join(name).display().to_string()
        }

        fn state(&self) -> State {
            let db = &self.db;
            let id_of = |t: &str| -> Option<i64> {
                db.conn
                    .query_row(
                        "SELECT id FROM record WHERE kind='decision' AND (object = ?1 OR object = ?2) ORDER BY id LIMIT 1",
                        [t, t.trim_end_matches('.')],
                        |r| r.get(0),
                    )
                    .ok()
            };
            let names = NAMES;
            let ids: Vec<(&str, i64)> = names
                .iter()
                .filter_map(|(n, t)| id_of(t).map(|i| (*n, i)))
                .collect();
            let name_of = |id: i64| {
                ids.iter()
                    .find(|(_, i)| *i == id)
                    .map(|(n, _)| *n)
                    .unwrap_or("?")
            };
            ids.iter()
                .map(|(n, id)| {
                    let (inv, by): (i64, Option<i64>) = db
                        .conn
                        .query_row(
                            "SELECT invalid, invalidated_by FROM record WHERE id=?1",
                            [id],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .unwrap();
                    (*n, (inv, by.map(name_of)))
                })
                .collect()
        }
    }

    /// How an order ends.
    #[derive(Debug)]
    struct End {
        state: State,
        /// The active rows of the user's own session, as kind and text: their statements and
        /// the episodes of their turns.
        own_rows: BTreeSet<String>,
        /// The rows left retired as "retired by an agent's statement, not restored", as kind
        /// and text, and their ids.
        marked: BTreeSet<String>,
        /// The statements `doctor` counts, the same way.
        doctor: BTreeSet<String>,
        /// Marked rows that a worker's turn wrote as the user's: they are the agent's.
        marked_of_a_worker: i64,
        /// What a worker's turn left active that reads as the user's, once every coordinator is
        /// on record: a typed statement, or an episode labelled `user:`.
        left_as_the_users: i64,
    }

    /// The store after the user's first statements and the steps of one order.
    fn play(shape: &Shape, order: &[Step]) -> Scene {
        let mut s = Scene::new();
        s.say("05", shape.first);
        for text in shape.said {
            s.say("05", text);
        }
        if shape.extra == Extra::OtherReplaced {
            let b: i64 = s
                .db
                .conn
                .query_row(
                    "SELECT id FROM record WHERE kind='decision' AND instr(object, 'Redis') > 0",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            s.db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,transcript_ref,dedup_hash,created_at,invalid,invalid_reason,invalidated_by) \
                     VALUES('decision','k','is','x other','x other','user_said',3,'own','elsewhere:1','hx',1,1,'superseded',?1)",
                    [b],
                )
                .unwrap();
        }
        for step in order {
            s.step(*step, &shape.tasks);
        }
        if shape.extra == Extra::ReadTwice {
            for _ in 0..2 {
                s.db.conn
                    .execute("DELETE FROM meta WHERE key LIKE 'wm:%'", [])
                    .unwrap();
                for (p, sid) in s.read.clone() {
                    ingest_transcript(&s.db, &p, &sid).unwrap();
                }
            }
        }
        s
    }

    const MARKED: &str = "invalid=1 AND invalidated_via LIKE 'agent_unrestored%'";

    /// How the order ends, and how many journal rows it wrote.
    fn run(shape: &Shape, order: &[Step]) -> (End, i64) {
        let s = play(shape, order);
        let count = |sql: &str| s.db.count(sql).unwrap();
        let rows = |filter: &str| -> BTreeSet<String> {
            s.db.conn
                .prepare(&format!(
                    "SELECT kind || ':' || object FROM record WHERE {filter}"
                ))
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(
            count("SELECT count(*) FROM record WHERE invalidated_by = id"),
            0,
            "{}: a record is its own heir, {order:?}",
            shape.name
        );
        let end = End {
            state: s.state(),
            own_rows: rows("invalid=0 AND session_id='own'"),
            marked: rows(MARKED),
            doctor: muninn_core::health::unrestored(&s.db)
                .iter()
                .map(|id| {
                    s.db.conn
                        .query_row(
                            "SELECT kind || ':' || object FROM record WHERE id=?1",
                            [id],
                            |r| r.get::<_, String>(0),
                        )
                        .unwrap()
                })
                .collect(),
            marked_of_a_worker: count(&format!(
                "SELECT count(*) FROM record WHERE {MARKED} AND session_id LIKE 'w%' \
                 AND origin IN ('user_said', 'tool_observed') \
                 AND NOT EXISTS (SELECT 1 FROM restatement j WHERE j.record_id = record.id)"
            )),
            left_as_the_users: count(
                "SELECT count(*) FROM record WHERE invalid=0 AND session_id LIKE 'w%' \
                 AND (origin='user_said' OR (kind='episode' AND origin='tool_observed'))",
            ),
        };
        (end, count("SELECT count(*) FROM restatement"))
    }

    /// Whether the user speaks while a worker's turn is on record and its coordinator's is not.
    ///
    /// A word of the user's read before a worker's turn counts too, when that turn is then read
    /// before its coordinator's (R2): every `Say` of a scene is dated after the workers' turns,
    /// and what the store holds no longer depends on which was read first. Read either way,
    /// the word was said after the worker's prompt was typed and before it was found out.
    fn spoken_mid_way(order: &[Step], tasks: &[String]) -> bool {
        let (mut waiting, mut handed) = (BTreeSet::new(), BTreeSet::new());
        let mut said = false;
        for step in order {
            let (typed, hands): (Vec<usize>, &[usize]) = match step {
                Worker(n) => (vec![*n], &[]),
                Turns(_, given) => (given.to_vec(), &[]),
                Relay(_, given, hands) => (vec![*given], hands),
                Coord(hands) => (Vec::new(), hands),
                // the coordinator's own word was typed before its worker ran: it is not a word
                // said between those two turns. It is one for any other worker whose
                // coordinator has not been read.
                Asks(hands, _) => {
                    if waiting
                        .iter()
                        .any(|t| !hands.iter().any(|h| &tasks[*h] == *t))
                    {
                        return true;
                    }
                    (Vec::new(), hands)
                }
                Say(_) if waiting.is_empty() => {
                    said = true;
                    continue;
                }
                Say(_) | Done(_) if waiting.is_empty() => continue,
                Say(_) | Done(_) => return true,
            };
            for t in typed {
                if !handed.contains(&tasks[t]) {
                    if said {
                        return true;
                    }
                    waiting.insert(&tasks[t]);
                }
            }
            for h in hands {
                handed.insert(&tasks[*h]);
                waiting.remove(&tasks[*h]);
            }
        }
        false
    }

    /// The statements of the user's in a shape: what they said before and during it. A
    /// statement only an agent's task made is on record in one order and absent in the other.
    fn the_users(shape: &Shape, st: &State) -> State {
        let said = |text: &str| {
            text == shape.first
                || text == X
                || shape.said.contains(&text)
                || shape
                    .orders
                    .iter()
                    .flatten()
                    .any(|s| matches!(s, Say(t) | Asks(_, t) if *t == text))
        };
        let texts = NAMES;
        st.iter()
            .filter(|(n, _)| texts.iter().any(|(name, text)| name == *n && said(text)))
            .map(|(n, v)| (*n, *v))
            .collect()
    }

    fn active(st: &State) -> Vec<&'static str> {
        st.iter()
            .filter(|(_, (inv, _))| *inv == 0)
            .map(|(n, _)| *n)
            .collect()
    }

    fn shape(name: &str, tasks: Vec<String>, orders: Vec<Vec<Step>>, restated: i64) -> Shape {
        Shape {
            name: name.to_string(),
            first: A,
            said: &[B],
            tasks,
            orders,
            current: "B",
            restated,
            extra: Extra::None,
        }
    }

    /// Every order of three things.
    const PERMS: [[usize; 3]; 6] = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];

    fn strict_shapes() -> Vec<Shape> {
        let one = || vec![task(0, A)];
        let pair = || vec![vec![Worker(0), Coord(&[0])], vec![Coord(&[0]), Worker(0)]];
        let mut shapes = vec![
            shape("A, B, a copy of A", one(), pair(), 1),
            Shape {
                extra: Extra::OtherReplaced,
                ..shape("B also replaced another statement", one(), pair(), 1)
            },
            Shape {
                said: &[B, A],
                current: "A",
                ..shape("A, B, A again, a copy of A", one(), pair(), 1)
            },
            Shape {
                extra: Extra::ReadTwice,
                ..shape("everything read twice more", one(), pair(), 1)
            },
        ];
        // two workers, different tasks that both carry A, one coordinator: the first worker's
        // task is the one whose hash sorts first, then the one whose hash sorts last
        let two = by_hash(vec![task(0, A), task(1, A)]);
        for (name, tasks) in [
            ("first", two.clone()),
            ("last", two.iter().rev().cloned().collect::<Vec<_>>()),
        ] {
            shapes.push(shape(
                &format!("two workers, one coordinator, the first worker's hash sorts {name}"),
                tasks,
                vec![
                    vec![Worker(0), Worker(1), Coord(&[0, 1])],
                    vec![Worker(0), Coord(&[0, 1]), Worker(1)],
                    vec![Coord(&[0, 1]), Worker(0), Worker(1)],
                ],
                2,
            ));
        }
        // the same two workers, one coordinator each, arriving in each order
        shapes.push(shape(
            "two workers, two coordinators",
            two.clone(),
            vec![
                vec![Worker(0), Worker(1), Coord(&[0]), Coord(&[1])],
                vec![Worker(0), Worker(1), Coord(&[1]), Coord(&[0])],
                vec![Worker(0), Coord(&[0]), Worker(1), Coord(&[1])],
                vec![Worker(0), Coord(&[1]), Worker(1), Coord(&[0])],
                vec![Coord(&[0]), Worker(0), Worker(1), Coord(&[1])],
                vec![Coord(&[1]), Worker(0), Worker(1), Coord(&[0])],
                vec![Coord(&[0]), Coord(&[1]), Worker(0), Worker(1)],
            ],
            2,
        ));
        shapes.push(Shape {
            extra: Extra::ReadTwice,
            ..shape(
                "two workers, two coordinators, everything read twice more",
                two.clone(),
                vec![
                    vec![Worker(0), Worker(1), Coord(&[0]), Coord(&[1])],
                    vec![Coord(&[0]), Coord(&[1]), Worker(0), Worker(1)],
                ],
                2,
            )
        });
        // the user says A again while a copy of A is on record: that word is theirs and stands,
        // whichever coordinator arrives after it
        shapes.push(Shape {
            current: "A",
            ..shape(
                "a copy of A, then the user says A again",
                one(),
                vec![
                    vec![Worker(0), Say(A), Coord(&[0])],
                    vec![Coord(&[0]), Worker(0), Say(A)],
                ],
                2,
            )
        });
        shapes.push(Shape {
            current: "A",
            ..shape(
                "two copies of A, the user says A again between the two coordinators",
                two.clone(),
                vec![
                    vec![Worker(0), Worker(1), Coord(&[0]), Say(A), Coord(&[1])],
                    vec![Worker(0), Worker(1), Coord(&[1]), Say(A), Coord(&[0])],
                    vec![Worker(0), Worker(1), Say(A), Coord(&[0]), Coord(&[1])],
                    vec![Coord(&[0]), Coord(&[1]), Worker(0), Worker(1), Say(A)],
                ],
                3,
            )
        });
        shapes.push(shape(
            "two workers, the same task",
            vec![task(0, A), task(0, A)],
            vec![
                vec![Worker(0), Worker(1), Coord(&[0])],
                vec![Coord(&[0]), Worker(0), Worker(1)],
            ],
            2,
        ));
        // three workers: one coordinator with the tasks in every hash order, then three
        // coordinators arriving in every order
        let three = by_hash(vec![task(0, A), task(1, A), task(2, A)]);
        for p in PERMS {
            shapes.push(shape(
                &format!("three workers, one coordinator, hash order {p:?}"),
                p.iter().map(|i| three[*i].clone()).collect(),
                vec![
                    vec![Worker(0), Worker(1), Worker(2), Coord(&[0, 1, 2])],
                    vec![Coord(&[0, 1, 2]), Worker(0), Worker(1), Worker(2)],
                ],
                3,
            ));
        }
        const HANDS: [&[usize]; 3] = [&[0], &[1], &[2]];
        let mut orders: Vec<Vec<Step>> = PERMS
            .iter()
            .map(|p| {
                let mut o = vec![Worker(0), Worker(1), Worker(2)];
                o.extend(p.iter().map(|i| Coord(HANDS[*i])));
                o
            })
            .collect();
        orders.push(vec![
            Coord(&[0]),
            Coord(&[1]),
            Coord(&[2]),
            Worker(0),
            Worker(1),
            Worker(2),
        ]);
        shapes.push(shape("three workers, three coordinators", three, orders, 3));
        // the copies restate different statements. A, B, C: worker 0 copies B, which retires C;
        // worker 1 copies C, which comes back beside B (B still holds A).
        let across = |first: &'static str, second: &'static str| {
            vec![
                format!("Draft the rollout checklist for the queue. {first}"),
                format!("Update the runbook for the queue. {second}"),
            ]
        };
        let each_order = || {
            vec![
                vec![Worker(0), Worker(1), Coord(&[0, 1])],
                vec![Worker(0), Worker(1), Coord(&[0]), Coord(&[1])],
                vec![Worker(0), Worker(1), Coord(&[1]), Coord(&[0])],
                vec![Worker(0), Coord(&[1]), Worker(1), Coord(&[0])],
                vec![Coord(&[0]), Coord(&[1]), Worker(0), Worker(1)],
            ]
        };
        shapes.push(Shape {
            said: &[B, C],
            current: "C",
            ..shape(
                "a copy of B, then a copy of C that comes back beside it",
                across(B, C),
                each_order(),
                2,
            )
        });
        // A, B: worker 0 copies B (moved to its turn); worker 1 copies A, which retires B and
        // the episode of worker 0's turn with it
        shapes.push(shape(
            "a copy of B, then a copy of A that retires it",
            across(B, A),
            each_order(),
            2,
        ));
        // Three copies: of A, of B, of both. The third restates A again and retires B again, so
        // B is in the list of what the first copy retired and of what the third did. When the
        // first copy's row has to wait, what it leaves marked must be what it retired, and not
        // what the third copy retired after it.
        shapes.push(shape(
            "copies of A, of B and of both",
            vec![task(0, A), task(1, B), format!("{} {B}", task(2, A))],
            vec![
                vec![
                    Worker(0),
                    Worker(1),
                    Worker(2),
                    Coord(&[1]),
                    Coord(&[0]),
                    Coord(&[2]),
                ],
                vec![
                    Worker(0),
                    Worker(1),
                    Worker(2),
                    Coord(&[0]),
                    Coord(&[2]),
                    Coord(&[1]),
                ],
                vec![
                    Worker(0),
                    Worker(1),
                    Worker(2),
                    Coord(&[2]),
                    Coord(&[1]),
                    Coord(&[0]),
                ],
                vec![
                    Coord(&[0]),
                    Coord(&[1]),
                    Coord(&[2]),
                    Worker(0),
                    Worker(1),
                    Worker(2),
                ],
            ],
            3,
        ));
        // A copy of A, then a copy of B that also states a change, then a copy of A again. B is
        // left marked twice over: as it stands, by the change that replaced it, and in the
        // state the second copy would return it to, by the first copy. The two marks have each
        // their own link; one must not take the place of the other.
        shapes.push(shape(
            "a record marked as it stands and in the state a copy would return it to",
            vec![task(0, A), format!("{} {C}", task(1, B)), task(2, A)],
            vec![
                vec![
                    Worker(0),
                    Worker(1),
                    Worker(2),
                    Coord(&[1]),
                    Coord(&[0]),
                    Coord(&[2]),
                ],
                vec![
                    Coord(&[0]),
                    Coord(&[1]),
                    Coord(&[2]),
                    Worker(0),
                    Worker(1),
                    Worker(2),
                ],
            ],
            3,
        ));
        // a task states a change the user never made: typed from the worker's turn it replaces
        // B, and when it is revoked B and the episode of B's turn come back
        shapes.push(shape(
            "a task states a change the user never made",
            vec![task(0, C)],
            pair(),
            0,
        ));
        shapes.push(shape(
            "a change the user never made, then a copy of B",
            vec![task(0, C), task(1, B)],
            each_order(),
            1,
        ));
        shapes.push(shape(
            "a copy of A, then a change the user never made",
            vec![task(0, A), task(1, C)],
            each_order(),
            1,
        ));
        // each agent's change replaces the one before it; revoking them unwinds to B
        shapes.push(shape(
            "two tasks each state a change the user never made",
            vec![task(0, C), task(1, D)],
            each_order(),
            0,
        ));
        // two tasks handed to the same worker session: two turns of one transcript, read
        // together and one at a time
        for (name, tasks) in [
            ("first", two.clone()),
            ("last", two.iter().rev().cloned().collect::<Vec<_>>()),
        ] {
            shapes.push(shape(
                &format!("two tasks to one worker session, the first task's hash sorts {name}"),
                tasks,
                vec![
                    vec![Turns(0, &[0, 1]), Coord(&[0, 1])],
                    vec![Turns(0, &[0]), Turns(0, &[1]), Coord(&[0, 1])],
                    vec![Turns(0, &[0]), Turns(0, &[1]), Coord(&[0]), Coord(&[1])],
                    vec![Turns(0, &[0]), Turns(0, &[1]), Coord(&[1]), Coord(&[0])],
                    vec![Turns(0, &[0]), Coord(&[1]), Turns(0, &[1]), Coord(&[0])],
                    vec![Turns(0, &[0]), Coord(&[0]), Turns(0, &[1]), Coord(&[1])],
                    vec![Coord(&[0, 1]), Turns(0, &[0, 1])],
                ],
                2,
            ));
        }
        shapes.push(shape(
            "two tasks to one worker session, one a change the user never made",
            vec![task(0, A), task(1, C)],
            vec![
                vec![Turns(0, &[0, 1]), Coord(&[0, 1])],
                vec![Turns(0, &[0]), Turns(0, &[1]), Coord(&[1]), Coord(&[0])],
                vec![Turns(0, &[0]), Turns(0, &[1]), Coord(&[0]), Coord(&[1])],
                vec![Coord(&[0, 1]), Turns(0, &[0, 1])],
            ],
            1,
        ));
        // a worker that is itself a coordinator: worker 2 is given task 0 and hands task 1 to
        // worker 1; both tasks carry A
        shapes.push(shape(
            "a worker that hands a task on",
            two.clone(),
            vec![
                vec![Worker(1), Relay(2, 0, &[1]), Coord(&[0])],
                vec![Relay(2, 0, &[1]), Worker(1), Coord(&[0])],
                vec![Worker(1), Coord(&[0]), Relay(2, 0, &[1])],
                vec![Relay(2, 0, &[1]), Coord(&[0]), Worker(1)],
                vec![Coord(&[0]), Worker(1), Relay(2, 0, &[1])],
                vec![Coord(&[0]), Relay(2, 0, &[1]), Worker(1)],
            ],
            2,
        ));
        // worker 1 states a change, which replaces B and the episode of worker 0's turn; worker
        // 2 copies B, which comes back beside the change. The change is revoked before that
        // copy is undone, so the copy's journal row must not return B to "replaced by it".
        shapes.push(shape(
            "a copy of B put back beside a change that is revoked first",
            vec![task(0, A), task(1, C), task(2, B)],
            vec![
                vec![
                    Worker(0),
                    Coord(&[0]),
                    Worker(1),
                    Worker(2),
                    Coord(&[1]),
                    Coord(&[2]),
                ],
                vec![
                    Coord(&[0]),
                    Worker(0),
                    Worker(1),
                    Worker(2),
                    Coord(&[1]),
                    Coord(&[2]),
                ],
                vec![
                    Coord(&[0]),
                    Worker(0),
                    Worker(1),
                    Worker(2),
                    Coord(&[2]),
                    Coord(&[1]),
                ],
                vec![
                    Coord(&[0]),
                    Coord(&[1]),
                    Coord(&[2]),
                    Worker(0),
                    Worker(1),
                    Worker(2),
                ],
            ],
            1,
        ));
        // the same, and worker 2's task carries the change too: the change is moved to worker
        // 2's turn, so it is revoked only when that move is undone, in the middle of the batch
        // that then undoes the copy of B
        shapes.push(shape(
            "a copy of B put back beside a change that the same task copies",
            vec![task(0, A), task(1, C), format!("{} {C}", task(2, B))],
            vec![
                vec![
                    Worker(0),
                    Coord(&[0]),
                    Worker(1),
                    Worker(2),
                    Coord(&[1]),
                    Coord(&[2]),
                ],
                vec![
                    Coord(&[0]),
                    Worker(0),
                    Worker(1),
                    Worker(2),
                    Coord(&[1]),
                    Coord(&[2]),
                ],
                vec![
                    Coord(&[0]),
                    Coord(&[1]),
                    Coord(&[2]),
                    Worker(0),
                    Worker(1),
                    Worker(2),
                ],
            ],
            2,
        ));
        // Worker 0's task states S, which replaces Q. Worker 1's task states M, which would have
        // replaced Q and does not touch S. When S is revoked, Q comes back and M, not yet known
        // to be an agent's, replaces it; when M is revoked, Q comes back again.
        shapes.push(Shape {
            first: Q,
            said: &[],
            current: "Q",
            ..shape(
                "what comes back is replaced by a later statement that is revoked in its turn",
                vec![task(0, S), task(1, M)],
                each_order(),
                0,
            )
        });
        shapes.push(shape(
            "a worker that hands on a change the user never made",
            vec![task(0, A), task(1, C)],
            vec![
                vec![Worker(1), Relay(2, 0, &[1]), Coord(&[0])],
                vec![Relay(2, 0, &[1]), Worker(1), Coord(&[0])],
                vec![Worker(1), Coord(&[0]), Relay(2, 0, &[1])],
                vec![Coord(&[0]), Relay(2, 0, &[1]), Worker(1)],
            ],
            0,
        ));
        // C1. The person types a statement in the turn whose agent hands on a task that copies
        // it. Read worker first, the worker's turn types the sentence, the coordinator's turn
        // revokes that row as an agent's, and the person's own sentence then meets the revoked
        // row by its hash: it is theirs, said for the first time, and replaces what it replaces.
        // Here the sentence changes what stands (C replaces B); `mid_way_shapes` has one on a
        // subject nothing was said about.
        shapes.push(Shape {
            current: "C",
            ..shape(
                "the coordinator's own prompt states the change the task copies",
                vec![task(0, C)],
                vec![
                    vec![Worker(0), Asks(&[0], C)],
                    vec![Asks(&[0], C), Worker(0)],
                ],
                1,
            )
        });
        // two workers given the same statement, one coordinator that states it
        shapes.push(Shape {
            current: "C",
            ..shape(
                "the coordinator's own prompt states the change two tasks copy",
                vec![task(0, C), task(1, C)],
                vec![
                    vec![Worker(0), Worker(1), Asks(&[0, 1], C)],
                    vec![Worker(0), Asks(&[0, 1], C), Worker(1)],
                    vec![Asks(&[0, 1], C), Worker(0), Worker(1)],
                ],
                1,
            )
        });
        // The turn that states the change and hands a copy on is itself a worker's: its prompt
        // read as the person's when it put the row back, and what that first saying retired
        // (B) comes back when its own coordinator arrives.
        shapes.push(shape(
            "a worker states the change it hands on",
            vec![task(0, C), task(1, C)],
            vec![
                vec![Worker(0), Relay(2, 1, &[0]), Coord(&[1])],
                vec![Relay(2, 1, &[0]), Worker(0), Coord(&[1])],
                vec![Worker(0), Coord(&[1]), Relay(2, 1, &[0])],
                vec![Coord(&[1]), Relay(2, 1, &[0]), Worker(0)],
            ],
            1,
        ));
        // A copy of A is read between two workers given the same change the user never made.
        // The second copy of the change moves it past the copy of A, which stands dated
        // between its two sayings without having been first said there: it replaced nothing
        // then, and everything is undone to B (the order probe's 36 bad orders of 5,472 on a
        // build that took the moved copy for a word said in between).
        shapes.push(shape(
            "a copy of A dated between two copies of a change the user never made",
            vec![task(0, C), task(1, A), task(2, C)],
            vec![
                vec![
                    Worker(1),
                    Worker(0),
                    Worker(2),
                    Coord(&[0]),
                    Coord(&[1]),
                    Coord(&[2]),
                ],
                vec![
                    Coord(&[0]),
                    Coord(&[1]),
                    Coord(&[2]),
                    Worker(0),
                    Worker(1),
                    Worker(2),
                ],
            ],
            1,
        ));
        // the same sentence said by the person only after the coordinator's turn undid the
        // worker's: put back, it is their first saying of it too
        shapes.push(Shape {
            current: "C",
            ..shape(
                "the person states a change only a task had made, after it was undone",
                vec![task(0, C)],
                vec![
                    vec![Worker(0), Coord(&[0]), Say(C)],
                    vec![Coord(&[0]), Worker(0), Say(C)],
                ],
                1,
            )
        });
        shapes
    }

    /// The user speaks again while a worker's turn is on record and its coordinator's is not.
    fn mid_way_shapes() -> Vec<Shape> {
        let two = by_hash(vec![task(0, A), task(1, A)]);
        vec![
            Shape {
                current: "C",
                ..shape(
                    "the user speaks before the coordinator arrives",
                    vec![task(0, A)],
                    vec![
                        vec![Worker(0), Say(C), Coord(&[0])],
                        vec![Coord(&[0]), Worker(0), Say(C)],
                    ],
                    1,
                )
            },
            Shape {
                current: "C",
                ..shape(
                    "two workers, the user speaks between the two coordinators",
                    two,
                    vec![
                        vec![Worker(0), Worker(1), Coord(&[0]), Say(C), Coord(&[1])],
                        vec![Worker(0), Worker(1), Coord(&[1]), Say(C), Coord(&[0])],
                        vec![Worker(0), Coord(&[0]), Worker(1), Say(C), Coord(&[1])],
                        vec![Coord(&[0]), Worker(0), Worker(1), Say(C), Coord(&[1])],
                        vec![Coord(&[0]), Say(C), Coord(&[1]), Worker(0), Worker(1)],
                    ],
                    2,
                )
            },
            // an agent's task states a change (C) the user never made; the user then says K
            Shape {
                current: "K",
                ..shape(
                    "a change the user never made, then the user speaks",
                    vec![task(0, C)],
                    vec![
                        vec![Worker(0), Say(K), Coord(&[0])],
                        vec![Coord(&[0]), Worker(0), Say(K)],
                        vec![Coord(&[0]), Say(K), Worker(0)],
                    ],
                    0,
                )
            },
            // the user's word takes one record by a rule that takes one, and with the agent's
            // prompt on record that one is the agent's (the three shapes of the test below)
            Shape {
                first: O,
                said: &[],
                current: "W",
                ..shape(
                    "the reply names the user's value",
                    vec![format!("Write the upgrade notes for the proxy. {P2}")],
                    word_mid_way(W),
                    0,
                )
            },
            Shape {
                first: Q3,
                said: &[],
                current: "Q7",
                ..shape("a quantity", vec![long_task(Q5)], word_mid_way(Q7), 0)
            },
            Shape {
                first: Q3,
                said: &[Q5],
                current: "Q7",
                ..shape(
                    "a quantity, the agent's prompt a copy",
                    vec![long_task(Q3)],
                    word_mid_way(Q7),
                    1,
                )
            },
            // The coordinator's own prompt is a statement of the user's, in the turn that hands
            // the task over. It was typed before the worker ran, so the worker's prompt is undone
            // before it is written: these orders must be equal, whatever its subject.
            Shape {
                first: Q,
                said: &[],
                current: "W",
                ..shape(
                    "the coordinator's own prompt is a word of the user's",
                    vec![task(0, S)],
                    vec![
                        vec![Worker(0), Asks(&[0], W)],
                        vec![Asks(&[0], W), Worker(0)],
                    ],
                    0,
                )
            },
            // the same on the subject the task changed (N would replace Q), and with a copy
            Shape {
                first: Q,
                said: &[],
                current: "N",
                ..shape(
                    "the coordinator's own prompt replaces what the task had retired",
                    vec![task(0, S)],
                    vec![
                        vec![Worker(0), Asks(&[0], N)],
                        vec![Asks(&[0], N), Worker(0)],
                    ],
                    0,
                )
            },
            Shape {
                current: "C",
                ..shape(
                    "the coordinator's own prompt replaces B, the task a copy of A",
                    vec![task(0, A)],
                    vec![
                        vec![Worker(0), Asks(&[0], C)],
                        vec![Asks(&[0], C), Worker(0)],
                    ],
                    1,
                )
            },
            // R3. Two coordinators, a worker each. The person types a change (D) in one
            // coordinator's prompt and its task copies it. Both workers are read first. The
            // other worker's copy of A retires the row the first worker typed, so the person's
            // sentence meets a row retired otherwise than as an agent's; or the other worker's
            // task carries D too, and the row stands at its turn by a journal row. Either way
            // D is the person's, at the person's turn, once every coordinator is read.
            Shape {
                current: "D",
                ..shape(
                    "the person's change, its worker's row retired by another worker's copy",
                    vec![task(0, D), task(1, A)],
                    vec![
                        vec![Worker(0), Worker(1), Asks(&[0], D), Coord(&[1])],
                        vec![Asks(&[0], D), Coord(&[1]), Worker(0), Worker(1)],
                    ],
                    1,
                )
            },
            Shape {
                current: "D",
                ..shape(
                    "the person's change, copied by both workers, the other one read first",
                    vec![task(0, D), task(1, D)],
                    vec![
                        vec![Worker(1), Worker(0), Asks(&[0], D), Coord(&[1])],
                        vec![Worker(0), Worker(1), Asks(&[0], D), Coord(&[1])],
                        vec![Asks(&[0], D), Coord(&[1]), Worker(0), Worker(1)],
                    ],
                    1,
                )
            },
            // I1 beside a copy. Another worker's copy of B moved B to the worker's turn, which
            // is dated after the coordinator's own prompt. That prompt (C) replaces B all the
            // same: undone, the copy leaves B at its earlier date, where C is the later word.
            Shape {
                current: "C",
                ..shape(
                    "the coordinator's own prompt replaces B while another worker's copy holds it",
                    vec![task(0, A), task(1, B)],
                    vec![
                        vec![Worker(1), Worker(0), Asks(&[0], C), Coord(&[1])],
                        vec![Worker(1), Asks(&[0], C), Worker(0), Coord(&[1])],
                        vec![Worker(1), Asks(&[0], C), Coord(&[1]), Worker(0)],
                        vec![Asks(&[0], C), Coord(&[1]), Worker(0), Worker(1)],
                    ],
                    1,
                )
            },
            // C1, on a subject nothing was said about: the coordinator's own prompt states what
            // the task copies, and B stands beside it (the strict shapes have the one that
            // replaces B)
            shape(
                "the coordinator's own prompt states what the task copies, on another subject",
                vec![format!("Write the upgrade notes for the proxy. {O}")],
                vec![
                    vec![Worker(0), Asks(&[0], O)],
                    vec![Asks(&[0], O), Worker(0)],
                ],
                1,
            ),
            // something outside ingest between the two turns: the person revokes O, a commit
            // pass retires it, a pass runs and retires nothing, the store is exported and
            // imported. O, which the task's change had retired, never met any of them.
            outside("the person revokes O between", Outside::Revoke(O)),
            outside("a commit pass retires O between", Outside::Gone(O)),
            outside("a pass that retires nothing runs between", Outside::Pass),
            outside("the store is exported and imported between", Outside::Moved),
            // a copy of A and then a copy of B wait: A arrives retired by the copy of B, and
            // what the copy of A had retired with B is still retired by A
            shape(
                "the store is exported and imported while two copies wait",
                vec![task(0, A), task(1, B)],
                vec![
                    vec![
                        Worker(0),
                        Worker(1),
                        Done(Outside::Moved),
                        Coord(&[0]),
                        Coord(&[1]),
                    ],
                    vec![
                        Coord(&[0]),
                        Coord(&[1]),
                        Worker(0),
                        Worker(1),
                        Done(Outside::Moved),
                    ],
                ],
                0,
            ),
            // the same while a copy of A waits for its coordinator: the journal stays behind,
            // so the copy cannot be inverted in the new store
            shape(
                "the store is exported and imported while a copy waits",
                vec![task(0, A)],
                vec![
                    vec![Worker(0), Done(Outside::Moved), Coord(&[0])],
                    vec![Worker(0), Coord(&[0]), Done(Outside::Moved)],
                    vec![Done(Outside::Moved), Worker(0), Coord(&[0])],
                    vec![Coord(&[0]), Worker(0), Done(Outside::Moved)],
                ],
                0,
            ),
        ]
    }

    /// O, a change of it in a worker's task, and one thing done to the store from outside
    /// ingest, at every position.
    fn outside(name: &str, what: Outside) -> Shape {
        Shape {
            first: O,
            said: &[],
            // revoked or retired by the pass, O is not active in any order
            current: match what {
                Outside::Revoke(_) | Outside::Gone(_) => "",
                _ => "O",
            },
            ..shape(
                name,
                vec![format!("Write the upgrade notes for the proxy. {P}")],
                vec![
                    vec![Worker(0), Done(what), Coord(&[0])],
                    vec![Worker(0), Coord(&[0]), Done(what)],
                    vec![Done(what), Worker(0), Coord(&[0])],
                    vec![Coord(&[0]), Worker(0), Done(what)],
                ],
                0,
            )
        }
    }

    /// One worker: the user's word before its coordinator arrives, and coordinator first.
    fn word_mid_way(word: &'static str) -> Vec<Vec<Step>> {
        vec![
            vec![Worker(0), Say(word), Coord(&[0])],
            vec![Coord(&[0]), Worker(0), Say(word)],
        ]
    }

    /// The acceptance invariant, for every ingest order of every shape, against the order with
    /// every coordinator first:
    ///
    /// - SUBSET, always: nothing is active that coordinator-first does not have active, and
    ///   nothing a worker's turn wrote is left reading as the user's;
    /// - EQUAL when no word of the user's arrives between a worker's turn and its
    ///   coordinator's: the same statements active, the same heirs, nothing left retired;
    /// - whatever coordinator-first serves and this order does not is marked as retired by an
    ///   agent's statement, and `doctor` counts exactly the marked rows, none of them a worker's;
    /// - no record is its own heir (`run`).
    #[test]
    fn nothing_is_served_that_coordinator_first_would_not_serve() {
        let mut wrong = Vec::new();
        let strict = strict_shapes();
        let exact = strict.len();
        let shapes = strict
            .into_iter()
            .chain(mid_way_shapes())
            .chain(later_word_shapes());
        for (n, s) in shapes.enumerate() {
            let (want, _) = run(&s, s.orders.last().unwrap());
            let served = active(&want.state);
            let one = n >= exact || served == vec![s.current];
            let current = s.current.is_empty() || served.contains(&s.current);
            if !one || !current || !want.doctor.is_empty() {
                wrong.push(format!("{}: coordinator first ends {want:?}", s.name));
            }
            for (i, order) in s.orders.iter().enumerate() {
                let (got, journal) = run(&s, order);
                if i == 0 {
                    assert!(
                        journal >= s.restated,
                        "{}: the first order restates {journal} times, not {}",
                        s.name,
                        s.restated
                    );
                }
                let mut why = Vec::new();
                if !active(&got.state).iter().all(|a| served.contains(a))
                    || !got.own_rows.is_subset(&want.own_rows)
                {
                    why.push("serves what coordinator-first does not");
                }
                if got.left_as_the_users != 0 {
                    why.push("a worker's row reads as the user's");
                }
                if !want
                    .own_rows
                    .difference(&got.own_rows)
                    .all(|r| got.marked.contains(r))
                {
                    why.push("a record left retired is not marked");
                }
                // what the person typed in a coordinator's own prompt is in no row of the
                // user's session: it is active wherever coordinator-first has it active, or
                // it is left retired with a mark
                let typed_lost = the_users(&s, &want.state).iter().any(|(name, (inv, _))| {
                    let text = NAMES.iter().find(|(n, _)| n == name).map(|(_, t)| *t);
                    *inv == 0
                        && got.state.get(name).is_none_or(|(i, _)| *i != 0)
                        && !text.is_some_and(|t| {
                            got.marked
                                .contains(&format!("decision:{}", t.trim_end_matches('.')))
                        })
                });
                if typed_lost {
                    why.push("a statement the user typed is retired and not marked");
                }
                // doctor against coordinator-first: it counts every statement of the user's
                // that is active there and not here, and nothing a worker's turn wrote
                let typed = |r: &&String| !r.starts_with("episode:");
                if !want
                    .own_rows
                    .difference(&got.own_rows)
                    .filter(typed)
                    .all(|r| got.doctor.contains(r))
                    // (a store that imported a worker's statement cannot tell it from one of
                    // the user's a copy moved there, and counts it)
                    || (got.marked_of_a_worker != 0
                        && !order.iter().any(|s| matches!(s, Done(Outside::Moved))))
                {
                    why.push("doctor does not count a statement coordinator-first serves");
                }
                if !spoken_mid_way(order, &s.tasks)
                    && (the_users(&s, &got.state) != the_users(&s, &want.state)
                        || got.own_rows != want.own_rows
                        || !got.marked.is_empty())
                {
                    why.push("differs with no word of the user's mid-way");
                }
                if !why.is_empty() {
                    wrong.push(format!(
                        "{}: {order:?}: {}\n  ends {got:?}\n  not  {want:?}",
                        s.name,
                        why.join("; ")
                    ));
                }
            }
        }
        assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
    }

    /// R3. Two coordinators, a worker each; the person types a change (D) in one coordinator's
    /// prompt and its task copies it. Both workers are read before that coordinator, the other
    /// coordinator after. D is current at the end, the person's, at the person's turn: not
    /// revoked as an agent's, and not left retired with a mark because a correction typed
    /// from the same sentence of the same turn counted as a word said since.
    #[test]
    fn a_change_typed_in_one_of_two_coordinators_is_current_whatever_the_other_worker_copied() {
        for carried in [A, B, D] {
            for workers in [[0usize, 1], [1, 0]] {
                let tasks = vec![task(0, D), task(1, carried)];
                let mut s = Scene::new();
                s.say("05", A);
                s.say("05", B);
                for w in workers {
                    s.step(Worker(w), &tasks);
                }
                s.step(Asks(&[0], D), &tasks);
                s.step(Coord(&[1]), &tasks);
                let (invalid, session): (i64, String) = s
                    .db
                    .conn
                    .query_row(
                        "SELECT invalid, session_id FROM record WHERE kind='decision' AND object = ?1",
                        [D.trim_end_matches('.')],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                assert_eq!(
                    (invalid, session.as_str()),
                    (0, "coord0"),
                    "the other task carries {carried:?}, workers read in order {workers:?}: {:?}",
                    s.state()
                );
            }
        }
    }

    /// A mark travels with an export; what made it does not. R is left retired, marked, by a
    /// copy of A in one store. The store is exported and imported. In the new store another
    /// worker copies another statement and its coordinator arrives, with nothing said between:
    /// that copy is undone, its journal row is the first of the new store as the other was of
    /// the old, and R stays retired.
    #[test]
    fn a_mark_brought_by_an_import_is_never_resolved_in_the_importing_store() {
        let tasks = vec![
            task(0, A),
            format!("Write the upgrade notes for the proxy. {O}"),
        ];
        let mut s = Scene::new();
        s.say("05", A);
        s.say("05", R);
        for step in [Worker(0), Say(M), Coord(&[0]), Done(Outside::Moved)] {
            s.step(step, &tasks);
        }
        assert_eq!(s.state()["R"].0, 1, "R is retired before the move");
        // dated before the worker's turn, so that its copy of O is a restatement
        s.say("05", O);
        s.say("05", P);
        for step in [Worker(1), Coord(&[1])] {
            s.step(step, &tasks);
        }
        let st = s.state();
        assert_eq!(active(&st), vec!["M", "P"], "{st:?}");
        assert_eq!(
            s.db.count("SELECT count(*) FROM restatement WHERE undone = 1")
                .unwrap(),
            1,
            "the copy of O was undone in the new store"
        );
        let marked =
            s.db.count(&format!(
                "SELECT count(*) FROM record WHERE {MARKED} AND kind='decision'"
            ))
            .unwrap();
        assert_eq!(marked, 1, "R is still marked, and doctor counts it");
        assert_eq!(muninn_core::health::unrestored(&s.db).len(), 1);
    }

    /// The person revokes O while an agent's change holds it retired, and the change is found
    /// out afterwards. `muninn revoke` answered no and changed nothing; the undo does not give
    /// O back. O keeps the reason `superseded` and takes the cause `revoked`: doctor does not
    /// count it, and the episode of its turn, which does come back, is not served, because the
    /// read path hides the episode of a turn whose statement was replaced.
    #[test]
    fn a_record_revoked_while_an_agents_change_held_it_is_not_given_back() {
        let tasks = vec![format!("Write the upgrade notes for the proxy. {P}")];
        let mut s = Scene::new();
        s.say("05", O);
        s.step(Worker(0), &tasks);
        let o: i64 = s
            .db
            .conn
            .query_row(
                "SELECT id FROM record WHERE kind='decision' AND object LIKE 'We go with openssl%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !muninn_core::filter::revoke(&s.db, o, "test").unwrap(),
            "retired already: revoke changes nothing and says so"
        );
        s.step(Coord(&[0]), &tasks);
        let (invalid, reason, via): (i64, String, String) =
            s.db.conn
                .query_row(
                    "SELECT invalid, invalid_reason, invalidated_via FROM record WHERE id=?1",
                    [o],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
        assert_eq!(
            (invalid, reason.as_str(), via.as_str()),
            (1, "superseded", "revoked")
        );
        assert!(active(&s.state()).is_empty(), "{:?}", s.state());
        assert!(muninn_core::health::unrestored(&s.db).is_empty());
        let twin: i64 =
            s.db.conn
                .query_row(
                    "SELECT id FROM record WHERE kind='episode' AND invalid=0 AND session_id='own'",
                    [],
                    |r| r.get(0),
                )
                .expect("the episode of O's turn came back");
        let terms: Vec<String> = ["openssl", "TLS", "backend"]
            .iter()
            .map(|t| t.to_string())
            .collect();
        let hits =
            muninn_core::recall::recall(&s.db, &terms, 10, &std::collections::HashSet::new())
                .unwrap();
        assert!(
            hits.iter().all(|h| h.id != twin && h.id != o),
            "{:?}",
            hits.iter().map(|h| h.id).collect::<Vec<_>>()
        );
    }

    /// A statement the user says again is a word of theirs said since, though it puts no new
    /// record on the store. A, then B. An agent's change replaces B, and another statement
    /// from another turn with it. The user says A again: A comes back, and the change, which
    /// still holds that other statement, stays beside it. With every coordinator first, saying A
    /// again retires B. So when the change is revoked, B must not come back.
    #[test]
    fn a_statement_said_again_counts_as_the_user_having_spoken() {
        let tasks = vec![task(0, C)];
        let mut s = Scene::new();
        s.say("05", A);
        s.say("05", B);
        s.step(Worker(0), &tasks);
        s.db.conn
            .execute(
                "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,transcript_ref,dedup_hash,created_at,invalid,invalid_reason,invalidated_by,invalidated_via) \
                 SELECT 'decision','k','is','x other','x other','imported',3,'own','elsewhere:1','hx',1,1,'superseded',id,'said' \
                 FROM record WHERE kind='decision' AND instr(object, 'SQS') > 0",
                [],
            )
            .unwrap();
        s.step(Say(A), &tasks);
        let st = s.state();
        assert_eq!(
            active(&st),
            vec!["A", "C"],
            "the change still stands beside A: {st:?}"
        );
        s.step(Coord(&[0]), &tasks);
        let st = s.state();
        assert_eq!(active(&st), vec!["A"], "{st:?}");
        let marked =
            s.db.count(&format!(
                "SELECT count(*) FROM record WHERE {MARKED} AND kind='decision'"
            ))
            .unwrap();
        assert_eq!(
            marked, 2,
            "B and the other statement are left retired, marked"
        );
    }

    /// The user spoke while an agent's prompt read as theirs, and the prompt is found out
    /// afterwards. Each shape ends exactly as ruled: what the prompt retired stays retired,
    /// marked, and `doctor` names it; nothing the user replaced is active.
    #[test]
    fn what_an_agents_prompt_retired_stays_retired_once_the_user_has_spoken() {
        /// (statement, invalid, heir, cause)
        type Want = &'static [(
            &'static str,
            i64,
            Option<&'static str>,
            Option<&'static str>,
        )];
        struct Case {
            name: &'static str,
            first: &'static str,
            said: &'static [&'static str],
            task: String,
            word: &'static str,
            want: Want,
        }
        const UN: Option<&str> = Some("agent_unrestored");
        let cases = [
            // the reply to the user's word names the value they had chosen, and the one record
            // that rule takes is the agent's change, which holds that value too
            Case {
                name: "the reply names the user's value",
                first: O,
                said: &[],
                task: format!("Write the upgrade notes for the proxy. {P2}"),
                word: W,
                want: &[
                    ("O", 1, Some("P2"), UN),
                    ("P2", 1, Some("W"), Some("ack")),
                    ("W", 0, None, None),
                ],
            },
            // the user's word takes the most recent holder of the unit, the agent's change
            // the reply names the agent's value: the user's word replaces the agent's change, and
            // what that change had retired is not looked for
            Case {
                name: "the reply names the agent's value",
                first: O,
                said: &[],
                task: format!("Write the upgrade notes for the proxy. {P}"),
                word: W2,
                want: &[
                    ("O", 1, Some("P"), UN),
                    ("P", 1, Some("W2"), Some("ack")),
                    ("W2", 0, None, None),
                ],
            },
            Case {
                name: "a quantity",
                first: Q3,
                said: &[],
                task: long_task(Q5),
                word: Q7,
                want: &[
                    ("Q3", 1, Some("Q5"), UN),
                    ("Q5", 1, Some("Q7"), Some("quantity")),
                    ("Q7", 0, None, None),
                ],
            },
            // the same on the journal path: the most recent holder is the agent's copy of Q3,
            // which had retired the user's Q5
            Case {
                name: "a quantity, the agent's prompt a copy",
                first: Q3,
                said: &[Q5],
                task: long_task(Q3),
                word: Q7,
                want: &[
                    ("Q3", 1, Some("Q7"), Some("quantity")),
                    ("Q5", 1, None, UN),
                    ("Q7", 0, None, None),
                ],
            },
            // the user's word would have replaced their statement and does not touch the
            // agent's change
            Case {
                name: "M replaces Q, not the task's S",
                first: Q,
                said: &[],
                task: task(0, S),
                word: M,
                want: &[
                    ("Q", 1, Some("S"), UN),
                    ("S", 1, None, Some("agent_prompt")),
                    ("M", 0, None, None),
                ],
            },
            Case {
                name: "N replaces Q, not the task's T",
                first: Q,
                said: &[],
                task: task(0, T),
                word: N,
                want: &[
                    ("Q", 1, Some("T"), UN),
                    ("T", 1, None, Some("agent_prompt")),
                    ("N", 0, None, None),
                ],
            },
            // the task copies A, which retires R; the user's M would have replaced R only
            Case {
                name: "M replaces R, not the copy of A",
                first: A,
                said: &[R],
                task: task(0, A),
                word: M,
                want: &[
                    ("A", 1, Some("R"), Some("said")),
                    ("R", 1, None, UN),
                    ("M", 0, None, None),
                ],
            },
            // the user's word replaces the agent's change (J) and not the statement of theirs
            // that the change had retired (G)
            Case {
                name: "the user's word replaces the agent's change, not what it retired",
                first: A,
                said: &[G],
                task: task(0, J),
                word: H,
                want: &[
                    ("A", 1, Some("G"), Some("said")),
                    ("G", 1, Some("J"), UN),
                    ("J", 1, Some("H"), Some("said")),
                    ("H", 0, None, None),
                ],
            },
        ];
        let mut wrong = Vec::new();
        for c in cases {
            let shape = Shape {
                first: c.first,
                said: c.said,
                ..shape(c.name, vec![c.task.clone()], Vec::new(), 0)
            };
            let s = play(&shape, &[Worker(0), Say(c.word), Coord(&[0])]);
            let st = s.state();
            let id_of = |name: &str| -> i64 {
                let text = NAMES.iter().find(|(n, _)| *n == name).unwrap().1;
                s.db.conn
                    .query_row(
                        "SELECT id FROM record WHERE kind='decision' AND (object = ?1 OR object = ?2) ORDER BY id LIMIT 1",
                        [text, text.trim_end_matches('.')],
                        |r| r.get(0),
                    )
                    .unwrap_or_else(|_| panic!("{}: {name} is not on record", c.name))
            };
            let mut ended = Vec::new();
            for (name, ..) in c.want {
                let via: Option<String> =
                    s.db.conn
                        .query_row(
                            "SELECT invalidated_via FROM record WHERE id=?1",
                            [id_of(name)],
                            |r| r.get(0),
                        )
                        .unwrap();
                let (invalid, heir) = st[name];
                ended.push((*name, invalid, heir, via));
            }
            let want: Vec<_> = c
                .want
                .iter()
                .map(|(n, i, h, v)| (*n, *i, *h, v.map(str::to_string)))
                .collect();
            let active: Vec<&str> = active(&st);
            if ended != want || active != vec![c.want.last().unwrap().0] {
                wrong.push(format!(
                    "{}: ends {ended:?}, active {active:?}, not {want:?}",
                    c.name
                ));
                continue;
            }
            // doctor: yellow, and it names every statement left retired
            let paths = muninn_core::paths::ProjectPaths::from_root(s.dir.path());
            let report = muninn_core::health::run(&paths, Some(&s.db), None, true);
            let check = report
                .checks
                .iter()
                .find(|k| k.name == "unrestored")
                .unwrap();
            let left: Vec<i64> = want
                .iter()
                .filter(|(.., via)| {
                    via.as_deref()
                        .is_some_and(|v| v.starts_with("agent_unrestored"))
                })
                .map(|(n, ..)| id_of(n))
                .collect();
            let named = |id: &i64| {
                let tag = format!("#{id}");
                check.detail.split([' ', ',']).any(|w| w == tag)
            };
            let counted =
                s.db.count(&format!(
                    "SELECT count(*) FROM record WHERE {MARKED} AND kind<>'episode'"
                ))
                .unwrap();
            if check.status != muninn_core::health::Status::Yellow
                || left.is_empty()
                || !left.iter().all(named)
                || !check
                    .detail
                    .starts_with(&format!("{counted} statement(s) "))
            {
                wrong.push(format!("{}: doctor says {check:?}, left {left:?}", c.name));
            }
        }
        assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
    }

    /// A later word of the user's that replaces one statement and not another.
    fn later_word_shapes() -> Vec<Shape> {
        let later = |name: &str,
                     first: &'static str,
                     said: &'static [&'static str],
                     tasks: Vec<String>,
                     word: &'static str,
                     current: &'static str,
                     restated: i64| {
            let orders = if tasks.len() == 1 {
                vec![
                    vec![Worker(0), Say(word), Coord(&[0])],
                    vec![Worker(0), Coord(&[0]), Say(word)],
                    vec![Say(word), Worker(0), Coord(&[0])],
                    vec![Coord(&[0]), Worker(0), Say(word)],
                ]
            } else {
                vec![
                    vec![Worker(0), Worker(1), Say(word), Coord(&[0]), Coord(&[1])],
                    vec![Worker(0), Worker(1), Say(word), Coord(&[1]), Coord(&[0])],
                    vec![Worker(0), Worker(1), Say(word), Coord(&[0, 1])],
                    vec![Worker(0), Say(word), Worker(1), Coord(&[0]), Coord(&[1])],
                    vec![Worker(0), Worker(1), Coord(&[0]), Say(word), Coord(&[1])],
                    vec![Worker(0), Worker(1), Coord(&[1]), Say(word), Coord(&[0])],
                    vec![Coord(&[0]), Coord(&[1]), Worker(0), Worker(1), Say(word)],
                ]
            };
            Shape {
                first,
                said,
                current,
                ..shape(name, tasks, orders, restated)
            }
        };
        vec![
            // the task's change replaces Q; the user's M would have replaced Q and does not
            // touch the change
            later(
                "M replaces Q, not the task's S",
                Q,
                &[],
                vec![task(0, S)],
                M,
                "M",
                0,
            ),
            later(
                "N replaces Q, not the task's T",
                Q,
                &[],
                vec![task(0, T)],
                N,
                "N",
                0,
            ),
            later(
                "M replaces Q, not the task's S; a second task copies Q",
                Q,
                &[],
                vec![task(0, S), task(1, Q)],
                M,
                "M",
                0,
            ),
            // the task copies A, which retires R; the user's M replaces R only
            later(
                "M replaces R, not the copy of A",
                A,
                &[R],
                vec![task(0, A)],
                M,
                "M",
                1,
            ),
            later(
                "M replaces R, not either copy of A",
                A,
                &[R],
                vec![task(0, A), task(1, A)],
                M,
                "M",
                2,
            ),
            // the task copies F, which retires G; the user's H replaces the copy of F and would
            // not have touched G
            later(
                "H replaces the copy of F, not G",
                F,
                &[G],
                vec![task(0, F)],
                H,
                "H",
                1,
            ),
            later(
                "H replaces a copy of F, not G",
                F,
                &[G],
                vec![task(0, F), task(1, F)],
                H,
                "H",
                2,
            ),
            // the task's change J replaces G; the user's H replaces J and would not have
            // touched G
            later(
                "H replaces the task's J, not G",
                A,
                &[G],
                vec![task(0, J)],
                H,
                "H",
                0,
            ),
            later(
                "H replaces the task's J, not G; a second task copies G",
                A,
                &[G],
                vec![task(0, J), task(1, G)],
                H,
                "H",
                0,
            ),
            // two tasks state the same change J: the second is a copy of the first, which moves
            // J to its turn, and the user's H replaces J there. G, which J had retired when the
            // first task typed it, is left retired and has to be marked.
            later(
                "H replaces the task's J in the turn of a second task that copied it",
                A,
                &[G],
                vec![task(0, J), task(1, J)],
                H,
                "H",
                1,
            ),
        ]
    }
}
