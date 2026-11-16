//! Write path (ENGINE.md §3): transcript → literal episodes and typed candidates →
//! redact → dedup → supersede → insert → caps → project Markdown → index. Byte-offset
//! watermarks make it incremental: a hook that expires loses time, never data.

use crate::episode::from_turn_all;
use crate::extract::{extract, trust_of, Candidate};
use crate::parse_any;
use muninn_core::caps::MAX_ACTIVE_RECORDS;
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
    /// Statements the user made again in the same words: a retired one put back, or an
    /// active one moved to the date it was last said.
    pub restated: usize,
    /// Prompts found to be an agent's task: turns marked so, and records already typed from
    /// such a turn in another session that were retired.
    pub agent_prompts: usize,
    pub agent_prompt_retired: usize,
    pub projected: usize,
    /// The last turn had no answer yet, so it was left for the next ingest.
    pub held_back: bool,
    /// Ids inserted by this call, for projection.
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

pub(crate) fn dedup(kind: &str, subject: &str, relation: &str, object: &str, body: &str) -> String {
    blake3::hash(format!("{kind}|{subject}|{relation}|{object}|{body}").as_bytes())
        .to_hex()
        .to_string()
}

/// The turns that stated record `param` (a SQL parameter such as `?2`), as a condition on
/// `transcript_ref`: its own turn, and the earlier ones a restatement moved it away from
/// (`record_turn`, I2). Every same-turn sweep reads this, so that a record replaced after it
/// was restated takes the episodes of each turn that stated it.
pub fn turns_of(param: &str) -> String {
    format!(
        "transcript_ref IN (SELECT transcript_ref FROM record WHERE id = {param} AND transcript_ref IS NOT NULL \
         UNION SELECT transcript_ref FROM record_turn WHERE record_id = {param})"
    )
}

/// A record retires only what was said no later than it (I1): `?1` is the new record's id. A
/// session read after a newer one (a transcript found late, a last message released when its
/// file went idle, a sentence `muninn remember` keeps from an older turn) is older word, and
/// the newest word is the one that holds. Without it the order the files were read in decided
/// which statement was current.
///
/// The date compared is the record's own, with one exception. An active record that was said
/// again moved to the date of that saying (`restate`), and the saying may be a worker's copy
/// not yet known as one: undone, the record goes back to its earlier date, active, and a word
/// of the person's dated between the two and read meanwhile would never have met it. The order
/// probes read that as a replaced value served. So a record a standing restatement moved while
/// it was active is compared by the date it had before the first such move: it is kept from
/// nothing but what is older than its first saying. A record a restatement put back from
/// retired is compared by the date it was put back at; undone, it is retired again.
const NOT_SAID_LATER: &str = "coalesce((SELECT min(j.prior_created) FROM restatement j \
     WHERE j.record_id = record.id AND j.undone <> 1 AND j.prior_invalid = 0), created_at) \
     <= (SELECT created_at FROM record WHERE id = ?1)";

/// Supersession (ENGINE.md §5.1): an active record with the same (subject, relation)
/// and a different object is retired and points at its replacement. An episode or a
/// correction is an event, and a later event does not make an earlier one false: their
/// subjects only name where they happened, so they never take part. Records written before
/// the subject carried a byte offset can still share one.
fn supersede(tx: &rusqlite::Connection, new_id: i64, c: &Candidate) -> Result<usize> {
    if matches!(c.kind, "episode" | "correction") {
        return Ok(0);
    }
    let n = tx.execute(
        &format!(
            "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1, invalidated_via='key' \
             WHERE invalid=0 AND subject=?2 AND relation=?3 AND object<>?4 AND id<>?1 AND kind=?5 \
             AND {NOT_SAID_LATER}"
        ),
        rusqlite::params![new_id, c.subject, c.relation, c.object, c.kind],
    )?;
    Ok(n)
}

