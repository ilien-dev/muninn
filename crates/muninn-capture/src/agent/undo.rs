//! Undoing what a prompt did while it read as the user's.
//!
//! A worker's turn ingested before its coordinator's is typed as the user's: its sentences
//! restate the user's records, replace them, and leave episodes labelled `user:`. When the
//! coordinator's turn arrives and shows an agent wrote the prompt, that is undone here, inside
//! the ingest transaction, as far as it can be undone without guessing.
//!
//! **The invariant.** Take any order in which workers' turns, coordinators' turns and the
//! user's words are ingested, and the same order with every coordinator moved first. Once
//! every coordinator's turn is on record:
//!
//! - SUBSET, always: every active record is active in the coordinator-first order too. Nothing
//!   is served that would not have been served had no prompt ever read as the user's.
//! - EQUAL, when the user said nothing between a worker's turn and its coordinator's: the same
//!   records are active and the retired ones have the same heirs.
//! - What the two orders differ in is only records left retired here, and each of them is
//!   marked (`invalidated_via` = `agent_unrestored`). `doctor` counts and names the typed
//!   statements of the user's among them; the episodes that went with them are marked and not
//!   counted.
//! - No record is its own heir, and nothing is deleted.
//!
//! **The rules.**
//!
//! 1. A statement the prompt typed is revoked ([`revoke`]).
//! 2. A restatement's own effect on the record it restated, its move or its revival, is
//!    inverted only while that record is exactly as the restatement left it ([`invert`]): the
//!    journal (`restatement`, written by `ingest::restate`) holds the state to return to.
//!    Otherwise the row waits as pending, and [`settle`] tries it again when its record's state
//!    is given back.
//! 3. A record the prompt retired, by a statement it typed or by a restatement, is given back
//!    only if nothing was said or done to the store since that retirement ([`give_back`], the
//!    one way a record comes back). A word of the user's typed while the record was retired
//!    never met it, and whether it would have replaced it is a supersession that never ran. A
//!    pass of maintain, a `muninn revoke`, an import read active records only and never met
//!    it either. None of that is run after the fact: the record stays retired, and is marked.
//! 4. An episode written as the user's is replaced by an agent's (`relabel_episodes`).
//!
//! The undo runs before anything of the coordinator's own batch is written
//! (`super::find_out`): the prompt that opens the turn which hands the task over was typed
//! before the worker ran, and meets the store with the worker's prompt already undone.
//!
//! "Since" is exact and reads no clock ([`store_changed_since`]). A statement retires what it
//! replaces when it is inserted, so a record with a greater id came after that retirement. A
//! journal row holds the newest record on the store when it was written (`after_record`), so
//! a record is before or after a restatement by its id, and two restatements by theirs. A
//! pass outside ingest leaves a row (`store_event`, `db::note_event`) with the newest record
//! and journal row it found.
//!
//! The mark is the cause alone. What made the retirement is kept beside it in `agent_retired`,
//! which no export carries: a marked record that an import brought has no link, nothing here
//! resolves it, and it stays retired.
//!
//! A word that stands in the way may itself be an agent's, not yet known as one. Nothing tells
//! the two apart, so the retirement is asked again each time more prompts are found out
//! ([`reconsider`]), and a mark is taken off when the record is given back.
//!
//! No wording is read to decide who wrote a prompt, and nothing a restatement did is
//! reconstructed from the current state of `record`: the journal says what it did.

use super::relabel_episodes;
use muninn_core::db::AGENT_UNRESTORED as UNRESTORED;

/// One row of the restatement journal.
struct Restatement {
    id: i64,
    record: i64,
    turn_ref: String,
    new_session: Option<String>,
    new_created: i64,
    prior_invalid: i64,
    prior_reason: Option<String>,
    prior_by: Option<i64>,
    prior_via: Option<String>,
    prior_at: Option<i64>,
    prior_ref: Option<String>,
    prior_session: Option<String>,
    prior_created: i64,
    added_turn: i64,
}

