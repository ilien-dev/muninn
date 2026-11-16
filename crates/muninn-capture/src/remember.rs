//! `muninn remember "<statement>" --kind …`: a statement an agent was asked to keep, written
//! through the path every typed record takes (`ingest::store_candidate`).
//!
//! Who said it is read from the session's transcript, never from the statement: found word for
//! word (whitespace folded) in a turn a person typed, it is the person's (`user_said`, trust 3,
//! pointing at that turn); anything else is the agent's wording, `agent_inferred` at trust 0,
//! which the read path frames as a hint and which retires nothing.
//!
//! One turn, one record: a statement of a turn the extractor has typed already is that
//! record, and a turn that holds a kept statement gives the extractor no second record of it
//! when it is ingested (`settle`).

use crate::extract::{self, Candidate};
use crate::ingest::{store_candidate, Earlier, Place, Stored};
use crate::model::{Session, Turn};
use crate::redact::redact;
use muninn_core::db::now_ms;
use muninn_core::sanitize::truncate_chars;
use muninn_core::{Db, Error, Result};
use serde::Serialize;
use std::path::Path;

/// The kinds a statement can be kept as.
pub const KINDS: &[&str] = &["decision", "invariant", "correction", "deadend"];

/// `meta` key: how many times `remember` wrote or found a record (`muninn status`).
pub const CALLS: &str = "remember_calls";

/// The session a statement was made in: its transcript and id. `None` when it could not be
/// identified.
pub type Source<'a> = Option<(&'a Path, &'a str)>;

/// What `remember` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Remembered {
    pub id: i64,
    pub kind: String,
    pub origin: String,
    pub trust: i64,
    /// `recorded`, `deduped` (the record was on file already: the same statement, or the
    /// record the extractor typed from the same turn) or `restated` (on file, and moved to
    /// this statement).
    pub outcome: &'static str,
    /// The turn the person said it in, by index in the transcript.
    pub turn: Option<usize>,
    pub transcript_ref: Option<String>,
    /// Why it was not taken as the person's, when it was not.
    pub lower: Option<&'static str>,
    /// The record is on file and retired: it is not served. A statement that meets its own
    /// record without restating it (the same turn again, or a turn older than what replaced
    /// it) leaves the record as it is, and "already on record" alone would read as current.
    pub retired: bool,
    /// What replaced it, when something did.
    pub heir: Option<i64>,
}

pub const NO_SESSION: &str = "the session could not be identified";
pub const NOT_FOUND: &str =
    "not found word for word, as whole sentences, in a message the user typed in this session";

