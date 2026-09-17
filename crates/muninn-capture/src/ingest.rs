//! Write path (ENGINE.md §3): transcript → literal episodes and typed candidates →
//! redact → dedup → supersede → insert → caps → project Markdown → index. Byte-offset
//! watermarks make it incremental: a hook that expires loses time, never data.

use crate::episode::from_turn_all;
use crate::extract::{extract, trust_of, Candidate};
use crate::parse_any;
use muninn_core::caps::{MAX_ACTIVE_RECORDS, MAX_INVARIANTS};
use muninn_core::db::now_ms;
use muninn_core::paths::ProjectPaths;
use muninn_core::{Db, Result};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Default, Serialize)]
pub struct IngestStats {
    pub turns: usize,
    pub inserted: usize,
    pub duplicates: usize,
    pub from_offset: u64,
    pub to_offset: u64,
    pub archived: usize,
    pub episodes: usize,
    pub decisions: usize,
    pub deadends: usize,
    pub corrections: usize,
    pub invariants: usize,
    pub superseded: usize,
    pub projected: usize,
    /// Ids inserted by this call, for projection and the sidecar.
    #[serde(skip)]
    pub new_ids: Vec<i64>,
}

fn watermark_key(transcript: &Path) -> String {
    format!(
        "wm:{}",
        blake3::hash(transcript.to_string_lossy().as_bytes()).to_hex()
    )
}

const INSERT: &str = "INSERT OR IGNORE INTO record(kind, subject, relation, object, body, origin, trust, anchor_path, session_id, transcript_ref, dedup_hash, created_at) \
     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)";

fn dedup(kind: &str, subject: &str, relation: &str, object: &str, body: &str) -> String {
    blake3::hash(format!("{kind}|{subject}|{relation}|{object}|{body}").as_bytes())
        .to_hex()
        .to_string()
}

/// Supersession (ENGINE.md §5.1): an active record with the same (subject, relation)
/// and a different object is retired and points at its replacement. Kinds whose
/// subject is unique per record (episodes, corrections, commits) never match.
fn supersede(tx: &rusqlite::Connection, new_id: i64, c: &Candidate) -> Result<usize> {
    if c.kind == "episode" {
        return Ok(0);
    }
    let n = tx.execute(
        "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1 \
         WHERE invalid=0 AND subject=?2 AND relation=?3 AND object<>?4 AND id<>?1 AND kind=?5",
        rusqlite::params![new_id, c.subject, c.relation, c.object, c.kind],
    )?;
    Ok(n)
}