/// A correction that repeats an earlier one from the same session replaces it. Corrections of
/// different things stand side by side.
fn supersede_correction(tx: &rusqlite::Connection, new_id: i64, c: &Candidate) -> Result<usize> {
    if c.kind != "correction" {
        return Ok(0);
    }
    let Some((session, _)) = c.subject.split_once('#') else {
        return Ok(0);
    };
    let olds: Vec<(i64, String)> = tx
        .prepare(&format!(
            "SELECT id, object FROM record WHERE invalid=0 AND kind='correction' AND id<>?1 \
             AND substr(subject, 1, length(?2) + 1) = ?2 || '#' AND {NOT_SAID_LATER}"
        ))?
        .query_map(rusqlite::params![new_id, session], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    let mut n = 0;
    for (id, old) in olds {
        if crate::extract::restates(&old, &c.object) {
            n += tx.execute(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1, invalidated_via='correction' WHERE id=?2",
                [new_id, id],
            )?;
        }
    }
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
        let mut st = tx.prepare(&format!(
            "SELECT id, object, transcript_ref FROM record WHERE invalid=0 AND kind='decision' \
             AND relation='user_decision' AND id<>?1 AND {NOT_SAID_LATER}"
        ))?;
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
        // said again after this statement: date order retires it and puts it back, which
        // `meets_a_later_saying` writes once everything this statement replaces is known
        let said_later: bool = tx.query_row(
            "SELECT o.created_at > n.created_at FROM record o, record n WHERE o.id = ?1 AND n.id = ?2",
            [id, new_id],
            |r| r.get(0),
        )?;
        if said_later {
            continue;
        }
        n += tx.execute(
            "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1, invalidated_via='said' WHERE id=?2 AND invalid=0",
            rusqlite::params![new_id, id],
        )?;
        if tref.is_some() {
            tx.execute(
                &format!(
                    "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1, invalidated_via='said' \
                     WHERE invalid=0 AND kind='episode' AND {} AND length(body) <= 700",
                    turns_of("?2")
                ),
                rusqlite::params![new_id, id],
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
    // or not that earlier message was recognised as a decision.
    //
    // WP-06 found most of what this retires on a real store to come from other sessions, weeks
    // earlier. Two narrower actions were measured against it and neither shipped: the same
    // session only, and a link (`replaces`) in place of the retirement. Both lowered
    // retirements on a design grid in loop 8's shape (8/24 to 7 and 6 of 24 with the same
    // session only, to 6 of 24 with a link), so this stays as 1.2.0 had it.
    {
        let eps: Vec<(i64, String, Option<String>)> = {
            let mut st = tx.prepare(&format!(
                "SELECT id, body, transcript_ref FROM record WHERE invalid=0 AND kind='episode' \
                 AND length(body) <= 700 AND id<>?1 AND {NOT_SAID_LATER}"
            ))?;
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
                    "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1, invalidated_via='said' WHERE id=?2 AND invalid=0",
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
                    "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1, invalidated_via='said' WHERE id=?2 AND invalid=0",
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
                        "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1, invalidated_via='said' \
                         WHERE invalid=0 AND kind='decision' AND relation='user_decision' \
                         AND (transcript_ref=?2 OR id IN (SELECT record_id FROM record_turn WHERE transcript_ref=?2))",
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
        let mut st = tx.prepare(&format!(
            "SELECT id, object, transcript_ref FROM record WHERE invalid=0 AND kind='decision' \
             AND relation='user_decision' AND id<>?1 AND {NOT_SAID_LATER}"
        ))?;
        let rows = st.query_map([new_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.flatten().collect()
    };
    let candidate = olds
        .into_iter()
        .filter(|(_, object, _)| holds(object))
        .min_by_key(|(_, object, _)| object.len());
    // The reply names a value that was never typed. That happens whenever the earlier message
    // stated it in no form this file recognises — "LFU cache keeps frequently accessed items
    // around, makes sense for our workload" — and it leaves the value on record as an episode
    // with nothing to retire it, so a question about the cache eviction policy reaches that
    // episode and not the decision that replaced it. Measured on one fixture's stores: the
    // per-prompt block held the old value and not the new one in eleven of fifty-four cells,
    // and the new decision shares no content word with the question because the words are all
    // in the episode.
    //
    // So the episode is retired, and `inherit_topic` then carries its words to the record that
    // replaced it, which is what makes the replacement reachable by the words it omits. The
    // guards are the same ones that make this safe for a decision and one more: the reply must
    // name the value outright, the value must be a whole word of the episode, only the
    // shortest match goes, and the episode has to be short enough to be a statement rather
    // than a transcript.
    let candidate = match candidate {
        Some(c) => Some(c),
        None => {
            let mut st = tx.prepare(&format!(
                "SELECT id, object, transcript_ref FROM record WHERE invalid = 0 \
                 AND kind = 'episode' AND length(body) <= 700 AND id <> ?1 AND {NOT_SAID_LATER}"
            ))?;
            let rows = st.query_map([new_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.flatten()
                .filter(|(_, object, _): &(i64, String, Option<String>)| holds(object))
                .min_by_key(|(_, object, _)| object.len())
        }
    };
    let Some((id, _, tref)) = candidate else {
        return Ok(0);
    };
    let mut n = tx.execute(
        "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1, invalidated_via='ack' \
         WHERE id=?2 AND invalid=0",
        rusqlite::params![new_id, id],
    )?;
    if n > 0 {
        if tref.is_some() {
            n += tx.execute(
                &format!(
                    "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1, invalidated_via='ack' \
                     WHERE invalid=0 AND kind='episode' AND {} AND length(body) <= 700",
                    turns_of("?2")
                ),
                rusqlite::params![new_id, id],
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
            "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1, invalidated_via='quantity' WHERE id=?2 AND invalid=0",
            rusqlite::params![new_id, id],
        )?;
        // the other record of the same turn states it too
        if tref.is_some() {
            n += tx.execute(
                &format!(
                    "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1, invalidated_via='quantity' \
                     WHERE invalid=0 AND {} AND length(body) <= 700",
                    turns_of("?2")
                ),
                rusqlite::params![new_id, id],
            )?;
        }
        break; // the most recent holder of the unit, and only it
    }
    if n > 0 {
        inherit_topic(tx, new_id)?;
    }
    Ok(n)
}

/// A typed statement the user makes again in the same words (WP-03). Its hash is already on
/// record, so the insert is ignored, and 1.2.0 stopped there: a statement retired by a later
/// change could never come back by being said again, so A → B → A left B current, and a rule
/// retired for any reason stayed retired however often it was typed. The restatement is the
/// newest thing said on its subject, and the newest word is the one that holds.
///
/// - Only what a person said counts (`user_said`); a tool's or a commit's repeat says nothing new.
/// - The same turn read a second time (its `transcript_ref` is the row's own) is not a
///   restatement, and neither is one older than what the row already records.
/// - An active row moves to the date it was last said.
/// - A row the person revoked is put back only by a statement made after the revocation (R1).
/// - A retired row is put back, unless what retired it (followed to the end of its chain of
///   heirs) is newer than the restatement. The record at that end — the current word that
///   replaced it — is retired with the restated row as its heir, together with the short
///   episode of its turn, but only when it replaced nothing else: a change that also retired
///   another statement still holds that one, and then both stay current, newest first.
/// - A statement that retires nothing when it is first said (`retires` off: a dead end kept in
///   the person's words) retires nothing when it is said again: it is put back, and the record
///   that had replaced it stands beside it.
/// - A statement of the person's is theirs even when its turn is older than what the row
///   records (`at.earlier`). The record's hash holds no session, so the row may be a worker's
///   copy of the same words, from a task written after the person spoke; and a worker's turn
///   is read before its coordinator's whenever it ends first, which is the usual order (C1).
///   Revoked as an agent's, the row is put back at the person's turn: the copy never counted,
///   and the person's word is the only one said. Active, it stays where it is and the person's
///   turn is written down as the one it was said in before ([`said_before`]): whatever turn
///   the row stands at for `muninn remember`, and for ingest when it stands at a later turn of
///   another session.
/// - A row revoked as an agent's and put back, by an older turn or a newer one, is said by the
///   person for the first time. It then replaces what a new statement replaces, which the
///   caller sees to ([`first_said`]).
///
/// Returns the row's id when it was moved or put back, and with it the journal row of a first
/// saying.
fn restate(
    tx: &rusqlite::Connection,
    hash: &str,
    origin: &str,
    tref: &str,
    at: &Place,
) -> Result<Option<(i64, Option<i64>)>> {
    let (created, sid, retires) = (at.created, at.sid, at.supersedes);
    if origin != "user_said" {
        return Ok(None);
    }
    #[allow(clippy::type_complexity)]
    let Ok((id, invalid, by, was_at, old_ref, old_sid, old_reason, old_via, old_at)) = tx
        .query_row(
            "SELECT id, invalid, invalidated_by, created_at, transcript_ref, session_id, \
             invalid_reason, invalidated_via, invalidated_at FROM record WHERE dedup_hash=?1",
            [hash],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                ))
            },
        )
    else {
        return Ok(None);
    };
    if old_ref.as_deref() == Some(tref) {
        return Ok(None);
    }
    // R1. A record the person revoked (`muninn revoke`) comes back only by being said after
    // the revocation. A session dated before it and read after it is older word, whenever it
    // is read: the revocation is the newest thing the person said about the record.
    if invalid == 1
        && old_reason.as_deref() == Some("revoked")
        && old_via.as_deref() == Some("revoked")
        && old_at.is_some_and(|revoked_at| created <= revoked_at)
    {
        return Ok(None);
    }
    if created <= was_at {
        let said_elsewhere = match at.earlier {
            Earlier::No => return Ok(None),
            Earlier::Any => true,
            Earlier::Elsewhere => created < was_at && old_sid.as_deref() != Some(sid),
        };
        if invalid == 0 {
            if let Some(old) = old_ref.as_ref().filter(|_| said_elsewhere) {
                said_before(
                    tx,
                    id,
                    (old, old_sid.as_deref(), was_at),
                    (tref, sid, created),
                )?;
            }
            return Ok(None);
        }
        if old_via.as_deref() != Some("agent_prompt") {
            // R3. The row was typed by a turn now known to be an agent's, and something
            // retired it before that turn was found out (another worker's copy of another
            // statement, say), so it was never revoked as an agent's. The person said these
            // words before the agent's turn: the row is theirs, at their turn. It stays
            // retired as it is. If what retired it is undone, it comes back at the person's
            // turn and is not taken for the agent's.
            if let Some(old) = old_ref.as_ref().filter(|_| said_elsewhere) {
                let agents: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM turn_prompt p JOIN agent_task a \
                                   ON a.hash = p.hash AND a.session_id <> p.session_id \
                                   WHERE p.transcript_ref = ?2) \
                     AND NOT EXISTS(SELECT 1 FROM restatement j WHERE j.record_id = ?1 \
                                    AND j.turn_ref = ?2 AND j.undone <> 1)",
                    rusqlite::params![id, old],
                    |r| r.get(0),
                )?;
                if agents {
                    tx.execute(
                        "UPDATE record SET created_at = ?2, transcript_ref = ?3, session_id = ?4 WHERE id = ?1",
                        rusqlite::params![id, created, tref, sid],
                    )?;
                }
            }
            return Ok(None);
        }
    }
    // Every restatement is journalled: what the record was before, what it is now, and (below)
    // what this restatement retired, so that one found later to be an agent's copy is undone
    // exactly (`agent::undo_restatements`).
    let move_to_now = |tx: &rusqlite::Connection| -> Result<i64> {
        // the turn it leaves is still a turn that stated it (I2)
        let mut added = 0;
        if let Some(old) = &old_ref {
            added = tx.execute(
                "INSERT OR IGNORE INTO record_turn(record_id, transcript_ref, session_id, created_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, old, old_sid, was_at],
            )?;
        }
        tx.execute(
            "UPDATE record SET invalid=0, invalid_reason=NULL, invalidated_by=NULL, \
             created_at=?2, transcript_ref=?3, session_id=?4 WHERE id=?1",
            rusqlite::params![id, created, tref, sid],
        )?;
        tx.execute(
            "INSERT INTO restatement(record_id, turn_ref, new_session, new_created, prior_invalid, \
             prior_reason, prior_by, prior_via, prior_at, prior_ref, prior_session, prior_created, added_turn, \
             after_record) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, (SELECT max(id) FROM record))",
            rusqlite::params![
                id, tref, sid, created, invalid, old_reason, by, old_via, old_at, old_ref, old_sid,
                was_at, added as i64
            ],
        )?;
        Ok(tx.last_insert_rowid())
    };
    if invalid == 0 {
        let journal = move_to_now(tx)?;
        // R2: a decision dated between the two sayings, read before the first of them and
        // left standing beside it, would have replaced it and been replaced in its turn
        said_in_between(tx, id, journal, was_at, created)?;
        return Ok(Some((id, None)));
    }
    let first = old_via.as_deref() == Some("agent_prompt");
    // the current end of what replaced it
    let mut chain = vec![id];
    let mut end: Option<(i64, i64, Option<String>)> = None;
    let mut next = by;
    while let Some(h) = next {
        if chain.contains(&h) {
            break;
        }
        chain.push(h);
        let Ok((inv, h_by, h_at, h_ref)) = tx.query_row(
            "SELECT invalid, invalidated_by, created_at, transcript_ref FROM record WHERE id=?1",
            [h],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        ) else {
            break;
        };
        if inv == 0 {
            end = Some((h, h_at, h_ref));
            break;
        }
        next = h_by;
    }
    if end.as_ref().is_some_and(|(_, at, _)| *at >= created) {
        return Ok(None);
    }
    let journal = move_to_now(tx)?;
    let note = |tx: &rusqlite::Connection, ids: &[i64]| -> Result<()> {
        for r in ids {
            tx.execute(
                "INSERT OR IGNORE INTO restatement_retired(restatement_id, record_id) VALUES (?1, ?2)",
                rusqlite::params![journal, r],
            )?;
        }
        Ok(())
    };
    if let Some((h, _, h_ref)) = end.filter(|_| retires) {
        // what else the current word replaced, besides this chain and the turns it came from
        let chain_list = chain
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let others: i64 = tx.query_row(
            &format!(
                "SELECT count(*) FROM record WHERE invalidated_by=?1 AND id NOT IN ({chain_list}) \
                 AND (transcript_ref IS NULL OR transcript_ref NOT IN \
                      (SELECT transcript_ref FROM record WHERE id IN ({chain_list}) AND transcript_ref IS NOT NULL \
                       UNION SELECT transcript_ref FROM record_turn WHERE record_id IN ({chain_list})) \
                      AND transcript_ref <> ?2)"
            ),
            rusqlite::params![h, old_ref.clone().unwrap_or_default()],
            |r| r.get(0),
        )?;
        if others == 0 {
            if tx.execute(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1, invalidated_via='restated' \
                 WHERE id=?2 AND invalid=0",
                [id, h],
            )? == 1
            {
                note(tx, &[h])?;
            }
            // its turn's short episode, and a record typed from the same sentence — "Actually,
            // switch to B" is typed as a correction too — state it as well
            if h_ref.is_some() {
                let pick = format!(
                    "invalid=0 AND {} AND length(body) <= 700 AND id <> ?1 \
                     AND (kind='episode' OR instr(body, (SELECT object FROM record WHERE id=?2)) > 0)",
                    turns_of("?2")
                );
                let ids: Vec<i64> = tx
                    .prepare(&format!("SELECT id FROM record WHERE {pick}"))?
                    .query_map(rusqlite::params![id, h], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                tx.execute(
                    &format!(
                        "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1, invalidated_via='restated' \
                         WHERE {pick}"
                    ),
                    rusqlite::params![id, h],
                )?;
                note(tx, &ids)?;
            }
        }
    }
    Ok(Some((id, first.then_some(journal))))
}

/// What a statement replaces when it is said: by key, a correction said again, and, unless it
/// is an answer to a dialog, by its words, its quantity and the reply that names both values.
fn replace(tx: &rusqlite::Connection, id: i64, c: &Candidate, at: &Place) -> Result<usize> {
    let mut superseded = supersede(tx, id, c)?;
    superseded += supersede_correction(tx, id, c)?;
    // An answer in AskUserQuestion carries the question's wording and every word of the
    // option picked. The choice is the person's and goes on record, but that text says
    // nothing about which earlier record it replaces.
    if !at.answer {
        superseded += supersede_said(tx, id, c)?;
        superseded += supersede_quantity(tx, id, c)?;
        superseded += supersede_via_ack(tx, id, c)?;
        if c.relation == "user_decision" {
            superseded += meets_a_later_saying(tx, id, c)?;
        }
    }
    // what was said after it, and read before it, stands beside it, marked as in conflict
    mark_against_later(tx, id, c, at)?;
    Ok(superseded)
}

/// The person says for the first time what only an agent's prompt had said (C1): record `id`,
/// revoked as an agent's, has just been put back at their turn by the restatement `journal`.
/// The agent's copy never counted, so nothing has met this statement yet, and it replaces what
/// it would have replaced typed new. Without this the value it changes stayed current beside
/// it in the worker-first order, and was retired in the other.
///
/// What it retires is written down as the restatement's (`restated`, `restatement_retired`):
/// if this turn is in its turn found to be an agent's, the undo of the journal row gives it
/// back (`agent::undo`), as it gives back what any restatement retired.
fn first_said(
    tx: &rusqlite::Connection,
    id: i64,
    journal: i64,
    c: &Candidate,
    at: &Place,
) -> Result<usize> {
    let held = |tx: &rusqlite::Connection| -> Result<Vec<i64>> {
        let mut st = tx.prepare_cached(
            "SELECT id FROM record WHERE invalid = 1 AND invalidated_by = ?1 \
             AND invalidated_via IN ('key', 'said', 'quantity', 'ack', 'correction')",
        )?;
        let ids = st
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    };
    let before = held(tx)?;
    let superseded = replace(tx, id, c, at)?;
    for r in held(tx)?.into_iter().filter(|r| !before.contains(r)) {
        tx.execute(
            "UPDATE record SET invalidated_via = 'restated' WHERE id = ?1",
            [r],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO restatement_retired(restatement_id, record_id) VALUES (?1, ?2)",
            [journal, r],
        )?;
    }
    Ok(superseded)
}

/// The date a record was first said at: before any standing restatement moved it or put it
/// back. A record first said after another did not exist when the other was said.
const FIRST_SAID: &str = "coalesce((SELECT min(j.prior_created) FROM restatement j \
     WHERE j.record_id = record.id AND j.undone <> 1), created_at)";

/// Whether a decision's key says it announced a change (`said:change:…`).
fn announces_change(subject: &str) -> bool {
    let rest = subject.strip_prefix("said:").unwrap_or(subject);
    rest.split_once(':')
        .is_some_and(|(flag, _)| flag == "change")
}

/// The test `supersede_said` applies: does `new`, which announces a change or not, replace
/// `old` by its words?
fn replaced_by_words(old: &str, new: &str, change: bool) -> bool {
    crate::extract::replaces_text(old, new, change)
        && (change || crate::extract::names_new_value(old, new))
}

/// Record `h` is retired with `id` as its heir, as a restatement of `id` retires the word that
/// had replaced it (`restate`): `h`, the short episode of its turn and a record typed from the
/// same sentence. Each is written down under the journal row, so the undo of that row gives
/// them back.
fn retire_under(tx: &rusqlite::Connection, id: i64, h: i64, journal: i64) -> Result<usize> {
    let note = |tx: &rusqlite::Connection, r: i64| -> Result<()> {
        tx.execute(
            "INSERT OR IGNORE INTO restatement_retired(restatement_id, record_id) VALUES (?1, ?2)",
            [journal, r],
        )?;
        Ok(())
    };
    let mut n = tx.execute(
        "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1, invalidated_via='restated' \
         WHERE id=?2 AND invalid=0",
        [id, h],
    )?;
    if n == 1 {
        note(tx, h)?;
    }
    let pick = format!(
        "invalid=0 AND {} AND length(body) <= 700 AND id <> ?1 \
         AND (kind='episode' OR instr(body, (SELECT object FROM record WHERE id=?2)) > 0)",
        turns_of("?2")
    );
    let ids: Vec<i64> = tx
        .prepare(&format!("SELECT id FROM record WHERE {pick}"))?
        .query_map([id, h], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    n += tx.execute(
        &format!(
            "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1, invalidated_via='restated' \
             WHERE {pick}"
        ),
        [id, h],
    )?;
    for r in ids {
        note(tx, r)?;
    }
    Ok(n)
}

/// R2. Record `a` was said, then `b` was said and replaces it by its words, then `a` was said
/// again; the journal row `journal` is that second saying of `a`, and it was read before `b`.
/// Read in date order, `b` would have retired `a` and the second saying would have put `a` back
/// and retired `b`. That is written here: the state the journal returns `a` to is "retired by
/// `b`", and `b` is retired with `a` as its heir unless it replaced something else as well, in
/// which case both stand, as in `restate`.
fn replaced_then_said_again(
    tx: &rusqlite::Connection,
    a: i64,
    b: i64,
    journal: i64,
    via: &str,
) -> Result<usize> {
    tx.execute(
        "UPDATE restatement SET prior_invalid = 1, prior_reason = 'superseded', prior_by = ?2, \
         prior_via = ?3, prior_at = (SELECT created_at FROM record WHERE id = ?2) WHERE id = ?1",
        rusqlite::params![journal, b, via],
    )?;
    let others: i64 = tx.query_row(
        "SELECT count(*) FROM record WHERE invalidated_by = ?1 AND id <> ?2 AND kind <> 'episode'",
        [b, a],
        |r| r.get(0),
    )?;
    if others > 0 {
        return Ok(0);
    }
    retire_under(tx, a, b, journal)
}

/// R2. The new decision `new_id` replaces by its words a decision that is dated after it only
/// because the person said it again later (a standing restatement moved it while it was
/// active, from a date before the new decision to one after). `supersede_said` leaves such a
/// record alone; [`replaced_then_said_again`] writes what date order would have left.
fn meets_a_later_saying(tx: &rusqlite::Connection, new_id: i64, c: &Candidate) -> Result<usize> {
    let (object, created): (String, i64) = tx.query_row(
        "SELECT object, created_at FROM record WHERE id = ?1",
        [new_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let change = announces_change(&c.subject);
    let rows: Vec<(i64, String, i64)> = tx
        .prepare(
            "SELECT r.id, r.object, j.id FROM record r JOIN restatement j ON j.record_id = r.id \
             WHERE r.invalid = 0 AND r.kind = 'decision' AND r.relation = 'user_decision' \
             AND r.id <> ?1 AND r.created_at > ?2 AND j.undone <> 1 AND j.prior_invalid = 0 \
             AND j.prior_created <= ?2 AND j.new_created > ?2 \
             ORDER BY r.id, j.prior_created DESC, j.id DESC",
        )?
        .query_map(rusqlite::params![new_id, created], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut n = 0;
    let mut seen = None;
    for (id, old_object, journal) in rows {
        if seen == Some(id) {
            continue;
        }
        seen = Some(id);
        if replaced_by_words(&old_object, &object, change) {
            n += replaced_then_said_again(tx, id, new_id, journal, "said")?;
        }
    }
    Ok(n)
}

/// R2. The older saying of record `id` has just been written down under the journal row
/// `journal` (`said_before`): the record was first said at `first`, and stands at `last`. A
/// decision of the person's dated between the two that replaces it by its words would, in
/// date order, have retired it and been retired by the second saying.
fn said_in_between(
    tx: &rusqlite::Connection,
    id: i64,
    journal: i64,
    first: i64,
    last: i64,
) -> Result<()> {
    let object: Option<String> = match tx.query_row(
        "SELECT object FROM record WHERE id = ?1 AND kind = 'decision' AND relation = 'user_decision'",
        [id],
        |r| r.get(0),
    ) {
        Ok(o) => Some(o),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.into()),
    };
    let Some(object) = object else {
        return Ok(());
    };
    // first said between the two: a record a restatement moved or put back there was said
    // before, and did not replace this one by being said again
    let rows: Vec<(i64, String, String)> = tx
        .prepare(&format!(
            "SELECT id, object, subject FROM record WHERE invalid = 0 AND kind = 'decision' \
             AND relation = 'user_decision' AND id <> ?1 AND created_at > ?2 AND created_at < ?3 \
             AND {FIRST_SAID} > ?2 ORDER BY created_at, id"
        ))?
        .query_map(rusqlite::params![id, first, last], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (b, b_object, b_subject) in rows {
        if replaced_by_words(&object, &b_object, announces_change(&b_subject)) {
            replaced_then_said_again(tx, id, b, journal, "said")?;
            break;
        }
    }
    Ok(())
}

/// R2. The other half of "a record retires only what was said no later than it". A record
/// read after a later one that would have replaced it is not retired: it stays on record
/// beside the later one, and the two are marked as in conflict (`filter::mark_conflict`), for
/// the agent to ask which stands. That holds for every rule that can be read from the two
/// records: the key, a correction said again, the words of a decision, a bare quantity.
///
/// Retiring it instead, as date order would, was tried twice and taken back. For the words of
/// a decision it retired a labelled statement of the B1 replay that the person still holds
/// (`v1:W02:1508`). For the other three, the later record may be a worker's prompt not yet
/// known as an agent's: the person's statement was then retired under a row that is revoked
/// once the coordinator is read, and the undo could not tell what to give back (nothing was
/// current in one read order, two statements in another).
///
/// The later record is the first, by date, that would replace this one; when that record has
/// itself been replaced since, the mark goes to the active record at the end of its heirs. A
/// row revoked as an agent's never counted and is passed over.
///
/// Not marked, because the later record's side cannot be read from the store: the reply that
/// names both values (`supersede_via_ack`, the reply is not kept as such) and the
/// subject-less change that takes the statement just before it (it depends on what the store
/// held then). An older statement read after one of those stays active beside it, unmarked.
/// So does an untyped message a later change shares its words with.
fn mark_against_later(tx: &rusqlite::Connection, id: i64, c: &Candidate, at: &Place) -> Result<()> {
    if c.kind == "episode" {
        return Ok(());
    }
    let created = at.created;
    // the usual case, a record newer than everything on file, costs one index probe
    let any_later: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM record WHERE invalid = 0 AND created_at > ?1)",
        [created],
        |r| r.get(0),
    )?;
    if !any_later {
        return Ok(());
    }
    let (invalid, object): (i64, String) = tx.query_row(
        "SELECT invalid, object FROM record WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if invalid != 0 {
        return Ok(());
    }
    const NOT_AN_AGENTS: &str = "invalidated_via IS NOT 'agent_prompt'";
    let mut later: Option<i64> = None;
    if c.kind != "correction" {
        later = match tx.query_row(
            &format!(
                "SELECT id FROM record WHERE subject = ?2 AND relation = ?3 AND object <> ?4 \
                 AND kind = ?5 AND id <> ?1 AND created_at > ?6 AND {FIRST_SAID} > ?6 \
                 AND {NOT_AN_AGENTS} ORDER BY {FIRST_SAID}, id LIMIT 1"
            ),
            rusqlite::params![id, c.subject, c.relation, c.object, c.kind, created],
            |r| r.get::<_, i64>(0),
        ) {
            Ok(h) => Some(h),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
    } else if let Some((session, _)) = c.subject.split_once('#') {
        let rows: Vec<(i64, String)> = tx
            .prepare(&format!(
                "SELECT id, object FROM record WHERE kind = 'correction' AND id <> ?1 \
                 AND substr(subject, 1, length(?2) + 1) = ?2 || '#' AND created_at > ?3 \
                 AND {NOT_AN_AGENTS} ORDER BY created_at, id"
            ))?
            .query_map(rusqlite::params![id, session, created], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        later = rows
            .into_iter()
            .find(|(_, other)| crate::extract::restates(&object, other))
            .map(|(h, _)| h);
    }
    if later.is_none() && c.relation == "user_decision" && !at.answer {
        let rows: Vec<(i64, String, String)> = tx
            .prepare(&format!(
                "SELECT id, object, subject FROM record WHERE invalid = 0 AND kind = 'decision' \
                 AND relation = 'user_decision' AND id <> ?1 AND created_at > ?2 \
                 AND {FIRST_SAID} > ?2 ORDER BY {FIRST_SAID}, id"
            ))?
            .query_map(rusqlite::params![id, created], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        later = rows
            .into_iter()
            .find(|(_, other, subject)| {
                replaced_by_words(&object, other, announces_change(subject))
            })
            .map(|(h, _, _)| h);
    }
    if later.is_none() && !at.answer {
        later = later_quantity(tx, id, &object, created)?;
    }
    let Some(mut later) = later else {
        return Ok(());
    };
    // the record that stands today where the later one stood
    for _ in 0..16 {
        let (invalid, heir): (i64, Option<i64>) = tx.query_row(
            "SELECT invalid, invalidated_by FROM record WHERE id = ?1",
            [later],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        match (invalid, heir) {
            (0, _) => break,
            (_, Some(h)) if h != id && h != later => later = h,
            _ => return Ok(()),
        }
    }
    muninn_core::filter::mark_conflict(tx, id, later)?;
    Ok(())
}

/// The later bare statement of a quantity that replaces the record `id` (`supersede_quantity`
/// read from the older side): the first later typed record that says nothing but a value in a
/// unit this one holds with another number, provided no record dated between the two holds
/// that unit with another number than the later one's. Such a record in between was the most
/// recent holder when the later statement was made, and the later statement took it instead.
fn later_quantity(
    tx: &rusqlite::Connection,
    id: i64,
    object: &str,
    created: i64,
) -> Result<Option<i64>> {
    let slots = crate::extract::quantity_slots(object);
    if slots.is_empty() {
        return Ok(None);
    }
    let collides = |a: &[(String, String)], b: &[(String, String)]| {
        a.iter()
            .any(|(unit, num)| b.iter().any(|(u2, n2)| u2 == unit && n2 != num))
    };
    let rows: Vec<(i64, String, String, i64)> = tx
        .prepare(
            "SELECT id, kind, object, created_at FROM record WHERE id <> ?1 AND created_at > ?2 \
             AND kind IN ('decision', 'episode', 'correction', 'invariant') AND length(body) <= 700 \
             AND invalidated_via IS NOT 'agent_prompt' ORDER BY created_at, id",
        )?
        .query_map(rusqlite::params![id, created], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let later = rows.iter().find(|(_, kind, text, _)| {
        kind != "episode" && {
            let s = crate::extract::quantity_slots(text);
            !s.is_empty()
                && crate::extract::words_outside_quantities(text).len() <= 1
                && collides(&slots, &s)
        }
    });
    let Some((heir, _, text, at)) = later else {
        return Ok(None);
    };
    let wanted = crate::extract::quantity_slots(text);
    let between = rows.iter().any(|(_, kind, other, when)| {
        when < at
            && matches!(kind.as_str(), "decision" | "episode")
            && collides(&crate::extract::quantity_slots(other), &wanted)
    });
    Ok((!between).then_some(*heir))
}

/// The record stands, active, at a turn (`now`: reference, session, date) newer than a turn of
/// the person's (`before`) that `muninn remember` has just found the same words in. The record
/// stays where it is: the newest saying holds. What is written is what the store would hold had
/// the older turn been kept first and the newer one restated it: the older turn as one that
/// stated the record (`record_turn`), and a journal row for the newer turn whose prior state is
/// the record active at the older one. If the newer turn is later found to be an agent's, the
/// undo of its restatements returns the record to the person's turn (`agent::undo`); if it
/// never is, the row is never read.
///
/// Nothing is written when the older turn is already on record as one that stated it. When the
/// record already has a journal row standing at its turn, it has a state to return to, and no
/// row is added; but if the person's turn is older than the first state the journal would
/// return it to, that state becomes the person's turn (R3). The first state may be a worker's
/// turn not yet known as one: returned there, the record would be revoked as an agent's though
/// the person said it before any agent did.
fn said_before(
    tx: &rusqlite::Connection,
    id: i64,
    now: (&str, Option<&str>, i64),
    before: (&str, &str, i64),
) -> Result<()> {
    let known: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM restatement WHERE record_id = ?1 AND turn_ref = ?2 AND undone = 0) \
         OR EXISTS(SELECT 1 FROM record_turn WHERE record_id = ?1 AND transcript_ref = ?3)",
        rusqlite::params![id, now.0, before.0],
        |r| r.get(0),
    )?;
    if known {
        // the journal's first standing row, if the record was active before it
        let first: Option<(i64, i64, i64)> = match tx.query_row(
            "SELECT id, prior_invalid, prior_created FROM restatement \
             WHERE record_id = ?1 AND undone <> 1 ORDER BY id LIMIT 1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ) {
            Ok(row) => Some(row),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        if let Some((journal, 0, prior_created)) = first {
            if before.2 < prior_created {
                tx.execute(
                    "UPDATE restatement SET prior_ref = ?2, prior_session = ?3, prior_created = ?4 WHERE id = ?1",
                    rusqlite::params![journal, before.0, before.1, before.2],
                )?;
            }
        }
        return Ok(());
    }
    tx.execute(
        "INSERT INTO record_turn(record_id, transcript_ref, session_id, created_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![id, before.0, before.1, before.2],
    )?;
    tx.execute(
        "INSERT INTO restatement(record_id, turn_ref, new_session, new_created, prior_invalid, \
         prior_ref, prior_session, prior_created, added_turn, after_record) \
         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, 1, (SELECT max(id) FROM record))",
        rusqlite::params![id, now.0, now.1, now.2, before.0, before.1, before.2],
    )?;
    said_in_between(tx, id, tx.last_insert_rowid(), before.2, now.2)?;
    Ok(())
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

/// Where and how a typed candidate is written: its turn's reference, session and date, whether
/// it is a dialog answer, whether it may retire what it replaces, and whether said again it
/// restates its record.
pub(crate) struct Place<'a> {
    pub tref: Option<String>,
    pub sid: &'a str,
    pub created: i64,
    pub answer: bool,
    pub anchor_hash: Option<String>,
    /// Ingest's candidates always do; a statement written at a trust below `user_said`
    /// (`remember.rs`) never retires anything.
    pub supersedes: bool,
    /// Whether the same statement said again in a later turn moves its record there, or puts it
    /// back (`restate`). Ingest's candidates always do. A statement written at a trust below
    /// `user_said` never does. A dead end kept in the person's words does, and retires nothing
    /// (`supersedes` off): two dead ends on one subject stand side by side.
    pub restates: bool,
    /// Whether the statement is taken as the person's from a turn older than the row its hash
    /// meets (`restate`).
    pub earlier: Earlier,
}

/// When a statement counts as the person's although its turn is older than the row its hash
/// meets (`restate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Earlier {
    /// Never: a statement written below `user_said`, and a released turn read again whole,
    /// which is the same saying and not an older one.
    No,
    /// Ingest's candidates: when the row was revoked as an agent's, or stands at a later turn
    /// of another session.
    Elsewhere,
    /// `muninn remember`, for words it found in a turn the person wrote: whichever turn the
    /// row stands at.
    Any,
}

/// What [`store_candidate`] did with a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stored {
    /// A new row, and how many rows it retired.
    Inserted(i64, usize),
    /// Its hash was on record and the row was moved to this statement (`restate`); and how
    /// many rows it retired, when the person said it for the first time (`first_said`).
    Restated(i64, usize),
    /// Its hash, which was on record, and nothing changed.
    Duplicate(String),
}

/// One typed candidate through dedupe, insert, supersession and restatement: the path every
/// typed record takes, whether ingest typed it or `muninn remember` was asked to keep it.
/// Inside the caller's write transaction.
pub(crate) fn store_candidate(
    tx: &rusqlite::Connection,
    c: &Candidate,
    at: &Place,
) -> Result<Stored> {
    let hash = dedup(c.kind, &c.subject, &c.relation, &c.object, &c.body);
    let n = tx
        .prepare_cached(
            "INSERT OR IGNORE INTO record(kind, subject, relation, object, body, origin, trust, anchor_path, anchor_hash, session_id, transcript_ref, dedup_hash, created_at) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?
        .execute(rusqlite::params![
            c.kind,
            c.subject,
            c.relation,
            c.object,
            c.body,
            c.origin,
            trust_of(c.origin),
            c.anchor_path,
            at.anchor_hash,
            at.sid,
            at.tref,
            hash,
            at.created
        ])?;
    if n == 1 {
        let id = tx.last_insert_rowid();
        if !at.supersedes {
            return Ok(Stored::Inserted(id, 0));
        }
        return Ok(Stored::Inserted(id, replace(tx, id, c, at)?));
    }
    // The same option picked again under the same question hashes to the same record, and an
    // answer says nothing about what it replaces: as above, it supersedes nothing, so it does
    // not restate either.
    let Some(tref) = at.tref.as_deref().filter(|_| !at.answer && at.restates) else {
        return Ok(Stored::Duplicate(hash));
    };
    Ok(match restate(tx, &hash, c.origin, tref, at)? {
        Some((id, Some(journal))) if at.supersedes => {
            Stored::Restated(id, first_said(tx, id, journal, c, at)?)
        }
        Some((id, _)) => Stored::Restated(id, 0),
        None => Stored::Duplicate(hash),
    })
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
    ingest(db, transcript, fallback_session, paths, false)
}

/// WP-10. As [`ingest_transcript_with`], and a last turn nothing answered is taken as it is
/// instead of being held for a next ingest that may never come: for SessionEnd, and for
/// `maintain` on a transcript idle long enough that no answer is still being written.
pub fn ingest_transcript_released(
    db: &Db,
    transcript: &Path,
    fallback_session: &str,
    paths: Option<&ProjectPaths>,
) -> Result<IngestStats> {
    ingest(db, transcript, fallback_session, paths, true)
}

/// How long a transcript must have been left alone before `maintain` releases its last turn.
pub const RELEASE_IDLE: std::time::Duration = std::time::Duration::from_secs(3_600);

/// Has nothing written to this transcript for [`RELEASE_IDLE`]?
pub fn is_idle(transcript: &Path) -> bool {
    std::fs::metadata(transcript)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age >= RELEASE_IDLE)
}

fn ingest(
    db: &Db,
    transcript: &Path,
    fallback_session: &str,
    paths: Option<&ProjectPaths>,
    release_held: bool,
) -> Result<IngestStats> {
    let key = watermark_key(transcript);
    let mut from: u64 = db.meta_get(&key)?.and_then(|s| s.parse().ok()).unwrap_or(0);
    // A last turn released before its answer (WP-10) is read again from its first byte, so
    // that an answer arriving later, when the session is resumed, finds its turn instead of
    // being dropped past the watermark. `released` is the turn's (start, end) as released.
    let rkey = format!("released:{key}");
    let released: Option<(u64, u64)> = db
        .meta_get(&rkey)?
        .and_then(|v| {
            let (a, b) = v.split_once(':')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .filter(|&(start, end)| from == end || from == start);
    if let Some((start, _)) = released {
        from = start;
    }
    let mut session = crate::parse_any_with_tasks(transcript, from)
        .map_err(|e| muninn_core::Error::io(transcript, e))?;
    let unanswered =
        |t: &crate::model::Turn| t.assistant_text.trim().is_empty() && t.tools.is_empty();
    // The released turn, read again. Grown (its answer arrived), it is read whole and its
    // prompt-only episode gives way. Unchanged, everything in it is on record already: it is
    // left out, and what follows it is read as a parse from the watermark would read it.
    let mut released_done = false;
    let reread: Option<String> = released.and_then(|(start, end)| {
        let first = session.turns.first()?;
        (first.start_offset == start && first.end_offset != end)
            .then(|| format!("{}:{end}", transcript.display()))
    });
    if let Some((start, end)) = released {
        let unchanged = session
            .turns
            .first()
            .is_some_and(|t| t.start_offset == start && t.end_offset == end);
        if reread.is_none() && unchanged {
            session.turns.remove(0);
            for (i, t) in session.turns.iter_mut().enumerate() {
                t.index = i;
            }
            if session.turns.is_empty() {
                session.end_offset = end;
            } else {
                released_done = true;
            }
        }
    }
    released_done |= reread.is_some();
    let release_now = match session.turns.last() {
        Some(t) if release_held && unanswered(t) => Some((t.start_offset, t.end_offset)),
        _ => None,
    };
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
        Some(t) if !release_held && unanswered(t) => {
            session.end_offset = match session.turns.len() {
                1 => from,
                n => session.turns[n - 2].end_offset,
            };
            session.turns.pop();
            true
        }
        _ => false,
    };
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
    let mut typed: Vec<(i64, &'static str)> = Vec::new();
    // the rows this connection has changed so far: whether the transaction below changes any
    // is what says the Markdown mirror is behind (WP-11)
    let rows_before = db.conn.total_changes();
    let tx = db.write_tx()?;
    // who wrote each prompt: a task another session handed over is an agent's (`agent.rs`)
    stats.agent_prompts = crate::agent::mark_from_store(&tx, &mut session, &sid)?;
    let session = session;
    // the tasks this batch hands over are found, and the prompts they show to be an agent's
    // undone, before anything of the batch is written: its own statements were typed before
    // the worker ran and have to meet the store as it was then
    stats.agent_prompt_retired = crate::agent::find_out(&tx, &session, &sid)?;
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
                .and_then(|ms| muninn_core::db::sane_created_at(ms, now))
                .unwrap_or(now);
            let anchor = crate::sole_anchor(&ep.files);
            // The compaction summary is the assistant's own earlier words, condensed by the
            // harness and replayed in the user's role. `tool_observed` is for what the
            // transcript shows happening; a paraphrase is not that, and at trust 0 the
            // renderer frames it as a hint rather than a fact — which is the vocabulary the
            // boot summary already uses for a summary's claims.
            let (origin, trust) = if crate::is_compaction_summary(&t.user_prompt) || t.agent_written
            {
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
        // a record `muninn remember` kept while a turn of this batch was being written points
        // at the turn's end from now on, as the ones typed below do
        let kept = crate::remember::settle(&tx, transcript, &session, &sid)?;
        // 2. typed candidates: corrections, invariants, commit-linked decisions, dead ends
        for c in extract(&session, &sid) {
            let answer = session.turns.get(c.turn_index).is_some_and(|t| t.answered);
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
                .and_then(|ms| muninn_core::db::sane_created_at(ms, now))
                .unwrap_or(now);
            let at = Place {
                tref: Some(format!("{}:{}", transcript.display(), c.end_offset)),
                sid: &sid,
                created,
                answer,
                anchor_hash,
                supersedes: true,
                restates: true,
                // the released turn read again whole is the saying already on record
                earlier: if reread.is_some() && c.turn_index == 0 {
                    Earlier::No
                } else {
                    Earlier::Elsewhere
                },
            };
            // the statement `muninn remember` kept from this turn is the turn's record
            if c.origin == "user_said"
                && at
                    .tref
                    .as_deref()
                    .is_some_and(|r| crate::remember::is_kept(&kept, r, &c))
            {
                stats.duplicates += 1;
                continue;
            }
            match store_candidate(&tx, &c, &at)? {
                Stored::Inserted(id, superseded) => {
                    stats.inserted += 1;
                    stats.new_ids.push(id);
                    typed.push((id, c.kind));
                    match c.kind {
                        "decision" => stats.decisions += 1,
                        "deadend" => stats.deadends += 1,
                        "correction" => stats.corrections += 1,
                        "invariant" => stats.invariants += 1,
                        _ => {}
                    }
                    stats.superseded += superseded;
                }
                Stored::Restated(id, superseded) => {
                    stats.duplicates += 1;
                    stats.restated += 1;
                    stats.new_ids.push(id);
                    stats.superseded += superseded;
                }
                Stored::Duplicate(_) => stats.duplicates += 1,
            }
        }
    }
    stats.superseded += retire_dead_ends_that_succeeded(&tx, &session, &sid)?;
    if let Some(old) = &reread {
        // the head episode of the turn as read now is the heir of the one the release wrote
        let new_ref = format!("{}:{}", transcript.display(), session.turns[0].end_offset);
        // the heir is a row of the grown turn, never one at the released ref
        let heir: Option<i64> = (new_ref != *old)
            .then(|| {
                tx.query_row(
                    "SELECT id FROM record WHERE kind = 'episode' AND invalid = 0 AND session_id = ?1 \
                     AND transcript_ref = ?2 ORDER BY id LIMIT 1",
                    rusqlite::params![sid, new_ref],
                    |r| r.get(0),
                )
                .ok()
            })
            .flatten();
        if let Some(h) = heir {
            stats.superseded += tx.execute(
                "UPDATE record SET invalid = 1, invalid_reason = 'superseded', invalidated_by = ?1, \
                 invalidated_via = 'answered' WHERE kind = 'episode' AND invalid = 0 AND transcript_ref = ?2 AND id <> ?1",
                rusqlite::params![h, old],
            )?;
        }
    }
    crate::agent::record_prompts(&tx, &session, &sid, transcript)?;
    // There is no cap on invariants any more (WP-04). 1.2.0 retired the oldest past sixty, and
    // on one real store that took a durable project-wide rule and two standing scope rules
    // while sixty newer sentences, a third of them observations that merely said "always",
    // held the slots. Age is not evidence that a rule stopped holding, and the session-start
    // block serves invariants newest first under its own budget, so the count no longer
    // bounds what reaches the agent. A rule leaves by being replaced, revoked or restated.
    //
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
    // the released turn's offsets travel with the records it wrote, in the same transaction
    if let Some((start, end)) = release_now {
        tx.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
            rusqlite::params![rkey, format!("{start}:{end}")],
        )?;
    } else if released_done {
        tx.execute("DELETE FROM meta WHERE key = ?1", [&rkey])?;
    }
    tx.commit()?;
    let wrote = db.conn.total_changes() != rows_before;
    // F3: cues for the typed records, the events of their kind
    for (id, kind) in &typed {
        let _ = muninn_core::cue::derive(db, *id, kind);
    }
    db.meta_set(&key, &session.end_offset.to_string())?;
    db.meta_set("ingest_watermark_ms", &now.to_string())?;
    // Only an ingest that wrote something leaves the mirror behind. Said on every call, the
    // 63 ingests a `Stop` runs over finished transcripts each asked the next `maintain` for
    // every record's file again.
    if wrote {
        db.meta_set("records_changed_since_project", "1")?;
    }
    db.meta_set("last_transcript_path", &transcript.to_string_lossy())?;
    if let Some(p) = paths {
        if !stats.new_ids.is_empty() || stats.superseded > 0 || stats.archived > 0 {
            stats.projected = muninn_core::project::project(p, db, &stats.new_ids)?;
        }
    }
    Ok(stats)
}

/// WP-09. A dead end is the last run of a command failing, and a hook reads one turn per Stop:
/// a later success of the same command in another batch never reached the dead end, so the
/// store depended on how Stop had sliced the transcript. Any run of a command that exited 0
/// retires that command's dead end of the same session (`invalidated_via = 'succeeded'`). A
/// dead end typed in this batch already knows the batch's own later runs.
fn retire_dead_ends_that_succeeded(
    tx: &rusqlite::Connection,
    session: &crate::model::Session,
    sid: &str,
) -> Result<usize> {
    let mut retire = tx.prepare_cached(
        "UPDATE record SET invalid = 1, invalid_reason = 'superseded', invalidated_via = 'succeeded' \
         WHERE invalid = 0 AND kind = 'deadend' AND session_id = ?1 AND subject = ?2",
    )?;
    // the batch's last run of each command decides: a pass followed by a failure is a
    // regression, and its dead end stands
    let mut last: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
    for t in &session.turns {
        for c in &t.tools {
            if c.name == "Bash" || c.name == "shell" {
                last.insert(crate::extract::norm_cmd(&c.target), c.exit_code == Some(0));
            }
        }
    }
    let mut n = 0;
    for (cmd, ok) in last {
        if ok {
            n += retire.execute(rusqlite::params![sid, format!("cmd:{cmd}")])?;
        }
    }
    Ok(n)
}

/// RFC 3339 → epoch ms (UTC only, which is what both harnesses write).
pub(crate) fn parse_rfc3339_ms(s: &str) -> Option<i64> {
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

/// Retire, once, the typed records that came from a compaction summary.
///
/// Before 1.1.0 a summary that opened with a notice (`<artifact-content-authored-by-others/>`)
/// was read as the user speaking, and its sentences went on record as decisions, invariants
/// and corrections at trust 3: 70 of them across one person's own projects (experiment
/// extract-v1). Refusing new ones leaves those in place, and with session starts served newest
/// first they compete for the same six or seven slots. Each is found by re-reading the turn its
/// `transcript_ref` names; a transcript with no summary in it is not parsed. Retired, never
/// deleted, with the reason in `meta` like any revocation. A `meta` key keeps it to one run.
pub fn retire_summary_records(db: &Db) -> Result<usize> {
    const DONE: &str = "summary_typed_records_retired";
    if db.meta_get(DONE)?.is_some() {
        return Ok(0);
    }
    let rows: Vec<(i64, String)> = db
        .conn
        .prepare(
            "SELECT id, transcript_ref FROM record WHERE invalid = 0 AND origin = 'user_said' \
             AND kind IN ('decision', 'invariant', 'correction') AND transcript_ref IS NOT NULL",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    let mut by_file: std::collections::HashMap<String, Vec<(i64, u64)>> =
        std::collections::HashMap::new();
    for (id, tref) in rows {
        if let Some((path, off)) = tref.rsplit_once(':') {
            if let Ok(off) = off.parse::<u64>() {
                by_file.entry(path.to_string()).or_default().push((id, off));
            }
        }
    }
    let mut retire = Vec::new();
    for (path, recs) in by_file {
        let p = Path::new(&path);
        let Ok(bytes) = std::fs::read(p) else {
            continue;
        };
        if !String::from_utf8_lossy(&bytes).contains("This session is being continued") {
            continue;
        }
        let Ok(session) = parse_any(p, 0) else {
            continue;
        };
        let summary_ends: std::collections::HashSet<u64> = session
            .turns
            .iter()
            .filter(|t| crate::is_compaction_summary(&t.user_prompt))
            .map(|t| t.end_offset)
            .collect();
        retire.extend(
            recs.into_iter()
                .filter(|(_, off)| summary_ends.contains(off))
                .map(|(id, _)| id),
        );
    }
    let tx = db.write_tx()?;
    let now = now_ms();
    for id in &retire {
        let n = tx.execute(
            "UPDATE record SET invalid = 1, invalid_reason = 'revoked', invalidated_via = 'summary' WHERE id = ?1 AND invalid = 0",
            [id],
        )?;
        if n == 1 {
            tx.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    format!("revoked:{id}"),
                    format!("{now}|compaction summary, not the user")
                ],
            )?;
        }
    }
    if !retire.is_empty() {
        muninn_core::db::note_event(&tx, "summary")?;
        tx.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('records_changed_since_project', '1')",
            [],
        )?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
        rusqlite::params![DONE, now.to_string()],
    )?;
    tx.commit()?;
    Ok(retire.len())
}

/// One pass that puts back what two bugs retired, up to 1.1.0.
///
/// A correction's subject held the turn's index, which restarts at 0 on every ingest, so a
/// correction could retire another one that had nothing to do with it. And `maintain` chose as
/// the replacement any record sharing a word with a commit's added lines, older ones included,
/// so a live statement ended up retired in favour of something said before it. Both leave a
/// shape nothing else can: a correction replaced by an unrelated correction of the same subject,
/// and a record replaced by an older one. Each restored id is noted in `meta` under `restored:<id>`.
/// A `meta` key keeps it to one run.
pub fn restore_false_retirements(db: &Db) -> Result<usize> {
    const DONE: &str = "false_retirements_restored";
    if db.meta_get(DONE)?.is_some() {
        return Ok(0);
    }
    let rows: Vec<(i64, String, String, bool)> = db
        .conn
        .prepare(
            "SELECT r.id, r.object, h.object, h.created_at < r.created_at \
             FROM record r JOIN record h ON h.id = r.invalidated_by \
             WHERE r.invalid = 1 AND r.invalid_reason = 'superseded' \
               AND ((r.kind = 'correction' AND h.kind = 'correction' AND h.subject = r.subject) \
                    OR h.created_at < r.created_at)",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .filter_map(|r| r.ok())
        .collect();
    let ids: Vec<i64> = rows
        .into_iter()
        .filter(|(_, old, new, older)| *older || !crate::extract::restates(old, new))
        .map(|(id, ..)| id)
        .collect();
    let tx = db.write_tx()?;
    let now = now_ms();
    for id in &ids {
        // `record_au_revalidate` puts the row back into the index and counts it; 1.2.0 did
        // both again by hand, which left every restored row indexed twice (WP-08)
        tx.execute(
            "UPDATE record SET invalid = 0, invalid_reason = NULL, invalidated_by = NULL WHERE id = ?1",
            [id],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
            rusqlite::params![format!("restored:{id}"), now.to_string()],
        )?;
    }
    if !ids.is_empty() {
        tx.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('records_changed_since_project', '1')",
            [],
        )?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
        rusqlite::params![DONE, now.to_string()],
    )?;
    tx.commit()?;
    Ok(ids.len())
}

/// One pass that puts back, again, what 1.2.0's restore pass had put back and the commit-diff
/// path then retired with no heir (WP-08). That path read any one-line hunk as a changed value
/// (WP-02), and a record already found wrongly retired once was retired by it a second time.
/// A record retired again by a newer statement, or for its anchor, is left. Each is noted under
/// `restored_again:<id>`, and a `meta` key keeps it to one run.
pub fn restore_retired_again(db: &Db) -> Result<usize> {
    const DONE: &str = "restored_retired_again";
    if db.meta_get(DONE)?.is_some() {
        return Ok(0);
    }
    let tx = db.write_tx()?;
    let ids: Vec<i64> = tx
        .prepare(
            "SELECT r.id FROM record r JOIN meta m ON m.key = 'restored:' || r.id \
             WHERE r.invalid = 1 AND r.invalid_reason = 'superseded' AND r.invalidated_by IS NULL",
        )?
        .query_map([], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    let now = now_ms();
    for id in &ids {
        tx.execute(
            "UPDATE record SET invalid = 0, invalid_reason = NULL WHERE id = ?1",
            [id],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
            rusqlite::params![format!("restored_again:{id}"), now.to_string()],
        )?;
    }
    if !ids.is_empty() {
        tx.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('records_changed_since_project', '1')",
            [],
        )?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
        rusqlite::params![DONE, now.to_string()],
    )?;
    tx.commit()?;
    Ok(ids.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use muninn_core::db::Mode;

    /// What the two bugs left is put back and found again by search; a real replacement,
    /// newer than what it retired, stays as it was.
    #[test]
    fn restores_what_the_two_bugs_retired_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let ins = |id: i64, kind: &str, subject: &str, object: &str, at: i64| {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,?2,?3,'r',?4,?4,'user_said',3,'s',?1,?5)",
                    rusqlite::params![id, kind, subject, object, at],
                )
                .unwrap();
        };
        let retire = |id: i64, by: i64| {
            db.conn
                .execute(
                    "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?2 WHERE id=?1",
                    [id, by],
                )
                .unwrap();
        };
        ins(
            1,
            "correction",
            "correction:abc#0",
            "el puerto del servidor",
            10,
        );
        ins(
            2,
            "correction",
            "correction:abc#0",
            "los logs van aparte",
            20,
        );
        retire(1, 2);
        ins(
            7,
            "correction",
            "correction:abc#0",
            "tabla usuarios lleva indice",
            21,
        );
        ins(
            8,
            "correction",
            "correction:abc#0",
            "tabla usuarios lleva indice unico",
            22,
        );
        retire(7, 8);
        ins(3, "episode", "session:s#1", "viejo", 30);
        ins(4, "decision", "said:state:x", "zanahoria", 40);
        retire(4, 3);
        ins(5, "decision", "said:state:y", "antes", 50);
        ins(6, "decision", "said:change:y", "despues", 60);
        retire(5, 6);
        assert_eq!(restore_false_retirements(&db).unwrap(), 2);
        let active = |id: i64| -> i64 {
            db.conn
                .query_row("SELECT 1 - invalid FROM record WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        assert_eq!((active(1), active(4), active(5), active(7)), (1, 1, 0, 0));
        let found: i64 = db
            .conn
            .query_row(
                "SELECT count(*) FROM record_fts WHERE record_fts MATCH 'zanahoria'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, 1);
        // WP-08: the trigger already indexes a row put back; 1.2.0 indexed it again by hand
        // and raised the counter twice, and `doctor` went RED on every store it touched
        let counter: i64 = db.meta_get("fts_rows").unwrap().unwrap().parse().unwrap();
        let active = db
            .count("SELECT count(*) FROM record WHERE invalid=0")
            .unwrap();
        assert_eq!(counter, active, "the counter matches the index");
        assert_eq!(restore_false_retirements(&db).unwrap(), 0, "runs once");
    }

    /// WP-08. 1.2.0 put back 48 records on one store and 20 were retired again: 15 of them by
    /// the commit-diff path with no heir, the rule WP-02 found retiring live records. Those are
    /// put back once more; one retired again by a newer statement, or because its anchor
    /// changed, is left as it is.
    #[test]
    fn a_restored_record_the_old_swap_rule_retired_again_is_put_back_once() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        // the newer statement that retired #2
        db.conn
            .execute(
                "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                 VALUES(4,'decision','4','is','4','4','user_said',3,'s','4',2)",
                [],
            )
            .unwrap();
        for (id, reason, by) in [
            (1, "superseded", None),
            (2, "superseded", Some(4)),
            (3, "anchor_changed", None),
        ] {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,'decision',?1,'is',?1,?1,'user_said',3,'s',?1,1)",
                    [id],
                )
                .unwrap();
            db.meta_set(&format!("restored:{id}"), "1").unwrap();
            db.conn
                .execute(
                    "UPDATE record SET invalid=1, invalid_reason=?2, invalidated_by=?3 WHERE id=?1",
                    rusqlite::params![id, reason, by],
                )
                .unwrap();
        }
        assert_eq!(restore_retired_again(&db).unwrap(), 1);
        let invalid = |id: i64| -> i64 {
            db.conn
                .query_row("SELECT invalid FROM record WHERE id=?1", [id], |r| r.get(0))
                .unwrap()
        };
        assert_eq!((invalid(1), invalid(2), invalid(3)), (0, 1, 1));
        assert_eq!(restore_retired_again(&db).unwrap(), 0, "runs once");
    }

    /// Two batches of the same session each start counting at turn 0. Two corrections about
    /// different things are both current, and an answer in AskUserQuestion retires nothing.
    #[test]
    fn a_later_batch_does_not_retire_an_earlier_correction() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let t = transcript(
            tmp.path(),
            "t.jsonl",
            &[
                user("No, el puerto del servidor es 8080"),
                assistant("Entendido."),
            ],
        );
        ingest_transcript(&db, &t, "stop").unwrap();
        let more = serde_json::to_string(&user("No, los logs van a stderr")).unwrap()
            + "\n"
            + &serde_json::to_string(&assistant("Entendido.")).unwrap()
            + "\n";
        let mut f = std::fs::OpenOptions::new().append(true).open(&t).unwrap();
        std::io::Write::write_all(&mut f, more.as_bytes()).unwrap();
        ingest_transcript(&db, &t, "stop").unwrap();
        let (active, subjects): (i64, i64) = db
            .conn
            .query_row(
                "SELECT count(*), count(DISTINCT subject) FROM record WHERE kind='correction' AND invalid=0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((active, subjects), (2, 2));
    }

    fn at(s: &str, ts: &str) -> serde_json::Value {
        let mut v = user(s);
        v["timestamp"] = serde_json::Value::String(ts.into());
        v
    }

    fn append(t: &Path, lines: &[serde_json::Value]) {
        let more: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let mut f = std::fs::OpenOptions::new().append(true).open(t).unwrap();
        std::io::Write::write_all(&mut f, more.as_bytes()).unwrap();
    }

    /// WP-03. A → B → A in the same words, one turn per ingest as `Stop` does. The third
    /// message hashes to the retired A and 1.2.0 dropped it as a duplicate, so B stayed the
    /// current word. The restatement is the newest thing said: A comes back, dated to it, and
    /// B is retired with A as its heir.
    #[test]
    fn a_decision_restated_after_its_replacement_prevails_again() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let a = "We go with Postgres for the job queue.";
        let b = "Actually, switch the job queue to Redis instead of Postgres.";
        let t = transcript(
            tmp.path(),
            "t.jsonl",
            &[at(a, "2026-11-05T10:00:00.000Z"), assistant("Understood.")],
        );
        ingest_transcript(&db, &t, "s1").unwrap();
        append(
            &t,
            &[
                at(b, "2026-11-05T10:01:00.000Z"),
                assistant("Understood, switching the job queue from Postgres to Redis."),
            ],
        );
        ingest_transcript(&db, &t, "s1").unwrap();
        let state = |text: &str| -> (i64, Option<i64>, i64) {
            db.conn
                .query_row(
                    "SELECT invalid, invalidated_by, created_at FROM record WHERE kind='decision' AND instr(object, ?1) > 0",
                    [text.trim_end_matches('.')],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap()
        };
        assert_eq!(state(a).0, 1, "B replaced A");
        append(
            &t,
            &[at(a, "2026-11-05T10:02:00.000Z"), assistant("Understood.")],
        );
        let st = ingest_transcript(&db, &t, "s1").unwrap();
        let a_id: i64 = db
            .conn
            .query_row(
                "SELECT id FROM record WHERE kind='decision' AND instr(object, ?1) > 0",
                [a.trim_end_matches('.')],
                |r| r.get(0),
            )
            .unwrap();
        let (a_inv, _, a_at) = state(a);
        let (b_inv, b_heir, _) = state(b);
        assert_eq!(a_inv, 0, "the restated A is current again: {st:?}");
        assert_eq!(
            a_at,
            parse_rfc3339_ms("2026-11-05T10:02:00.000Z").unwrap(),
            "dated to the restatement"
        );
        assert_eq!((b_inv, b_heir), (1, Some(a_id)), "and B is retired by it");
        let via: String = db
            .conn
            .query_row(
                "SELECT invalidated_via FROM record WHERE kind='decision' AND instr(object, ?1) > 0",
                [b.trim_end_matches('.')],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(via, "restated");
        assert_eq!(
            db.count("SELECT count(*) FROM record WHERE invalid=0 AND body LIKE '%Redis%'")
                .unwrap(),
            0,
            "nothing still serves B: not its episode, not the correction typed from the same sentence"
        );
    }

    /// WP-03. A rule retired for any reason and then typed again verbatim is a rule the user
    /// holds now. 1.2.0 left zero active invariants. Reading the same turn a second time — a
    /// watermark that went back — is not saying it again, and changes nothing.
    #[test]
    fn a_retired_rule_typed_again_is_active_and_a_reread_is_not_a_restatement() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let rule = "Never deploy on Fridays without the checklist.";
        let t = transcript(
            tmp.path(),
            "t.jsonl",
            &[
                at(rule, "2026-11-05T10:00:00.000Z"),
                assistant("Understood."),
            ],
        );
        ingest_transcript(&db, &t, "s1").unwrap();
        db.conn
            .execute(
                "UPDATE record SET invalid=1, invalid_reason='cap' WHERE kind='invariant'",
                [],
            )
            .unwrap();
        // the watermark goes back and the same turn is read again
        db.conn
            .execute("DELETE FROM meta WHERE key LIKE 'wm:%'", [])
            .unwrap();
        ingest_transcript(&db, &t, "s1").unwrap();
        let active = || {
            db.count("SELECT count(*) FROM record WHERE kind='invariant' AND invalid=0")
                .unwrap()
        };
        assert_eq!(active(), 0, "the same turn read twice is one statement");
        append(
            &t,
            &[
                at(rule, "2026-11-06T09:00:00.000Z"),
                assistant("Understood."),
            ],
        );
        ingest_transcript(&db, &t, "s1").unwrap();
        assert_eq!(active(), 1, "said again, it holds again");
    }

    fn in_session(mut v: serde_json::Value, sid: &str) -> serde_json::Value {
        v["sessionId"] = serde_json::Value::String(sid.into());
        v
    }

    /// WP-06, as it ships: a change retires an earlier untyped episode it shares its words
    /// with, in its own session and in another. The two narrower actions measured against this
    /// (same session only; a link instead of a retirement) each left more stale statements
    /// active on the design grid, and the arm that ships has to keep this behaviour pinned.
    #[test]
    fn a_change_retires_an_untyped_episode_in_any_session() {
        let first = "The nightly batch export runs on the old cron box.";
        let change = "Actually move the nightly batch export to the new scheduler box.";
        let episode_active = |same_session: bool| -> i64 {
            let tmp = tempfile::tempdir().unwrap();
            let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
            let a = transcript(
                tmp.path(),
                "a.jsonl",
                &[at(first, "2026-11-05T10:00:00.000Z"), assistant("Noted.")],
            );
            ingest_transcript(&db, &a, "s1").unwrap();
            let sid = if same_session { "s1" } else { "s2" };
            let b = transcript(
                tmp.path(),
                "b.jsonl",
                &[
                    in_session(at(change, "2026-11-05T10:05:00.000Z"), sid),
                    in_session(assistant("Noted."), sid),
                ],
            );
            ingest_transcript(&db, &b, sid).unwrap();
            db.count(
                "SELECT count(*) FROM record WHERE kind='episode' AND invalid=0 AND body LIKE '%old cron box%'",
            )
            .unwrap()
        };
        assert_eq!(episode_active(true), 0, "the same conversation");
        assert_eq!(episode_active(false), 0, "another conversation");
    }

    /// R-17. A transcript line dated far in the future would put its records ahead of every
    /// real one on every newest-first path. Such a date is not believed: the record is dated
    /// when it was read.
    #[test]
    fn a_turn_dated_in_the_future_is_dated_when_it_was_read() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let t = transcript(
            tmp.path(),
            "t.jsonl",
            &[
                at(
                    "Never deploy on Fridays without the checklist.",
                    "2999-01-01T00:00:00.000Z",
                ),
                assistant("Understood."),
            ],
        );
        let before = now_ms();
        ingest_transcript(&db, &t, "s1").unwrap();
        let latest: i64 = db.count("SELECT max(created_at) FROM record").unwrap();
        assert!(
            (before..=now_ms()).contains(&latest),
            "{latest} is not the time it was read"
        );
    }

    /// I2. A said in one session, said again in a second, replaced in a third. The restatement
    /// moves the record to its latest turn, and the replacement then swept only that turn's
    /// episode: the first session's episode stating A stayed active and could be served.
    /// Every turn the record was stated in is its own.
    ///
    /// The word-overlap pass over episodes (loop 2) also reaches the first session's episode
    /// when the change shares enough words with it, which is why this case alone does not
    /// depend on `record_turn`; the two below replace A by a path loop 2 does not reach.
    #[test]
    fn a_restated_record_takes_every_turn_it_was_said_in_when_it_goes() {
        let left = said_twice_then(
            "We go with Postgres for the job queue.",
            "Actually, switch the job queue to Redis instead of Postgres.",
            "Understood, switching the job queue from Postgres to Redis.",
            "go with Postgres",
        );
        assert!(left.is_empty(), "still served: {left:?}");
    }

    /// A said on day one and again on day two, each in its own session, then replaced on
    /// day three by `change` with `reply`; returns the active records whose body still holds
    /// `needle`, as `kind session`. The days are far enough apart that the anaphoric pass,
    /// bounded to three hours, cannot reach them.
    fn said_twice_then(a: &str, change: &str, reply: &str, needle: &str) -> Vec<String> {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        for (file, sid, text, ts, reply) in [
            (
                "a.jsonl",
                "s1",
                a,
                "2026-11-05T10:00:00.000Z",
                "Understood.",
            ),
            (
                "b.jsonl",
                "s2",
                a,
                "2026-11-06T10:00:00.000Z",
                "Understood.",
            ),
            ("c.jsonl", "s3", change, "2026-11-07T10:00:00.000Z", reply),
        ] {
            let t = transcript(
                tmp.path(),
                file,
                &[
                    in_session(at(text, ts), sid),
                    in_session(assistant(reply), sid),
                ],
            );
            ingest_transcript(&db, &t, sid).unwrap();
        }
        let mut st = db
            .conn
            .prepare(
                "SELECT kind || ' ' || session_id FROM record WHERE invalid=0 AND body LIKE ?1",
            )
            .unwrap();
        st.query_map([format!("%{needle}%")], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect()
    }

    /// I2 through the acknowledgement: "Let's use rustls instead" shares no content word with
    /// the statement it replaces, so only the reply names the pair, and the word-overlap pass
    /// never sees the first session's episode. Only the record's earlier turns reach it.
    #[test]
    fn a_restated_record_replaced_through_the_reply_takes_its_earlier_turns() {
        let left = said_twice_then(
            "We are using openssl for the TLS backend.",
            "Let's use rustls instead",
            "Got it, switching the TLS backend from openssl to rustls.",
            "using openssl",
        );
        assert!(left.is_empty(), "still served: {left:?}");
    }

    /// I2 through a quantity: "bump to 7 attempts" shares one word with what it replaces, too
    /// few for the word-overlap pass; the unit is the slot.
    #[test]
    fn a_restated_record_replaced_by_a_quantity_takes_its_earlier_turns() {
        let left = said_twice_then(
            "We use 3 attempts for the payment worker.",
            "bump to 7 attempts",
            "Understood.",
            "3 attempts",
        );
        assert!(left.is_empty(), "still served: {left:?}");
    }

    /// An answer picked in AskUserQuestion, as the transcript carries it: a two-question dialog,
    /// since a one-question pick is not typed (`extract::answer_candidate`).
    fn picked(id: &str, question: &str, option: &str, ts: &str) -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({"type":"assistant","sessionId":"s1","timestamp":ts,"message":{"role":"assistant","content":[
                {"type":"tool_use","id":id,"name":"AskUserQuestion","input":{"questions":[{"question":question}]}}]}}),
            serde_json::json!({"type":"user","sessionId":"s1","timestamp":ts,"message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":id,"content":format!("Your questions have been answered: \"{question}\"=\"{option}\", \"Retention of finished jobs\"=\"Seven days\". You can now continue with the user's answers in mind.")}]}}),
            assistant("Noted."),
        ]
    }

    /// I3. An option picked in AskUserQuestion says nothing about which earlier record it
    /// replaces, so it supersedes nothing; and the same option picked again under the same
    /// question hashes to the same record, which is not the user saying it again either. It
    /// may not put a retired record back nor retire what replaced it.
    #[test]
    fn the_same_option_picked_again_restates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let q = "Job queue store";
        let a = "We go with Postgres for the job queue";
        let mut first = vec![
            at(
                "Help me pick the job queue store.",
                "2026-11-05T09:59:00.000Z",
            ),
            assistant("Let me ask."),
        ];
        first.extend(picked("q1", q, a, "2026-11-05T10:00:00.000Z"));
        let t = transcript(tmp.path(), "t.jsonl", &first);
        ingest_transcript(&db, &t, "s1").unwrap();
        let rows: Vec<String> = db
            .conn
            .prepare("SELECT kind || ' ' || object FROM record")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        let answer_rows = db
            .count("SELECT count(*) FROM record WHERE kind='decision' AND object LIKE '%Postgres%'")
            .unwrap();
        assert!(
            answer_rows > 0,
            "the pick is on record as a decision: {rows:?}"
        );
        append(
            &t,
            &[
                at(
                    "Actually, switch the job queue to Redis instead of Postgres.",
                    "2026-11-05T10:01:00.000Z",
                ),
                assistant("Understood, switching the job queue from Postgres to Redis."),
            ],
        );
        ingest_transcript(&db, &t, "s1").unwrap();
        let redis = || {
            db.count("SELECT count(*) FROM record WHERE kind='decision' AND invalid=0 AND object LIKE '%Redis%'")
                .unwrap()
        };
        assert_eq!(redis(), 1);
        append(&t, &picked("q2", q, a, "2026-11-05T10:02:00.000Z"));
        let st = ingest_transcript(&db, &t, "s1").unwrap();
        assert_eq!(st.restated, 0, "{st:?}");
        assert_eq!(redis(), 1, "the change stands");
    }

    /// WP-04. Past sixty active invariants 1.2.0 retired the oldest, whatever they said: age
    /// says nothing about whether a rule still holds, and delivery already serves invariants
    /// newest first under its budget, so the count no longer protects anything. The rule that
    /// is oldest is still a rule the user stated.
    #[test]
    fn a_sixty_first_rule_retires_no_older_one() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        for i in 0..60 {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('invariant',?1,'must',?1,?1,'user_said',3,'s',?1,?2)",
                    rusqlite::params![format!("never do thing number {i}"), i + 1],
                )
                .unwrap();
        }
        let t = transcript(
            tmp.path(),
            "t.jsonl",
            &[
                user("Never deploy on Fridays without the checklist."),
                assistant("Understood."),
            ],
        );
        let st = ingest_transcript(&db, &t, "s1").unwrap();
        assert_eq!(st.invariants, 1);
        assert_eq!(st.archived, 0);
        assert_eq!(
            db.count("SELECT count(*) FROM record WHERE kind='invariant' AND invalid=0")
                .unwrap(),
            61
        );
    }

    /// WP-03. A statement repeated while it is still current is the newest thing said about
    /// its subject, and newest first has to see it as such.
    #[test]
    fn an_active_statement_said_again_moves_to_its_latest_date() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let rule = "Never deploy on Fridays without the checklist.";
        let t = transcript(
            tmp.path(),
            "t.jsonl",
            &[
                at(rule, "2026-11-05T10:00:00.000Z"),
                assistant("Understood."),
            ],
        );
        ingest_transcript(&db, &t, "s1").unwrap();
        append(
            &t,
            &[
                at(rule, "2026-11-09T10:00:00.000Z"),
                assistant("Understood."),
            ],
        );
        ingest_transcript(&db, &t, "s1").unwrap();
        let (n, created): (i64, i64) = db
            .conn
            .query_row(
                "SELECT count(*), max(created_at) FROM record WHERE kind='invariant'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            created,
            parse_rfc3339_ms("2026-11-09T10:00:00.000Z").unwrap()
        );
    }

    /// Records a summary put on record before 1.1.0 are retired by the one-time pass, and a
    /// record the user really typed in the same transcript is not.
    #[test]
    fn records_from_a_summary_are_retired_once() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let t = transcript(
            tmp.path(),
            "t.jsonl",
            &[
                user("<artifact-content-authored-by-others/>\nNotice.\nThis session is being continued from a previous conversation that ran out of context.\n- Never edit the generated files"),
                assistant("Continuing."),
                user("Nunca uses pkill en este proyecto"),
                assistant("Entendido."),
            ],
        );
        let session = parse_any(&t, 0).unwrap();
        let ends: Vec<u64> = session.turns.iter().map(|t| t.end_offset).collect();
        for (i, (body, end)) in [
            ("user: Never edit the generated files\n", ends[0]),
            ("user: Nunca uses pkill en este proyecto\n", ends[1]),
        ]
        .iter()
        .enumerate()
        {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,transcript_ref,dedup_hash,created_at) \
                     VALUES('invariant',?1,'must',?2,?2,'user_said',3,'s1',?3,?1,1)",
                    rusqlite::params![format!("k{i}"), body, format!("{}:{}", t.display(), end)],
                )
                .unwrap();
        }
        assert_eq!(retire_summary_records(&db).unwrap(), 1);
        let active: Vec<String> = db
            .conn
            .prepare("SELECT body FROM record WHERE invalid = 0")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(active, vec!["user: Nunca uses pkill en este proyecto\n"]);
        assert_eq!(retire_summary_records(&db).unwrap(), 0, "runs once");
    }

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

