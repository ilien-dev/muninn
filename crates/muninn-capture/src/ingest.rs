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
    /// The last turn had no answer yet, so it was left for the next ingest.
    pub held_back: bool,
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
    let (change, _) = parse(&c.subject);
    let olds: Vec<(i64, String, Option<String>)> = {
        let mut st = tx.prepare(
            "SELECT id, object, transcript_ref FROM record WHERE invalid=0 AND kind='decision' AND relation='user_decision' AND id<>?1",
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
    let new_object: String = tx
        .query_row("SELECT object FROM record WHERE id=?1", [new_id], |r| {
            r.get(0)
        })
        .unwrap_or_default();
    for (id, old_object, tref) in olds {
        let new_value = crate::extract::names_new_value(&old_object, &new_object);
        if !crate::extract::replaces_text(&old_object, &new_object, change)
            || !(change || new_value)
        {
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
    // loop 3: a new decision that names a value the earlier one did not, on the same content
    // words, replaces it even without a change marker ("use X for the Y" after "we go with W for
    // the Y"); the earlier short episode goes with it
    let new_text: String = tx
        .query_row("SELECT object FROM record WHERE id=?1", [new_id], |r| {
            r.get(0)
        })
        .unwrap_or_default();
    let new_names = crate::extract::name_tokens(&new_text);
    let new_created: i64 = tx
        .query_row("SELECT created_at FROM record WHERE id=?1", [new_id], |r| {
            r.get(0)
        })
        .unwrap_or(i64::MAX);
    let mut matched_any = n > 0;
    // loop 2: a change also retires an earlier short episode on the same content words, whether
    // or not that earlier message was recognised as a decision
    {
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
            let new_value = crate::extract::names_new_value(user_part, &new_text);
            if crate::extract::replaces_text(user_part, &new_text, change) && (change || new_value)
            {
                n += tx.execute(
                    "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1 WHERE id=?2 AND invalid=0",
                    rusqlite::params![new_id, id],
                )?;
                matched_any = true;
            }
        }
    }
    // loop 3: a short change that names no topic of its own and introduces a new name refers to
    // the most recent earlier short user statement that named something, within three hours
    // loop 5: the named value must be new to the store (a name already recorded is a
    // follow-up about it, not a replacement), or the message must withdraw what came before
    let novel: Vec<&String> = new_names
        .iter()
        .filter(|w| {
            let like = format!("%{w}%");
            tx.query_row(
                "SELECT count(*) FROM record WHERE id<>?1 AND (transcript_ref IS NULL OR transcript_ref<>?2) AND lower(body) LIKE ?3",
                rusqlite::params![new_id, own_ref.clone().unwrap_or_default(), like],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(1)
                == 0
        })
        .collect();
    let withdrawal = crate::extract::is_withdrawal(&new_text);
    // loop 7: the message need no longer be anaphoric. A change that carries its own words
    // ("moving to AWS Secrets Manager") names no topic the earlier decision shares — 23 of 30
    // held-out pairs share no content word at all — so the lexical test cannot reach them and
    // the gate on `is_anaphoric` was rejecting the whole class. What still has to hold: the
    // message announces a change, nothing was matched on words, and it introduces a value the
    // store has not seen. The guard below keeps this from walking onto another change.
    if change
        && !matched_any
        && (!novel.is_empty() || (withdrawal && new_names.is_empty()))
        && crate::extract::is_anaphoric(&new_text)
    {
        // The most recent earlier short statement — but never one that was itself a change.
        // A run of changes arriving together ("switch A to A2", "switch B to B2", …) would
        // otherwise have each one retire the previous change rather than the decision it
        // replaced, which loses a live fact silently: measured on the loop-6 held-out set in
        // block order, plain recency took `kept_b` from 16/30 to 9/30. A statement that
        // announces a change is skipped and the walk continues to the decision under it.
        let prev: Option<(i64, String)> = tx
            .query_row(
                "SELECT e.id, e.body FROM record e WHERE e.invalid=0 AND e.kind='episode' AND length(e.body) <= 700 \
                 AND e.created_at < ?1 AND e.created_at >= ?1 - 10800000 AND (e.transcript_ref IS NULL OR e.transcript_ref <> ?2) \
                 AND NOT EXISTS (SELECT 1 FROM record d WHERE d.kind='decision' AND d.relation='user_decision' \
                                 AND d.transcript_ref = e.transcript_ref AND d.subject LIKE 'said:change:%') \
                 ORDER BY e.created_at DESC LIMIT 1",
                rusqlite::params![new_created, own_ref.clone().unwrap_or_default()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        if let Some((id, body)) = prev {
            let user_part = body
                .strip_prefix("user: ")
                .unwrap_or(&body)
                .split("\nassistant: ")
                .next()
                .unwrap_or("");
            let old_names = crate::extract::name_tokens(user_part);
            if !user_part.contains('?')
                && (withdrawal && new_names.is_empty()
                    || (!old_names.is_empty() && novel.iter().any(|w| !old_names.contains(w))))
            {
                n += tx.execute(
                    "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1 WHERE id=?2 AND invalid=0",
                    rusqlite::params![new_id, id],
                )?;
                // and a decision record taken from that same turn
                let tref: Option<String> = tx
                    .query_row("SELECT transcript_ref FROM record WHERE id=?1", [id], |r| {
                        r.get(0)
                    })
                    .ok()
                    .flatten();
                if let Some(tref) = tref {
                    n += tx.execute(
                        "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1 \
                         WHERE invalid=0 AND kind='decision' AND relation='user_decision' AND transcript_ref=?2",
                        rusqlite::params![new_id, tref],
                    )?;
                }
            }
        }
    }
    if n > 0 {
        inherit_topic(tx, new_id)?;
    }
    Ok(n)
}

/// Supersession by measured value. "payment worker: 3 attempts" and "bump to 7 attempts"
/// share one content word, `attempts`, which is one short of what `replaces` asks for — and
/// lowering that floor was measured and rejected, because a single shared word pairs
/// unrelated decisions [Z5]. The unit of a quantity is not an ordinary shared word: it names
/// the slot, and a different number in the same slot is a different decision.
///
/// The guard that keeps it from walking onto another subsystem's budget is on the *new*
/// statement: it may say nothing but the value ("bump to 7 attempts", at most one content
/// word outside its quantities). A statement that names its own subject ("the search path
/// retries 5 times") is not a bare correction of something earlier and is left to the
/// lexical rules. The most recent record holding that unit is the one replaced.
/// Retire what the assistant's own acknowledgement says was replaced.
///
/// `[Z5]` is the lexical ceiling: 23 of 30 held-out replacements share no content word with
/// what they replace, so `supersede_said` cannot pair them, and on the plain head-to-head 14 of
/// Muninn's 15 failing cells were a stale value served as current. The reply in the same turn
/// routinely names both values — "Got it — switching the TLS backend from openssl to rustls" —
/// and it is already captured. `ack_replacement` reads only the shapes that state a pair, and
/// only when the pair's replacement side is part of what the user actually decided.
///
/// How often a reply states the pair depends on what the assistant was shown: 27% of recorded
/// change replies in sessions where Muninn was injecting the earlier decision, 10% with
/// claude-mem, and 0 of 45 with no memory in the loop at all. Delivery of the stale record is
/// what makes its retirement possible, which is exactly the case this is for.
///
/// Two further guards, because false retirement is the worst thing this can do and loop 8's
/// gate is 0 of 30: the named value must be a whole word of the record it retires, and at most
/// one record is retired — the shortest match, which is the one whose subject the value is.
fn supersede_via_ack(tx: &rusqlite::Connection, new_id: i64, c: &Candidate) -> Result<usize> {
    if c.relation != "user_decision" || c.ack.is_empty() {
        return Ok(0);
    }
    let new_object: String = tx
        .query_row("SELECT object FROM record WHERE id=?1", [new_id], |r| {
            r.get(0)
        })
        .unwrap_or_default();
    let Some(gone) = crate::extract::ack_replacement(&c.ack, &new_object) else {
        return Ok(0);
    };
    let holds = |text: &str| {
        text.to_lowercase()
            .split(|ch: char| !ch.is_alphanumeric() && ch != '.' && ch != '-' && ch != '_')
            .any(|w| w == gone)
    };
    if holds(&new_object) {
        return Ok(0);
    }
    let olds: Vec<(i64, String, Option<String>)> = {
        let mut st = tx.prepare(
            "SELECT id, object, transcript_ref FROM record WHERE invalid=0 AND kind='decision' \
             AND relation='user_decision' AND id<>?1",
        )?;
        let rows = st.query_map([new_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.flatten().collect()
    };
    let Some((id, _, tref)) = olds
        .into_iter()
        .filter(|(_, object, _)| holds(object))
        .min_by_key(|(_, object, _)| object.len())
    else {
        return Ok(0);
    };
    let mut n = tx.execute(
        "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1 \
         WHERE id=?2 AND invalid=0",
        rusqlite::params![new_id, id],
    )?;
    if n > 0 {
        if let Some(tref) = tref {
            n += tx.execute(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1 \
                 WHERE invalid=0 AND kind='episode' AND transcript_ref=?2 AND length(body) <= 700",
                rusqlite::params![new_id, tref],
            )?;
        }
        crate::ingest::inherit_topic(tx, new_id)?;
    }
    Ok(n)
}

fn supersede_quantity(tx: &rusqlite::Connection, new_id: i64, c: &Candidate) -> Result<usize> {
    if c.kind == "episode" {
        // the episode of the same turn carries the same sentence; acting on both would
        // retire the same record twice and count it twice
        return Ok(0);
    }
    let new_text: String = tx
        .query_row("SELECT object FROM record WHERE id=?1", [new_id], |r| {
            r.get(0)
        })
        .unwrap_or_default();
    let new_slots = crate::extract::quantity_slots(&new_text);
    if new_slots.is_empty() || crate::extract::words_outside_quantities(&new_text).len() > 1 {
        return Ok(0);
    }
    let (own_ref, new_created): (Option<String>, i64) = tx
        .query_row(
            "SELECT transcript_ref, created_at FROM record WHERE id=?1",
            [new_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((None, i64::MAX));
    let olds: Vec<(i64, String, Option<String>)> = {
        let mut st = tx.prepare(
            "SELECT id, object, transcript_ref FROM record WHERE invalid=0 AND id<>?1 \
             AND kind IN ('decision','episode') AND length(body) <= 700 AND created_at < ?2 \
             AND (transcript_ref IS NULL OR transcript_ref <> ?3) ORDER BY created_at DESC",
        )?;
        let rows = st.query_map(
            rusqlite::params![new_id, new_created, own_ref.clone().unwrap_or_default()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        rows.flatten().collect()
    };
    let mut n = 0;
    for (id, old_object, tref) in olds {
        let old_slots = crate::extract::quantity_slots(&old_object);
        let collides = old_slots
            .iter()
            .any(|(unit, num)| new_slots.iter().any(|(u2, n2)| u2 == unit && n2 != num));
        if !collides {
            continue;
        }
        n += tx.execute(
            "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1 WHERE id=?2 AND invalid=0",
            rusqlite::params![new_id, id],
        )?;
        // the other record of the same turn states it too
        if let Some(tref) = tref {
            n += tx.execute(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1 \
                 WHERE invalid=0 AND transcript_ref=?2 AND length(body) <= 700",
                rusqlite::params![new_id, tref],
            )?;
        }
        break; // the most recent holder of the unit, and only it
    }
    if n > 0 {
        inherit_topic(tx, new_id)?;
    }
    Ok(n)
}

/// A change that replaced something takes over the replaced statements' topic words (their
/// names excluded) in its supersession key, so a question about the topic reaches it and a
/// later change on the same topic still finds it.
pub fn inherit_topic(tx: &rusqlite::Connection, new_id: i64) -> Result<()> {
    let subject: String =
        tx.query_row("SELECT subject FROM record WHERE id=?1", [new_id], |r| {
            r.get(0)
        })?;
    let mut st = tx.prepare("SELECT body FROM record WHERE invalidated_by=?1")?;
    let olds: Vec<String> = st.query_map([new_id], |r| r.get(0))?.flatten().collect();
    // `said:change:a b c` splits into a key and its words. An episode's subject is
    // `session:<id>#<turn>`, which has no words at all — splitting it the same way made the
    // session id one of them, and since the words are sorted, where the id landed depended on
    // its own first character: the same conversation produced `session:<id>#0 calver
    // versioning` in one run and `session:calver <id>#0 versioning` in the next. Harmless to
    // retrieval, which tokenises both the same way, and fatal to the claim that the same
    // input gives the same store, which is how it was found.
    let (head, words) = if subject.starts_with("session:") {
        (subject.as_str(), "")
    } else {
        subject
            .rsplit_once(':')
            .unwrap_or(("said:change", subject.as_str()))
    };
    let mut set: Vec<String> = words
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    // The key is indexed and never shown: it takes every content word of the replaced
    // statements, names included, which is what makes the replacement findable by the words
    // it omits. Nothing of the replaced statement reaches the *body*. A `topic:` line used to,
    // and it restated the retired value whenever that value was an ordinary lowercase word —
    // `topic: backend openssl` under "Let's use rustls instead", which is the one thing F1
    // exists to prevent. Removing it moved no figure of loops 8, 9, 10 or 11 in either
    // direction, on either order, in any arm.
    let mut added: Vec<String> = Vec::new();
    for b in olds {
        let user_part = b
            .strip_prefix("user: ")
            .unwrap_or(&b)
            .split("\nassistant: ")
            .next()
            .unwrap_or("")
            .to_string();
        for w in crate::extract::topic_words(&user_part) {
            if !set.contains(&w) {
                set.push(w.clone());
                added.push(w.clone());
            }
        }
    }
    if added.is_empty() {
        return Ok(());
    }
    set.sort();
    let (old_subject, object, old_body): (String, String, String) = tx.query_row(
        "SELECT subject, object, body FROM record WHERE id=?1",
        [new_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let new_subject = format!("{head}:{}", set.join(" "));
    tx.execute(
        "UPDATE record SET subject=?1 WHERE id=?2",
        rusqlite::params![new_subject, new_id],
    )?;
    // the full-text index only follows inserts and (in)validation: re-index this row by hand
    tx.execute(
        "INSERT INTO record_fts(record_fts, rowid, subject, object, body) VALUES ('delete', ?1, ?2, ?3, ?4)",
        rusqlite::params![new_id, old_subject, object, old_body],
    )?;
    tx.execute(
        "INSERT INTO record_fts(rowid, subject, object, body) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![new_id, new_subject, object, old_body],
    )?;
    Ok(())
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
    let mut session =
        parse_any(transcript, from).map_err(|e| muninn_core::Error::io(transcript, e))?;
    // A turn whose assistant has not answered yet is not over. Consuming it and moving the
    // watermark past the prompt loses the whole exchange: the answer then arrives after the
    // watermark, the parser has no turn to attach it to, and drops it. The read that lands
    // there is not hypothetical — `maintain` resumes ingest detached, while the session is
    // still being written — though how often it happens in practice is not measured, and
    // comparing this project's live store against a whole re-read of the same transcripts
    // shows no shortfall today.
    //
    // So the last turn is held back when nothing has answered it, and the watermark stops
    // where that turn began. The next ingest reads the turn from its first byte, complete.
    // `a_turn_read_before_its_answer_is_left_for_the_next_ingest` fails without this.
    let held = match session.turns.last() {
        Some(t) if t.assistant_text.trim().is_empty() && t.tools.is_empty() => {
            session.end_offset = match session.turns.len() {
                1 => from,
                n => session.turns[n - 2].end_offset,
            };
            session.turns.pop();
            true
        }
        _ => false,
    };
    let session = session;
    let sid = if session.session_id.is_empty() {
        fallback_session.to_string()
    } else {
        session.session_id.clone()
    };
    let mut stats = IngestStats {
        held_back: held,
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
            let anchor = crate::sole_anchor(&ep.files);
            // The compaction summary is the assistant's own earlier words, condensed by the
            // harness and replayed in the user's role. `tool_observed` is for what the
            // transcript shows happening; a paraphrase is not that, and at trust 0 the
            // renderer frames it as a hint rather than a fact — which is the vocabulary the
            // boot summary already uses for a summary's claims.
            let (origin, trust) = if crate::is_compaction_summary(&t.user_prompt) {
                ("agent_inferred", 0i64)
            } else {
                ("tool_observed", 1i64)
            };
            let n = ins.execute(rusqlite::params![
                "episode",
                ep.subject,
                "happened",
                ep.object,
                ep.body,
                origin,
                trust,
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
                stats.superseded += supersede_quantity(&tx, id, &c)?;
                stats.superseded += supersede_via_ack(&tx, id, &c)?;
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
    // caps: active records beyond 20 000 → the oldest episodes with nothing pointing at them
    //
    // `ENGINE.md` says "sin referencias entrantes" and the query did not ask. An episode can
    // be another record's heir — 68 of them are in this project's own store — and archiving
    // one leaves a lineage whose `replaces #n` names a record that is itself retired, so
    // `muninn show` answers "retired" to a question the catalogue told the agent to ask.
    let active: i64 = tx.query_row("SELECT count(*) FROM record WHERE invalid=0", [], |r| {
        r.get(0)
    })?;
    if active > MAX_ACTIVE_RECORDS {
        let over = active - MAX_ACTIVE_RECORDS;
        stats.archived += tx.execute(
            "UPDATE record SET invalid=1, invalid_reason='cap' WHERE id IN (\
               SELECT id FROM record WHERE invalid=0 AND kind='episode' \
               AND id NOT IN (SELECT invalidated_by FROM record WHERE invalidated_by IS NOT NULL) \
               ORDER BY created_at ASC LIMIT ?1)",
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
    fn assistant(s: &str) -> serde_json::Value {
        serde_json::json!({"type":"assistant","sessionId":"s1","message":{"role":"assistant","content":[{"type":"text","text":s}]}})
    }
    fn tool_use(id: &str, cmd: &str) -> serde_json::Value {
        serde_json::json!({"type":"assistant","sessionId":"s1","message":{"role":"assistant","content":[{"type":"tool_use","id":id,"name":"Bash","input":{"command":cmd}}]}})
    }
    fn tool_result(id: &str, out: &str, code: i64) -> serde_json::Value {
        serde_json::json!({"type":"user","sessionId":"s1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":out,"is_error":code!=0}]},"toolUseResult":{"exitCode":code}})
    }

    /// The harness replays its compaction summary in the user's role. It is the assistant's
    /// own earlier words condensed, so it is neither what a person typed nor what the
    /// transcript observed: 33 of the 118 episodes one of this project's transcripts yields
    /// come from those turns, and all of them read `user:` at trust 1.
    #[test]
    fn a_compaction_summary_is_not_the_user_speaking() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let long: String = "Details that make the summary long enough to chunk. ".repeat(40);
        let t = transcript(
            tmp.path(),
            "t.jsonl",
            &[
                user(&format!(
                    "This session is being continued from a previous conversation that ran \
                     out of context. {long}"
                )),
                assistant("Continuing."),
                user("we are switching to rustls"),
                assistant("Noted."),
            ],
        );
        ingest_transcript(&db, &t, "s1").unwrap();
        let rows: Vec<(String, String, i64)> = db
            .conn
            .prepare("SELECT body, origin, trust FROM record WHERE kind='episode' ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        let (summary, spoken): (Vec<_>, Vec<_>) =
            rows.iter().partition(|(b, _, _)| b.starts_with("summary"));
        assert!(
            summary.len() >= 2,
            "head and at least one continuation: {rows:?}"
        );
        for (_, origin, trust) in &summary {
            assert_eq!((origin.as_str(), *trust), ("agent_inferred", 0));
        }
        assert!(!spoken.is_empty());
        for (b, origin, trust) in &spoken {
            assert!(b.starts_with("user"), "{b}");
            assert_eq!((origin.as_str(), *trust), ("tool_observed", 1));
        }
    }

    /// A hook can read the transcript between a prompt and its answer — `maintain` resumes
    /// ingest detached while the session is still being written. If that read consumes the
    /// prompt and moves the watermark past it, the answer arrives after the watermark, the
    /// parser has no turn to attach it to, and the whole exchange is gone: the module's own
    /// first line promises a hook that expires loses time, never data.
    ///
    /// Ingesting the prompt alone must therefore capture nothing and keep the watermark, so
    /// the next ingest reads the turn from its first byte.
    #[test]
    fn a_turn_read_before_its_answer_is_left_for_the_next_ingest() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let half = transcript(tmp.path(), "t.jsonl", &[user("we are switching to rustls")]);
        let st = ingest_transcript(&db, &half, "s1").unwrap();
        assert!(st.held_back);
        assert_eq!(st.inserted, 0);
        assert_eq!(
            st.to_offset, 0,
            "the watermark may not pass an unanswered prompt"
        );

        // the answer arrives; the same file, read again, now holds the whole turn
        transcript(
            tmp.path(),
            "t.jsonl",
            &[
                user("we are switching to rustls"),
                assistant("Noted — the TLS backend is rustls from here."),
            ],
        );
        let st2 = ingest_transcript(&db, &half, "s1").unwrap();
        assert!(!st2.held_back);
        assert!(st2.inserted > 0, "{st2:?}");
        let body: String = db
            .conn
            .query_row(
                "SELECT body FROM record WHERE kind='episode' ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(body.contains("switching to rustls"), "{body}");
        assert!(body.contains("the TLS backend is rustls"), "{body}");
    }

    /// Two stores given the same conversation must hold the same subjects. They did not: an
    /// episode's session id was sorted in among the inherited topic words, so its own first
    /// character decided the order.
    #[test]
    fn an_episodes_subject_does_not_sort_its_session_id_among_the_topic() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::from_root(tmp.path());
        for d in paths.all_dirs() {
            std::fs::create_dir_all(d).unwrap();
        }
        let db = Db::open(&paths.db_path(), Mode::ReadWrite).unwrap();
        let subject_after = |sid: &str| {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('episode',?1,'said','semver is cleaner','user: semver is cleaner\n','tool_observed',1,'s',?2,1)",
                    rusqlite::params![format!("session:{sid}#0"), sid],
                )
                .unwrap();
            let new = db.conn.last_insert_rowid();
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,invalid,invalidated_by,created_at) \
                     VALUES('decision',?1,'user_decision','calver for versioning','user: calver for versioning\n','user_said',3,'s',?2,1,?3,1)",
                    rusqlite::params![format!("said:state:{sid}old"), format!("{sid}old"), new],
                )
                .unwrap();
            inherit_topic(&db.conn, new).unwrap();
            db.conn
                .query_row::<String, _, _>("SELECT subject FROM record WHERE id=?1", [new], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        // one session id sorts before the topic words, one after
        let early = subject_after("aaaa");
        let late = subject_after("zzzz");
        let shape = |s: &str| {
            s.split(' ')
                .map(|w| if w.starts_with("session:") { "<id>" } else { w })
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(shape(&early), shape(&late), "{early} vs {late}");
        assert!(
            early.starts_with("session:"),
            "the key stays in front: {early}"
        );
    }

    /// The record that replaces another inherits its topic *in its key*, which is indexed and
    /// never shown, so a question about the topic reaches it. Nothing of the replaced
    /// statement reaches the body: a `topic:` line used to, and it restated the retired value
    /// whenever that value was an ordinary lowercase word — `name_tokens` hides `PgBouncer`
    /// and not `sequelize` — which is the one thing F1 exists to prevent.
    #[test]
    fn an_inherited_topic_never_restates_the_value_it_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::from_root(tmp.path());
        for d in paths.all_dirs() {
            std::fs::create_dir_all(d).unwrap();
        }
        let db = Db::open(&paths.db_path(), Mode::ReadWrite).unwrap();
        let ins = |body: &str, hash: &str| {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('decision',?1,'user_decision',?2,?3,'user_said',3,'s',?4,1)",
                    rusqlite::params![format!("said:state:{hash}"), body, format!("user: {body}\n"), hash],
                )
                .unwrap();
            db.conn.last_insert_rowid()
        };
        let old = ins("sequelize for the orm layer", "old");
        let new = ins("moving to typeorm", "new");
        db.conn
            .execute(
                "UPDATE record SET invalid=1, invalidated_by=?1 WHERE id=?2",
                rusqlite::params![new, old],
            )
            .unwrap();
        inherit_topic(&db.conn, new).unwrap();
        let (body, subject): (String, String) = db
            .conn
            .query_row("SELECT body, subject FROM record WHERE id=?1", [new], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert!(
            !body.contains("sequelize"),
            "the replaced value is not restated: {body}"
        );
        assert!(
            !body.contains("layer"),
            "nothing of the replaced statement is shown: {body}"
        );
        // it is still *findable* by both: the key is indexed and never shown
        assert!(
            subject.contains("sequelize"),
            "still reachable by the old value: {subject}"
        );
        assert!(subject.contains("layer"), "and by its topic: {subject}");
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
        // the assistant's reply is what ends the turn; a prompt alone is held for the next
        // ingest (`a_turn_read_before_its_answer_is_left_for_the_next_ingest`)
        let t2 = transcript(
            tmp.path(),
            "b.jsonl",
            &[user("Nunca uses pkill en Bash!"), assistant("Entendido.")],
        );
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

    /// End to end: a replacement that shares no content word with what it replaces is retired
    /// anyway, because the assistant's own reply in that turn named both values.
    ///
    /// `[Z5]` is why this exists — 23 of 30 held-out replacements share no content word, and on
    /// the plain head-to-head 14 of Muninn's 15 failing cells were a stale value served as
    /// current. Its reach is small and measured: of 990 replies recorded by earlier grids,
    /// written before this mechanism existed, 21% state the pair.
    #[test]
    fn a_reply_that_names_both_values_retires_the_one_it_replaced() {
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
                user("We are using openssl for the TLS backend"),
                assistant("Noted, openssl for TLS."),
            ],
        );
        ingest_transcript_with(&db, &t1, "s1", Some(&paths)).unwrap();
        let active = |needle: &str| -> i64 {
            db.count(&format!(
                "SELECT count(*) FROM record WHERE invalid=0 AND kind='decision' AND object LIKE '%{needle}%'"
            ))
            .unwrap()
        };
        assert!(active("openssl") > 0, "the first decision is on record");

        // "Let's use rustls instead" shares no content word with "TLS backend: openssl"
        let t2 = transcript(
            tmp.path(),
            "b.jsonl",
            &[
                user("Let's use rustls instead"),
                assistant("Got it, switching the TLS backend from openssl to rustls."),
            ],
        );
        ingest_transcript_with(&db, &t2, "s2", Some(&paths)).unwrap();
        assert_eq!(active("openssl"), 0, "the reply named it, so it is retired");
        assert!(active("rustls") > 0, "and the replacement is active");
    }

    /// The same messages with a reply that names only the new value retire nothing: the
    /// mechanism reads what was said, never what it could guess.
    #[test]
    fn a_reply_that_names_only_the_new_value_retires_nothing() {
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
                user("We are using openssl for the TLS backend"),
                assistant("Noted."),
            ],
        );
        ingest_transcript_with(&db, &t1, "s1", Some(&paths)).unwrap();
        let t2 = transcript(
            tmp.path(),
            "b.jsonl",
            &[
                user("Let's use rustls instead"),
                assistant("Got it, switching to rustls."),
            ],
        );
        ingest_transcript_with(&db, &t2, "s2", Some(&paths)).unwrap();
        assert!(
            db.count("SELECT count(*) FROM record WHERE invalid=0 AND kind='decision' AND object LIKE '%openssl%'")
                .unwrap()
                > 0,
            "nothing said which value went, so nothing is retired by this path"
        );
    }
}
