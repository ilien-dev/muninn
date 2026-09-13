//! Heartbeat: one line *before* any work and one after, so a hook that starts
//! failing shows as failed rows and one that stops firing shows as absence [G3].
//!
//! Read hooks must not write the database, so heartbeats are appended to
//! `.muninn/log/heartbeat.jsonl` (O_APPEND, one line per event). The write path
//! folds them into the `heartbeat` table.

use crate::db::{now_ms, Db};
use crate::error::{Error, Result};
use crate::paths::ProjectPaths;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Line {
    Start {
        hook: String,
        session: String,
        at: i64,
        pid: u32,
    },
    Finish {
        hook: String,
        session: String,
        at: i64,
        pid: u32,
        ok: bool,
        ms: f64,
        err: Option<String>,
    },
}

pub struct Heartbeat {
    hook: String,
    session: String,
    started: Instant,
    started_at: i64,
    log: std::path::PathBuf,
    finished: bool,
}

fn append(path: &Path, line: &Line) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| Error::io(path, e))?;
    let mut s = serde_json::to_string(line)?;
    s.push('\n');
    // A single write under PIPE_BUF-ish size is atomic enough for a log line.
    f.write_all(s.as_bytes()).map_err(|e| Error::io(path, e))?;
    Ok(())
}

impl Heartbeat {
    /// Writes the start line immediately. Failure to write is reported, not fatal:
    /// a hook must still do its job when the log directory is unwritable.
    pub fn start(paths: &ProjectPaths, hook: &str, session: &str) -> (Heartbeat, Option<Error>) {
        let started_at = now_ms();
        let log = paths.heartbeat_log();
        let err = append(
            &log,
            &Line::Start {
                hook: hook.to_string(),
                session: session.to_string(),
                at: started_at,
                pid: std::process::id(),
            },
        )
        .err();
        (
            Heartbeat {
                hook: hook.to_string(),
                session: session.to_string(),
                started: Instant::now(),
                started_at,
                log,
                finished: false,
            },
            err,
        )
    }

    pub fn elapsed_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }

    pub fn finish(mut self, ok: bool, err: Option<String>) -> f64 {
        self.finished = true;
        let ms = self.elapsed_ms();
        let _ = append(
            &self.log,
            &Line::Finish {
                hook: self.hook.clone(),
                session: self.session.clone(),
                at: self.started_at,
                pid: std::process::id(),
                ok,
                ms,
                err,
            },
        );
        ms
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        if !self.finished {
            let _ = append(
                &self.log,
                &Line::Finish {
                    hook: self.hook.clone(),
                    session: self.session.clone(),
                    at: self.started_at,
                    pid: std::process::id(),
                    ok: false,
                    ms: self.elapsed_ms(),
                    err: Some("dropped without finish".into()),
                },
            );
        }
    }
}

/// Read the tail of the heartbeat log (bounded to `max_bytes`) as parsed lines.
pub fn read_tail(paths: &ProjectPaths, max_bytes: u64) -> Vec<Line> {
    use std::io::{Read, Seek, SeekFrom};
    let path = paths.heartbeat_log();
    let Ok(mut f) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    // Only regular files: a device (e.g. /dev/full) reads forever.
    let Ok(meta) = f.metadata() else {
        return Vec::new();
    };
    if !meta.is_file() {
        return Vec::new();
    }
    let len = meta.len();
    let start = len.saturating_sub(max_bytes);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if f.take(max_bytes + 1).read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.lines();
    if start > 0 {
        lines.next(); // drop the partial first line
    }
    lines
        .filter_map(|l| serde_json::from_str::<Line>(l).ok())
        .collect()
}

/// Fold the log into the `heartbeat` table (write path only). The file is renamed
/// first so concurrent appenders start a fresh file and nothing is lost.
pub fn fold_into_db(paths: &ProjectPaths, db: &Db) -> Result<usize> {
    let path = paths.heartbeat_log();
    if !path.exists() {
        return Ok(0);
    }
    let folding = path.with_extension("jsonl.folding");
    // If a previous fold died half-way, finish it first.
    if !folding.exists() {
        std::fs::rename(&path, &folding).map_err(|e| Error::io(&path, e))?;
    }
    let text = std::fs::read_to_string(&folding).map_err(|e| Error::io(&folding, e))?;
    let mut n = 0usize;
    let tx = db.conn.unchecked_transaction()?;
    {
        let mut ins = tx.prepare(
            "INSERT INTO heartbeat(hook, session_id, started_at, ok, error, ms) VALUES (?1,?2,?3,?4,?5,?6)",
        )?;
        let mut upd = tx.prepare(
            "UPDATE heartbeat SET ok=?1, error=?2, ms=?3 WHERE hook=?4 AND session_id=?5 AND started_at=?6 AND ok IS NULL",
        )?;
        for l in text.lines() {
            match serde_json::from_str::<Line>(l) {
                Ok(Line::Start {
                    hook, session, at, ..
                }) => {
                    ins.execute(rusqlite::params![
                        hook,
                        session,
                        at,
                        Option::<i64>::None,
                        Option::<String>::None,
                        Option::<f64>::None
                    ])?;
                    n += 1;
                }
                Ok(Line::Finish {
                    hook,
                    session,
                    at,
                    ok,
                    ms,
                    err,
                    ..
                }) => {
                    let changed =
                        upd.execute(rusqlite::params![ok as i64, err, ms, hook, session, at])?;
                    if changed == 0 {
                        ins.execute(rusqlite::params![hook, session, at, ok as i64, err, ms])?;
                    }
                    n += 1;
                }
                Err(_) => {} // a torn line is dropped, never fatal
            }
        }
    }
    tx.commit()?;
    std::fs::remove_file(&folding).map_err(|e| Error::io(&folding, e))?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Mode;

    #[test]
    fn start_then_finish_then_fold() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::from_root(tmp.path());
        let (hb, err) = Heartbeat::start(&paths, "SessionStart", "s1");
        assert!(err.is_none());
        hb.finish(true, None);
        let tail = read_tail(&paths, 1 << 20);
        assert_eq!(tail.len(), 2);
        let db = Db::open(&paths.db_path(), Mode::ReadWrite).unwrap();
        let n = fold_into_db(&paths, &db).unwrap();
        assert_eq!(n, 2);
        let rows = db
            .count("SELECT count(*) FROM heartbeat WHERE ok=1")
            .unwrap();
        assert_eq!(rows, 1);
        assert!(!paths.heartbeat_log().exists());
    }

    #[test]
    fn drop_without_finish_is_recorded_as_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::from_root(tmp.path());
        {
            let (_hb, _) = Heartbeat::start(&paths, "Stop", "s1");
        }
        let tail = read_tail(&paths, 1 << 20);
        assert!(matches!(tail.last(), Some(Line::Finish { ok: false, .. })));
    }
}