#[cfg(test)]
mod deadend_batch_tests {
    use super::ingest_transcript;
    use muninn_core::db::{Db, Mode};
    use serde_json::json;
    use std::path::Path;

    fn turn(prompt: &str, id: &str, ok: bool) -> Vec<serde_json::Value> {
        let body = if ok {
            "test result: ok. 4 passed"
        } else {
            "Exit code 101\nerror[E0425]: cannot find value"
        };
        vec![
            json!({"type":"user","sessionId":"s1","message":{"role":"user","content":prompt}}),
            json!({"type":"assistant","sessionId":"s1","message":{"role":"assistant","content":[
                {"type":"tool_use","id":id,"name":"Bash","input":{"command":"cargo test -p facturas"}}]}}),
            json!({"type":"user","sessionId":"s1","message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":id,"content":body,"is_error":!ok}]},
                "toolUseResult":{"stdout":"","stderr":"","interrupted":false}}),
            json!({"type":"assistant","sessionId":"s1","message":{"role":"assistant","content":[{"type":"text","text":"Listo."}]}}),
        ]
    }

    fn write(p: &Path, lines: &[serde_json::Value]) {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(p, text).unwrap();
    }

    fn dead_ends(db: &Db) -> i64 {
        db.count("SELECT count(*) FROM record WHERE invalid=0 AND kind='deadend'")
            .unwrap()
    }

    /// WP-09. A test fails in one turn and passes in the next. Read whole, the session leaves
    /// no dead end; read one turn per Stop, as the hook does, it left one for good, because a
    /// later success in another batch never reached it. The two must agree.
    #[test]
    fn a_dead_end_goes_when_the_same_command_later_succeeds_in_another_batch() {
        let mut both = turn("corre las pruebas", "a", false);
        both.extend(turn("arreglado, corre otra vez", "b", true));

        let tmp = tempfile::tempdir().unwrap();
        let whole = Db::open(&tmp.path().join("whole.db"), Mode::ReadWrite).unwrap();
        let p = tmp.path().join("whole.jsonl");
        write(&p, &both);
        ingest_transcript(&whole, &p, "s1").unwrap();

        let batched = Db::open(&tmp.path().join("batched.db"), Mode::ReadWrite).unwrap();
        let q = tmp.path().join("batched.jsonl");
        write(&q, &both[..4]);
        ingest_transcript(&batched, &q, "s1").unwrap();
        assert_eq!(dead_ends(&batched), 1, "the failure alone is a dead end");
        write(&q, &both);
        ingest_transcript(&batched, &q, "s1").unwrap();

        assert_eq!(dead_ends(&whole), 0);
        assert_eq!(dead_ends(&batched), 0, "the later success retires it");
        let via: String = batched
            .conn
            .query_row(
                "SELECT invalidated_via FROM record WHERE kind='deadend'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(via, "succeeded");
    }

    /// A run that passed and then failed in the same batch is a regression: its dead end
    /// stands. Only the batch's last run of the command decides.
    #[test]
    fn a_pass_then_a_failure_leaves_the_dead_end() {
        let mut both = turn("corre las pruebas", "a", true);
        both.extend(turn("cambié el parser, corre otra vez", "b", false));
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let p = tmp.path().join("t.jsonl");
        write(&p, &both);
        ingest_transcript(&db, &p, "s1").unwrap();
        assert_eq!(dead_ends(&db), 1, "the regression is a dead end");
    }
}