/// `restatement.undone` holds one of three states. Applied (0): the restatement stands.
const UNDONE: i64 = 1;
/// Its turn is known to be an agent's, and its record was no longer as it left it.
const PENDING: i64 = 2;

/// The causes a supersession leaves on the record it retires (`invalidated_via`).
const SUPERSEDED: &str = "'key', 'said', 'quantity', 'ack', 'correction'";

/// The kinds a statement is typed as.
const TYPED: &str = "'decision', 'invariant', 'correction'";

/// The turns known to be an agent's: a prompt whose hash another session handed over.
const AGENT_TURNS: &str = "(SELECT p.transcript_ref FROM turn_prompt p \
     JOIN agent_task a ON a.hash = p.hash AND a.session_id <> p.session_id)";

/// A retirement an agent's prompt made.
#[derive(Clone, Copy)]
enum Made {
    /// By a statement the prompt typed, when it was inserted: a supersession.
    Statement(i64),
    /// By a restatement: its journal row, and the record it restated.
    Restatement { journal: i64, record: i64 },
    /// By a restatement, as far as its journal row left a mark: the rows linked to it, and no
    /// row that only looks like one it retired ([`reconsider`]).
    Marked(i64),
}

impl Made {
    /// The heir a record it retired keeps when it is left retired, marked. A statement stays
    /// the heir: it is revoked, or retired by a later word, and either way the chain runs on
    /// from it. A restated record does not: it has gone back to its own prior state, which may
    /// be "replaced by" the very record it had retired.
    fn heir(self) -> Option<i64> {
        match self {
            Made::Statement(heir) => Some(heir),
            Made::Restatement { .. } | Made::Marked(_) => None,
        }
    }

    /// The link a marked record has to it (`agent_retired`), as a column and its value. The
    /// link is in that table and nowhere on the record: a marked record that came in by an
    /// import has none, and nothing here finds it.
    fn link(self) -> (&'static str, i64) {
        match self {
            Made::Statement(heir) => ("statement", heir),
            Made::Restatement { journal, .. } | Made::Marked(journal) => ("journal", journal),
        }
    }

    /// Whether a row is linked to it: `row` is the record's id, and `held_by` 0 for the record
    /// as it stands, or the journal row whose prior state is meant.
    fn linked(self, row: &str, held_by: &str) -> String {
        let (column, id) = self.link();
        format!(
            "EXISTS (SELECT 1 FROM agent_retired l WHERE l.record_id = {row}              AND l.held_by = {held_by} AND l.{column} = {id})"
        )
    }

    /// What a retirement of this kind leaves on a row it retires, as a filter on the heir and
    /// cause columns named `by` and `via`, before any mark.
    fn left(self, by: &str, via: &str) -> Option<String> {
        match self {
            Made::Statement(heir) => Some(format!("{by} = {heir} AND {via} IN ({SUPERSEDED})")),
            Made::Restatement { record, .. } => {
                Some(format!("{by} = {record} AND {via} = 'restated'"))
            }
            Made::Marked(_) => None,
        }
    }

    /// The records still retired by it, as a filter on `record`: exactly as it left them
    /// (`invalidated_by` and the cause are the whole of what a retirement leaves on a row, and
    /// a retired row changes only by being put back), or marked since and linked to it.
    ///
    /// Two restatements of one record leave the same heir and cause on what they retire, and
    /// one row can be in the lists of both. It is the later one's, while that one stands: a
    /// row is in a list only if it was active when the list was made.
    fn retired(self) -> String {
        // by id and by heir, so that each side of the OR is read through an index
        let (column, id) = self.link();
        let marked = format!(
            "id IN (SELECT l.record_id FROM agent_retired l WHERE l.held_by = 0 AND l.{column} = {id})              AND invalidated_via = '{UNRESTORED}'"
        );
        let left = match (self, self.left("invalidated_by", "invalidated_via")) {
            (Made::Restatement { journal, record }, Some(left)) => format!(
                " OR {left} AND {journal} = (SELECT max(x.restatement_id) FROM restatement_retired x \
                 JOIN restatement o ON o.id = x.restatement_id \
                 WHERE x.record_id = record.id AND o.record_id = {record} \
                 AND (o.undone <> {UNDONE} OR o.id = {journal}))"
            ),
            (_, Some(left)) => format!(" OR {left}"),
            (_, None) => String::new(),
        };
        format!("invalid = 1 AND ({marked}{left})")
    }

