//! Read-hook delivery log: which records were delivered (or why nothing was),
//! appended by read hooks and folded into `fire_ledger` by the write path.

use muninn_core::{Db, ProjectPaths, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;

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

pub fn append(paths: &ProjectPaths, line: &Line) {
    let _ = std::fs::create_dir_all(paths.log_dir());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(paths))
    {
        if let Ok(s) = serde_json::to_string(line) {
            let _ = writeln!(f, "{s}");
        }
    }
}

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
    // the prompt blocks served had already been named by the catalogue in that session.
    // `fire_session` indexes exactly this lookup.
    {
        let epoch: i64 = db
            .meta_get("compaction_epoch")
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if let Ok(mut st) = db.conn.prepare(
            "SELECT record_id FROM fire_ledger \
             WHERE session_id = ?1 AND compaction_epoch = ?2 AND record_id IS NOT NULL",
        ) {
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
                } else {
                    out.extend(v.ids);
                }
            }
        }
    }
    out
}

/// Fold into `fire_ledger` (one row per delivered record, one per silence).
pub fn fold_into_db(paths: &ProjectPaths, db: &Db) -> Result<usize> {
    let epoch: i64 = db
        .meta_get("compaction_epoch")?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let tx = db.write_tx()?;
    // from the watermark: the live file is never renamed under concurrent appenders
    let taken = muninn_core::logfold::take(&tx, &log_path(paths))?;
    let mut n = 0;
    {
        let mut ins = tx.prepare("INSERT INTO fire_ledger(session_id, compaction_epoch, record_id, fired_at, tokens, reason) VALUES (?1,?2,?3,?4,?5,?6)")?;
        for l in &taken.lines {
            let Ok(v) = serde_json::from_str::<Line>(l) else {
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