#[cfg(test)]
mod held_turn_tests {
    use super::{ingest_transcript, ingest_transcript_released};
    use muninn_core::db::{Db, Mode};
    use serde_json::json;

    /// WP-10. The last message of a session that never got an answer was held back for the
    /// next ingest, and nothing ever came: SessionEnd and `maintain` called the same function.
    /// Released, it is on record; a later ordinary ingest finds nothing more.
    #[test]
    fn an_unanswered_last_message_is_stored_when_released() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let p = tmp.path().join("t.jsonl");
        let lines = [
            json!({"type":"user","sessionId":"s1","message":{"role":"user","content":"Revisa el reporte"}}),
            json!({"type":"assistant","sessionId":"s1","message":{"role":"assistant","content":[{"type":"text","text":"Revisado."}]}}),
            json!({"type":"user","sessionId":"s1","message":{"role":"user","content":"No, nunca borres los reportes viejos del servidor."}}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&p, text).unwrap();
        let held = ingest_transcript(&db, &p, "s1").unwrap();
        assert!(held.held_back);
        let typed = || {
            db.count("SELECT count(*) FROM record WHERE invalid=0 AND kind<>'episode'")
                .unwrap()
        };
        assert_eq!(typed(), 0, "held while the session may still answer");
        let st = ingest_transcript_released(&db, &p, "s1", None).unwrap();
        assert!(!st.held_back);
        assert!(typed() > 0, "the last message is on record: {st:?}");
        let again = ingest_transcript(&db, &p, "s1").unwrap();
        assert_eq!((again.turns, again.inserted), (0, 0));
    }
}