    /// The same for a later journal row: one whose record that retirement had retired, and
    /// which put the record back, so that the retirement is the state it would return to.
    fn recorded(self) -> String {
        let marked = format!(
            "prior_via = '{UNRESTORED}' AND {}",
            self.linked("restatement.record_id", "restatement.id")
        );
        let left = match (self, self.left("prior_by", "prior_via")) {
            (Made::Restatement { journal, record }, Some(left)) => format!(
                " OR {left} AND {journal} = (SELECT max(x.restatement_id) FROM restatement_retired x \
                 JOIN restatement o ON o.id = x.restatement_id \
                 WHERE x.record_id = restatement.record_id AND o.record_id = {record} \
                 AND o.id < restatement.id AND (o.undone <> {UNDONE} OR o.id = {journal}))"
            ),
            (_, Some(left)) => format!(" OR {left}"),
            (_, None) => String::new(),
        };
        format!("prior_invalid = 1 AND ({marked}{left})")
    }
}

/// Journal rows, newest first.
fn journal_rows(
    tx: &rusqlite::Connection,
    filter: &str,
    params: &[&dyn rusqlite::ToSql],
) -> muninn_core::Result<Vec<Restatement>> {
    let rows = tx
        .prepare(&format!(
            "SELECT id, record_id, turn_ref, new_session, new_created, prior_invalid, prior_reason, prior_by, \
             prior_via, prior_at, prior_ref, prior_session, prior_created, added_turn FROM restatement \
             WHERE {filter} ORDER BY id DESC"
        ))?
        .query_map(params, |r| {
            Ok(Restatement {
                id: r.get(0)?,
                record: r.get(1)?,
                turn_ref: r.get(2)?,
                new_session: r.get(3)?,
                new_created: r.get(4)?,
                prior_invalid: r.get(5)?,
                prior_reason: r.get(6)?,
                prior_by: r.get(7)?,
                prior_via: r.get(8)?,
                prior_at: r.get(9)?,
                prior_ref: r.get(10)?,
                prior_session: r.get(11)?,
                prior_created: r.get(12)?,
                added_turn: r.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn id_list(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

/// The restatements of the tasks found in one ingest are undone together.
///
/// A journal row is applied, undone, or pending:
///
/// - The rows of every task found are taken newest first by journal id, whatever order the
///   task hashes sort in, so two copies of one statement are unwound in the reverse of the
///   order they were made.
/// - A row that [`invert`] cannot invert, because its record is not as the row left it,
///   becomes pending, not undone, and what it retired is left retired and marked. What changed
///   the record is either the user's, and stands, or something an agent's turn did whose
///   coordinator has not arrived. The journal cannot tell the two apart and does not need to:
///   [`settle`] tries the pending rows of a record again whenever its state is given back.
///
/// Returns how many records were retired on the way.
pub(super) fn undo_restatements(
    tx: &rusqlite::Connection,
    hashes: &[String],
    sid: &str,
) -> muninn_core::Result<usize> {
    // A marked record that was said again since the last undo: the restatement recorded the
    // mark as the state to return to, and the record's own link, which no retirement has
    // touched since, is the link of that state. It is moved to the row before anything here
    // marks the record anew.
    tx.execute(
        &format!(
            "UPDATE agent_retired SET held_by =                  (SELECT min(j.id) FROM restatement j WHERE j.record_id = agent_retired.record_id                   AND j.undone <> {UNDONE} AND j.prior_via = '{UNRESTORED}')              WHERE held_by = 0              AND record_id IN (SELECT j.record_id FROM restatement j                                WHERE j.undone <> {UNDONE} AND j.prior_via = '{UNRESTORED}'                                AND NOT EXISTS (SELECT 1 FROM agent_retired o                                                WHERE o.record_id = j.record_id AND o.held_by = j.id))              AND record_id NOT IN (SELECT id FROM record WHERE invalid = 1                                    AND invalidated_via = '{UNRESTORED}')"
        ),
        [],
    )?;
    let mut rows = Vec::new();
    for h in hashes {
        rows.extend(journal_rows(
            tx,
            "undone = 0 AND turn_ref IN (SELECT transcript_ref FROM turn_prompt WHERE hash = ?1 AND session_id <> ?2)",
            rusqlite::params![h, sid],
        )?);
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.id));
    rows.dedup_by_key(|r| r.id);
    let mut retired = 0;
    for row in rows {
        match invert(tx, row.id)? {
            Some(back) => retired += settle(tx, back)?,
            None => {
                tx.execute(
                    "UPDATE restatement SET undone = ?2 WHERE id = ?1 AND undone = 0",
                    [row.id, PENDING],
                )?;
                leave_retired(
                    tx,
                    Made::Restatement {
                        journal: row.id,
                        record: row.record,
                    },
                )?;
            }
        }
    }
    Ok(retired)
}

/// Follows through on records whose state was just given back (`back`), until nothing moves:
///
/// - the pending journal rows of those records are tried again, newest first;
/// - whichever of them now stands in a turn known to be an agent's is retired as that turn's
///   rows are: a typed statement is revoked ([`revoke`], which gives back what it replaced),
///   an episode is relabelled. The pass over a task's turn only sees what is there when the
///   task is found; a row another agent's turn had retired comes back later, here.
///
/// Each round feeds the next with the records it gave back. A journal row is inverted once and
/// a record is revoked once (a revoked record is never given back), so the rounds end. Returns
/// how many records were retired.
pub(super) fn settle(tx: &rusqlite::Connection, mut back: Vec<i64>) -> muninn_core::Result<usize> {
    let mut retired = 0;
    while !back.is_empty() {
        back.sort_unstable();
        back.dedup();
        let ids = id_list(&back);
        let mut next = Vec::new();
        for p in journal_rows(
            tx,
            &format!("undone = {PENDING} AND record_id IN ({ids})"),
            &[],
        )? {
            if let Some(b) = invert(tx, p.id)? {
                next.extend(b);
            }
        }
        let scope = format!("id IN ({ids}) AND transcript_ref IN {AGENT_TURNS}");
        let (n, b) = revoke(tx, &scope, &[])?;
        next.extend(b);
        retired += n + relabel_episodes(tx, &scope, &[])?;
        back = next;
    }
    Ok(retired)
}

/// What an agent's prompt typed at trust 3 is revoked (`invalidated_via = 'agent_prompt'`), and
/// what that statement itself replaced by a supersession is given back ([`give_back`]) where
/// the user has not spoken since: the user's statement and the episode of its turn.
///
/// Only a statement still active is revoked, or one left retired with a mark: it is the
/// agent's, and the mark is for records of the user's. One a later word has replaced is
/// retired already and stays as it is; what it had replaced stays replaced, and is marked.
///
/// A record that a restatement still standing moved into the turn was not typed by it: it is
/// the user's, the journal gives it back, and it is never revoked. What a restatement retired
/// (`restated`) belongs to the journal too.
///
/// Returns how many records were revoked, and the ones given back.
pub(super) fn revoke(
    tx: &rusqlite::Connection,
    scope: &str,
    params: &[&dyn rusqlite::ToSql],
) -> muninn_core::Result<(usize, Vec<i64>)> {
    let ids: Vec<(i64, bool, bool)> = tx
        .prepare(&format!(
            "SELECT id, invalid = 0 OR invalidated_via IS '{UNRESTORED}', \
             invalidated_via IS 'agent_prompt' FROM record WHERE origin = 'user_said' \
             AND kind IN ({TYPED}) AND {scope} \
             AND NOT EXISTS (SELECT 1 FROM restatement j WHERE j.record_id = record.id \
                             AND j.turn_ref = record.transcript_ref AND j.undone <> {UNDONE}) \
             ORDER BY id"
        ))?
        .query_map(params, |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // every statement of the scope is revoked before anything is given back: a statement of
    // the same turn is not a word of the user's said since
    let mut revoked = Vec::new();
    for (id, standing, revoked_before) in ids {
        let imported: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM store_event WHERE what = 'import' AND after_record >= ?1)",
            [id],
            |r| r.get(0),
        )?;
        // What it holds retired as a restatement, with no journal row of this store to say so,
        // came in with it: marked too.
        if imported && !revoked_before {
            tx.execute(
                &format!(
                    "UPDATE record SET invalidated_via = '{UNRESTORED}' WHERE invalid = 1 \
                     AND invalidated_by = ?1 AND invalidated_via = 'restated' \
                     AND id NOT IN (SELECT record_id FROM restatement_retired)"
                ),
                [id],
            )?;
        }
        if standing {
            // The journal does not travel with an export. In a store that imported this record,
            // nothing says whether the turn typed it or a copy moved a statement of the user's
            // there, nor what the copy retired. It is retired either way, and marked instead
            // of revoked, with what it holds retired: counted, and never given back here.
            tx.execute(
                "UPDATE record SET invalid = 1, invalid_reason = 'revoked', invalidated_by = NULL, \
                 invalidated_via = ?2 WHERE id = ?1",
                rusqlite::params![id, if imported { UNRESTORED } else { "agent_prompt" }],
            )?;
            if imported {
                leave_retired(tx, Made::Statement(id))?;
            } else {
                revoked.push(id);
            }
        } else if !revoked_before {
            leave_retired(tx, Made::Statement(id))?;
        }
    }
    let mut back = Vec::new();
    for id in &revoked {
        back.extend(give_back(tx, Made::Statement(*id))?);
    }
    Ok((revoked.len(), back))
}

/// The exact inverse of one journal row, while its record is still as the row left it: active,
/// in the restating turn, with that turn's session and date. Returns the records whose state
/// it changed, or `None` when the record has changed since.
///
/// The restated record gets back its prior state, whatever the user has said since: it was
/// there, as the copy, for every later word to meet. What the restatement retired is another
/// matter, and goes through [`give_back`].
///
/// The row is read here, by id, and not taken from a list read earlier: an inversion or a
/// revoke in between may have rewritten its prior state.
fn invert(tx: &rusqlite::Connection, id: i64) -> muninn_core::Result<Option<Vec<i64>>> {
    let rows = journal_rows(
        tx,
        "id = ?1 AND undone <> ?2",
        rusqlite::params![id, UNDONE],
    )?;
    let Some(j) = rows.first() else {
        return Ok(None);
    };
    let left: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM record WHERE id = ?1 AND invalid = 0 AND invalidated_by IS NULL \
         AND transcript_ref = ?2 AND session_id IS ?3 AND created_at = ?4)",
        rusqlite::params![j.record, j.turn_ref, j.new_session, j.new_created],
        |r| r.get(0),
    )?;
    if !left {
        return Ok(None);
    }
    tx.execute(
        "UPDATE record SET invalid = ?2, invalid_reason = ?3, invalidated_by = ?4, invalidated_via = ?5, \
         invalidated_at = ?6, transcript_ref = ?7, session_id = ?8, created_at = ?9 WHERE id = ?1",
        rusqlite::params![
            j.record,
            j.prior_invalid,
            j.prior_reason,
            j.prior_by,
            j.prior_via,
            j.prior_at,
            j.prior_ref,
            j.prior_session,
            j.prior_created
        ],
    )?;
    // The record was active, so a link it kept as it stood is void; if the state it returns to
    // is a marked one, the link held for this row becomes the record's own.
    tx.execute(
        "DELETE FROM agent_retired WHERE record_id = ?1 AND held_by = 0",
        [j.record],
    )?;
    tx.execute(
        "UPDATE agent_retired SET held_by = 0 WHERE record_id = ?1 AND held_by = ?2",
        [j.record, j.id],
    )?;
    if j.added_turn == 1 {
        if let Some(old) = &j.prior_ref {
            tx.execute(
                "DELETE FROM record_turn WHERE record_id = ?1 AND transcript_ref = ?2",
                rusqlite::params![j.record, old],
            )?;
        }
    }
    tx.execute(
        "UPDATE restatement SET undone = ?2 WHERE id = ?1",
        [j.id, UNDONE],
    )?;
    let mut touched = vec![j.record];
    touched.extend(give_back(
        tx,
        Made::Restatement {
            journal: j.id,
            record: j.record,
        },
    )?);
    Ok(Some(touched))
}

/// Whether the store changed since a retirement was made, by a word of the user's or by a
/// pass outside ingest. Exact, by id,
/// with no clock (the module's head says how).
///
/// - A statement of the user's typed after it, unless revoked; or one they said again after
///   it, while that restatement stands, neither inverted nor pending. A word of an agent's
///   whose coordinator has not arrived counts as the user's until it is found out
///   ([`reconsider`] asks again then).
/// - A pass outside ingest that retires or revokes records and ran after it (`store_event`):
///   maintain's commit and revert passes, the anchor validator, `muninn revoke`, an import.
///   Such a pass reads active records only, so it never met a record that was retired then.
///
/// A journal row an earlier build wrote without `after_record` cannot be placed: what it
/// retired is not given back. It says nothing about any other retirement.
fn store_changed_since(tx: &rusqlite::Connection, made: Made) -> muninn_core::Result<bool> {
    // R3. A row the person's own turn took over from a worker's (`ingest::restate`) sits
    // beside the correction typed from the same sentence. A record of the turn that what
    // would be given back stands at is not a word said after it, the rule [`revoke`] applies
    // to one turn's statements.
    let spared = match made {
        Made::Statement(_) => String::new(),
        Made::Restatement { journal, .. } | Made::Marked(journal) => format!(
            " AND (transcript_ref IS NULL OR transcript_ref NOT IN \
                  (SELECT r.transcript_ref FROM record r JOIN restatement_retired x ON x.record_id = r.id \
                   WHERE x.restatement_id = {journal} AND r.transcript_ref IS NOT NULL))"
        ),
    };
    let (after_record, restated_since, done_since) = match made {
        Made::Statement(heir) => (
            Some(heir),
            format!("after_record >= {heir}"),
            format!("after_record >= {heir}"),
        ),
        Made::Restatement { journal, .. } | Made::Marked(journal) => (
            tx.query_row(
                "SELECT after_record FROM restatement WHERE id = ?1",
                [journal],
                |r| r.get::<_, Option<i64>>(0),
            )?,
            format!("id > {journal}"),
            format!("after_journal >= {journal}"),
        ),
    };
    let Some(after_record) = after_record else {
        return Ok(true);
    };
    Ok(tx.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM record WHERE id > ?1 AND origin = 'user_said' \
                           AND kind IN ({TYPED}) AND invalidated_via IS NOT 'agent_prompt'{spared}) \
             OR EXISTS(SELECT 1 FROM restatement WHERE undone = 0 AND {restated_since}) \
             OR EXISTS(SELECT 1 FROM store_event WHERE {done_since})"
        ),
        [after_record],
        |r| r.get(0),
    )?)
}

