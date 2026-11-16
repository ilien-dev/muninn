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
        /// Where the time went, in order: `[name, ms]` for each step the hook marked
        /// ([`mark`]). Counted from the start of the process's work, so the steps before the
        /// start line (reading the input, noting the session) are in it and not in `ms`.
        /// Kept in the log only, and written only: the `heartbeat` table has no column for
        /// it, and a reader of the log passes over it.
        #[serde(skip_deserializing, skip_serializing_if = "Vec::is_empty")]
        phases: Vec<(String, f64)>,
        /// Times the store was busy and the hook waited, and the milliseconds it slept.
        #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
        busy: Option<(u32, u64)>,
    },
}

/// When the last step ended, and the steps so far, each with its milliseconds.
type PhaseClock = (Instant, Vec<(String, f64)>);

thread_local! {
    /// The phase clock of the thread a hook runs on. A hook is one process doing one thing on
    /// one thread, so one clock serves it (R-19: a 45 ms mode in real session starts that the
    /// total alone could not place).
    static PHASES: std::cell::RefCell<Option<PhaseClock>> =
        const { std::cell::RefCell::new(None) };
}

/// Start the phase clock. Called first thing in a hook; without it `mark` does nothing.
pub fn clock_start() {
    PHASES.with(|c| *c.borrow_mut() = Some((Instant::now(), Vec::new())));
}

/// The step that just ended, by name: its time is what passed since the previous mark.
pub fn mark(name: &str) {
    PHASES.with(|c| {
        if let Some((last, steps)) = c.borrow_mut().as_mut() {
            let now = Instant::now();
            steps.push((
                name.to_string(),
                (now.duration_since(*last).as_secs_f64() * 1e6).round() / 1e3,
            ));
            *last = now;
        }
    });
}

fn take_phases() -> Vec<(String, f64)> {
    PHASES
        .with(|c| c.borrow_mut().take())
        .map(|(_, steps)| steps)
        .unwrap_or_default()
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
    crate::logfold::append_line(path, line)
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
                phases: take_phases(),
                busy: crate::db::busy_waits(),
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
                    phases: take_phases(),
                    busy: crate::db::busy_waits(),
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

/// How long a folded heartbeat row is kept.
pub const KEEP_MS: i64 = 30 * 86_400_000;
/// The `meta` key that counts the prompt hooks whose rows `prune` took out.
pub const PRUNED_PROMPTS: &str = "heartbeat_pruned_prompts";

/// Take the rows older than [`KEEP_MS`] out of the `heartbeat` table, and say how many went.
///
/// Every hook run left a row and nothing took one out: 147,979 rows and 19.0 MB on one store,
/// 30 % of its file (WP-13). The table has one reader, `muninn ledger`, which counts the
/// prompt hooks ever recorded, so the prompt rows that go are added to a count that stays
/// (`prompts_recorded`). The health gate reads the log's tail, never this table.
pub fn prune(db: &Db) -> Result<usize> {
    let cutoff = now_ms() - KEEP_MS;
    let tx = db.write_tx()?;
    let prompts: i64 = tx.query_row(
        "SELECT count(*) FROM heartbeat WHERE hook = 'UserPromptSubmit' AND started_at < ?1",
        [cutoff],
        |r| r.get(0),
    )?;
    if prompts > 0 {
        tx.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = CAST(CAST(value AS INTEGER) + ?2 AS TEXT)",
            rusqlite::params![PRUNED_PROMPTS, prompts],
        )?;
    }
    let n = tx.execute("DELETE FROM heartbeat WHERE started_at < ?1", [cutoff])?;
    tx.commit()?;
    Ok(n)
}

/// The prompt hooks ever recorded: the rows the table holds and the ones `prune` took out.
pub fn prompts_recorded(db: &Db) -> i64 {
    let held = db
        .count("SELECT count(*) FROM heartbeat WHERE hook = 'UserPromptSubmit'")
        .unwrap_or(0);
    let pruned = db
        .meta_get(PRUNED_PROMPTS)
        .ok()
        .flatten()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    held + pruned
}

/// Fold the log into the `heartbeat` table (write path only), from the watermark:
/// the live file is never renamed under concurrent appenders (see `logfold`).
pub fn fold_into_db(paths: &ProjectPaths, db: &Db) -> Result<usize> {
    let path = paths.heartbeat_log();
    let tx = db.write_tx()?;
    let taken = crate::logfold::take(&tx, &path)?;
    let mut n = 0usize;
    {
        let mut ins = tx.prepare(
            "INSERT INTO heartbeat(hook, session_id, started_at, ok, error, ms) VALUES (?1,?2,?3,?4,?5,?6)",
        )?;
        let mut upd = tx.prepare(
            "UPDATE heartbeat SET ok=?1, error=?2, ms=?3 WHERE hook=?4 AND session_id=?5 AND started_at=?6 AND ok IS NULL",
        )?;
        for l in &taken.lines {
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
    taken.finish();
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
        assert!(crate::logfold::pending(&db.conn, &paths.heartbeat_log()).is_empty());
    }

    /// R-19: the finish line says where the time went, and a line written before it did
    /// still reads.
    #[test]
    fn the_finish_line_carries_the_marked_phases_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::from_root(tmp.path());
        clock_start();
        mark("input");
        let (hb, _) = Heartbeat::start(&paths, "SessionStart", "s1");
        mark("open");
        mark("query");
        hb.finish(true, None);
        let text = std::fs::read_to_string(paths.heartbeat_log()).unwrap();
        let finish: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        let names: Vec<&str> = finish["phases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p[0].as_str().unwrap())
            .collect();
        assert_eq!(names, ["input", "open", "query"]);
        assert!(finish["phases"][0][1].as_f64().unwrap() >= 0.0);
        // a line of an earlier build has no phases and is the same finish
        let old = r#"{"ev":"finish","hook":"Stop","session":"s","at":1,"pid":2,"ok":true,"ms":3.5,"err":null}"#;
        assert!(matches!(
            serde_json::from_str::<Line>(old).unwrap(),
            Line::Finish { phases, busy: None, .. } if phases.is_empty()
        ));
        // Read back, a line's phases are passed over, not built: the health line parses 256 KB
        // of this log at every session start, and building ten names a line cost that hook
        // about a millisecond (12.69 ms against 11.35 on a bench store, same log, the build
        // before this one).
        assert!(matches!(
            serde_json::from_str::<Line>(text.lines().last().unwrap()).unwrap(),
            Line::Finish { phases, .. } if phases.is_empty()
        ));
        // and the fold takes a line with phases as it takes any other
        let db = Db::open(&paths.db_path(), Mode::ReadWrite).unwrap();
        assert_eq!(fold_into_db(&paths, &db).unwrap(), 2);
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