/// Supersession for decisions stated in conversation (PREREGISTRATION.md, 2026-09-17): an
/// active user decision whose content words the new one shares (`extract::replaces`) is
/// retired and points at its replacement; so is the literal episode of the same turn when
/// that turn was short (the episode is the same statement, and serving it would restate the
/// retired decision). Long turns keep their episode.
fn supersede_said(tx: &rusqlite::Connection, new_id: i64, c: &Candidate) -> Result<usize> {
    if c.relation != "user_decision" {
        return Ok(0);
    }
    let parse = |subject: &str| -> (bool, Vec<String>) {
        let rest = subject.strip_prefix("said:").unwrap_or(subject);
        let (flag, words) = rest.split_once(':').unwrap_or(("state", rest));
        (
            flag == "change",
            words
                .split(' ')
                .filter(|w| !w.is_empty())
                .map(str::to_string)
                .collect(),
        )
    };
    let (change, new_words) = parse(&c.subject);
    let olds: Vec<(i64, String, Option<String>)> = {
        let mut st = tx.prepare(
            "SELECT id, subject, transcript_ref FROM record WHERE invalid=0 AND kind='decision' AND relation='user_decision' AND id<>?1",
        )?;
        let rows = st.query_map([new_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.flatten().collect()
    };
    let mut n = 0;
    let own_ref: Option<String> = tx
        .query_row(
            "SELECT transcript_ref FROM record WHERE id=?1",
            [new_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    for (id, subject, tref) in olds {
        let (_, old_words) = parse(&subject);
        if !crate::extract::replaces(&old_words, &new_words, change) {
            continue;
        }
        n += tx.execute(
            "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1 WHERE id=?2 AND invalid=0",
            rusqlite::params![new_id, id],
        )?;
        if let Some(tref) = tref {
            tx.execute(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1 \
                 WHERE invalid=0 AND kind='episode' AND transcript_ref=?2 AND length(body) <= 700",
                rusqlite::params![new_id, tref],
            )?;
        }
    }
    // loop 2: a change also retires an earlier short episode on the same content words, whether
    // or not that earlier message was recognised as a decision
    if change {
        let eps: Vec<(i64, String, Option<String>)> = {
            let mut st = tx.prepare(
                "SELECT id, body, transcript_ref FROM record WHERE invalid=0 AND kind='episode' AND length(body) <= 700 AND id<>?1",
            )?;
            let rows = st.query_map([new_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.flatten().collect()
        };
        for (id, body, tref) in eps {
            if tref.is_some() && tref == own_ref {
                continue;
            }
            let user_part = body
                .strip_prefix("user: ")
                .unwrap_or(&body)
                .split("\nassistant: ")
                .next()
                .unwrap_or("");
            let words = crate::extract::topic_words(user_part);
            if crate::extract::replaces(&words, &new_words, true) {
                n += tx.execute(
                    "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1 WHERE id=?2 AND invalid=0",
                    rusqlite::params![new_id, id],
                )?;
            }
        }
    }
    Ok(n)
}

/// Ingest one transcript from its watermark; `paths` enables the Markdown projection.
pub fn ingest_transcript(
    db: &Db,
    transcript: &Path,
    fallback_session: &str,
) -> Result<IngestStats> {
    ingest_transcript_with(db, transcript, fallback_session, None)
}

pub fn ingest_transcript_with(
    db: &Db,
    transcript: &Path,
    fallback_session: &str,
    paths: Option<&ProjectPaths>,
) -> Result<IngestStats> {
    let key = watermark_key(transcript);
    let from: u64 = db.meta_get(&key)?.and_then(|s| s.parse().ok()).unwrap_or(0);
    let session = parse_any(transcript, from).map_err(|e| muninn_core::Error::io(transcript, e))?;
    let sid = if session.session_id.is_empty() {
        fallback_session.to_string()
    } else {
        session.session_id.clone()
    };
    let mut stats = IngestStats {
        turns: session.turns.len(),
        from_offset: from,
        to_offset: session.end_offset,
        ..Default::default()
    };
    let now = now_ms();
    let mut typed: Vec<(i64, &'static str, Option<String>, String)> = Vec::new();
    let tx = db.write_tx()?;
    {
        let mut ins = tx.prepare(INSERT)?;
        // 1. literal episodes, one or more per turn
        for (t, ep) in session
            .turns
            .iter()
            .flat_map(|t| from_turn_all(&sid, t).into_iter().map(move |e| (t, e)))
        {
            let hash = dedup("episode", &ep.subject, "happened", &ep.object, &ep.body);
            let created = t
                .timestamp
                .as_deref()
                .and_then(parse_rfc3339_ms)
                .unwrap_or(now);
            let anchor = ep.files.first().cloned();
            let n = ins.execute(rusqlite::params![
                "episode",
                ep.subject,
                "happened",
                ep.object,
                ep.body,
                "tool_observed",
                1i64,
                anchor,
                sid,
                format!("{}:{}", transcript.display(), t.end_offset),
                hash,
                created
            ])?;
            if n == 1 {
                stats.inserted += 1;
                stats.episodes += 1;
                stats.new_ids.push(tx.last_insert_rowid());
            } else {
                stats.duplicates += 1;
            }
        }
        // 2. typed candidates: corrections, invariants, commit-linked decisions, dead ends
        let mut ins_anchor = tx.prepare(
            "INSERT OR IGNORE INTO record(kind, subject, relation, object, body, origin, trust, anchor_path, anchor_hash, session_id, transcript_ref, dedup_hash, created_at) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        for c in extract(&session, &sid) {
            let hash = dedup(c.kind, &c.subject, &c.relation, &c.object, &c.body);
            // the anchor's content hash at capture time; the validator compares later [K11]
            let anchor_hash = match (&c.anchor_path, paths) {
                (Some(ap), Some(p)) if c.kind != "deadend" => {
                    muninn_core::filter::file_hash(&p.source_root(), ap)
                }
                _ => None,
            };
            let created = session
                .turns
                .get(c.turn_index)
                .and_then(|t| t.timestamp.as_deref())
                .and_then(parse_rfc3339_ms)
                .unwrap_or(now);
            let n = ins_anchor.execute(rusqlite::params![
                c.kind,
                c.subject,
                c.relation,
                c.object,
                c.body,
                c.origin,
                trust_of(c.origin),
                c.anchor_path,
                anchor_hash,
                sid,
                format!("{}:{}", transcript.display(), c.end_offset),
                hash,
                created
            ])?;
            if n == 1 {
                let id = tx.last_insert_rowid();
                stats.inserted += 1;
                stats.new_ids.push(id);
                typed.push((id, c.kind, c.anchor_path.clone(), c.body.clone()));
                match c.kind {
                    "decision" => stats.decisions += 1,
                    "deadend" => stats.deadends += 1,
                    "correction" => stats.corrections += 1,
                    "invariant" => stats.invariants += 1,
                    _ => {}
                }
                stats.superseded += supersede(&tx, id, &c)?;
                stats.superseded += supersede_said(&tx, id, &c)?;
            } else {
                stats.duplicates += 1;
            }
        }
    }
    // caps [P5]: invariants beyond 60 → the oldest without a rule referencing it
    let inv: i64 = tx.query_row(
        "SELECT count(*) FROM record WHERE invalid=0 AND kind='invariant'",
        [],
        |r| r.get(0),
    )?;
    if inv > MAX_INVARIANTS {
        let over = inv - MAX_INVARIANTS;
        let n = tx.execute(
            "UPDATE record SET invalid=1, invalid_reason='cap' WHERE id IN (\
               SELECT id FROM record WHERE invalid=0 AND kind='invariant' \
               AND id NOT IN (SELECT rationale_ref FROM rule WHERE rationale_ref IS NOT NULL) \
               ORDER BY created_at ASC LIMIT ?1)",
            [over],
        )?;
        stats.archived += n;
    }
    // caps: active records beyond 20 000 → the oldest episodes (retained, not served)
    let active: i64 = tx.query_row("SELECT count(*) FROM record WHERE invalid=0", [], |r| {
        r.get(0)
    })?;
    if active > MAX_ACTIVE_RECORDS {
        let over = active - MAX_ACTIVE_RECORDS;
        stats.archived += tx.execute(
            "UPDATE record SET invalid=1, invalid_reason='cap' WHERE id IN (SELECT id FROM record WHERE invalid=0 AND kind='episode' ORDER BY created_at ASC LIMIT ?1)",
            [over],
        )?;
    }
    tx.commit()?;
    // F3: cues for the typed records — dir from the anchor, symbols from the graph
    // (lexical fallback), one hop of callers, event by kind
    for (id, kind, anchor, body) in &typed {
        let syms: Vec<String> = anchor
            .as_deref()
            .map(|a| defs_in(db, a))
            .unwrap_or_default();
        let mut refs: Vec<String> = Vec::new();
        for s in syms.iter().take(4) {
            refs.extend(referrers_of(db, s));
        }
        let _ = muninn_core::cue::derive(db, *id, kind, anchor.as_deref(), body, &syms, &refs);
    }
    db.meta_set(&key, &session.end_offset.to_string())?;
    db.meta_set("ingest_watermark_ms", &now.to_string())?;
    db.meta_set("records_changed_since_render", "1")?;
    db.meta_set("last_transcript_path", &transcript.to_string_lossy())?;
    if let Some(p) = paths {
        if !stats.new_ids.is_empty() || stats.superseded > 0 || stats.archived > 0 {
            stats.projected = muninn_core::project::project(p, db, &stats.new_ids)?;
        }
    }
    Ok(stats)
}

fn defs_in(db: &Db, path: &str) -> Vec<String> {
    let Ok(mut st) = db
        .conn
        .prepare("SELECT short_name FROM symbol WHERE path = ?1 ORDER BY line LIMIT 12")
    else {
        return vec![];
    };
    st.query_map([path], |r| r.get(0))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

fn referrers_of(db: &Db, short: &str) -> Vec<String> {
    let Ok(mut st) = db.conn.prepare("SELECT DISTINCT from_name FROM symbol_ref WHERE to_name = ?1 AND from_name IS NOT NULL LIMIT 8") else { return vec![] };
    st.query_map([short], |r| r.get(0))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// RFC 3339 → epoch ms (UTC only, which is what both harnesses write).
fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let (date, rest) = s.split_once('T')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let m: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let time = rest.trim_end_matches('Z');
    let mut t = time.split(':');
    let hh: i64 = t.next()?.parse().ok()?;
    let mm: i64 = t.next()?.parse().ok()?;
    let ss: f64 = t.next()?.split(['+', '-']).next()?.parse().ok()?;
    // days from civil
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400_000 + hh * 3_600_000 + mm * 60_000 + (ss * 1000.0) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use muninn_core::db::Mode;

    #[test]
    fn rfc3339_roundtrip() {
        assert_eq!(
            parse_rfc3339_ms("2026-09-12T14:03:11.084Z"),
            Some(1_789_221_791_084)
        );
    }

    /// A tiny Claude-shaped transcript: a correction, an invariant, a commit and a
    /// repeated failure; then the same invariant restated with a different object.
    fn transcript(dir: &Path, name: &str, lines: &[serde_json::Value]) -> std::path::PathBuf {
        let p = dir.join(name);
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&p, text).unwrap();
        p
    }

    fn user(s: &str) -> serde_json::Value {
        serde_json::json!({"type":"user","sessionId":"s1","timestamp":"2026-09-12T14:03:11.084Z","message":{"role":"user","content":s}})
    }
    fn tool_use(id: &str, cmd: &str) -> serde_json::Value {
        serde_json::json!({"type":"assistant","sessionId":"s1","message":{"role":"assistant","content":[{"type":"tool_use","id":id,"name":"Bash","input":{"command":cmd}}]}})
    }
    fn tool_result(id: &str, out: &str, code: i64) -> serde_json::Value {
        serde_json::json!({"type":"user","sessionId":"s1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":out,"is_error":code!=0}]},"toolUseResult":{"exitCode":code}})
    }

    #[test]
    fn typed_kinds_supersession_and_projection() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::from_root(tmp.path());
        for d in paths.all_dirs() {
            std::fs::create_dir_all(d).unwrap();
        }
        let db = Db::open(&paths.db_path(), Mode::ReadWrite).unwrap();
        let t1 = transcript(
            tmp.path(),
            "a.jsonl",
            &[
                user("No, así no. Nunca uses pkill en bash."),
                tool_use("t1", "cargo test -p x"),
                tool_result("t1", "error[E0308]", 101),
                tool_use("t2", "cargo test -p x"),
                tool_result("t2", "error[E0308]", 101),
                user("commit"),
                tool_use("t3", "git commit -m 'fix'"),
                tool_result("t3", "[master 1a2b3c4] fix\n 1 file changed", 0),
            ],
        );
        let st = ingest_transcript_with(&db, &t1, "s1", Some(&paths)).unwrap();
        assert_eq!(st.corrections, 1, "{st:?}");
        assert_eq!(st.invariants, 1);
        assert_eq!(st.decisions, 1);
        assert_eq!(st.deadends, 1);
        assert!(st.projected >= st.inserted);
        assert!(
            paths
                .records_dir()
                .join("invariant")
                .read_dir()
                .unwrap()
                .count()
                == 1
        );
        assert!(paths.index_md().exists());
        // the same invariant key with a different object supersedes the first
        let t2 = transcript(tmp.path(), "b.jsonl", &[user("Nunca uses pkill en Bash!")]);
        let st2 = ingest_transcript_with(&db, &t2, "s2", Some(&paths)).unwrap();
        assert_eq!(st2.invariants, 1);
        assert_eq!(st2.superseded, 1);
        let active: i64 = db
            .count("SELECT count(*) FROM record WHERE invalid=0 AND kind='invariant'")
            .unwrap();
        assert_eq!(active, 1);
        let old: (i64, String) = db
            .conn
            .query_row("SELECT invalidated_by, invalid_reason FROM record WHERE invalid=1 AND kind='invariant'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(old.1, "superseded");
        assert!(old.0 > 0);
        // watermark: re-ingesting the same file inserts nothing
        let st3 = ingest_transcript_with(&db, &t1, "s1", Some(&paths)).unwrap();
        assert_eq!(st3.inserted, 0);
        // export/import roundtrip
        let out = tmp.path().join("x.jsonl");
        let n = muninn_core::project::export_jsonl(&db, &out, true).unwrap();
        assert!(n >= 5);
        let db2p = tmp.path().join("other.db");
        let db2 = Db::open(&db2p, Mode::ReadWrite).unwrap();
        let ist = muninn_core::project::import(&db2, &out).unwrap();
        assert_eq!(ist.inserted, n);
        let ist2 = muninn_core::project::import(&db2, &paths.records_dir()).unwrap();
        assert_eq!(
            ist2.inserted, 0,
            "markdown re-import of the same records is all duplicates: {}",
            ist2.duplicates
        );
    }
}