/// A record the person revoked while this retirement held it retired is not given back and
/// not marked: `muninn revoke` changed nothing on it then and left a note
/// (`agent_retired`, `held_by` -1, dated like an event). Its cause becomes `revoked`, which no
/// filter here matches again. Its reason stays `superseded`, so the episode of its turn is
/// still hidden on the read path (`recall::stale_turns`) when that episode comes back.
///
/// A note older than the retirement says nothing about it: the record was put back since.
fn keep_revoked(tx: &rusqlite::Connection, made: Made) -> muninn_core::Result<()> {
    let noted_since = match made {
        Made::Statement(heir) => format!("f.statement >= {heir}"),
        Made::Restatement { journal, .. } | Made::Marked(journal) => {
            format!("f.journal >= {journal}")
        }
    };
    tx.execute(
        &format!(
            "UPDATE record SET invalidated_via = 'revoked' WHERE {} \
             AND EXISTS (SELECT 1 FROM agent_retired f WHERE f.record_id = record.id \
                         AND f.held_by = -1 AND {noted_since})",
            made.retired()
        ),
        [],
    )?;
    Ok(())
}

/// Gives back what an agent's prompt retired, if nothing was said or done since: every record
/// still retired by that retirement is made active again. Otherwise nothing comes back
/// ([`leave_retired`]). Every path that returns a record goes through here.
///
/// One thing follows from a retirement being undone. A restatement that put such a record back
/// in the meantime recorded the retirement as the state to return to; while that journal row
/// stands, its prior state is rewritten to active. If it is an agent's copy, its own inversion
/// then returns the record to where the user left it; if it is the user's, the prior is never
/// read.
///
/// Returns the records made active.
fn give_back(tx: &rusqlite::Connection, made: Made) -> muninn_core::Result<Vec<i64>> {
    keep_revoked(tx, made)?;
    if store_changed_since(tx, made)? {
        leave_retired(tx, made)?;
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = tx
        .prepare(&format!(
            "SELECT id FROM record WHERE {} ORDER BY id",
            made.retired()
        ))?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for id in &ids {
        tx.execute(
            "UPDATE record SET invalid = 0, invalid_reason = NULL, invalidated_by = NULL WHERE id = ?1",
            [id],
        )?;
    }
    tx.execute(
        &format!(
            "UPDATE restatement SET prior_invalid = 0, prior_reason = NULL, prior_by = NULL, \
             prior_via = NULL, prior_at = NULL WHERE undone <> {UNDONE} AND {}",
            made.recorded()
        ),
        [],
    )?;
    let (column, id) = made.link();
    tx.execute(
        &format!("DELETE FROM agent_retired WHERE {column} = ?1 AND held_by >= 0"),
        [id],
    )?;
    Ok(ids)
}

/// What an agent's prompt retired stays retired, and is marked so that it can be found: the
/// records still retired by that retirement, and the prior state of a later journal row that
/// recorded it. The mark is the cause `agent_unrestored`; what made the retirement is written
/// beside it, in `agent_retired`, so that it can be asked again ([`reconsider`]).
fn leave_retired(tx: &rusqlite::Connection, made: Made) -> muninn_core::Result<()> {
    keep_revoked(tx, made)?;
    let (column, id) = made.link();
    let heir = made.heir();
    // (table, the record's id, what holds the mark, the rows, the cause and heir columns)
    for (table, key, held_by, filter, via, by) in [
        (
            "record",
            "id",
            "0",
            made.retired(),
            "invalidated_via",
            "invalidated_by",
        ),
        (
            "restatement",
            "record_id",
            "id",
            format!("undone <> {UNDONE} AND {}", made.recorded()),
            "prior_via",
            "prior_by",
        ),
    ] {
        let ids: Vec<(i64, i64)> = tx
            .prepare(&format!(
                "SELECT {key}, {held_by} FROM {table} WHERE {filter}"
            ))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        tx.execute(
            &format!("UPDATE {table} SET {via} = '{UNRESTORED}', {by} = ?1 WHERE {filter}"),
            [heir],
        )?;
        for (record, held_by) in ids {
            tx.execute(
                &format!(
                    "INSERT OR REPLACE INTO agent_retired(record_id, held_by, {column}) VALUES (?1, ?2, ?3)"
                ),
                [record, held_by, id],
            )?;
        }
    }
    Ok(())
}

/// The statements an agent's turn typed that a second agent's copy moved to its own turn, and
/// that a later word retired there. The copy's journal row is pending, so the statement never
/// goes back to the turn that typed it, where [`revoke`] would have found it; what it replaced
/// when it was typed stays replaced, and has to be marked all the same.
///
/// The turn that typed a statement is the one its first journal row moved it from.
fn copied_and_replaced(tx: &rusqlite::Connection) -> muninn_core::Result<Vec<i64>> {
    let ids = tx
        .prepare(&format!(
            "SELECT DISTINCT j.record_id FROM restatement j JOIN record r ON r.id = j.record_id \
             WHERE j.undone = {PENDING} AND r.invalid = 1 AND r.invalidated_via IS NOT 'agent_prompt' \
             AND (SELECT f.prior_ref FROM restatement f WHERE f.record_id = j.record_id \
                  ORDER BY f.id LIMIT 1) IN {AGENT_TURNS}"
        ))?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Asks again about every record left retired and marked, after the prompts of one ingest
/// have been found out: a statement that stood in the way of giving it back may have been one
/// of theirs, revoked now.
///
/// - A marked row that an agent's own turn wrote is the agent's: its statement is revoked and
///   its episode relabelled, as the rest of that turn was, and it is no record of the user's
///   left retired.
/// - Each retirement that left a mark is put to [`give_back`] again, if it is one that has
///   been undone: by a statement since revoked, or by a journal row since inverted. A
///   retirement by a statement that a later word replaced, or by a row still pending, stands.
///
/// Only the rows linked in `agent_retired` are read, so a marked row that an import brought
/// has no retirement to ask about, and stays retired. What comes back is followed through
/// ([`settle`]), and the rows are read again until nothing comes back. A record given back is
/// not marked again, so the rounds end. Returns how many records were retired on the way.
pub(super) fn reconsider(tx: &rusqlite::Connection) -> muninn_core::Result<usize> {
    let mut retired = 0;
    loop {
        let own = format!(
            "invalidated_via = '{UNRESTORED}' AND transcript_ref IN {AGENT_TURNS} \
             AND id IN (SELECT record_id FROM agent_retired WHERE held_by = 0)"
        );
        let (n, mut back) = revoke(tx, &own, &[])?;
        retired += n + relabel_episodes(tx, &own, &[])?;
        for id in copied_and_replaced(tx)? {
            leave_retired(tx, Made::Statement(id))?;
        }
        let links: Vec<(Option<i64>, Option<i64>)> = tx
            .prepare(&format!(
                "SELECT DISTINCT l.statement, l.journal FROM agent_retired l \
                 JOIN record r ON r.id = l.record_id \
                 WHERE l.held_by = 0 AND r.invalid = 1 AND r.invalidated_via = '{UNRESTORED}'"
            ))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for link in links {
            let undone =
                |sql: &str, id: i64| -> bool { tx.query_row(sql, [id], |_| Ok(())).is_ok() };
            let made = match link {
                (Some(heir), _)
                    if undone(
                        "SELECT 1 FROM record WHERE id = ?1 AND invalidated_via IS 'agent_prompt'",
                        heir,
                    ) =>
                {
                    Some(Made::Statement(heir))
                }
                (None, Some(journal))
                    if undone(
                        "SELECT 1 FROM restatement WHERE id = ?1 AND undone = 1",
                        journal,
                    ) =>
                {
                    Some(Made::Marked(journal))
                }
                _ => None,
            };
            if let Some(made) = made {
                back.extend(give_back(tx, made)?);
            }
        }
        if back.is_empty() {
            return Ok(retired);
        }
        retired += settle(tx, back)?;
    }
}