#[cfg(test)]
mod released_turn_tests {
    use super::{ingest_transcript, ingest_transcript_released};
    use muninn_core::db::{Db, Mode};
    use serde_json::json;

    /// WP-10, the other half. A last message released before its answer, then the session is
    /// resumed and the answer arrives: the answer is not lost, the prompt-only episode the
    /// release wrote gives way to the whole turn, and nothing typed is written twice.
    #[test]
    fn a_released_turn_answered_later_is_read_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let p = tmp.path().join("t.jsonl");
        let mut lines = vec![
            json!({"type":"user","sessionId":"s1","message":{"role":"user","content":"Revisa el recibo de marzo"}}),
            json!({"type":"assistant","sessionId":"s1","message":{"role":"assistant","content":[{"type":"text","text":"Revisado."}]}}),
            json!({"type":"user","sessionId":"s1","message":{"role":"user","content":"No, nunca redondees los importes de los recibos."}}),
        ];
        let write = |lines: &[serde_json::Value]| {
            let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
            std::fs::write(&p, text).unwrap();
        };
        write(&lines);
        ingest_transcript_released(&db, &p, "s1", None).unwrap();
        let typed = || {
            db.count("SELECT count(*) FROM record WHERE invalid=0 AND kind<>'episode'")
                .unwrap()
        };
        let after_release = typed();
        assert!(after_release > 0);
        let all = || db.count("SELECT count(*) FROM record").unwrap();
        let rows = all();
        // a Stop, and a second release, before any answer: nothing more is written
        ingest_transcript(&db, &p, "s1").unwrap();
        ingest_transcript_released(&db, &p, "s1", None).unwrap();
        assert_eq!(all(), rows);
        // resumed: the answer to the released message, and a next turn
        lines.push(json!({"type":"assistant","sessionId":"s1","message":{"role":"assistant","content":[{"type":"text","text":"De acuerdo: los importes van con sus céntimos."}]}}));
        lines.push(json!({"type":"user","sessionId":"s1","message":{"role":"user","content":"Gracias, sigue con abril"}}));
        lines.push(json!({"type":"assistant","sessionId":"s1","message":{"role":"assistant","content":[{"type":"text","text":"Sigo con abril."}]}}));
        write(&lines);
        ingest_transcript(&db, &p, "s1").unwrap();
        let answered = db
            .count("SELECT count(*) FROM record WHERE invalid=0 AND kind='episode' AND body LIKE '%redondees%' AND body LIKE '%céntimos%'")
            .unwrap();
        assert_eq!(answered, 1, "the answer is on record with its message");
        let prompt_only = db
            .count("SELECT count(*) FROM record WHERE invalid=0 AND kind='episode' AND body LIKE '%redondees%' AND body NOT LIKE '%céntimos%'")
            .unwrap();
        assert_eq!(
            prompt_only, 0,
            "the released episode gave way to the whole turn"
        );
        assert_eq!(typed(), after_release, "nothing typed is written twice");
        assert!(db
            .count("SELECT count(*) FROM record WHERE invalid=0 AND kind='episode' AND body LIKE '%abril%'")
            .unwrap()
            > 0);
    }

    /// The released turn read again whole is the saying already on record, at the date it was
    /// said: not an older turn of the person's (C1), and not a restatement. Nothing is written
    /// to the journal and no other turn is put down as one that stated the rule.
    #[test]
    fn a_released_turn_read_again_is_not_an_older_saying() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let p = tmp.path().join("t.jsonl");
        let mut lines = vec![
            json!({"type":"user","sessionId":"s1","timestamp":"2026-11-02T10:00:00Z","message":{"role":"user","content":"No, nunca redondees los importes de los recibos."}}),
        ];
        let write = |lines: &[serde_json::Value]| {
            let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
            std::fs::write(&p, text).unwrap();
        };
        write(&lines);
        ingest_transcript_released(&db, &p, "s1", None).unwrap();
        let rules = || {
            db.count("SELECT count(*) FROM record WHERE invalid=0 AND kind='invariant'")
                .unwrap()
        };
        assert_eq!(rules(), 1);
        lines.push(json!({"type":"assistant","sessionId":"s1","message":{"role":"assistant","content":[{"type":"text","text":"De acuerdo."}]}}));
        write(&lines);
        ingest_transcript(&db, &p, "s1").unwrap();
        assert_eq!(rules(), 1);
        assert_eq!(
            db.count(
                "SELECT (SELECT count(*) FROM restatement) + (SELECT count(*) FROM record_turn)"
            )
            .unwrap(),
            0
        );
    }
}

