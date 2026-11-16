//! Read-hook delivery log: which records were delivered (or why nothing was),
//! appended by read hooks and folded into `fire_ledger` by the write path.

use muninn_core::{Db, ProjectPaths, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Line {
    pub at: i64,
    pub session: String,
    pub arm: String,
    pub ids: Vec<i64>,
    pub tokens: usize,
    pub reason: String,
}

pub fn log_path(paths: &ProjectPaths) -> std::path::PathBuf {
    paths.log_dir().join("delivery.jsonl")
}

/// A line as it is written: stamped with the compaction epoch its session is in when the
/// hook appends it, so the fold can file it under that epoch (R-02).
#[derive(Serialize, Deserialize)]
struct Stamped<L> {
    #[serde(flatten)]
    line: L,
    #[serde(default)]
    epoch: i64,
}

pub fn append(paths: &ProjectPaths, line: &Line) {
    let stamped = Stamped {
        line,
        epoch: epoch(paths, &line.session),
    };
    let _ = muninn_core::logfold::append_line(&log_path(paths), &stamped);
}

/// Whether a line with this reason put its records in front of this session's agent, so
/// that they count as delivered for the rest of the epoch. The ledger keeps every line for
/// its counts; only these exclude a record from the next answer.
///
/// - `gated:` lines name records the budget cut. Counted as delivered, a record cut once was
///   withheld for the whole session: on a 1.2.0 store 7,642 of 7,661 (session, record) pairs
///   gated were never delivered in that session (R-01).
/// - `cue:event:subagent` lines are a subagent's delivery, logged under its parent's session
///   id: the parent never saw them.
/// - `catalog` lines list a record by a title of at most 72 characters, which is not the
///   record. Counted as delivered, the record a session's catalogue named was the one its
///   prompts could no longer reach (R-13).
pub fn shown_to_session(reason: &str) -> bool {
    !(reason.contains("gated:") || reason.contains(SUBAGENT_EVENT) || reason.ends_with("catalog"))
}

/// `shown_to_session` in SQL, for the ledger rows. The partial index `fire_session_seen`
/// (`schema.sql`) is declared with these exact words, which is what lets SQLite use it: it then
/// holds only the rows that count, and a long session's budget cuts are never read.
pub const SHOWN_ROWS_SQL: &str = "reason NOT LIKE '%gated:%' AND reason NOT LIKE '%cue:event:subagent%' AND reason NOT LIKE '%catalog'";

/// The reason a subagent's event delivery is logged under.
pub const SUBAGENT_EVENT: &str = "cue:event:subagent";

/// Ids delivered in this session and not yet folded (so the same record is not
/// repeated before the write path runs). The log keeps folded lines now; reading only
/// past the fold watermark keeps the window the gates measured, when a fold removed
/// the file.
pub fn delivered_ids(
    paths: &ProjectPaths,
    db: &Db,
    session: &str,
) -> std::collections::HashSet<i64> {
    let mut out = std::collections::HashSet::new();
    // What has already been folded into `fire_ledger`, which the pending file no longer holds.
    //
    // `SessionStart` spawns `maintain` detached, `maintain` folds the ledger and advances its
    // watermark, and from that moment `pending` is empty — so a record delivered earlier in the
    // *same* session was delivered again. Measured on the head-to-head: 167 of the 224 records
    // the prompt blocks served had already been named by the catalogue in that session (that
    // repeat was the fold's; a catalogue line itself no longer counts, see `shown_to_session`).
    // The partial index `fire_session_seen` answers this lookup and holds only the rows that
    // count (`SHOWN_ROWS_SQL`): a long session's budget cuts, most of its rows, are not read.
    //
    // Only the rows of the session's current epoch: the ledger restarts at each compaction
    // [K1]. The epoch used to be read from a `meta` key nothing wrote, so every row of every
    // store was filed under epoch 0 and a compaction stopped restarting anything once the
    // next fold had consumed its `epoch:` line (R-02).
    {
        let epoch = epoch(paths, session);
        if let Ok(mut st) = db.conn.prepare(&format!(
            "SELECT record_id FROM fire_ledger \
             WHERE session_id = ?1 AND compaction_epoch = ?2 AND record_id IS NOT NULL \
               AND {SHOWN_ROWS_SQL}"
        )) {
            if let Ok(rows) =
                st.query_map(rusqlite::params![session, epoch], |r| r.get::<_, i64>(0))
            {
                out.extend(rows.flatten().filter(|id| *id > 0));
            }
        }
    }
    for l in muninn_core::logfold::pending(&db.conn, &log_path(paths)) {
        if let Ok(v) = serde_json::from_str::<Line>(&l) {
            if v.session == session {
                if v.reason.starts_with("epoch:") {
                    out.clear();
                } else if shown_to_session(&v.reason) {
                    out.extend(v.ids);
                }
            }
        }
    }
    out
}

/// Fold into `fire_ledger` (one row per delivered record, one per silence).
pub fn fold_into_db(paths: &ProjectPaths, db: &Db) -> Result<usize> {
    let tx = db.write_tx()?;
    // from the watermark: the live file is never renamed under concurrent appenders
    let taken = muninn_core::logfold::take(&tx, &log_path(paths))?;
    let mut n = 0;
    {
        let mut ins = tx.prepare("INSERT INTO fire_ledger(session_id, compaction_epoch, record_id, fired_at, tokens, reason) VALUES (?1,?2,?3,?4,?5,?6)")?;
        for l in &taken.lines {
            // each row under the epoch its line was appended in; a line written before lines
            // carried one reads as epoch 0, which is where every row of that time was filed
            let Ok(Stamped { line: v, epoch }) = serde_json::from_str::<Stamped<Line>>(l) else {
                continue;
            };
            if v.ids.is_empty() {
                ins.execute(rusqlite::params![
                    v.session,
                    epoch,
                    Option::<i64>::None,
                    v.at,
                    0i64,
                    format!("{}:{}", v.arm, v.reason)
                ])?;
                n += 1;
            } else {
                let per = v.tokens / v.ids.len().max(1);
                for id in &v.ids {
                    // ids of records that are not in this store (the control arm logs
                    // the foreign store's ids negated) keep their tokens, not their id
                    let rid = if *id > 0 { Some(*id) } else { None };
                    ins.execute(rusqlite::params![
                        v.session,
                        epoch,
                        rid,
                        v.at,
                        per as i64,
                        format!("{}:{}", v.arm, v.reason)
                    ])?;
                    n += 1;
                }
            }
        }
    }
    // The render fingerprint, rolled here because a read hook may not write the store.
    //
    // Health check 6 watches for the session-start reinjection going stale — the same
    // invariants delivered every session while new ones arrive — by comparing the last two
    // renders. It compared two `meta` keys **that nothing in this repository ever wrote**, so
    // it has read `cold: fewer than two renders` since the day it was added and could never
    // read anything else. A check that cannot fire is worse than no check: it holds a slot in
    // `10/11 GREEN` and tells the reader something is being watched.
    //
    // What is hashed is the set of record ids the session-start event delivery served, in
    // order, which is exactly the thing that going stale would mean.
    if let Some(latest) = taken
        .lines
        .iter()
        .filter_map(|l| serde_json::from_str::<Line>(l).ok())
        .filter(|v| v.reason.contains("session_start") && !v.ids.is_empty())
        .max_by_key(|v| v.at)
    {
        let mut ids = latest.ids.clone();
        ids.sort_unstable();
        let hash = blake3::hash(format!("{ids:?}").as_bytes())
            .to_hex()
            .to_string();
        let prev: Option<String> = tx
            .query_row(
                "SELECT value FROM meta WHERE key='last_render_hash'",
                [],
                |r| r.get(0),
            )
            .ok();
        if prev.as_deref() != Some(hash.as_str()) || prev.is_none() {
            if let Some(p) = prev {
                tx.execute(
                    "INSERT INTO meta(key,value) VALUES('prev_render_hash',?1) \
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    [p],
                )?;
            }
            tx.execute(
                "INSERT INTO meta(key,value) VALUES('last_render_hash',?1) \
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [&hash],
            )?;
            // The pool this render was drawn from, pinned to the hash above: what the
            // event cue can serve at session start is the active invariants and
            // corrections, and nothing else. Check 6 calls a render frozen when the hash
            // has not moved while this number has, which is the only reading under which
            // "the same invariants every session while new ones arrive" is true.
            //
            // It cannot be the key the check first read, `records_changed_since_render` —
            // the projection trigger, renamed here to `records_changed_since_project` after
            // its name talked a check into reading it. Every ingest, every anchor retirement and
            // every import sets it, and a commit decision sets it exactly as an invariant
            // does — so on a healthy store, where invariants are stable and commits keep
            // arriving, the check read RED. Measured on this repository's own store: three
            // session-start deliveries of the same six invariants, flag at 1, check RED
            // with nothing frozen about it.
            let pool: i64 = tx.query_row(
                "SELECT count(*) FROM served_record WHERE kind IN ('invariant','correction')",
                [],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO meta(key,value) VALUES('last_render_pool',?1) \
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [pool.to_string()],
            )?;
        } else {
            // identical to the last one: keep both, which is what check 6 reads as frozen
            tx.execute(
                "INSERT INTO meta(key,value) VALUES('prev_render_hash',?1) \
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [&hash],
            )?;
            // `DO NOTHING`, so the number keeps belonging to the render that first produced
            // this hash. A store whose hash was already standing still when the pool key was
            // introduced would otherwise carry none, and check 6 has no floor to compare
            // against: it would read green for as long as the freeze lasted.
            let pool: i64 = tx.query_row(
                "SELECT count(*) FROM served_record WHERE kind IN ('invariant','correction')",
                [],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO meta(key,value) VALUES('last_render_pool',?1) \
                 ON CONFLICT(key) DO NOTHING",
                [pool.to_string()],
            )?;
        }
    }
    tx.commit()?;
    taken.finish();
    Ok(n)
}

/// Compaction epoch of a session: a small counter file in the log dir (a read hook may
/// write there). The ledger restarts per epoch [K1]: delivered ids are those of the
/// current epoch only.
fn epoch_path(paths: &ProjectPaths, session: &str) -> std::path::PathBuf {
    paths.log_dir().join(format!(
        "epoch-{}.txt",
        session
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect::<String>()
    ))
}

pub fn epoch(paths: &ProjectPaths, session: &str) -> i64 {
    std::fs::read_to_string(epoch_path(paths, session))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Within this long of the last epoch move, a compaction hook belongs to the same compaction.
/// Two compactions of one session cannot be this close: the context has to fill up again.
const SAME_COMPACTION_MS: u128 = 30_000;

/// Move the session to its next epoch, once per compaction, and say whether this call did.
///
/// One compaction runs two hooks, `SessionStart` with `source=compact` and then `PostCompact`,
/// 150 to 250 ms apart in the heartbeats of a 1.2.0 store. Both moved the epoch and logged the
/// reinjection, so every compaction was counted twice in the ledger, and only the session
/// start's context reached the transcript. Whichever hook comes first moves the epoch; the
/// other, within `SAME_COMPACTION_MS` of it, does not.
pub fn begin_compaction(paths: &ProjectPaths, session: &str) -> bool {
    let recent = std::fs::metadata(epoch_path(paths, session))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|d| d.as_millis() < SAME_COMPACTION_MS);
    if recent {
        return false;
    }
    bump_epoch(paths, session);
    true
}

pub fn bump_epoch(paths: &ProjectPaths, session: &str) -> i64 {
    let e = epoch(paths, session) + 1;
    let _ = std::fs::create_dir_all(paths.log_dir());
    let _ = std::fs::write(epoch_path(paths, session), e.to_string());
    // deliveries of the previous epoch no longer count as delivered
    append(
        paths,
        &Line {
            at: muninn_core::db::now_ms(),
            session: session.to_string(),
            arm: String::new(),
            ids: vec![],
            tokens: 0,
            reason: format!("epoch:{e}"),
        },
    );
    e
}

#[cfg(test)]
mod delivered_tests {
    use super::*;
    use muninn_core::db::Mode;

    fn store() -> (tempfile::TempDir, ProjectPaths, Db) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::from_root(tmp.path());
        std::fs::create_dir_all(paths.log_dir()).unwrap();
        let db = Db::open(&paths.db_path(), Mode::ReadWrite).unwrap();
        for i in 1..8 {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,'decision',?2,'is','o','b','user_said',3,'s',?2,1)",
                    rusqlite::params![i, format!("k{i}")],
                )
                .unwrap();
        }
        (tmp, paths, db)
    }

    fn log(paths: &ProjectPaths, session: &str, ids: Vec<i64>, reason: &str) {
        append(
            paths,
            &Line {
                at: 1,
                session: session.into(),
                arm: "literal".into(),
                ids,
                tokens: 10,
                reason: reason.into(),
            },
        );
    }

    fn sorted(s: std::collections::HashSet<i64>) -> Vec<i64> {
        let mut v: Vec<i64> = s.into_iter().collect();
        v.sort_unstable();
        v
    }

    /// R-01: a record the budget cut was never shown, so it is not delivered. Counting it as
    /// delivered withheld it for the rest of the session, pending or folded alike; so did a
    /// subagent's delivery, logged under its parent's session.
    #[test]
    fn only_what_the_session_was_shown_counts_as_delivered() {
        let (_tmp, paths, db) = store();
        log(&paths, "s", vec![1], "literal");
        log(&paths, "s", vec![2, 3], "gated:budget:2");
        log(&paths, "s", vec![4], "cue:event:session_start");
        log(&paths, "s", vec![5], "gated:budget:9");
        log(&paths, "s", vec![6], SUBAGENT_EVENT);
        assert_eq!(
            sorted(delivered_ids(&paths, &db, "s")),
            vec![1, 4],
            "pending"
        );
        fold_into_db(&paths, &db).unwrap();
        assert_eq!(
            sorted(delivered_ids(&paths, &db, "s")),
            vec![1, 4],
            "folded"
        );
        let rows: i64 = db
            .conn
            .query_row(
                "SELECT count(*) FROM fire_ledger WHERE record_id IN (2,3,5,6)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            rows, 4,
            "the ledger still counts what was cut, for the denominator"
        );
    }

    /// The SQL form of `shown_to_session` says the same thing, and the partial index is declared
    /// with its exact words, or SQLite would not use it.
    #[test]
    fn the_ledger_rows_that_count_are_the_lines_that_count() {
        let (_tmp, _paths, db) = store();
        for reason in [
            "literal",
            "literal:literal",
            "literal:gated:budget:54",
            "gated:intent",
            "literal:cue:event:session_start",
            "literal:cue:event:post_compact",
            "literal:cue:event:subagent",
            "literal:catalog",
            "catalog",
            "literal:cue:dir:src",
        ] {
            let sql: bool = db
                .conn
                .query_row(
                    &format!("SELECT {SHOWN_ROWS_SQL} FROM (SELECT ?1 AS reason)"),
                    [reason],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(sql, shown_to_session(reason), "{reason}");
        }
        assert!(muninn_core::db::SCHEMA_SQL.contains(SHOWN_ROWS_SQL));
        let plan: String = db
            .conn
            .query_row(
                &format!(
                    "EXPLAIN QUERY PLAN SELECT record_id FROM fire_ledger WHERE session_id = 's' \
                     AND compaction_epoch = 0 AND record_id IS NOT NULL AND {SHOWN_ROWS_SQL}"
                ),
                [],
                |r| r.get(3),
            )
            .unwrap();
        assert!(plan.contains("fire_session_seen"), "{plan}");
    }

    /// R-02: after a compaction the ledger restarts, and it stays restarted once a fold has
    /// consumed the `epoch:` line. What was delivered before the compaction can be offered
    /// again; what came after it is still delivered.
    #[test]
    fn a_compaction_restarts_the_ledger_for_good() {
        let (_tmp, paths, db) = store();
        log(&paths, "s", vec![1], "literal");
        log(&paths, "t", vec![3], "literal");
        fold_into_db(&paths, &db).unwrap();
        assert_eq!(bump_epoch(&paths, "s"), 1);
        log(&paths, "s", vec![2], "literal");
        assert_eq!(sorted(delivered_ids(&paths, &db, "s")), vec![2], "pending");
        fold_into_db(&paths, &db).unwrap();
        assert_eq!(sorted(delivered_ids(&paths, &db, "s")), vec![2], "folded");
        assert_eq!(
            sorted(delivered_ids(&paths, &db, "t")),
            vec![3],
            "another session's epoch did not move"
        );
        let epochs: Vec<(i64, i64)> = db
            .conn
            .prepare("SELECT record_id, compaction_epoch FROM fire_ledger WHERE record_id IS NOT NULL ORDER BY record_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(epochs, vec![(1, 0), (2, 1), (3, 0)]);
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use muninn_core::db::Mode;

    /// Health check 6 watches the session-start reinjection for going stale, by comparing the
    /// last two renders. It compared two `meta` keys nothing ever wrote, so it read `cold:
    /// fewer than two renders` from the day it was added and could never read anything else.
    #[test]
    fn folding_a_delivery_rolls_the_render_fingerprint() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::from_root(tmp.path());
        std::fs::create_dir_all(paths.log_dir()).unwrap();
        let db = Db::open(&paths.db_path(), Mode::ReadWrite).unwrap();
        for i in 1..4 {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,'invariant',?2,'must','o','b','user_said',3,'s',?2,1)",
                    rusqlite::params![i, format!("k{i}")],
                )
                .unwrap();
        }
        let deliver = |ids: Vec<i64>, at: i64| {
            append(
                &paths,
                &Line {
                    at,
                    session: "s".into(),
                    arm: "literal".into(),
                    ids,
                    tokens: 30,
                    reason: "cue:event:session_start".into(),
                },
            );
        };
        let get = |k: &str| db.meta_get(k).ok().flatten();

        deliver(vec![1, 2], 100);
        fold_into_db(&paths, &db).unwrap();
        let first = get("last_render_hash").expect("a render is recorded");
        assert!(get("prev_render_hash").is_none(), "one render is not two");

        // the same invariants again: the check needs both keys equal to call it frozen
        deliver(vec![2, 1], 200);
        fold_into_db(&paths, &db).unwrap();
        assert_eq!(get("prev_render_hash").as_deref(), Some(first.as_str()));
        assert_eq!(get("last_render_hash").as_deref(), Some(first.as_str()));

        // a different set rolls the old one back and clears the changed flag
        deliver(vec![3], 300);
        fold_into_db(&paths, &db).unwrap();
        assert_eq!(get("prev_render_hash").as_deref(), Some(first.as_str()));
        assert_ne!(get("last_render_hash").as_deref(), Some(first.as_str()));
        assert!(
            get("last_render_pool").is_some(),
            "a new render pins the pool it was drawn from"
        );
    }
}
