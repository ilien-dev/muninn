//! Database access. Two modes and nothing in between:
//! read hooks open `ReadOnly` (`query_only=1`, no writes are physically possible);
//! only the asynchronous write path opens `ReadWrite`.

use crate::error::{Error, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;

/// 2: the FTS index is stemmed (`porter`), so an index built by a v1 binary holds raw
/// tokens the v2 read path no longer asks for and has to be rebuilt once.
pub const SCHEMA_VERSION: i64 = 2;
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
/// Longest single wait step before the read hook's busy handler gives up.
const BACKOFF_CEILING_MS: u64 = 50;
const BUSY_BACKOFF_MS: [u64; 6] = [1, 2, 5, 10, 25, BACKOFF_CEILING_MS];

impl Db {
    /// Open an existing database. `ReadOnly` fails if the file does not exist.
    /// Writers wait up to 700 ms for another writer (a hook must stay under its own
    /// timeout); `open_with_busy` lets the detached write path wait longer.
    pub fn open(path: &Path, mode: Mode) -> Result<Db> {
        Self::open_with_busy(path, mode, 700)
    }

    /// A write transaction that takes the lock up front (BEGIN IMMEDIATE), so a
    /// concurrent writer produces a wait, never an immediate SQLITE_BUSY on upgrade.
    pub fn write_tx(&self) -> Result<rusqlite::Transaction<'_>> {
        Ok(rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?)
    }

    pub fn open_with_busy(path: &Path, mode: Mode, busy_ms: u64) -> Result<Db> {
        let flags = match mode {
            Mode::ReadOnly => OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            Mode::ReadWrite => {
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_CREATE
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX
            }
        };
        let conn = Connection::open_with_flags(path, flags)?;
        match mode {
            // a read hook never spins past its own timeout: ~93 ms in total, then it
            // gives up and stays silent
            Mode::ReadOnly => conn.busy_handler(Some(|attempt: i32| {
                let i = attempt as usize;
                if i >= BUSY_BACKOFF_MS.len() {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(BUSY_BACKOFF_MS[i]));
                true
            }))?,
            // the write path is asynchronous and may wait for another writer (maintain
            // re-indexing symbols, a concurrent Stop) instead of dropping an ingest
            Mode::ReadWrite => conn.busy_timeout(Duration::from_millis(busy_ms))?,
        }
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
    ///
    /// One IMMEDIATE transaction around the whole of it: the version read, the schema and the
    /// version write must see the same file. Without it a second writer (a detached
    /// `maintain` racing a hook) could read an old version, wait while a newer binary
    /// stamps its own, and then write this binary's version over it.
    pub fn migrate(&self) -> Result<()> {
        let tx = self.write_tx()?;
        self.migrate_locked()?;
        tx.commit()?;
        Ok(())
    }

    fn migrate_locked(&self) -> Result<()> {
        // The version has to be read before the schema is applied: every statement in it is
        // `IF NOT EXISTS`, so a table whose *definition* changed survives its own schema.
        let before = self.schema_version()?;
        if before > SCHEMA_VERSION {
            return Err(Error::SchemaTooNew {
                found: before,
                supported: SCHEMA_VERSION,
            });
        }
        if before > 0 && before < 2 {
            self.rebuild_fts_for_v2()?;
        }
        self.conn.execute_batch(SCHEMA_SQL)?;
        let found = self.schema_version()?;
        if found > SCHEMA_VERSION {
            return Err(Error::SchemaTooNew {
                found,
                supported: SCHEMA_VERSION,
            });
        }
        if found < SCHEMA_VERSION {
            if found < 2 {
                self.fill_fts_from_record()?;
            }
            // Future migrations go here, ordered by version.
            self.conn.execute(
                "INSERT INTO meta(key, value) VALUES ('schema_version', ?1) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [SCHEMA_VERSION.to_string()],
            )?;
        }
        Ok(())
    }

    /// v1 → v2: the tokenizer changed, so the index has to go. Dropping it is safe — the
    /// text lives in `record`, the index is external-content — and cheaper than reasoning
    /// about a half-stemmed index. `record_vocab` reads the index and goes with it.
    fn rebuild_fts_for_v2(&self) -> Result<()> {
        self.conn
            .execute_batch("DROP TABLE IF EXISTS record_vocab; DROP TABLE IF EXISTS record_fts;")?;
        Ok(())
    }

    /// Repopulate the index from the records that may be served. Only `invalid = 0` rows
    /// go in: an FTS5 `rebuild` would take every row, retired ones included, and the whole
    /// point of the triggers is that a retired record leaves the index.
    ///
    /// Only ever called with an empty index — either the table was just dropped for the
    /// tokenizer change, or the store is new — because there is no cheap way to ask an
    /// external-content FTS5 table how many rows its *index* holds: `count(*)` is answered
    /// from the content table, which is `record`, retired rows included. A guard written
    /// against that count skipped the fill on every migration and left the index empty.
    fn fill_fts_from_record(&self) -> Result<usize> {
        let rows = self.conn.execute(
            "INSERT INTO record_fts(rowid, subject, object, body) \
             SELECT id, subject, object, body FROM record WHERE invalid = 0",
            [],
        )?;
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES ('fts_rows', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [rows.to_string()],
        )?;
        Ok(rows)
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

    /// Whether F1's serving gate (`served_record`, `schema.sql`) exists. A store written by
    /// an older binary has not got it yet; `migrate()` adds it on the next write open, and
    /// until then every serving query fails to prepare — silence, never a retired record.
    pub fn has_served_view(&self) -> bool {
        self.conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='view' AND name='served_record'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n == 1)
            .unwrap_or(false)
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

    /// An index added after a store was created has to reach that store, or the query it was
    /// added for scans instead. `record_recent` and `record_heir` are both younger than any
    /// store in use: the catalogue's page and its `replaces #n` read them, and without them
    /// SessionStart measured 128 ms against a 10 ms contract. This project has shipped a
    /// migration that did not run once already.
    #[test]
    fn an_index_added_later_reaches_a_store_that_predates_it() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("m.db");
        {
            let db = Db::open(&p, Mode::ReadWrite).unwrap();
            for ix in ["record_recent", "record_heir"] {
                db.conn
                    .execute_batch(&format!("DROP INDEX IF EXISTS {ix};"))
                    .unwrap();
            }
            let n: i64 = db
                .count("SELECT count(*) FROM sqlite_master WHERE type='index' AND name IN ('record_recent','record_heir')")
                .unwrap();
            assert_eq!(n, 0, "the store now looks like one made before them");
        }
        // any write open applies the schema, which is where `CREATE INDEX IF NOT EXISTS` lives
        let db = Db::open(&p, Mode::ReadWrite).unwrap();
        let n: i64 = db
            .count("SELECT count(*) FROM sqlite_master WHERE type='index' AND name IN ('record_recent','record_heir')")
            .unwrap();
        assert_eq!(n, 2, "both are back after the next write open");
    }

    /// A store written by a v1 binary carries an unstemmed index. Opening it with this one
    /// must rebuild the index — with the *servable* rows only, never the retired ones — and
    /// leave every counter coherent. Existing installs take this path exactly once.
    #[test]
    fn a_v1_store_is_migrated_and_keeps_only_servable_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("m.db");
        {
            // a v1 store: the schema as it was, with the unstemmed tokenizer
            let db = Db::open(&p, Mode::ReadWrite).unwrap();
            db.conn
                .execute_batch("DROP TABLE record_vocab; DROP TABLE record_fts;")
                .unwrap();
            db.conn
                .execute_batch(
                    "CREATE VIRTUAL TABLE record_fts USING fts5(subject, object, body, \
                     content='record', content_rowid='id', tokenize='unicode61 remove_diacritics 2'); \
                     CREATE VIRTUAL TABLE record_vocab USING fts5vocab('record_fts','col');",
                )
                .unwrap();
            for (obj, invalid) in [("we are pooling connections", 0), ("we used PgBouncer", 1)] {
                db.conn
                    .execute(
                        "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,invalid,created_at) \
                         VALUES('decision',?1,'is',?2,?2,'user_said',3,'s',?1,?3,1)",
                        rusqlite::params![obj, obj, invalid],
                    )
                    .unwrap();
            }
            // the insert triggers have already indexed the servable row and skipped the
            // retired one, exactly as they do in the real write path
            db.conn
                .execute("UPDATE meta SET value='1' WHERE key='schema_version'", [])
                .unwrap();
        }
        let db = Db::open(&p, Mode::ReadWrite).unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        let matches = |q: &str| -> i64 {
            db.conn
                .query_row(
                    "SELECT count(*) FROM record_fts WHERE record_fts MATCH ?1",
                    [q],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(
            matches("\"pgbouncer\""),
            0,
            "the retired record does not come back into the index"
        );
        // stemmed now: the plural reaches the singular
        assert_eq!(matches("\"connection\""), 1, "the rebuilt index is stemmed");
        let rows: String = db
            .conn
            .query_row("SELECT value FROM meta WHERE key='fts_rows'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(rows, "1", "the row counter matches the index");
        db.quick_check().unwrap();
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