/// Whitespace folded to single spaces, nothing else touched.
fn fold(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whitespace folded, and the stop that ends the last sentence taken off: a statement typed
/// with or without its full stop is the same statement.
fn canon(s: &str) -> String {
    fold(s)
        .trim_end_matches(['.', '!', ';'])
        .trim_end()
        .to_string()
}

/// Does `stmt` (in [`canon`] form) state one or more whole consecutive sentences of `words`,
/// or the whole of it? Sentences are the extractor's (`extract::split_sentences`), so a match
/// starts and ends on a sentence's boundaries and never inside a word. A fragment of a
/// sentence is not what the person said: without its "never" it says the opposite.
fn whole_sentences(words: &str, stmt: &str) -> bool {
    if stmt.is_empty() {
        return false;
    }
    let words = soften_breaks(words);
    let words = words.as_str();
    if canon(words) == stmt {
        return true;
    }
    let base = words.as_ptr() as usize;
    let spans: Vec<(usize, usize)> = extract::split_sentences(words)
        .into_iter()
        .map(|p| {
            let at = p.as_ptr() as usize - base;
            (at, at + p.len())
        })
        .collect();
    for i in 0..spans.len() {
        for j in i..spans.len() {
            let span = canon(&words[spans[i].0..spans[j].1]);
            if span.len() > stmt.len() {
                break;
            }
            if span == stmt {
                return true;
            }
        }
    }
    false
}

/// Line breaks as the whole-sentence check reads them. The extractor's splitter ends a sentence
/// at every line break, and a hard-wrapped message would then cut "Nunca" from the rest of its
/// sentence. Here a break ends a sentence only after a stop (`.` `!` `?` `;` `…`; a colon
/// introduces what follows and ends nothing) or at a
/// blank line; any other break is a space. Same length in bytes, so spans need no mapping.
/// Only this check reads it: extraction keeps its own split.
fn soften_breaks(w: &str) -> String {
    let mut out = String::with_capacity(w.len());
    for (i, c) in w.char_indices() {
        if c != '\n' {
            out.push(c);
            continue;
        }
        let before = out.trim_end_matches([' ', '\t', '\r']).chars().last();
        let after = w[i + 1..].chars().find(|x| !matches!(x, ' ' | '\t' | '\r'));
        let ends = match before {
            None => true,
            Some(b) => matches!(b, '.' | '!' | '?' | ';' | '…' | '\n'),
        } || matches!(after, None | Some('\n'));
        out.push(if ends { '\n' } else { ' ' });
    }
    out
}

/// The last turn of `s` in which a person typed `stmt` in their own words, as whole sentences.
fn said_in(s: &Session, stmt: &str) -> Option<usize> {
    let stmt = canon(stmt);
    s.turns
        .iter()
        .rposition(|t| extract::persons_words(t).is_some_and(|w| whole_sentences(&w, &stmt)))
}

/// `meta` key of the records kept from turns of `transcript` that ingest has not read yet.
fn open_key(transcript: &Path) -> String {
    format!(
        "remember_open:{}",
        blake3::hash(transcript.to_string_lossy().as_bytes()).to_hex()
    )
}

/// The record the extractor makes of `stmt` as said in turn `t`: from the statement alone as
/// the turn's prompt, so that the same statement said again is the same record; otherwise the
/// extractor's record of the turn as written, when it holds the statement. Either way the
/// extractor's own subject, relation and object, so the record dedupes, restates and
/// supersedes as an extracted one does.
fn extracted(sid: &str, s: &Session, t: &Turn, stmt: &str, kind: &str) -> Option<Candidate> {
    let of_kind = |c: &Candidate| c.kind == kind && c.origin == "user_said";
    let alone = Session {
        session_id: s.session_id.clone(),
        turns: vec![Turn {
            user_prompt: stmt.to_string(),
            answered: false,
            ..t.clone()
        }],
        ..Default::default()
    };
    if let Some(c) = extract::extract(&alone, sid).into_iter().find(of_kind) {
        return Some(c);
    }
    extract::extract(s, sid)
        .into_iter()
        .find(|c| c.turn_index == t.index && of_kind(c) && fold(&c.body).contains(stmt))
}

/// A record of `kind` for `stmt` when the extractor makes none.
fn plain(kind: &str, stmt: &str, origin: &'static str, sid: &str, t: Option<&Turn>) -> Candidate {
    let speaker = if origin == "user_said" {
        "user"
    } else {
        "agent"
    };
    let object = redact(truncate_chars(stmt, 300));
    let words = extract::topic_words(stmt).join(" ");
    let (kind, subject, relation) = match kind {
        "invariant" => ("invariant", extract::norm_key(stmt, 80), "must"),
        "correction" => (
            "correction",
            format!(
                "correction:{}#{}",
                extract::sid_head(sid),
                t.map_or_else(|| now_ms().to_string(), |t| t.start_offset.to_string())
            ),
            "user_said",
        ),
        "deadend" => (
            "deadend",
            format!("said:deadend:{words}"),
            "tried_and_failed",
        ),
        _ => ("decision", format!("said:state:{words}"), "user_decision"),
    };
    Candidate {
        kind,
        ack: String::new(),
        subject,
        relation: relation.to_string(),
        object,
        body: redact(&format!("{speaker}: {}\n", truncate_chars(stmt, 1_500))),
        origin,
        anchor_path: None,
        turn_index: t.map_or(0, |t| t.index),
        end_offset: t.map_or(0, |t| t.end_offset),
    }
}

/// The typed record already on file from turn `t` of session `sid` that holds `stmt`. The
/// turn is known by its session and its end offset, so a transcript read from another place
/// (a copy) still finds it.
fn on_file(tx: &rusqlite::Connection, sid: &str, t: &Turn, stmt: &str) -> Result<Option<i64>> {
    let rows: Vec<(i64, String)> = tx
        .prepare(
            "SELECT id, body FROM record WHERE invalid = 0 AND kind <> 'episode' \
             AND session_id = ?1 AND transcript_ref LIKE ?2 ORDER BY id",
        )?
        .query_map(rusqlite::params![sid, format!("%:{}", t.end_offset)], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .find(|(_, body)| fold(body).contains(stmt))
        .map(|(id, _)| id))
}

/// Keep `statement` as a record of `kind`, said in the session `source` names.
pub fn remember(db: &Db, statement: &str, kind: &str, source: Source) -> Result<Remembered> {
    remember_with(db, statement, kind, source, &[])
}

/// [`remember`], with the other transcripts this project's session log names (`others`). A
/// task one of them handed over to an agent shows, by its hash, that a prompt of this session
/// is that agent's before the store knows it: the coordinator's turn may not be ingested yet.
pub fn remember_with(
    db: &Db,
    statement: &str,
    kind: &str,
    source: Source,
    others: &[&Path],
) -> Result<Remembered> {
    let stmt = fold(statement);
    if stmt.is_empty() {
        return Err(Error::Other(
            "nothing to remember: the statement is empty".into(),
        ));
    }
    let Some(kind) = KINDS.iter().copied().find(|k| *k == kind) else {
        return Err(Error::Other(format!(
            "unknown kind `{kind}`: one of {}",
            KINDS.join(", ")
        )));
    };
    // parsed before the transaction: reading a transcript needs no lock
    let session = match source {
        Some((path, _)) => {
            let mut s = crate::parse_any_with_tasks(path, 0).map_err(|e| Error::io(path, e))?;
            // the tasks the other known sessions handed over (`agent::session_task_hashes`)
            let mut handed = std::collections::HashSet::new();
            for o in others.iter().filter(|o| **o != path) {
                if let Ok(os) = crate::parse_any_with_tasks(o, 0) {
                    handed.extend(crate::agent::session_task_hashes(&os));
                }
            }
            if !handed.is_empty() {
                crate::agent::mark_agent_written(&mut s, |h| handed.contains(h));
            }
            Some(s)
        }
        None => None,
    };
    let tx = db.write_tx()?;
    let mut found: Option<(Session, usize)> = None;
    if let (Some(mut s), Some((_, sid))) = (session, source) {
        // a prompt another session handed over to an agent is not the person's
        crate::agent::mark_from_store(&tx, &mut s, sid)?;
        if let Some(i) = said_in(&s, &stmt) {
            found = Some((s, i));
        }
    }
    let lower = match (&found, source) {
        (Some(_), _) => None,
        (None, Some(_)) => Some(NOT_FOUND),
        (None, None) => Some(NO_SESSION),
    };
    let sid = source.map_or("remember", |(_, sid)| sid);
    let (id, outcome, turn) = match &found {
        Some((s, i)) => {
            let t = &s.turns[*i];
            match on_file(&tx, sid, t, &stmt)? {
                Some(id) => (id, "deduped", Some(t.index)),
                None => {
                    let c = extracted(sid, s, t, &stmt, kind)
                        .unwrap_or_else(|| plain(kind, &stmt, "user_said", sid, Some(t)));
                    let created = t
                        .timestamp
                        .as_deref()
                        .and_then(crate::ingest::parse_rfc3339_ms)
                        .and_then(|ms| muninn_core::db::sane_created_at(ms, now_ms()))
                        .unwrap_or_else(now_ms);
                    let place = Place {
                        tref: source.map(|(p, _)| format!("{}:{}", p.display(), t.end_offset)),
                        sid,
                        created,
                        answer: t.answered,
                        anchor_hash: None,
                        // A dead end said in words replaces nothing: two on one subject stand
                        // side by side, and one found to be an agent's has nothing to give back.
                        supersedes: c.kind != "deadend",
                        // Said again it is restated like any other statement of the person's.
                        // The record's hash does not hold the session, so the words a worker
                        // kept from its task and the person's own are one row: the person's
                        // statement moves it to their turn, or puts it back once revoked.
                        restates: true,
                        // The person's turn may be older than the row its hash meets: a
                        // coordinator copied their words into a task and the worker kept them.
                        earlier: Earlier::Any,
                    };
                    let (id, outcome, elsewhere) = match store_candidate(&tx, &c, &place)? {
                        Stored::Inserted(id, _) => (id, "recorded", false),
                        Stored::Restated(id, _) => (id, "restated", false),
                        Stored::Duplicate(hash) => {
                            let (id, at): (i64, Option<String>) = tx.query_row(
                                "SELECT id, transcript_ref FROM record WHERE dedup_hash = ?1",
                                [hash],
                                |r| Ok((r.get(0)?, r.get(1)?)),
                            )?;
                            (id, "deduped", at != place.tref)
                        }
                    };
                    // A record that stands at another turn may come back to this one, when that
                    // turn is found to be an agent's (`ingest::said_before`): this turn's ingest
                    // settles it then, as it settles one kept here.
                    if outcome != "deduped" || elsewhere {
                        // the turn's ingest settles it (`settle`)
                        if let Some((path, _)) = source {
                            let key = open_key(path);
                            let ids = match meta(&tx, &key)? {
                                Some(v) if !v.is_empty() => format!("{v},{id}"),
                                _ => id.to_string(),
                            };
                            set_meta(&tx, &key, &ids)?;
                        }
                    }
                    (id, outcome, Some(t.index))
                }
            }
        }
        None => {
            let c = plain(kind, &stmt, "agent_inferred", sid, None);
            let place = Place {
                tref: None,
                sid,
                created: now_ms(),
                answer: false,
                anchor_hash: None,
                supersedes: false,
                restates: false,
                earlier: Earlier::No,
            };
            match store_candidate(&tx, &c, &place)? {
                Stored::Inserted(id, _) => (id, "recorded", None),
                Stored::Restated(id, _) => (id, "restated", None),
                Stored::Duplicate(hash) => (
                    tx.query_row("SELECT id FROM record WHERE dedup_hash = ?1", [hash], |r| {
                        r.get(0)
                    })?,
                    "deduped",
                    None,
                ),
            }
        }
    };
    // what the record carries now: an earlier record or a restatement decide it
    #[allow(clippy::type_complexity)]
    let (kind, origin, trust, tref, retired, heir): (
        String,
        String,
        i64,
        Option<String>,
        bool,
        Option<i64>,
    ) = tx.query_row(
        "SELECT kind, origin, trust, transcript_ref, invalid = 1, invalidated_by FROM record WHERE id = ?1",
        [id],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )?;
    tx.execute(
        "INSERT INTO meta(key, value) VALUES (?1, '1') \
         ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
        [CALLS],
    )?;
    tx.commit()?;
    Ok(Remembered {
        id,
        kind,
        origin,
        trust,
        outcome,
        turn,
        transcript_ref: tref,
        lower,
        retired,
        heir: heir.filter(|_| retired),
    })
}

fn meta(tx: &rusqlite::Connection, key: &str) -> Result<Option<String>> {
    match tx.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| {
        r.get::<_, String>(0)
    }) {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn set_meta(tx: &rusqlite::Connection, key: &str, value: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

/// The statements kept from turns of this batch, by the reference their turn's records
/// carry, with the body each one holds.
pub(crate) type Kept = Vec<(String, String)>;

/// Ingest's half, inside its write transaction, once the batch's episodes are written and
/// before its typed records are. A statement kept while its turn was still being written points
/// where the transcript stood then. It now points at the turn's end, the reference every record
/// of that turn carries, and so does the journal row of a restatement it made: that row is
/// dated where the extractor's restatement of the turn would be (`after_record`, the newest
/// record now). The store is then as the extractor's path leaves it, and undoing the turn as an
/// agent's finds the row (`agent::undo`). A kept statement whose turn this ingest knows to be
/// an agent's is undone as one (`agent::undo_kept_turn`). The rest are returned, so that the
/// turn gives the extractor no second record of the same statement (`is_kept`).
pub(crate) fn settle(
    tx: &rusqlite::Connection,
    transcript: &Path,
    s: &Session,
    sid: &str,
) -> Result<Kept> {
    let key = open_key(transcript);
    let Some(open) = meta(tx, &key)? else {
        return Ok(Vec::new());
    };
    let prefix = format!("{}:", transcript.display());
    let mut left = Vec::new();
    let mut kept = Vec::new();
    let mut agent_turns: Vec<(usize, String)> = Vec::new();
    for id in open.split(',').filter_map(|v| v.parse::<i64>().ok()) {
        // retired or not: a later word may have retired it before its turn was ingested, and its
        // journal row still has to meet the turn (`undo` reads the row, not the record's state)
        let row: Option<(Option<String>, String)> = tx
            .query_row(
                "SELECT transcript_ref, body FROM record WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let Some((tref, body)) = row else {
            continue;
        };
        let Some(at) = tref
            .as_deref()
            .and_then(|r| r.strip_prefix(prefix.as_str()))
            .and_then(|o| o.parse::<u64>().ok())
        else {
            // moved since by a restatement in another transcript
            continue;
        };
        match s
            .turns
            .iter()
            .find(|t| t.start_offset < at && at <= t.end_offset)
        {
            Some(t) => {
                let to = format!("{prefix}{}", t.end_offset);
                let from = format!("{prefix}{at}");
                if t.end_offset != at {
                    tx.execute(
                        "UPDATE record SET transcript_ref = ?2 WHERE id = ?1",
                        rusqlite::params![id, to],
                    )?;
                }
                tx.execute(
                    "UPDATE restatement SET turn_ref = ?3, after_record = (SELECT max(id) FROM record) \
                     WHERE record_id = ?1 AND turn_ref = ?2 AND undone = 0",
                    rusqlite::params![id, from, to],
                )?;
                if t.agent_written {
                    agent_turns.push((t.index, to));
                    continue;
                }
                let said = body.strip_prefix("user: ").unwrap_or(&body).to_string();
                kept.push((to, fold(&said)));
            }
            // a turn this batch does not reach yet
            None if s.turns.last().is_none_or(|t| at > t.end_offset) => left.push(id.to_string()),
            None => {}
        }
    }
    if left.is_empty() {
        tx.execute("DELETE FROM meta WHERE key = ?1", [&key])?;
    } else {
        set_meta(tx, &key, &left.join(","))?;
    }
    agent_turns.sort();
    agent_turns.dedup();
    for (i, tref) in agent_turns {
        crate::agent::undo_kept_turn(tx, &s.turns[i], sid, &tref)?;
    }
    Ok(kept)
}

/// Is `c`, typed from the turn at `tref`, a statement kept from that turn already?
pub(crate) fn is_kept(kept: &Kept, tref: &str, c: &Candidate) -> bool {
    kept.iter()
        .any(|(at, said)| at == tref && fold(&c.body).contains(said.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use muninn_core::db::Mode;
    use serde_json::{json, Value};
    use std::path::PathBuf;

    fn user(s: &str, ts: &str) -> Value {
        json!({"type":"user","sessionId":"s1","timestamp":ts,"message":{"role":"user","content":s}})
    }
    fn assistant(s: &str) -> Value {
        json!({"type":"assistant","sessionId":"s1","message":{"role":"assistant","content":[{"type":"text","text":s}]}})
    }
    fn edit(id: &str) -> Value {
        json!({"type":"assistant","sessionId":"s1","message":{"role":"assistant","content":[{"type":"tool_use","id":id,"name":"Edit","input":{"file_path":"src/lista.rs","old_string":"a","new_string":"b"}}]}})
    }
    fn write(dir: &Path, name: &str, lines: &[Value]) -> PathBuf {
        let p = dir.join(name);
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&p, text).unwrap();
        p
    }
    fn append(p: &Path, lines: &[Value]) {
        let more: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let mut f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
        std::io::Write::write_all(&mut f, more.as_bytes()).unwrap();
    }
    fn store(dir: &Path) -> Db {
        Db::open(&dir.join("m.db"), Mode::ReadWrite).unwrap()
    }
    fn row(db: &Db, id: i64) -> (String, String, i64, i64, Option<String>) {
        db.conn
            .query_row(
                "SELECT kind, origin, trust, invalid, transcript_ref FROM record WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap()
    }
    fn typed(db: &Db) -> i64 {
        db.count("SELECT count(*) FROM record WHERE kind <> 'episode'")
            .unwrap()
    }

    const RULE: &str = "Nunca borres una factura emitida";

    fn session_with(dir: &Path, prompt: &str) -> PathBuf {
        write(
            dir,
            "s1.jsonl",
            &[
                user(
                    "Hola, empecemos con el listado.",
                    "2026-11-05T10:00:00.000Z",
                ),
                assistant("Claro."),
                user(prompt, "2026-11-05T10:05:00.000Z"),
                assistant("Entendido."),
            ],
        )
    }

    /// Said word for word in a turn the person typed: the person's, at trust 3, at that turn.
    #[test]
    fn a_statement_the_user_typed_is_theirs_and_points_at_its_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let t = session_with(
            tmp.path(),
            "El listado quedó bien.\nNunca   borres una\nfactura emitida, aunque esté repetida.",
        );
        // dictation breaks a sentence over two lines: the two lines are said together
        let r = remember(
            &db,
            "Nunca borres una factura emitida, aunque esté repetida.",
            "invariant",
            Some((&t, "s1")),
        )
        .unwrap();
        assert_eq!(
            (r.origin.as_str(), r.trust, r.outcome),
            ("user_said", 3, "recorded")
        );
        assert_eq!(r.turn, Some(1));
        let s = crate::parse_any(&t, 0).unwrap();
        let want = format!("{}:{}", t.display(), s.turns[1].end_offset);
        assert_eq!(r.transcript_ref.as_deref(), Some(want.as_str()));
        let (kind, origin, trust, invalid, tref) = row(&db, r.id);
        assert_eq!(
            (kind.as_str(), origin.as_str(), trust, invalid),
            ("invariant", "user_said", 3, 0)
        );
        assert_eq!(tref, Some(want));
        assert_eq!(db.meta_get(CALLS).unwrap().as_deref(), Some("1"));
    }

    /// The agent's wording, or the user's with a word changed, is not the user's: a lower
    /// origin, never trust 3, and the command says why.
    #[test]
    fn a_statement_not_found_word_for_word_is_the_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let t = session_with(tmp.path(), "No borres nunca una factura que ya se emitió.");
        for (stmt, kind) in [
            (RULE, "invariant"),
            ("Las facturas emitidas no se borran", "decision"),
        ] {
            let r = remember(&db, stmt, kind, Some((&t, "s1"))).unwrap();
            assert_eq!(
                (r.origin.as_str(), r.trust),
                ("agent_inferred", 0),
                "{stmt}"
            );
            assert_eq!(r.lower, Some(NOT_FOUND));
            assert_eq!(r.turn, None);
            let (_, origin, trust, _, _) = row(&db, r.id);
            assert_eq!((origin.as_str(), trust), ("agent_inferred", 0));
        }
        // no session at all
        let r = remember(&db, RULE, "invariant", None).unwrap();
        assert_eq!(
            (r.origin.as_str(), r.trust, r.lower),
            ("agent_inferred", 0, Some(NO_SESSION))
        );
    }

    /// Only what a person typed counts: not a prompt another session's agent handed over, not
    /// the question of a dialog the agent asked, not what the person pasted.
    #[test]
    fn words_no_person_typed_are_not_the_users() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        // a dialog: the question is the agent's, the option the person's
        let dialog = "Your questions have been answered: \"¿Nunca borres una factura emitida o se pueden anular?\"=\"Se anulan con una nota de crédito\". You can now continue with these answers in mind.";
        let pasted = format!("Mira lo que dice el manual: <pasted_content id=\"a1\">{RULE}. Y sigue.</pasted_content> ¿Qué opinas?");
        let t = write(
            tmp.path(),
            "s1.jsonl",
            &[
                user(&pasted, "2026-11-05T10:00:00.000Z"),
                assistant("Lo leo."),
                json!({"type":"assistant","sessionId":"s1","message":{"role":"assistant","content":[{"type":"tool_use","id":"q1","name":"AskUserQuestion","input":{"questions":[]}}]}}),
                json!({"type":"user","sessionId":"s1","timestamp":"2026-11-05T10:02:00.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"q1","content":dialog}]}}),
                assistant("De acuerdo."),
            ],
        );
        let r = remember(&db, RULE, "invariant", Some((&t, "s1"))).unwrap();
        assert_eq!(
            (r.origin.as_str(), r.lower),
            ("agent_inferred", Some(NOT_FOUND))
        );
        let r = remember(
            &db,
            "Se anulan con una nota de crédito",
            "decision",
            Some((&t, "s1")),
        )
        .unwrap();
        assert_eq!(
            r.origin.as_str(),
            "user_said",
            "the option is the person's pick"
        );

        // a prompt another session handed over to an agent
        let task = format!("{RULE}. Revisa el módulo de facturas y dime qué encuentras en él.");
        let coord = write(
            tmp.path(),
            "c1.jsonl",
            &[
                json!({"type":"user","sessionId":"c1","timestamp":"2026-11-05T09:00:00.000Z","message":{"role":"user","content":"Lanza un agente para revisar las facturas."}}),
                json!({"type":"assistant","sessionId":"c1","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Agent","input":{"prompt":task}}]}}),
                json!({"type":"assistant","sessionId":"c1","message":{"role":"assistant","content":[{"type":"text","text":"Hecho."}]}}),
            ],
        );
        crate::ingest::ingest_transcript(&db, &coord, "c1").unwrap();
        let w = write(
            tmp.path(),
            "w1.jsonl",
            &[
                json!({"type":"user","sessionId":"w1","timestamp":"2026-11-05T09:01:00.000Z","message":{"role":"user","content":task}}),
                json!({"type":"assistant","sessionId":"w1","message":{"role":"assistant","content":[{"type":"text","text":"Reviso."}]}}),
            ],
        );
        let r = remember(&db, RULE, "invariant", Some((&w, "w1"))).unwrap();
        assert_eq!(
            (r.origin.as_str(), r.lower),
            ("agent_inferred", Some(NOT_FOUND))
        );
    }

    /// The extractor typed the same statement from the same turn: one record, whichever came
    /// first.
    #[test]
    fn the_record_the_extractor_types_from_the_turn_is_the_same_record() {
        let tmp = tempfile::tempdir().unwrap();
        // the turn was ingested first
        let db = store(tmp.path());
        let t = session_with(tmp.path(), &format!("{RULE}."));
        crate::ingest::ingest_transcript(&db, &t, "s1").unwrap();
        let before = typed(&db);
        assert_eq!(before, 1, "the extractor types the rule");
        let r = remember(&db, RULE, "invariant", Some((&t, "s1"))).unwrap();
        assert_eq!((r.origin.as_str(), r.outcome), ("user_said", "deduped"));
        assert_eq!(typed(&db), before);

        // `remember` came first, the turn was ingested after
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let t = session_with(tmp.path(), &format!("{RULE}."));
        let r = remember(&db, RULE, "invariant", Some((&t, "s1"))).unwrap();
        assert_eq!(r.outcome, "recorded");
        let st = crate::ingest::ingest_transcript(&db, &t, "s1").unwrap();
        assert_eq!(st.invariants, 0, "{st:?}");
        assert_eq!(typed(&db), 1);
    }

    /// Kept while its turn was still being written, the record points where the transcript
    /// stood; once the turn is read whole it points at the turn's end, as an extracted one does.
    #[test]
    fn a_record_kept_mid_turn_points_at_the_turn_once_it_is_over() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let t = write(
            tmp.path(),
            "s1.jsonl",
            &[
                user(
                    "Las anuladas van al final de la lista, en gris.",
                    "2026-11-05T10:00:00.000Z",
                ),
                edit("e1"),
            ],
        );
        let r = remember(
            &db,
            "Las anuladas van al final de la lista, en gris.",
            "decision",
            Some((&t, "s1")),
        )
        .unwrap();
        assert_eq!(r.origin.as_str(), "user_said");
        append(&t, &[assistant("Hecho: las anuladas van al final.")]);
        crate::ingest::ingest_transcript(&db, &t, "s1").unwrap();
        let s = crate::parse_any(&t, 0).unwrap();
        let want = format!("{}:{}", t.display(), s.turns[0].end_offset);
        assert_eq!(row(&db, r.id).4, Some(want));
        assert_eq!(typed(&db), 1, "one record for the turn");
    }

    /// Said again in a later session, the statement restates the record it has.
    #[test]
    fn a_statement_said_again_restates_its_record() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let t1 = session_with(tmp.path(), &format!("{RULE}."));
        crate::ingest::ingest_transcript(&db, &t1, "s1").unwrap();
        let t2 = write(
            tmp.path(),
            "s2.jsonl",
            &[
                json!({"type":"user","sessionId":"s2","timestamp":"2026-11-07T10:00:00.000Z","message":{"role":"user","content":format!("Lo repito. {RULE}.")}}),
                json!({"type":"assistant","sessionId":"s2","message":{"role":"assistant","content":[{"type":"text","text":"Sí."}]}}),
            ],
        );
        let r = remember(&db, RULE, "invariant", Some((&t2, "s2"))).unwrap();
        assert_eq!((r.origin.as_str(), r.outcome), ("user_said", "restated"));
        assert_eq!(typed(&db), 1);
        assert!(row(&db, r.id)
            .4
            .unwrap()
            .starts_with(&t2.display().to_string()));
    }

    /// A change the person stated retires what it replaces; the agent's wording retires
    /// nothing, whatever it says.
    #[test]
    fn a_change_the_user_stated_supersedes_and_the_agents_wording_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let t1 = session_with(tmp.path(), "Usamos PostgreSQL para el catálogo.");
        crate::ingest::ingest_transcript(&db, &t1, "s1").unwrap();
        let old: i64 = db
            .conn
            .query_row(
                "SELECT id FROM record WHERE kind='decision' AND invalid=0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // the agent's paraphrase: kept, and the person's decision stands
        let r = remember(
            &db,
            "El catálogo pasa a SQLite en vez de PostgreSQL",
            "decision",
            Some((&t1, "s1")),
        )
        .unwrap();
        assert_eq!(r.origin.as_str(), "agent_inferred");
        assert_eq!(row(&db, old).3, 0, "a trust 0 statement retires nothing");
        // the person's own words
        let change = "En vez de PostgreSQL, el catálogo pasa a SQLite";
        let t2 = write(
            tmp.path(),
            "s2.jsonl",
            &[
                json!({"type":"user","sessionId":"s2","timestamp":"2026-11-07T10:00:00.000Z","message":{"role":"user","content":format!("{change}. ¿Lo cambias?")}}),
                json!({"type":"assistant","sessionId":"s2","message":{"role":"assistant","content":[{"type":"text","text":"Lo cambio a SQLite."}]}}),
            ],
        );
        let r = remember(&db, change, "decision", Some((&t2, "s2"))).unwrap();
        assert_eq!(r.origin.as_str(), "user_said");
        assert_eq!(
            row(&db, old).3,
            1,
            "the person's change retires the old decision"
        );
        assert!(!r.retired, "the change is current");
        // I5. The agent now asks for the old sentence to be kept. It is on record, and it is
        // retired: the answer says so and names what replaced it, so nobody takes "already on
        // record" for "this is what holds".
        let again = remember(
            &db,
            "Usamos PostgreSQL para el catálogo.",
            "decision",
            Some((&t1, "s1")),
        )
        .unwrap();
        assert_eq!((again.id, again.outcome), (old, "deduped"));
        assert_eq!((again.retired, again.heir), (true, Some(r.id)));
    }

    #[test]
    fn every_kind_is_kept_and_an_unknown_kind_or_an_empty_statement_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let t = session_with(
            tmp.path(),
            "Probé a compilar con la versión vieja y no funciona. Eso no, así no se hace.",
        );
        for (stmt, kind) in [
            (
                "Probé a compilar con la versión vieja y no funciona",
                "deadend",
            ),
            ("Eso no, así no se hace", "correction"),
        ] {
            let r = remember(&db, stmt, kind, Some((&t, "s1"))).unwrap();
            assert_eq!((r.kind.as_str(), r.origin.as_str()), (kind, "user_said"));
        }
        assert!(remember(&db, RULE, "claim", Some((&t, "s1"))).is_err());
        assert!(remember(&db, "   ", "decision", Some((&t, "s1"))).is_err());
        assert_eq!(db.meta_get(CALLS).unwrap().as_deref(), Some("2"));
    }

    fn u(sid: &str, s: &str, ts: &str) -> Value {
        json!({"type":"user","sessionId":sid,"timestamp":ts,"message":{"role":"user","content":s}})
    }
    fn a(sid: &str, s: &str) -> Value {
        json!({"type":"assistant","sessionId":sid,"message":{"role":"assistant","content":[{"type":"text","text":s}]}})
    }
    fn e(sid: &str, id: &str) -> Value {
        json!({"type":"assistant","sessionId":sid,"message":{"role":"assistant","content":[{"type":"tool_use","id":id,"name":"Edit","input":{"file_path":"src/lista.rs","old_string":"a","new_string":"b"}}]}})
    }
    /// A coordinator's session that hands `task` to an agent.
    fn coordinator(dir: &Path, task: &str) -> PathBuf {
        write(
            dir,
            "c1.jsonl",
            &[
                u(
                    "c1",
                    "Lanza un agente para revisar las facturas.",
                    "2026-11-07T09:00:00.000Z",
                ),
                json!({"type":"assistant","sessionId":"c1","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Agent","input":{"prompt":task}}]}}),
                a("c1", "Hecho."),
            ],
        )
    }

    /// Every row a restatement and an undo read or write, with the directory taken out of the
    /// transcript references so that two stores in two directories compare.
    fn snapshot(db: &Db, dir: &Path) -> Vec<String> {
        let d = format!("{}", dir.display());
        let mut out = Vec::new();
        for q in [
            "SELECT id, kind, subject, object, origin, trust, invalid, invalid_reason, invalidated_by, \
             invalidated_via, transcript_ref, session_id, created_at FROM record ORDER BY id",
            "SELECT record_id, turn_ref, new_session, new_created, prior_invalid, prior_reason, prior_by, \
             prior_via, prior_at, prior_ref, prior_session, prior_created, added_turn, after_record, undone \
             FROM restatement ORDER BY id",
            "SELECT restatement_id, record_id FROM restatement_retired ORDER BY 1, 2",
            "SELECT record_id, transcript_ref, session_id, created_at FROM record_turn ORDER BY 1, 2",
            "SELECT hash, session_id, transcript_ref FROM turn_prompt ORDER BY 1, 2",
        ] {
            let mut st = db.conn.prepare(q).unwrap();
            let n = st.column_count();
            let rows: Vec<String> = st
                .query_map([], |r| {
                    let mut v = Vec::new();
                    for i in 0..n {
                        let x: rusqlite::types::Value = r.get(i)?;
                        v.push(match x {
                            rusqlite::types::Value::Text(s) => format!("'{}'", s.replace(&d, "")),
                            other => format!("{other:?}"),
                        });
                    }
                    Ok(v.join("|"))
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            out.extend(rows);
        }
        out
    }

    fn active_typed_users(db: &Db) -> Vec<(i64, String)> {
        db.conn
            .prepare(
                "SELECT id, transcript_ref FROM record WHERE invalid = 0 AND kind <> 'episode' \
                 AND origin = 'user_said' ORDER BY id",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// Ruling 1 of Task 7's first review: the statement is the person's only as one or more
    /// whole consecutive sentences of their message, or the whole of it. A fragment that drops
    /// the negation says the opposite; a piece of a word or a single word says nothing.
    #[test]
    fn only_whole_sentences_the_user_typed_are_theirs() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let msg = "Nunca borres una factura emitida. El listado va primero, por fecha. Las anuladas van en gris.";
        let t = session_with(tmp.path(), msg);
        for stmt in [
            "borres una factura emitida",
            "unca borres una factura emitida",
            "factura",
            "Nunca borres una factura emitida. El listado",
        ] {
            let r = remember(&db, stmt, "decision", Some((&t, "s1"))).unwrap();
            assert_eq!(
                (r.origin.as_str(), r.trust, r.lower),
                ("agent_inferred", 0, Some(NOT_FOUND)),
                "{stmt}"
            );
        }
        for stmt in [
            "Nunca borres una factura emitida",
            "Nunca borres una factura emitida.",
            "El listado va primero, por fecha. Las anuladas van en gris.",
            msg,
        ] {
            let r = remember(&db, stmt, "decision", Some((&t, "s1"))).unwrap();
            assert_eq!(
                (r.origin.as_str(), r.trust, r.turn),
                ("user_said", 3, Some(1)),
                "{stmt}"
            );
        }
    }

    const SAID_AGAIN: &str = "Lo repito para el equipo nuevo. Nunca borres una factura emitida.";

    /// Two stores: the person says the rule, then says it again in a turn the extractor reads
    /// at ingest; in the second, `remember` keeps it while that turn is still being written.
    /// `then` runs on each store afterwards.
    fn said_again_both_ways(then: impl Fn(&Db, &Path)) -> [Vec<String>; 2] {
        let mut out = Vec::new();
        for kept in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let db = store(tmp.path());
            let t1 = session_with(tmp.path(), &format!("{RULE}."));
            crate::ingest::ingest_transcript(&db, &t1, "s1").unwrap();
            let t2 = write(
                tmp.path(),
                "s2.jsonl",
                &[
                    u("s2", SAID_AGAIN, "2026-11-07T10:00:00.000Z"),
                    e("s2", "e1"),
                ],
            );
            if kept {
                let r = remember(&db, RULE, "invariant", Some((&t2, "s2"))).unwrap();
                assert_eq!((r.origin.as_str(), r.outcome), ("user_said", "restated"));
            }
            append(&t2, &[a("s2", "Entendido.")]);
            crate::ingest::ingest_transcript(&db, &t2, "s2").unwrap();
            then(&db, tmp.path());
            out.push(snapshot(&db, tmp.path()));
        }
        out.try_into().unwrap()
    }

    /// Ruling 2: once its turn is ingested, a restatement `remember` made mid-turn leaves the
    /// record, its journal row and their relation to the turn's prompt exactly as the
    /// extractor's restatement of the same turn at ingest leaves them.
    #[test]
    fn a_restatement_kept_mid_turn_settles_as_the_extractor_would() {
        let [extracted, kept] = said_again_both_ways(|_, _| {});
        assert_eq!(kept, extracted);
    }

    /// Ruling 2, then the turn is found to be an agent's copy: the restatement is undone and
    /// the person's earlier record is active again, at its own turn, as on the extractor's path.
    #[test]
    fn a_kept_restatement_found_to_be_an_agents_copy_is_undone() {
        let [extracted, kept] = said_again_both_ways(|db, dir| {
            let c = coordinator(dir, SAID_AGAIN);
            crate::ingest::ingest_transcript(db, &c, "c1").unwrap();
            let users = active_typed_users(db);
            assert_eq!(users.len(), 1, "{users:?}");
            let t1 = dir.join("s1.jsonl");
            let s = crate::parse_any(&t1, 0).unwrap();
            assert_eq!(
                users[0].1,
                format!("{}:{}", t1.display(), s.turns[1].end_offset),
                "the person's record is back at its own turn"
            );
        });
        assert_eq!(kept, extracted);
    }

    /// Ruling 3: a worker kept a sentence of the prompt its coordinator wrote, before either
    /// was ingested. Whichever is ingested first, no record of the worker's turn stays active
    /// as the person's, and what the kept statement had restated is given back as the undo of
    /// an agent's prompt gives it back.
    #[test]
    fn a_statement_kept_from_a_prompt_an_agent_was_handed_is_undone_in_either_order() {
        let task =
            "Revisa el módulo de facturas y dime qué encuentras. Nunca borres una factura emitida.";
        for coordinator_first in [true, false] {
            for said_before in [false, true] {
                let tmp = tempfile::tempdir().unwrap();
                let db = store(tmp.path());
                let t0 = tmp.path().join("s1.jsonl");
                if said_before {
                    session_with(tmp.path(), &format!("{RULE}."));
                    crate::ingest::ingest_transcript(&db, &t0, "s1").unwrap();
                }
                let w = write(
                    tmp.path(),
                    "w1.jsonl",
                    &[u("w1", task, "2026-11-07T09:01:00.000Z"), e("w1", "e1")],
                );
                let r = remember(&db, RULE, "invariant", Some((&w, "w1"))).unwrap();
                assert_eq!(r.origin.as_str(), "user_said", "nothing tells yet");
                let c = coordinator(tmp.path(), task);
                append(&w, &[a("w1", "Reviso.")]);
                if coordinator_first {
                    crate::ingest::ingest_transcript(&db, &c, "c1").unwrap();
                    crate::ingest::ingest_transcript(&db, &w, "w1").unwrap();
                } else {
                    crate::ingest::ingest_transcript(&db, &w, "w1").unwrap();
                    crate::ingest::ingest_transcript(&db, &c, "c1").unwrap();
                }
                let users = active_typed_users(&db);
                let case =
                    format!("coordinator_first={coordinator_first} said_before={said_before}");
                if said_before {
                    let s = crate::parse_any(&t0, 0).unwrap();
                    let want = format!("{}:{}", t0.display(), s.turns[1].end_offset);
                    assert_eq!(users.len(), 1, "{case}: {users:?}");
                    assert_eq!(
                        users[0].1, want,
                        "{case}: the person's record is given back"
                    );
                } else {
                    assert!(users.is_empty(), "{case}: {users:?}");
                }
                assert!(
                    users.iter().all(|(_, r)| !r.contains("w1.jsonl")),
                    "{case}: {users:?}"
                );
            }
        }
    }

    /// Ruling 3, at call time: when another session's transcript already shows this session's
    /// prompt was handed over, the statement is not the person's.
    #[test]
    fn a_prompt_another_known_session_handed_over_is_not_the_users_at_call_time() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let task =
            "Revisa el módulo de facturas y dime qué encuentras. Nunca borres una factura emitida.";
        let w = write(
            tmp.path(),
            "w1.jsonl",
            &[u("w1", task, "2026-11-07T09:01:00.000Z"), e("w1", "e1")],
        );
        let c = coordinator(tmp.path(), task);
        let r = remember_with(&db, RULE, "invariant", Some((&w, "w1")), &[c.as_path()]).unwrap();
        assert_eq!(
            (r.origin.as_str(), r.trust, r.lower),
            ("agent_inferred", 0, Some(NOT_FOUND))
        );
    }

    /// Fix round 2, ruling 3a: whatever its kind, a statement kept from a prompt an agent was
    /// handed does not stay active as the person's, in either ingest order.
    #[test]
    fn a_kept_statement_of_every_kind_from_an_agents_prompt_is_undone_in_either_order() {
        let task =
            "Revisa el módulo de facturas y dime qué encuentras. Nunca borres una factura emitida.";
        for kind in KINDS {
            for coordinator_first in [true, false] {
                let tmp = tempfile::tempdir().unwrap();
                let db = store(tmp.path());
                let w = write(
                    tmp.path(),
                    "w1.jsonl",
                    &[u("w1", task, "2026-11-07T09:01:00.000Z"), e("w1", "e1")],
                );
                let r = remember(&db, RULE, kind, Some((&w, "w1"))).unwrap();
                assert_eq!((r.origin.as_str(), r.kind.as_str()), ("user_said", *kind));
                let c = coordinator(tmp.path(), task);
                append(&w, &[a("w1", "Reviso.")]);
                if coordinator_first {
                    crate::ingest::ingest_transcript(&db, &c, "c1").unwrap();
                    crate::ingest::ingest_transcript(&db, &w, "w1").unwrap();
                } else {
                    crate::ingest::ingest_transcript(&db, &w, "w1").unwrap();
                    crate::ingest::ingest_transcript(&db, &c, "c1").unwrap();
                }
                let left: Vec<(i64, String, i64)> = db
                    .conn
                    .prepare(
                        "SELECT id, kind, trust FROM record WHERE invalid = 0 AND kind <> 'episode' \
                         AND origin = 'user_said'",
                    )
                    .unwrap()
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                assert!(
                    left.is_empty(),
                    "{kind} coordinator_first={coordinator_first}: {left:?}"
                );
                let via: Option<String> = db
                    .conn
                    .query_row(
                        "SELECT invalidated_via FROM record WHERE id = ?1",
                        [r.id],
                        |x| x.get(0),
                    )
                    .unwrap();
                assert_eq!(via.as_deref(), Some("agent_prompt"), "{kind}");
            }
        }
    }

    /// Records by turn and text, with their state, so two stores whose ids differ compare.
    fn states(db: &Db, dir: &Path) -> Vec<(String, String, String, i64, Option<String>)> {
        let d = format!("{}", dir.display());
        db.conn
            .prepare(
                "SELECT kind, body, coalesce(transcript_ref, ''), invalid, invalidated_via FROM record \
                 WHERE kind <> 'episode' ORDER BY transcript_ref, body",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?.replace(&d, ""),
                    r.get(3)?,
                    r.get(4)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// Fix round 2, ruling 3b: a kept restatement whose record a later word of the person's
    /// retired before its turn was ingested still has its journal row settled, and undone as
    /// the fail-safe undo leaves it. A worker's copy of the person's A brought A back and so
    /// retired B; the person then replaced A in another session; then the coordinator's turn and
    /// the worker's are ingested. Against the same sequence with no `remember`: every record
    /// active there is active here or retired with the mark of a record not given back, and
    /// nothing is active here that is not active there.
    #[test]
    fn a_kept_restatement_retired_before_its_turn_is_ingested_is_undone_as_the_fail_safe_leaves_it()
    {
        let task = "Revisa el módulo de facturas y dime qué encuentras. Usamos PostgreSQL para el catálogo.";
        let mut out = Vec::new();
        for kept in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let db = store(tmp.path());
            let t1 = write(
                tmp.path(),
                "s1.jsonl",
                &[
                    u(
                        "s1",
                        "Usamos PostgreSQL para el catálogo.",
                        "2026-11-05T10:00:00.000Z",
                    ),
                    a("s1", "De acuerdo, PostgreSQL."),
                    u(
                        "s1",
                        "En vez de PostgreSQL, el catálogo pasa a SQLite.",
                        "2026-11-05T10:05:00.000Z",
                    ),
                    a("s1", "Cambio el catálogo a SQLite."),
                ],
            );
            crate::ingest::ingest_transcript(&db, &t1, "s1").unwrap();
            let w = write(
                tmp.path(),
                "w1.jsonl",
                &[u("w1", task, "2026-11-07T09:01:00.000Z"), e("w1", "e1")],
            );
            if kept {
                let r = remember(
                    &db,
                    "Usamos PostgreSQL para el catálogo",
                    "decision",
                    Some((&w, "w1")),
                )
                .unwrap();
                assert_eq!((r.origin.as_str(), r.outcome), ("user_said", "restated"));
            }
            let t3 = write(
                tmp.path(),
                "s3.jsonl",
                &[
                    u(
                        "s3",
                        "En vez de PostgreSQL, el catálogo pasa a MySQL.",
                        "2026-11-07T11:00:00.000Z",
                    ),
                    a("s3", "Cambio el catálogo a MySQL."),
                ],
            );
            crate::ingest::ingest_transcript(&db, &t3, "s3").unwrap();
            let c = coordinator(tmp.path(), task);
            append(&w, &[a("w1", "Reviso.")]);
            crate::ingest::ingest_transcript(&db, &c, "c1").unwrap();
            crate::ingest::ingest_transcript(&db, &w, "w1").unwrap();
            if kept {
                let pending: Vec<(String, i64)> = db
                    .conn
                    .prepare("SELECT turn_ref, undone FROM restatement")
                    .unwrap()
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                let s = crate::parse_any(&w, 0).unwrap();
                let at = format!("{}:{}", w.display(), s.turns[0].end_offset);
                assert!(
                    pending.iter().all(|(r, u)| *r == at && *u != 0),
                    "the journal row is settled at the turn and no longer applied: {pending:?}"
                );
            }
            out.push(states(&db, tmp.path()));
        }
        let (plain, kept) = (&out[0], &out[1]);
        for row in kept.iter().filter(|r| r.3 == 0) {
            assert!(
                plain
                    .iter()
                    .any(|p| p.0 == row.0 && p.1 == row.1 && p.2 == row.2 && p.3 == 0),
                "active only with remember: {row:?}\n{kept:#?}\n{plain:#?}"
            );
        }
        for row in plain.iter().filter(|r| r.3 == 0) {
            let here = kept
                .iter()
                .find(|k| k.0 == row.0 && k.1 == row.1 && k.2 == row.2);
            assert!(
                here.is_some_and(|k| k.3 == 0 || k.4.as_deref() == Some("agent_unrestored")),
                "active without remember, neither active nor marked with it: {row:?} -> {here:?}"
            );
        }
    }

    /// Fix round 2, ruling 1r: in the whole-sentence check a line break ends a sentence only
    /// after a stop or at a blank line; otherwise it is a space, so a hard-wrapped "Nunca" is not
    /// cut from its sentence.
    #[test]
    fn a_line_break_inside_a_sentence_is_a_space() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let t = session_with(
            tmp.path(),
            "Revisa el listado.\nNunca\nborres una factura emitida del sistema.",
        );
        let r = remember(
            &db,
            "borres una factura emitida del sistema",
            "invariant",
            Some((&t, "s1")),
        )
        .unwrap();
        assert_eq!(
            (r.origin.as_str(), r.lower),
            ("agent_inferred", Some(NOT_FOUND))
        );
        let r = remember(
            &db,
            "Nunca borres una factura emitida del sistema",
            "invariant",
            Some((&t, "s1")),
        )
        .unwrap();
        assert_eq!(r.origin.as_str(), "user_said", "dictated over two lines");
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let t = session_with(
            tmp.path(),
            "Nunca borres una factura emitida.\nEl listado va primero\n\nLas anuladas van en gris",
        );
        for stmt in [
            "El listado va primero",
            "Las anuladas van en gris",
            "Nunca borres una factura emitida",
        ] {
            let r = remember(&db, stmt, "decision", Some((&t, "s1"))).unwrap();
            assert_eq!(r.origin.as_str(), "user_said", "{stmt}");
        }
    }

    /// Fix round 3, ruling A: a colon does not end a sentence, so a line break after one is a
    /// space and the rule it introduces is not cut from its negation.
    #[test]
    fn a_line_break_after_a_colon_is_a_space() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let t = session_with(
            tmp.path(),
            "No hagas nunca lo siguiente:\nborrar una factura emitida del sistema.",
        );
        let r = remember(
            &db,
            "borrar una factura emitida del sistema",
            "invariant",
            Some((&t, "s1")),
        )
        .unwrap();
        assert_eq!((r.origin.as_str(), r.trust), ("agent_inferred", 0));
    }

    fn deadends(db: &Db) -> Vec<(i64, String, i64, Option<String>)> {
        db.conn
            .prepare("SELECT id, origin, invalid, invalidated_via FROM record WHERE kind = 'deadend' ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// Fix round 3, ruling B: a remembered dead end retires nothing. The person's dead end stays
    /// active when a worker keeps a reworded copy from its task, in either ingest order, and the
    /// copy is revoked.
    #[test]
    fn a_remembered_dead_end_retires_nothing_and_its_copy_is_revoked_in_either_order() {
        let mine = "Compilar con la versión vieja no funciona";
        let task = "Revisa la compilación y dime qué encuentras. No funciona compilar con la versión vieja.";
        for coordinator_first in [true, false] {
            let tmp = tempfile::tempdir().unwrap();
            let db = store(tmp.path());
            let t = session_with(tmp.path(), &format!("{mine}."));
            let own = remember(&db, mine, "deadend", Some((&t, "s1"))).unwrap();
            assert_eq!(own.origin.as_str(), "user_said");
            let w = write(
                tmp.path(),
                "w1.jsonl",
                &[u("w1", task, "2026-11-07T09:01:00.000Z"), e("w1", "e1")],
            );
            let copy = remember(
                &db,
                "No funciona compilar con la versión vieja",
                "deadend",
                Some((&w, "w1")),
            )
            .unwrap();
            assert_eq!(copy.origin.as_str(), "user_said", "nothing tells yet");
            assert_eq!(row(&db, own.id).3, 0, "the copy retired nothing");
            let c = coordinator(tmp.path(), task);
            append(&w, &[a("w1", "Reviso.")]);
            if coordinator_first {
                crate::ingest::ingest_transcript(&db, &c, "c1").unwrap();
                crate::ingest::ingest_transcript(&db, &w, "w1").unwrap();
            } else {
                crate::ingest::ingest_transcript(&db, &w, "w1").unwrap();
                crate::ingest::ingest_transcript(&db, &c, "c1").unwrap();
            }
            let d = deadends(&db);
            let case = format!("coordinator_first={coordinator_first}: {d:?}");
            assert!(d.iter().any(|x| x.0 == own.id && x.2 == 0), "{case}");
            assert!(
                d.iter()
                    .any(|x| x.0 == copy.id && x.2 == 1 && x.3.as_deref() == Some("agent_prompt")),
                "{case}"
            );
        }
    }

    /// Fix round 3, ruling B: two dead ends the person stated on one subject stand side by side.
    #[test]
    fn two_dead_ends_on_one_subject_both_stand() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let t = session_with(
            tmp.path(),
            "Compilar con la versión vieja no funciona. No funciona compilar con la versión vieja.",
        );
        let first = remember(
            &db,
            "Compilar con la versión vieja no funciona",
            "deadend",
            Some((&t, "s1")),
        )
        .unwrap();
        let second = remember(
            &db,
            "No funciona compilar con la versión vieja",
            "deadend",
            Some((&t, "s1")),
        )
        .unwrap();
        assert_ne!(first.id, second.id);
        let d = deadends(&db);
        assert!(d.iter().all(|x| x.2 == 0), "{d:?}");
        assert_eq!(d.len(), 2, "{d:?}");
    }

    const DEAD: &str = "Compilar con la versión vieja no funciona";
    const DEAD_TASK: &str =
        "Revisa la compilación y dime qué encuentras. Compilar con la versión vieja no funciona.";

    /// What a restatement and an undo read or write, without the columns that name the kind,
    /// so that the same sequence played with two kinds compares.
    fn kindless(db: &Db, dir: &Path) -> Vec<String> {
        let d = format!("{}", dir.display());
        let mut out = Vec::new();
        for q in [
            "SELECT id, kind = 'episode', object, origin, trust, invalid, invalid_reason, invalidated_by, \
             invalidated_via, transcript_ref, session_id, created_at FROM record ORDER BY id",
            "SELECT record_id, turn_ref, new_session, new_created, prior_invalid, prior_reason, prior_by, \
             prior_via, prior_ref, prior_session, prior_created, added_turn, after_record, undone \
             FROM restatement ORDER BY id",
            "SELECT restatement_id, record_id FROM restatement_retired ORDER BY 1, 2",
            "SELECT record_id, transcript_ref, session_id, created_at FROM record_turn ORDER BY 1, 2",
            "SELECT hash, session_id, transcript_ref FROM turn_prompt ORDER BY 1, 2",
            "SELECT record_id, held_by FROM agent_retired ORDER BY 1, 2",
        ] {
            let mut st = db.conn.prepare(q).unwrap();
            let n = st.column_count();
            let rows: Vec<String> = st
                .query_map([], |r| {
                    let mut v = Vec::new();
                    for i in 0..n {
                        let x: rusqlite::types::Value = r.get(i)?;
                        v.push(match x {
                            rusqlite::types::Value::Text(s) => format!("'{}'", s.replace(&d, "")),
                            other => format!("{other:?}"),
                        });
                    }
                    Ok(v.join("|"))
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            out.extend(rows);
        }
        out
    }

    /// A worker keeps a sentence of the task its coordinator wrote, as `kind`; the person then
    /// says the same words in a session of their own and has them kept, before the worker's and
    /// the coordinator's turns are ingested or after. Checks what the person's call answers, and
    /// returns the store, its directory, the record and the person's transcript.
    fn copied_then_said(
        kind: &str,
        person_first: bool,
        coordinator_first: bool,
    ) -> (Db, tempfile::TempDir, i64, PathBuf) {
        let case =
            format!("{kind} person_first={person_first} coordinator_first={coordinator_first}");
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let w = write(
            tmp.path(),
            "w1.jsonl",
            &[
                u("w1", DEAD_TASK, "2026-11-07T09:01:00.000Z"),
                e("w1", "e1"),
            ],
        );
        let copy = remember(&db, DEAD, kind, Some((&w, "w1"))).unwrap();
        assert_eq!(
            copy.origin.as_str(),
            "user_said",
            "{case}: nothing tells yet"
        );
        let s1 = write(
            tmp.path(),
            "s1.jsonl",
            &[
                u("s1", &format!("{DEAD}."), "2026-11-07T10:00:00.000Z"),
                e("s1", "e0"),
            ],
        );
        let person = |db: &Db| {
            let r = remember(db, DEAD, kind, Some((&s1, "s1"))).unwrap();
            // what the command answers is true of the row it names
            let (k, origin, trust, invalid, tref) = row(db, r.id);
            assert_eq!(
                (
                    r.kind.as_str(),
                    r.origin.as_str(),
                    r.trust,
                    &r.transcript_ref
                ),
                (k.as_str(), origin.as_str(), trust, &tref),
                "{case}"
            );
            assert_eq!(
                (origin.as_str(), trust, invalid),
                ("user_said", 3, 0),
                "{case}"
            );
            assert!(tref.unwrap().contains("s1.jsonl"), "{case}");
            assert_eq!((r.id, r.outcome), (copy.id, "restated"), "{case}");
        };
        if person_first {
            person(&db);
        }
        let c = coordinator(tmp.path(), DEAD_TASK);
        append(&w, &[a("w1", "Reviso.")]);
        if coordinator_first {
            crate::ingest::ingest_transcript(&db, &c, "c1").unwrap();
            crate::ingest::ingest_transcript(&db, &w, "w1").unwrap();
        } else {
            crate::ingest::ingest_transcript(&db, &w, "w1").unwrap();
            crate::ingest::ingest_transcript(&db, &c, "c1").unwrap();
        }
        if !person_first {
            let (_, _, _, invalid, tref) = row(&db, copy.id);
            let via: Option<String> = db
                .conn
                .query_row(
                    "SELECT invalidated_via FROM record WHERE id = ?1",
                    [copy.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                (invalid, via.as_deref()),
                (1, Some("agent_prompt")),
                "{case}"
            );
            assert!(tref.unwrap().contains("w1.jsonl"), "{case}");
            person(&db);
        }
        append(&s1, &[a("s1", "Anotado.")]);
        crate::ingest::ingest_transcript(&db, &s1, "s1").unwrap();
        (db, tmp, copy.id, s1)
    }

    /// Fix round 4: a dead end the person states in the words a worker already kept from its
    /// task is the person's. Before or after the worker's turn is found to be an agent's, in
    /// either ingest order, it ends active, the person's, at the person's turn, with its
    /// restatement on the journal; and every row a restatement or an undo touches is as the same
    /// sequence leaves it for a decision, which the undo of an agent's prompt covers itself.
    #[test]
    fn a_dead_end_the_person_states_in_a_workers_kept_words_is_theirs_in_every_order() {
        for person_first in [true, false] {
            for coordinator_first in [true, false] {
                let case =
                    format!("person_first={person_first} coordinator_first={coordinator_first}");
                let (db, tmp, id, s1) =
                    copied_then_said("deadend", person_first, coordinator_first);
                let s = crate::parse_any(&s1, 0).unwrap();
                let at = format!("{}:{}", s1.display(), s.turns[0].end_offset);
                let (kind, origin, trust, invalid, tref) = row(&db, id);
                assert_eq!(
                    (kind.as_str(), origin.as_str(), trust, invalid),
                    ("deadend", "user_said", 3, 0),
                    "{case}"
                );
                assert_eq!(tref.as_deref(), Some(at.as_str()), "{case}");
                assert_eq!(active_typed_users(&db), vec![(id, at.clone())], "{case}");
                let journal: Vec<(i64, String, i64)> = db
                    .conn
                    .prepare("SELECT record_id, turn_ref, undone FROM restatement")
                    .unwrap()
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                assert_eq!(journal, vec![(id, at, 0)], "{case}");
                let (ddb, dtmp, _, _) =
                    copied_then_said("decision", person_first, coordinator_first);
                assert_eq!(
                    kindless(&db, tmp.path()),
                    kindless(&ddb, dtmp.path()),
                    "{case}: as the same sequence leaves a decision"
                );
            }
        }
    }

    /// Fix round 4: a dead end said again comes back and retires no other. One retired with
    /// another as its heir, however that came to be, is put back by the person's saying it again,
    /// and the heir stands beside it.
    #[test]
    fn a_dead_end_said_again_comes_back_and_retires_no_other() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let other = "No funciona compilar con la versión vieja";
        let t = session_with(tmp.path(), &format!("{DEAD}. {other}."));
        let first = remember(&db, DEAD, "deadend", Some((&t, "s1"))).unwrap();
        let second = remember(&db, other, "deadend", Some((&t, "s1"))).unwrap();
        db.conn
            .execute(
                "UPDATE record SET invalid = 1, invalid_reason = 'superseded', invalidated_by = ?2, \
                 invalidated_via = 'key' WHERE id = ?1",
                [first.id, second.id],
            )
            .unwrap();
        let t2 = write(
            tmp.path(),
            "s2.jsonl",
            &[
                u(
                    "s2",
                    &format!("Lo repito. {DEAD}."),
                    "2026-11-07T10:00:00.000Z",
                ),
                a("s2", "Anotado."),
            ],
        );
        let again = remember(&db, DEAD, "deadend", Some((&t2, "s2"))).unwrap();
        assert_eq!((again.id, again.outcome), (first.id, "restated"));
        let d = deadends(&db);
        assert!(d.len() == 2 && d.iter().all(|x| x.2 == 0), "{d:?}");
        assert_eq!(
            db.count("SELECT count(*) FROM restatement_retired")
                .unwrap(),
            0
        );
        assert!(row(&db, first.id).4.unwrap().contains("s2.jsonl"));
    }

    /// Fix round 4: the agent's wording is as before. Said again it is the record on file,
    /// unmoved and with no journal row, and a dead end in it retires nothing.
    #[test]
    fn the_agents_wording_said_again_restates_nothing_and_retires_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let t = session_with(tmp.path(), &format!("{DEAD}."));
        let own = remember(&db, DEAD, "deadend", Some((&t, "s1"))).unwrap();
        assert_eq!(own.origin.as_str(), "user_said");
        let reworded = "No funciona compilar con la versión vieja";
        for kind in ["deadend", "decision"] {
            let first = remember(&db, reworded, kind, Some((&t, "s1"))).unwrap();
            assert_eq!(
                (first.origin.as_str(), first.trust, first.outcome),
                ("agent_inferred", 0, "recorded"),
                "{kind}"
            );
            let before = row(&db, first.id);
            let t2 = write(
                tmp.path(),
                "s2.jsonl",
                &[
                    u("s2", "Sigamos con el listado.", "2026-11-07T10:00:00.000Z"),
                    a("s2", "Sí."),
                ],
            );
            for source in [Some((t2.as_path(), "s2")), None] {
                let again = remember(&db, reworded, kind, source).unwrap();
                assert_eq!(
                    (again.id, again.origin.as_str(), again.trust, again.outcome),
                    (first.id, "agent_inferred", 0, "deduped"),
                    "{kind}"
                );
            }
            assert_eq!(row(&db, first.id), before, "{kind}: unmoved");
        }
        assert_eq!(db.count("SELECT count(*) FROM restatement").unwrap(), 0);
        assert_eq!(row(&db, own.id).3, 0, "the person's dead end stands");
        assert_eq!(
            db.count("SELECT count(*) FROM record WHERE invalid = 1")
                .unwrap(),
            0
        );
    }

    /// When the person's statement is kept, in `said_then_copied`.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum When {
        /// Before the worker keeps its copy.
        First,
        /// After the worker's copy, before the worker's and the coordinator's turns are ingested.
        AfterTheCopy,
        /// After those turns are ingested and the copy is revoked.
        AfterTheRevoke,
    }

    /// The person states something at 08:00; a coordinator copies it into a task, and the
    /// worker keeps the copy from its prompt at 09:01, as `kind`. The person's own statement is
    /// kept at `when`. Each answer to the person is checked against the stored row. Returns the
    /// store, its directory, the record and the person's transcript.
    fn said_then_copied(
        kind: &str,
        when: When,
        coordinator_first: bool,
    ) -> (Db, tempfile::TempDir, i64, PathBuf) {
        let case = format!("{kind} {when:?} coordinator_first={coordinator_first}");
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let s1 = write(
            tmp.path(),
            "s1.jsonl",
            &[
                u("s1", &format!("{DEAD}."), "2026-11-07T08:00:00.000Z"),
                e("s1", "e0"),
            ],
        );
        let w = write(
            tmp.path(),
            "w1.jsonl",
            &[
                u("w1", DEAD_TASK, "2026-11-07T09:01:00.000Z"),
                e("w1", "e1"),
            ],
        );
        let person = |db: &Db| -> i64 {
            let r = remember(db, DEAD, kind, Some((&s1, "s1"))).unwrap();
            // what the command answers is true of the row it names
            let (k, origin, trust, invalid, tref) = row(db, r.id);
            assert_eq!(
                (
                    r.kind.as_str(),
                    r.origin.as_str(),
                    r.trust,
                    &r.transcript_ref
                ),
                (k.as_str(), origin.as_str(), trust, &tref),
                "{case}"
            );
            assert_eq!(
                (origin.as_str(), trust, invalid),
                ("user_said", 3, 0),
                "{case}: the row answered with is active"
            );
            r.id
        };
        let mut id = None;
        if when == When::First {
            id = Some(person(&db));
        }
        let copy = remember(&db, DEAD, kind, Some((&w, "w1"))).unwrap();
        assert_eq!(
            copy.origin.as_str(),
            "user_said",
            "{case}: nothing tells yet"
        );
        if when == When::AfterTheCopy {
            id = Some(person(&db));
        }
        let c = coordinator(tmp.path(), DEAD_TASK);
        append(&w, &[a("w1", "Reviso.")]);
        if coordinator_first {
            crate::ingest::ingest_transcript(&db, &c, "c1").unwrap();
            crate::ingest::ingest_transcript(&db, &w, "w1").unwrap();
        } else {
            crate::ingest::ingest_transcript(&db, &w, "w1").unwrap();
            crate::ingest::ingest_transcript(&db, &c, "c1").unwrap();
        }
        if when == When::AfterTheRevoke {
            let got = person(&db);
            assert!(
                row(&db, got).4.unwrap().contains("s1.jsonl"),
                "{case}: at the person's turn"
            );
            id = Some(got);
        }
        assert_eq!(id, Some(copy.id), "{case}: one record");
        append(&s1, &[a("s1", "Anotado.")]);
        crate::ingest::ingest_transcript(&db, &s1, "s1").unwrap();
        (db, tmp, copy.id, s1)
    }

    /// Fix round 5: the person said it first, in a turn older than the worker's copy of it.
    /// Whenever the person's own statement is kept (before the copy, after it, or after the copy
    /// is revoked), whatever its kind and whichever turn is ingested first, it ends as one active
    /// record of the person's at the person's turn, and nothing stays active at the worker's.
    #[test]
    fn a_statement_older_than_a_workers_copy_of_it_ends_as_the_persons_in_every_order() {
        for kind in ["deadend", "decision"] {
            for coordinator_first in [true, false] {
                let mut ends = Vec::new();
                for when in [When::First, When::AfterTheCopy, When::AfterTheRevoke] {
                    let case = format!("{kind} {when:?} coordinator_first={coordinator_first}");
                    let (db, tmp, id, s1) = said_then_copied(kind, when, coordinator_first);
                    let s = crate::parse_any(&s1, 0).unwrap();
                    let at = format!("{}:{}", s1.display(), s.turns[0].end_offset);
                    let (k, origin, trust, invalid, tref) = row(&db, id);
                    assert_eq!(
                        (k.as_str(), origin.as_str(), trust, invalid),
                        (kind, "user_said", 3, 0),
                        "{case}"
                    );
                    assert_eq!(tref.as_deref(), Some(at.as_str()), "{case}");
                    assert_eq!(active_typed_users(&db), vec![(id, at.clone())], "{case}");
                    let session: String = db
                        .conn
                        .query_row("SELECT session_id FROM record WHERE id = ?1", [id], |r| {
                            r.get(0)
                        })
                        .unwrap();
                    assert_eq!(session, "s1", "{case}");
                    let journal: Vec<(i64, String, i64)> = db
                        .conn
                        .prepare("SELECT record_id, turn_ref, undone FROM restatement")
                        .unwrap()
                        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                        .unwrap()
                        .collect::<rusqlite::Result<_>>()
                        .unwrap();
                    if when == When::AfterTheRevoke {
                        // the person's word put the revoked row back: its own restatement stands
                        assert_eq!(journal, vec![(id, at, 0)], "{case}");
                    } else {
                        // the worker's copy was a restatement of the person's, and is undone
                        let ws = crate::parse_any(&tmp.path().join("w1.jsonl"), 0).unwrap();
                        let wat = format!(
                            "{}:{}",
                            tmp.path().join("w1.jsonl").display(),
                            ws.turns[0].end_offset
                        );
                        assert_eq!(journal, vec![(id, wat, 1)], "{case}");
                        assert_eq!(
                            db.count("SELECT count(*) FROM record_turn").unwrap(),
                            0,
                            "{case}: the worker's turn is not one that stated it"
                        );
                        ends.push(kindless(&db, tmp.path()));
                    }
                }
                assert_eq!(
                    ends[0], ends[1],
                    "{kind} coordinator_first={coordinator_first}: whichever call came first"
                );
            }
        }
    }

    /// Fix round 5, the control: the person's statement is on record, active, at their own turn,
    /// and they have it kept again from that turn. It is the record on file: unmoved, and no
    /// journal row.
    #[test]
    fn a_statement_kept_again_from_its_own_turn_is_the_record_on_file() {
        for kind in ["deadend", "decision"] {
            for ingested in [false, true] {
                let tmp = tempfile::tempdir().unwrap();
                let db = store(tmp.path());
                let t = session_with(tmp.path(), &format!("{DEAD}."));
                let first = remember(&db, DEAD, kind, Some((&t, "s1"))).unwrap();
                assert_eq!(
                    (first.origin.as_str(), first.outcome),
                    ("user_said", "recorded")
                );
                if ingested {
                    crate::ingest::ingest_transcript(&db, &t, "s1").unwrap();
                }
                let before = row(&db, first.id);
                let again = remember(&db, DEAD, kind, Some((&t, "s1"))).unwrap();
                assert_eq!(
                    (again.id, again.origin.as_str(), again.trust, again.outcome),
                    (first.id, "user_said", 3, "deduped"),
                    "{kind} ingested={ingested}"
                );
                assert_eq!(row(&db, first.id), before, "{kind} ingested={ingested}");
                assert_eq!(db.count("SELECT count(*) FROM restatement").unwrap(), 0);
                assert_eq!(db.count("SELECT count(*) FROM record_turn").unwrap(), 0);
            }
        }
    }

    /// Fix round 5: the record stands, active, at a later turn nothing shows to be an agent's
    /// (here it is the person's own), and the person has the same words kept from an older turn
    /// of another session. The record stays where it is, the newest saying, and the answer is
    /// `deduped`. The older turn is written down as the one it was said in before, as it would be
    /// had that turn been kept first.
    #[test]
    fn a_statement_kept_from_an_older_turn_leaves_the_record_at_the_newer() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let older = write(
            tmp.path(),
            "s1.jsonl",
            &[
                u("s1", &format!("{DEAD}."), "2026-11-07T08:00:00.000Z"),
                a("s1", "Anotado."),
            ],
        );
        let newer = write(
            tmp.path(),
            "s2.jsonl",
            &[
                u(
                    "s2",
                    &format!("Lo repito. {DEAD}."),
                    "2026-11-07T10:00:00.000Z",
                ),
                a("s2", "Anotado."),
            ],
        );
        let first = remember(&db, DEAD, "decision", Some((&newer, "s2"))).unwrap();
        let before = row(&db, first.id);
        let again = remember(&db, DEAD, "decision", Some((&older, "s1"))).unwrap();
        assert_eq!((again.id, again.outcome), (first.id, "deduped"));
        assert_eq!(row(&db, first.id), before, "unmoved");
        assert_eq!(again.transcript_ref, before.4, "answered as it stands");
        // said in the other order, the store holds the same
        let tmp2 = tempfile::tempdir().unwrap();
        let db2 = store(tmp2.path());
        for (name, sid) in [("s1.jsonl", "s1"), ("s2.jsonl", "s2")] {
            std::fs::copy(tmp.path().join(name), tmp2.path().join(name)).unwrap();
            remember(&db2, DEAD, "decision", Some((&tmp2.path().join(name), sid))).unwrap();
        }
        assert_eq!(kindless(&db, tmp.path()), kindless(&db2, tmp2.path()));
    }
}