#[cfg(test)]
mod released_then_new_turn_tests {
    use super::{ingest_transcript, ingest_transcript_released};
    use muninn_core::db::{Db, Mode};
    use serde_json::json;

    /// Released while unanswered, then the session is resumed with a new message and the
    /// released one never gets an answer. The released message's episode stays on record,
    /// once, and nothing is ever its own heir.
    #[test]
    fn a_released_message_followed_by_a_new_one_keeps_its_episode() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let p = tmp.path().join("t.jsonl");
        let mut lines = vec![
            json!({"type":"user","sessionId":"s1","message":{"role":"user","content":"Revisa el recibo de marzo"}}),
            json!({"type":"assistant","sessionId":"s1","message":{"role":"assistant","content":[{"type":"text","text":"Revisado."}]}}),
            json!({"type":"user","sessionId":"s1","message":{"role":"user","content":"Apunta también el gasto de la papelería del lunes"}}),
        ];
        let write = |lines: &[serde_json::Value]| {
            let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
            std::fs::write(&p, text).unwrap();
        };
        write(&lines);
        ingest_transcript_released(&db, &p, "s1", None).unwrap();
        let released = || {
            db.count("SELECT count(*) FROM record WHERE invalid=0 AND kind='episode' AND body LIKE '%papelería%'")
                .unwrap()
        };
        assert_eq!(released(), 1);
        lines.push(json!({"type":"user","sessionId":"s1","message":{"role":"user","content":"Y ahora el de abril, por favor"}}));
        lines.push(json!({"type":"assistant","sessionId":"s1","message":{"role":"assistant","content":[{"type":"text","text":"Abril listo."}]}}));
        write(&lines);
        ingest_transcript(&db, &p, "s1").unwrap();
        assert_eq!(
            released(),
            1,
            "the released message is still on record, once"
        );
        assert_eq!(
            db.count(
                "SELECT count(*) FROM record WHERE body LIKE '%papelería%' AND kind='episode'"
            )
            .unwrap(),
            1,
            "and written once"
        );
        assert_eq!(
            db.count("SELECT count(*) FROM record WHERE invalidated_by = id")
                .unwrap(),
            0
        );
    }
}

