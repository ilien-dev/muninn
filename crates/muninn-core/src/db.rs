//! Database access. Two modes and nothing in between:
//! read hooks open `ReadOnly` (`query_only=1`, no writes are physically possible);
//! only the asynchronous write path opens `ReadWrite`.

use crate::error::{Error, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;

pub const SCHEMA_VERSION: i64 = 1;
pub const SCHEMA_SQL: &str = include_str!("schema.sql");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    ReadOnly,
    ReadWrite,
}

pub struct Db {
    pub conn: Connection,
    pub mode: Mode,
}

/// Bounded backoff for SQLITE_BUSY. The published operational log had 34 contention
/// failures in month one [Q3]; a hook must never spin past its own timeout.
const BUSY_BACKOFF_MS: [u64; 6] = [1, 2, 5, 10, 25, 50];

impl Db {
    /// Open an existing database. `ReadOnly` fails if the file does not exist.
    pub fn open(path: &Path, mode: Mode) -> Result<Db> {
        let flags = match mode {
            Mode::ReadOnly => OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            Mode::ReadWrite => {
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_CREATE
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX
            }
        };
        let conn = Connection::open_with_flags(path, flags)?;
        conn.busy_handler(Some(|attempt: i32| {
            let i = attempt as usize;
            if i >= BUSY_BACKOFF_MS.len() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(BUSY_BACKOFF_MS[i]));
            true
        }))?;
        match mode {
            Mode::ReadOnly => {
                conn.execute_batch("PRAGMA query_only=1; PRAGMA temp_store=MEMORY;")?;
            }
            Mode::ReadWrite => {
                conn.execute_batch(
                    "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; \
                     PRAGMA temp_store=MEMORY; PRAGMA foreign_keys=ON;",
                )?;
            }
        }
        let db = Db { conn, mode };
        if mode == Mode::ReadWrite {
            db.migrate()?;
        } else {
            db.check_schema_not_newer()?;
        }
        Ok(db)
    }

    /// Apply the schema and record its version. Idempotent.
    pub fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(SCHEMA_SQL)?;
        let found = self.schema_version()?;
        if found > SCHEMA_VERSION {
            return Err(Error::SchemaTooNew {
                found,
                supported: SCHEMA_VERSION,
            });
        }
        if found < SCHEMA_VERSION {
            // Future migrations go here, ordered by version.
            self.conn.execute(
                "INSERT INTO meta(key, value) VALUES ('schema_version', ?1) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [SCHEMA_VERSION.to_string()],
            )?;
        }
        Ok(())
    }

    fn check_schema_not_newer(&self) -> Result<()> {
        let found = self.schema_version()?;
        if found > SCHEMA_VERSION {
            return Err(Error::SchemaTooNew {
                found,
                supported: SCHEMA_VERSION,
            });
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        let has_meta: i64 = self.conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='meta'",
            [],
            |r| r.get(0),
        )?;
        if has_meta == 0 {
            return Ok(0);
        }
        let v: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .ok();
        Ok(v.and_then(|s| s.parse().ok()).unwrap_or(0))
    }

    /// `PRAGMA quick_check`: Ok(()) when the file is sound.
    pub fn quick_check(&self) -> Result<()> {
        let s: String = self
            .conn
            .query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if s == "ok" {
            Ok(())
        } else {
            Err(Error::Integrity(s))
        }
    }

    /// Run `quick_check` and record the outcome in `meta` so read hooks can report
    /// integrity in O(1). `quick_check` is O(file size) (9.6 ms at 5 MB measured),
    /// which is why it never runs in the read path. Skipped if run within `min_age_ms`.
    pub fn record_quick_check(&self, min_age_ms: i64) -> Result<bool> {
        let last: i64 = self
            .meta_get("quick_check_at")?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let now = now_ms();
        if now - last < min_age_ms && now >= last {
            return Ok(self.meta_get("quick_check_result")?.as_deref() == Some("ok"));
        }
        let res = self.quick_check();
        self.meta_set("quick_check_at", &now.to_string())?;
        self.meta_set(
            "quick_check_result",
            &res.as_ref()
                .map(|_| "ok".to_string())
                .unwrap_or_else(|e| e.to_string()),
        )?;
        Ok(res.is_ok())
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
            .ok())
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    pub fn count(&self, sql: &str) -> Result<i64> {
        Ok(self.conn.query_row(sql, [], |r| r.get(0))?)
    }
}

/// Epoch milliseconds. `MUNINN_FAKE_NOW_MS` overrides it for tests (clock faults).
pub fn now_ms() -> i64 {
    if let Ok(v) = std::env::var("MUNINN_FAKE_NOW_MS") {
        if let Ok(n) = v.parse() {
            return n;
        }
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_schema_and_version() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("m.db");
        let db = Db::open(&p, Mode::ReadWrite).unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        db.quick_check().unwrap();
        let ro = Db::open(&p, Mode::ReadOnly).unwrap();
        let err = ro
            .conn
            .execute("INSERT INTO meta(key,value) VALUES('x','y')", []);
        assert!(err.is_err(), "read-only handle must not write");
    }

    #[test]
    fn fts_follows_invalid_bit() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        db.conn.execute(
            "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
             VALUES('decision','retry.policy','is','exponential','we use exponential backoff','user_said',3,'s1','h1',1)",
            [],
        ).unwrap();
        let n = db
            .count("SELECT count(*) FROM record_fts WHERE record_fts MATCH 'exponential'")
            .unwrap();
        assert_eq!(n, 1);
        db.conn
            .execute(
                "UPDATE record SET invalid=1, invalid_reason='revoked' WHERE id=1",
                [],
            )
            .unwrap();
        let n = db
            .count("SELECT count(*) FROM record_fts WHERE record_fts MATCH 'exponential'")
            .unwrap();
        assert_eq!(n, 0, "invalidated rows leave the index");
        let kept = db.count("SELECT count(*) FROM record").unwrap();
        assert_eq!(kept, 1, "rows are retained, never deleted");
    }
}
