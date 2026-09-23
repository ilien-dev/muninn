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