#[cfg(test)]
mod read_order_tests {
    use super::{ingest_transcript, ingest_transcript_released};
    use muninn_core::db::{Db, Mode};
    use serde_json::{json, Value};

    fn said(sid: &str, at: &str, text: &str, reply: Option<&str>) -> Vec<Value> {
        let mut v = vec![
            json!({"type":"user","sessionId":sid,"timestamp":at,"message":{"role":"user","content":text}}),
        ];
        if let Some(r) = reply {
            v.push(json!({"type":"assistant","sessionId":sid,"message":{"role":"assistant","content":[{"type":"text","text":r}]}}));
        }
        v
    }

    fn failed(sid: &str, at: &str, runs: usize) -> Vec<Value> {
        let mut v = vec![
            json!({"type":"user","sessionId":sid,"timestamp":at,"message":{"role":"user","content":"run the ledger tests"}}),
        ];
        for i in 0..runs {
            let id = format!("{sid}-{i}");
            v.push(json!({"type":"assistant","sessionId":sid,"message":{"role":"assistant","content":[
                {"type":"tool_use","id":id,"name":"Bash","input":{"command":"cargo test -p ledger"}}]}}));
            v.push(json!({"type":"user","sessionId":sid,"message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":id,"content":"Exit code 101\nerror","is_error":true}]},
                "toolUseResult":{"stdout":"","stderr":"","interrupted":false}}));
        }
        v.push(json!({"type":"assistant","sessionId":sid,"message":{"role":"assistant","content":[{"type":"text","text":"They fail."}]}}));
        v
    }

    /// Two sessions, `old` dated before `new`, read in the order given (`older_last`: the older
    /// one second). Returns whether a row holding `needle` in its body is still active.
    fn stands(old: &[Value], new: &[Value], older_last: bool, needle: &str) -> bool {
        active(old, new, older_last, needle, "")
    }

    fn active(old: &[Value], new: &[Value], older_last: bool, needle: &str, and: &str) -> bool {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let write = |name: &str, lines: &[Value]| {
            let p = tmp.path().join(name);
            let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
            std::fs::write(&p, text).unwrap();
            p
        };
        let (a, b) = (write("old.jsonl", old), write("new.jsonl", new));
        if older_last {
            ingest_transcript(&db, &b, "s-new").unwrap();
            ingest_transcript(&db, &a, "s-old").unwrap();
        } else {
            ingest_transcript(&db, &a, "s-old").unwrap();
            ingest_transcript(&db, &b, "s-new").unwrap();
        }
        db.conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM record WHERE invalid=0 AND instr(body, ?1) > 0 {and}"
                ),
                [needle],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            > 0
    }

    const DAY1: &str = "2026-11-01T10:00:00.000Z";
    const DAY2: &str = "2026-11-02T10:00:00.000Z";

    /// I1. A session read after a newer one is older word: nothing it says retires what was
    /// said after it. Each rule that retires by what a statement says is read in both orders;
    /// in the order of the dates the newer statement stands as it always did.
    #[test]
    fn an_older_statement_read_last_retires_nothing_said_after_it() {
        // by its words (`supersede_said`)
        let old = said(
            "s-old",
            DAY1,
            "Let's use openssl for the TLS backend.",
            Some("Done."),
        );
        let new = said(
            "s-new",
            DAY2,
            "Actually, switch the TLS backend to rustls.",
            Some("Done."),
        );
        assert!(stands(&old, &new, false, "rustls"));
        assert!(stands(&old, &new, true, "rustls"), "by its words");
        // an untyped message of the newer session, by a change's words (loop 2)
        let old = said(
            "s-old",
            DAY1,
            "Actually move the nightly batch export to the new scheduler box.",
            Some("Noted."),
        );
        let new = said(
            "s-new",
            DAY2,
            "The nightly batch export runs on the old cron box.",
            Some("Noted."),
        );
        assert!(stands(&old, &new, false, "old cron box"));
        assert!(
            stands(&old, &new, true, "old cron box"),
            "an episode, by a change's words"
        );
        // by the reply that names both values (`supersede_via_ack`)
        let old = said(
            "s-old",
            DAY1,
            "Let's use rustls instead",
            Some("Got it, switching the TLS backend from openssl to rustls."),
        );
        let new = said(
            "s-new",
            DAY2,
            "We are using openssl for the TLS backend",
            Some("Noted, openssl for TLS."),
        );
        assert!(stands(&old, &new, false, "We are using openssl"));
        assert!(
            stands(&old, &new, true, "We are using openssl"),
            "by the reply"
        );
        // by key (`supersede`): a dead end of the same command with another count
        let (old, new) = (failed("s-old", DAY1, 2), failed("s-new", DAY2, 1));
        assert!(stands(&old, &new, false, "after 1 failed run"));
        assert!(stands(&old, &new, true, "after 1 failed run"), "by key");
    }

    /// I1, in one session: a correction that says an earlier one again replaces it, and read
    /// after a later one (two files of one session) it does not.
    #[test]
    fn an_older_correction_read_last_does_not_replace_a_later_one() {
        let first = said(
            "s1",
            DAY1,
            "No, the server port is 8080, not 9090.",
            Some("Understood."),
        );
        let second = said(
            "s1",
            DAY2,
            "No, the server port is 8443 behind the proxy.",
            Some("Understood."),
        );
        let typed = "AND kind = 'correction'";
        assert!(
            !active(&first, &second, false, "8080", typed),
            "in date order"
        );
        assert!(active(&first, &second, true, "8443", typed), "read last");
    }

    /// I1 through the idle release (WP-10). A last message nobody answered is held at Stop and
    /// put on record later, when its transcript has been idle: by then the decision made the
    /// next day in another session is on record, and the released message is older word.
    #[test]
    fn a_released_last_message_does_not_retire_a_later_decision() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let write = |name: &str, lines: &[Value]| {
            let p = tmp.path().join(name);
            let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
            std::fs::write(&p, text).unwrap();
            p
        };
        let a = write(
            "a.jsonl",
            &said("s-a", DAY1, "Let's use openssl for the TLS backend.", None),
        );
        let b = write(
            "b.jsonl",
            &said(
                "s-b",
                DAY2,
                "Actually, switch the TLS backend to rustls.",
                Some("Done."),
            ),
        );
        let st = ingest_transcript(&db, &a, "s-a").unwrap();
        assert!(st.held_back, "nothing answered it yet");
        ingest_transcript(&db, &b, "s-b").unwrap();
        ingest_transcript_released(&db, &a, "s-a", None).unwrap();
        let active = |needle: &str| {
            db.count(&format!(
                "SELECT count(*) FROM record WHERE invalid=0 AND kind='decision' AND object LIKE '%{needle}%'"
            ))
            .unwrap()
        };
        assert_eq!(active("rustls"), 1, "the later decision stands");
        // R2: on record beside it, and the two are marked as disagreeing
        assert_eq!(active("openssl"), 1, "the released one is on record");
        assert_eq!(marked(&db, "openssl", "rustls"), (1, 1));
    }

    /// How many of the other's ids each of two decisions (found by a word of their text) is
    /// marked in conflict with (`filter::conflicts_of`).
    fn marked(db: &Db, a: &str, b: &str) -> (usize, usize) {
        let id = |needle: &str| -> i64 {
            db.conn
                .query_row(
                    "SELECT id FROM record WHERE kind='decision' AND instr(object, ?1) > 0",
                    [needle],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let (a, b) = (id(a), id(b));
        let with = |x: i64, y: i64| {
            muninn_core::filter::conflicts_of(db, x)
                .unwrap()
                .iter()
                .filter(|i| **i == y)
                .count()
        };
        (with(a, b), with(b, a))
    }

    /// Sessions, each a list of transcript lines, read in `order`. Returns the store.
    fn read(sessions: &[Vec<Value>], order: &[usize]) -> (tempfile::TempDir, Db) {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        for i in order {
            let p = tmp.path().join(format!("s{i}.jsonl"));
            let text: String = sessions[*i].iter().map(|l| format!("{l}\n")).collect();
            std::fs::write(&p, text).unwrap();
            ingest_transcript(&db, &p, &format!("s{i}")).unwrap();
        }
        (tmp, db)
    }

    /// (active, retired with the row holding `heir` as its heir) among the rows of `kind`
    /// whose body holds `needle`.
    fn state(db: &Db, kind: &str, needle: &str, heir: &str) -> (i64, i64) {
        db.conn
            .query_row(
                "SELECT coalesce(sum(o.invalid = 0), 0), coalesce(sum(o.invalid = 1 AND EXISTS \
                 (SELECT 1 FROM record h WHERE h.id = o.invalidated_by AND instr(h.body, ?3) > 0)), 0) \
                 FROM record o WHERE o.kind = ?1 AND instr(o.body, ?2) > 0",
                [kind, needle, heir],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    const DAY3: &str = "2026-11-03T10:00:00.000Z";

    /// R2, the rule by a decision's words. Its reversal was measured and cost a labelled item
    /// of B1, so it is not reversed: an older decision read after the one that would have
    /// replaced it stays on record beside it, and each is marked as in conflict with the other
    /// in the blocks, the catalogue and `why`. In date order the older one is retired as ever.
    #[test]
    fn an_older_decision_read_last_stands_beside_the_later_one_marked_as_in_conflict() {
        let sessions = [
            said(
                "a",
                DAY1,
                "Let's use openssl for the TLS backend.",
                Some("Done."),
            ),
            said(
                "b",
                DAY2,
                "Actually, switch the TLS backend to rustls.",
                Some("Done."),
            ),
        ];
        let (_tmp, db) = read(&sessions, &[0, 1]);
        assert_eq!(state(&db, "decision", "openssl", "rustls"), (0, 1));
        let (_tmp, db) = read(&sessions, &[1, 0]);
        assert_eq!(state(&db, "decision", "openssl", "rustls").0, 1);
        assert_eq!(state(&db, "decision", "rustls", "").0, 1);
        assert_eq!(marked(&db, "openssl", "rustls"), (1, 1));
        // retire one, by any route, and the mark is gone with it
        db.conn
            .execute(
                "UPDATE record SET invalid=1, invalid_reason='revoked' WHERE kind='decision' AND object LIKE '%openssl%'",
                [],
            )
            .unwrap();
        let rustls: i64 = db
            .conn
            .query_row(
                "SELECT id FROM record WHERE kind='decision' AND object LIKE '%rustls%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(muninn_core::filter::conflicts_of(&db, rustls)
            .unwrap()
            .is_empty());
    }

    /// R2, the other rules that can be read from both records: the key, a correction said
    /// again, a bare quantity. Read in date order the later record retires the older one, as
    /// ever. Read last, the older one retires nothing and is not retired: both stay on record,
    /// each marked as in conflict with the other (the stored mark, `filter::mark_conflict`).
    /// Retiring the older one there was tried and taken back: under a worker's row not yet
    /// known as an agent's it lost the person's statement
    /// (`agent::older_statement_under_a_workers_row_tests`).
    #[test]
    fn an_older_record_read_last_stands_beside_the_later_one_marked_for_each_rule() {
        // (older session, newer session, kind, what is replaced, what replaces it)
        let pairs = [
            (
                failed("a", DAY1, 2),
                failed("b", DAY2, 1),
                "deadend",
                "after 2 failed run",
                "after 1 failed run",
            ),
            (
                said(
                    "s1",
                    DAY1,
                    "No, the server port is 8080, not 9090.",
                    Some("Understood."),
                ),
                said(
                    "s1",
                    DAY2,
                    "No, the server port is 8443 behind the proxy.",
                    Some("Understood."),
                ),
                "correction",
                "8080",
                "8443",
            ),
            (
                said(
                    "a",
                    DAY1,
                    "We use 3 attempts for the payment worker.",
                    Some("Understood."),
                ),
                said("b", DAY2, "bump to 7 attempts", Some("Understood.")),
                "decision",
                "3 attempts",
                "7 attempts",
            ),
        ];
        for (old, new, kind, gone, stays) in pairs {
            let sessions = [old, new];
            let (_tmp, db) = read(&sessions, &[0, 1]);
            assert_eq!(
                state(&db, kind, gone, stays),
                (0, 1),
                "{kind}: in date order {gone:?} is retired with {stays:?} as its heir"
            );
            let (_tmp, db) = read(&sessions, &[1, 0]);
            let id = |needle: &str| -> i64 {
                db.conn
                    .query_row(
                        "SELECT id FROM record WHERE kind = ?1 AND invalid = 0 AND instr(body, ?2) > 0",
                        [kind, needle],
                        |r| r.get(0),
                    )
                    .unwrap_or_else(|e| panic!("{kind}: {needle:?} is active, read last or not: {e}"))
            };
            let (older, later) = (id(gone), id(stays));
            assert_eq!(
                (
                    muninn_core::filter::marked_conflicts(&db, older).unwrap(),
                    muninn_core::filter::marked_conflicts(&db, later).unwrap()
                ),
                (vec![later], vec![older]),
                "{kind}: the older record read last and the later one are marked, each with the other"
            );
        }
    }

    /// R2. A on day 1, B on day 3 replacing it, A said again on day 5: A is current and B is
    /// retired with A as its heir, in all six read orders.
    #[test]
    fn a_statement_said_again_after_its_replacement_is_current_in_every_read_order() {
        let a = "We go with Postgres for the job queue.";
        let b = "Actually, switch the job queue to Redis instead of Postgres.";
        let sessions = [
            said("a", DAY1, a, Some("Understood.")),
            said("b", DAY3, b, Some("Understood.")),
            said("c", "2026-11-05T10:00:00.000Z", a, Some("Understood.")),
        ];
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let (_tmp, db) = read(&sessions, &order);
            assert_eq!(
                state(&db, "decision", "go with Postgres", "").0,
                1,
                "A is current, order {order:?}"
            );
            assert_eq!(
                state(&db, "decision", "Redis instead", "go with Postgres"),
                (0, 1),
                "B is retired with A as its heir, order {order:?}"
            );
        }
    }
}
