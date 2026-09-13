//! Read-hook delivery log: which records were delivered (or why nothing was),
//! appended by read hooks and folded into `fire_ledger` by the write path.

use muninn_core::db::now_ms;
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

/// Ids delivered in this session (so the same record is not repeated).
pub fn delivered_ids(paths: &ProjectPaths, session: &str) -> std::collections::HashSet<i64> {
    let mut out = std::collections::HashSet::new();
    if let Ok(s) = std::fs::read_to_string(log_path(paths)) {
        for l in s.lines() {
            if let Ok(v) = serde_json::from_str::<Line>(l) {
                if v.session == session {
                    out.extend(v.ids);
                }
            }
        }
    }
    out
}

/// Fold into `fire_ledger` (one row per delivered record, one per silence).
pub fn fold_into_db(paths: &ProjectPaths, db: &Db) -> Result<usize> {
    let path = log_path(paths);
    if !path.exists() {
        return Ok(0);
    }
    let folding = path.with_extension("jsonl.folding");
    if !folding.exists() {
        std::fs::rename(&path, &folding).map_err(|e| muninn_core::Error::io(&path, e))?;
    }
    let text =
        std::fs::read_to_string(&folding).map_err(|e| muninn_core::Error::io(&folding, e))?;
    let epoch: i64 = db
        .meta_get("compaction_epoch")?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut n = 0;
    let tx = db.conn.unchecked_transaction()?;
    {
        let mut ins = tx.prepare("INSERT INTO fire_ledger(session_id, compaction_epoch, record_id, fired_at, tokens, reason) VALUES (?1,?2,?3,?4,?5,?6)")?;
        for l in text.lines() {
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
    if let Err(e) = tx.commit() {
        // give the lines back to the next fold instead of stranding them
        let _ = std::fs::rename(&folding, &path);
        return Err(e.into());
    }
    let _ = now_ms();
    std::fs::remove_file(&folding).map_err(|e| muninn_core::Error::io(&folding, e))?;
    Ok(n)
}
