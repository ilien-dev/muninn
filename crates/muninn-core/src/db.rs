//! Database access. Two modes and nothing in between:
//! read hooks open `ReadOnly` (`query_only=1`, no writes are physically possible);
//! only the asynchronous write path opens `ReadWrite`.

use crate::error::{Error, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;

/// 2: the FTS index is stemmed (`porter`), so an index built by a v1 binary holds raw
/// tokens the v2 read path no longer asks for and has to be rebuilt once.
/// 3: a retirement carries when it happened and what made it (`invalidated_at`,
/// `invalidated_via`), and the index is rebuilt once, because 1.2.0's restore pass put rows
/// back into it twice and raised the counter twice.
/// 4: who wrote a prompt, the journal of restatements, and what an agent's prompt retired.
/// 5: the parts usage-v4's section D read as unused are gone: the embedding sidecar's table
/// (no store held a vector), the symbol graph's tables, the directory and symbol cue kinds
/// (no delivery in any store's ledger came from one) and the `turn_context` table.
pub const SCHEMA_VERSION: i64 = 5;
pub const SCHEMA_SQL: &str = include_str!("schema.sql");

/// The `meta` key the v5 migration leaves: the file holds the pages of the tables it dropped.
const COMPACT_PENDING: &str = "compact_pending";

/// The cause (`invalidated_via`) of a row that an agent's prompt retired and that was not given
/// back when the prompt was found out, because something was said or done to the store since.
/// What retired it is in `agent_retired`, not on the row. Written by `muninn-capture`
/// (`agent/undo.rs`), counted by `doctor` and `status`.
pub const AGENT_UNRESTORED: &str = "agent_unrestored";

/// Leaves a row in `store_event`: a pass outside ingest that retires or revokes records ran,
/// or could have acted. Called inside that pass's own write. The undo of an agent's prompt
/// reads it (`muninn-capture`, `agent/undo.rs`): such a pass only looks at active records, so
/// it never met a record the prompt had retired, and that record is not given back.
///
/// A pass that judges records and retires some does it in three steps, and the two helpers
/// below are the first and the last:
///
/// 1. [`open_pass`]: one short write transaction that leaves an event and reads the records
///    the pass will judge. A record an agent's prompt retired before this is not in the read,
///    and the event keeps it from being given back; one retired after it is in the read.
/// 2. The work, with no transaction open: git, hashing.
/// 3. [`close_pass`]: one short write transaction that retires what is still live, and leaves
///    a second event whenever the pass decided to retire a record of its read, still live or
///    not. That event is what keeps back a record a prompt retired during the work: the pass
///    would have retired it, and could not.
pub fn note_event(conn: &Connection, what: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO store_event(what, after_record, after_journal) VALUES (?1, (SELECT coalesce(max(id), 0) FROM record), (SELECT coalesce(max(id), 0) FROM restatement))",
        [what],
    )?;
    Ok(())
}

/// The first step of a pass: the event, when the pass leaves one on opening, and the read of
/// what it will judge, in one write transaction. A pass that reads every active record of its
/// kind again on every run (the anchor validator) needs no event here: a record given back
/// later is in its next read.
pub fn open_pass<T>(
    db: &Db,
    what: &str,
    event: bool,
    read: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    let tx = db.write_tx()?;
    if event {
        note_event(&tx, what)?;
    }
    let seen = read(&tx)?;
    tx.commit()?;
    Ok(seen)
}

/// The last step of a pass: `apply` retires what is still live and says whether the pass
/// decided to retire any record of its read, live or not; if it did, the event is left in the
/// same write transaction.
pub fn close_pass<T>(
    db: &Db,
    what: &str,
    apply: impl FnOnce(&Connection) -> Result<(T, bool)>,
) -> Result<T> {
    let tx = db.write_tx()?;
    let (done, decided) = apply(&tx)?;
    if decided {
        note_event(&tx, what)?;
    }
    tx.commit()?;
    Ok(done)
}

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

/// Busy waits of this process's read connections: how many, and the milliseconds slept.
static BUSY_WAITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static BUSY_SLEPT_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `(waits, ms slept)` when a read hook found the store busy, for its heartbeat line; `None`
/// when it never did.
pub fn busy_waits() -> Option<(u32, u64)> {
    use std::sync::atomic::Ordering::Relaxed;
    let n = BUSY_WAITS.load(Relaxed);
    (n > 0).then(|| (n, BUSY_SLEPT_MS.load(Relaxed)))
}

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
        let mut conn = Connection::open_with_flags(path, flags)?;
        match mode {
            // a read hook never spins past its own timeout: ~93 ms in total, then it
            // gives up and stays silent
            Mode::ReadOnly => conn.busy_handler(Some(|attempt: i32| {
                let i = attempt as usize;
                if i >= BUSY_BACKOFF_MS.len() {
                    return false;
                }
                use std::sync::atomic::Ordering::Relaxed;
                BUSY_WAITS.fetch_add(1, Relaxed);
                BUSY_SLEPT_MS.fetch_add(BUSY_BACKOFF_MS[i], Relaxed);
                std::thread::sleep(Duration::from_millis(BUSY_BACKOFF_MS[i]));
                true
            }))?,
            // the write path is asynchronous and may wait for another writer (maintain,
            // a concurrent Stop) instead of dropping an ingest
            Mode::ReadWrite => conn.busy_timeout(Duration::from_millis(busy_ms))?,
        }
        match mode {
            Mode::ReadOnly => {
                conn.execute_batch("PRAGMA query_only=1; PRAGMA temp_store=MEMORY;")?;
            }
            Mode::ReadWrite => {
                set_wal(&conn, busy_ms)?;
                conn.execute_batch(
                    "PRAGMA synchronous=NORMAL; PRAGMA temp_store=MEMORY; \
                     PRAGMA foreign_keys=ON;",
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
        if before > 0 && before < 3 {
            // before the schema: its triggers name these columns
            self.add_retirement_columns()?;
        }
        if before > 0 && before < 5 {
            // before the schema: it creates the indexes of the `cue` table this rebuilds
            self.drop_removed_parts()?;
        }
        self.conn.execute_batch(SCHEMA_SQL)?;
        if before > 0 {
            // a v3 `record_turn`, and one an early v4 build wrote, predate the columns that let a
            // restatement be undone, and an early v4 journal the one that dates a restatement;
            // adding them is idempotent
            self.add_v4_columns()?;
        }
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
            } else if found < 3 {
                self.rebuild_fts()?;
            }
            // Future migrations go here, ordered by version.
            if before > 0 {
                // the Markdown mirror of a store an earlier build made is in that build's
                // shape, and is written again only when asked
                self.conn.execute(
                    "INSERT INTO meta(key, value) VALUES ('records_changed_since_project', '1') \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    [],
                )?;
            }
            self.conn.execute(
                "INSERT INTO meta(key, value) VALUES ('schema_version', ?1) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [SCHEMA_VERSION.to_string()],
            )?;
        }
        // The build that last wrote the store. Only `init` stamped it, so every store in use
        // still named the build that created it (1.0.0, 1.0.3 or 1.0.5 on eleven of them).
        if self.meta_get("muninn_version")?.as_deref() != Some(crate::VERSION) {
            self.meta_set("muninn_version", crate::VERSION)?;
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

    /// The columns v4 added to tables an earlier build may have created without them, added
    /// only where they are not there yet.
    fn add_v4_columns(&self) -> Result<()> {
        for (table, col, ty) in [
            ("record_turn", "session_id", "TEXT"),
            ("record_turn", "created_at", "INTEGER"),
            ("restatement", "after_record", "INTEGER"),
        ] {
            let has: i64 = self.conn.query_row(
                "SELECT count(*) FROM pragma_table_info(?1) WHERE name = ?2",
                [table, col],
                |r| r.get(0),
            )?;
            if has == 0 {
                self.conn
                    .execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {col} {ty};"))?;
            }
        }
        Ok(())
    }

    /// → v5: what the schema no longer has is taken out of a store made before. No record is
    /// touched.
    ///
    /// - The embedding sidecar: its table of vectors and the two `meta` keys it wrote.
    /// - The symbol graph: its three tables and the `meta` key of its last run.
    /// - The turn context: a table nothing wrote.
    /// - Directory and symbol cues. SQLite cannot change a CHECK, so `cue` is made again with
    ///   the kinds that are left, and the schema then creates its indexes again. A group is a
    ///   conjunction: one that held a directory or a symbol cue goes whole, since what is left
    ///   of it would fire on fewer conditions than were set. That takes a dead end's
    ///   `pre_edit` event with its directory.
    ///
    /// The pages this frees stay in the file, so it leaves a note for `compact_if_pending`.
    fn drop_removed_parts(&self) -> Result<()> {
        self.conn.execute_batch(
            "DROP TABLE IF EXISTS record_vec; \
             DROP TABLE IF EXISTS symbol; \
             DROP TABLE IF EXISTS symbol_ref; \
             DROP TABLE IF EXISTS symbol_file; \
             DROP TABLE IF EXISTS turn_context; \
             DELETE FROM meta WHERE key IN ('embed_model', 'embed_at_ms', 'symbols_indexed_ms');",
        )?;
        self.meta_set(COMPACT_PENDING, "1")?;
        let has_cue: i64 = self.conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'cue'",
            [],
            |r| r.get(0),
        )?;
        if has_cue == 1 {
            // Kept aside and put back, not renamed into place: a rename makes SQLite read every
            // trigger again, and on a v1 store the index they write to is already dropped.
            self.conn.execute_batch(
                "CREATE TEMP TABLE cue_kept AS \
                     SELECT c.record_id, c.kind, c.key, c.grp FROM cue c \
                     WHERE NOT EXISTS (SELECT 1 FROM cue d \
                                       WHERE d.record_id = c.record_id AND d.grp = c.grp \
                                         AND d.kind IN ('dir', 'symbol')) \
                     ORDER BY c.rowid; \
                 DROP TABLE cue; \
                 CREATE TABLE cue ( \
                     record_id   INTEGER NOT NULL REFERENCES record(id), \
                     kind        TEXT    NOT NULL CHECK (kind IN ('event','after','cooldown','keyword','glob')), \
                     key         TEXT    NOT NULL, \
                     grp         INTEGER NOT NULL DEFAULT 0 \
                 ); \
                 INSERT INTO cue(record_id, kind, key, grp) \
                     SELECT record_id, kind, key, grp FROM cue_kept; \
                 DROP TABLE cue_kept;",
            )?;
        }
        Ok(())
    }

    /// Leave the note `compact_if_pending` reads: many rows were just deleted, and their pages
    /// stay in the file until it is rewritten.
    pub fn ask_for_compaction(&self) -> Result<()> {
        self.meta_set(COMPACT_PENDING, "1")
    }

    /// Give back to the file system the pages the v5 migration freed, once. Returns whether it
    /// did. A dropped table leaves its pages in the file, and the symbol graph was most of
    /// one: copies of three stores in use went from 69.3 MB to 33.8, from 24.7 to 4.9 and from
    /// 11.5 to 0.3. Not for a hook: it rewrites the file under the write lock (269 ms for the
    /// largest of those). If another connection keeps it from running, the note stays and the
    /// next run tries again.
    pub fn compact_if_pending(&self) -> Result<bool> {
        if self.meta_get(COMPACT_PENDING)?.is_none() {
            return Ok(false);
        }
        self.conn.execute_batch("VACUUM;")?;
        self.conn
            .execute("DELETE FROM meta WHERE key = ?1", [COMPACT_PENDING])?;
        // the file itself shrinks when the log is written back into it; a reader holding the
        // log open only delays that
        let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        Ok(true)
    }

    /// Fold the write-ahead log into the store and empty it. The last thing a writer does.
    ///
    /// SQLite does this when the last connection closes, and only then. A writer that closes
    /// while a read hook is connected leaves its log behind, and every read hook after it,
    /// each the only connection for a few milliseconds, replays that log before its first
    /// query: 2.69 ms of open against 0.60 ms with a 4.2 MB log beside a 20,000-record store.
    /// Folded here, the log is empty whoever closes last. A reader in the middle of a query
    /// is waited for 100 ms at most; past that the log stays for the next writer.
    pub fn fold_log(&self) {
        if self.mode != Mode::ReadWrite {
            return;
        }
        let _ = self.conn.busy_timeout(Duration::from_millis(100));
        let _ = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }

    /// v2 → v3: two columns on `record`, added only where they are not there yet, so a store
    /// that a crashed migration left half-done is finished rather than refused.
    fn add_retirement_columns(&self) -> Result<()> {
        let has = |col: &str| -> Result<bool> {
            Ok(self.conn.query_row(
                "SELECT count(*) FROM pragma_table_info('record') WHERE name = ?1",
                [col],
                |r| r.get::<_, i64>(0),
            )? > 0)
        };
        for (col, ty) in [("invalidated_at", "INTEGER"), ("invalidated_via", "TEXT")] {
            if !has(col)? {
                self.conn
                    .execute_batch(&format!("ALTER TABLE record ADD COLUMN {col} {ty};"))?;
            }
        }
        Ok(())
    }

    /// Empty the index and refill it from the servable rows, resetting the counter to match.
    /// For an index whose counter and contents have drifted from `record` — 1.2.0's restore
    /// pass indexed each restored row twice — and the one thing `doctor` sends a reader to
    /// `maintain` for when the two disagree.
    pub fn rebuild_fts(&self) -> Result<usize> {
        self.conn
            .execute_batch("INSERT INTO record_fts(record_fts) VALUES('delete-all');")?;
        self.fill_fts_from_record()
    }

    /// Rebuild the index when its counter and the active rows disagree, which is what
    /// `doctor`'s fts check compares; `maintain` runs this. Returns the rows indexed when it
    /// had to, `None` when the two already agree.
    pub fn repair_fts_if_drifted(&self) -> Result<Option<usize>> {
        let tx = self.write_tx()?;
        let counter: i64 = self
            .meta_get("fts_rows")?
            .and_then(|s| s.parse().ok())
            .unwrap_or(-1);
        let active = self.count("SELECT count(*) FROM record WHERE invalid=0")?;
        if counter == active {
            return Ok(None);
        }
        let n = self.rebuild_fts()?;
        tx.commit()?;
        Ok(Some(n))
    }

    /// Bring back to milliseconds the records dated past tomorrow because their unit was
    /// wrong (R-17). One that no unit makes sane is left as it is, for `doctor` to report.
    /// Returns how many were changed.
    pub fn repair_future_dates(&self) -> Result<usize> {
        let now = now_ms();
        let tx = self.write_tx()?;
        let rows: Vec<(i64, i64)> = self
            .conn
            .prepare("SELECT id, created_at FROM record WHERE created_at > ?1")?
            .query_map([now + FUTURE_SLACK_MS], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        let mut n = 0;
        for (id, at) in rows {
            if let Some(fixed) = sane_created_at(at, now) {
                n += self.conn.execute(
                    "UPDATE record SET created_at = ?2 WHERE id = ?1",
                    [id, fixed],
                )?;
            }
        }
        tx.commit()?;
        Ok(n)
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

/// Put the file in WAL mode, waiting up to `busy_ms` for another writer.
///
/// A file that is not in WAL mode yet (new, or an empty file where the store should be)
/// needs an exclusive lock for the switch, and SQLite takes that lock without consulting
/// the busy handler: with a second writer in the file the pragma fails at once with
/// SQLITE_BUSY, whatever `busy_timeout` says. A hook racing the detached `maintain` on
/// such a file gave up there and left the store unmigrated. The wait is done here instead.
fn set_wal(conn: &Connection, busy_ms: u64) -> Result<()> {
    let t = std::time::Instant::now();
    let mut attempt = 0;
    loop {
        match conn.execute_batch("PRAGMA journal_mode=WAL;") {
            Ok(()) => return Ok(()),
            Err(e)
                if e.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)
                    && t.elapsed() < Duration::from_millis(busy_ms) =>
            {
                let step = BUSY_BACKOFF_MS[attempt.min(BUSY_BACKOFF_MS.len() - 1)];
                std::thread::sleep(Duration::from_millis(step));
                attempt += 1;
            }
            Err(e) => return Err(e.into()),
        }
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

/// The earliest `created_at` a record can sensibly carry: 2000-01-01, epoch ms.
const EARLIEST_MS: i64 = 946_684_800_000;
/// How far past the clock a `created_at` may sit: one day, for clocks that disagree.
const FUTURE_SLACK_MS: i64 = 86_400_000;

/// A record's `created_at`, made sane or refused (R-17).
///
/// Every newest-first path trusts this column — the catalogue, the session-start block,
/// `show` — and nothing guarded it: three records on one store carried nanoseconds, and made
/// active on a copy they took three of six session-start slots, dated year 56807327. A value
/// too large to be milliseconds is read as micro- or nanoseconds and brought back to
/// milliseconds; one that is still before 2000 or more than a day past `now` is refused.
pub fn sane_created_at(ms: i64, now: i64) -> Option<i64> {
    let mut v = ms;
    while v > now + FUTURE_SLACK_MS && v / 1000 >= EARLIEST_MS {
        v /= 1000;
    }
    (EARLIEST_MS..=now + FUTURE_SLACK_MS)
        .contains(&v)
        .then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R-17. Milliseconds pass; nanoseconds and microseconds of the same instant come back as
    /// milliseconds; a date before 2000 or past tomorrow is refused.
    #[test]
    fn a_created_at_in_the_wrong_unit_or_the_future_is_caught() {
        let now = 1_794_000_000_000; // 2026-11
        let ms = 1_793_000_000_000;
        assert_eq!(sane_created_at(ms, now), Some(ms));
        assert_eq!(sane_created_at(ms * 1_000_000, now), Some(ms));
        assert_eq!(sane_created_at(ms * 1_000, now), Some(ms));
        assert_eq!(sane_created_at(now + 3_600_000, now), Some(now + 3_600_000));
        assert_eq!(sane_created_at(now + 30 * 86_400_000, now), None);
        assert_eq!(sane_created_at(1, now), None);
        assert_eq!(sane_created_at(-5, now), None);
    }

    /// R-17. An active record already dated past tomorrow is brought back to its own date
    /// when its unit was wrong; one that cannot be read in any unit is left for `doctor`.
    #[test]
    fn records_dated_in_the_wrong_unit_are_brought_back() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let ms = 1_793_000_000_000i64;
        for (id, at) in [(1, ms * 1_000_000), (2, i64::MAX / 2), (3, ms)] {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,'invariant',?1,'must',?1,?1,'user_said',3,'s',?1,?2)",
                    rusqlite::params![id, at],
                )
                .unwrap();
        }
        assert_eq!(db.repair_future_dates().unwrap(), 1);
        let at = |id: i64| -> i64 {
            db.conn
                .query_row("SELECT created_at FROM record WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        assert_eq!((at(1), at(2), at(3)), (ms, i64::MAX / 2, ms));
    }

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

    /// An empty file where the store should be, and several writers opening it at once: a
    /// hook and the detached `maintain` its SessionStart spawned. Every one of them has to
    /// come back with a migrated store.
    #[test]
    fn writers_racing_to_open_an_empty_store_all_migrate_it() {
        for _ in 0..50 {
            let tmp = tempfile::tempdir().unwrap();
            let p = tmp.path().join("m.db");
            std::fs::write(&p, b"").unwrap();
            let gate = std::sync::Arc::new(std::sync::Barrier::new(4));
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let (p, gate) = (p.clone(), gate.clone());
                    std::thread::spawn(move || {
                        gate.wait();
                        Db::open(&p, Mode::ReadWrite).map(|db| db.schema_version())
                    })
                })
                .collect();
            for h in handles {
                match h.join().unwrap() {
                    Ok(v) => assert_eq!(v.unwrap(), SCHEMA_VERSION),
                    Err(e) => panic!("a racing writer failed to open the store: {e}"),
                }
            }
        }
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

    /// A store written by 1.2.0, schema v2, exactly as that release created it, with the
    /// index drift its one-time restore pass left behind: the row went back into the index
    /// twice and the counter was raised twice. Opening it with this binary adds the columns
    /// that date and explain a retirement, rebuilds the index from the servable rows once, and
    /// every later retirement carries a time and a cause.
    /// An intermediate build of v4 wrote `record_turn` without the two columns that undo a
    /// restatement. Its stores say version 4 and still have to gain them.
    #[test]
    fn an_early_v4_store_gains_the_record_turn_columns() {
        // the two shapes earlier builds of v4 wrote: the prompt tables alone, then with the
        // record_turn columns; neither had the restatement journal
        const PROMPT_TABLES: &str =
            "CREATE TABLE agent_task (hash TEXT NOT NULL, session_id TEXT NOT NULL, \
             PRIMARY KEY (hash, session_id)); \
             CREATE TABLE turn_prompt (hash TEXT NOT NULL, session_id TEXT NOT NULL, \
             transcript_ref TEXT NOT NULL, PRIMARY KEY (hash, transcript_ref));";
        for with_columns in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let p = tmp.path().join("m.db");
            {
                let c = Connection::open(&p).unwrap();
                c.execute_batch(include_str!("testdata/schema_v3.sql"))
                    .unwrap();
                c.execute_batch(PROMPT_TABLES).unwrap();
                if with_columns {
                    c.execute_batch(
                        "ALTER TABLE record_turn ADD COLUMN session_id TEXT; \
                         ALTER TABLE record_turn ADD COLUMN created_at INTEGER;",
                    )
                    .unwrap();
                }
                c.execute(
                    "INSERT INTO meta(key, value) VALUES ('schema_version', '4') \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    [],
                )
                .unwrap();
            }
            let db = Db::open(&p, Mode::ReadWrite).unwrap();
            assert_eq!(
                db.count(
                    "SELECT count(*) FROM pragma_table_info('record_turn') \
                     WHERE name IN ('session_id', 'created_at')"
                )
                .unwrap(),
                2,
                "columns, early v4 with_columns={with_columns}"
            );
            for table in [
                "restatement",
                "restatement_retired",
                "agent_retired",
                "store_event",
            ] {
                assert_eq!(
                    db.count(&format!(
                        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='{table}'"
                    ))
                    .unwrap(),
                    1,
                    "{table}, early v4 with_columns={with_columns}"
                );
            }
            assert_eq!(
                db.count("SELECT count(*) FROM sqlite_master WHERE type='index' AND name='restatement_record'")
                    .unwrap(),
                1,
                "the journal's record index, early v4 with_columns={with_columns}"
            );
            assert_eq!(
                db.count(
                    "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='record_active_ref'"
                )
                .unwrap(),
                1,
                "the index a found-out turn's rows are read by"
            );
            assert_eq!(
                db.count("SELECT count(*) FROM pragma_table_info('restatement') WHERE name = 'after_record'")
                    .unwrap(),
                1,
                "the journal dates a restatement against the records, early v4 with_columns={with_columns}"
            );
        }
    }

    /// The last v4 shape before a journal row could be pending: the journal tables as that build
    /// created them, with rows in it. Opening it adds the index the pending rows are found by
    /// and the column a give-back is dated by (`after_record`), and leaves the rows as they are;
    /// `undone` keeps its column and gains a third value.
    #[test]
    fn a_v4_store_with_a_journal_gains_its_index_and_the_column_a_give_back_is_dated_by() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("m.db");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(include_str!("testdata/schema_v3.sql"))
                .unwrap();
            c.execute_batch(
                "CREATE TABLE agent_task (hash TEXT NOT NULL, session_id TEXT NOT NULL, \
                 PRIMARY KEY (hash, session_id)); \
                 CREATE TABLE turn_prompt (hash TEXT NOT NULL, session_id TEXT NOT NULL, \
                 transcript_ref TEXT NOT NULL, PRIMARY KEY (hash, transcript_ref)); \
                 CREATE INDEX turn_prompt_hash ON turn_prompt(hash); \
                 ALTER TABLE record_turn ADD COLUMN session_id TEXT; \
                 ALTER TABLE record_turn ADD COLUMN created_at INTEGER; \
                 CREATE TABLE restatement (id INTEGER PRIMARY KEY, record_id INTEGER NOT NULL, \
                 turn_ref TEXT NOT NULL, new_session TEXT, new_created INTEGER NOT NULL, \
                 prior_invalid INTEGER NOT NULL, prior_reason TEXT, prior_by INTEGER, \
                 prior_via TEXT, prior_at INTEGER, prior_ref TEXT, prior_session TEXT, \
                 prior_created INTEGER NOT NULL, added_turn INTEGER NOT NULL DEFAULT 0, \
                 undone INTEGER NOT NULL DEFAULT 0); \
                 CREATE INDEX restatement_turn ON restatement(turn_ref); \
                 CREATE TABLE restatement_retired (\
                 restatement_id INTEGER NOT NULL REFERENCES restatement(id), \
                 record_id INTEGER NOT NULL, PRIMARY KEY (restatement_id, record_id)); \
                 INSERT INTO restatement(record_id, turn_ref, new_created, prior_invalid, \
                 prior_created, undone) VALUES (1, 'a:1', 2, 0, 1, 0), (1, 'b:1', 3, 0, 2, 1); \
                 INSERT INTO restatement_retired(restatement_id, record_id) VALUES (1, 7);",
            )
            .unwrap();
            c.execute(
                "INSERT INTO meta(key, value) VALUES ('schema_version', '4') \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )
            .unwrap();
        }
        for _ in 0..2 {
            let db = Db::open(&p, Mode::ReadWrite).unwrap();
            assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
            assert_eq!(
                db.count("SELECT count(*) FROM sqlite_master WHERE type='index' AND name='restatement_record'")
                    .unwrap(),
                1
            );
            assert_eq!(
                db.count("SELECT count(*) FROM pragma_table_info('restatement') WHERE name = 'after_record'")
                    .unwrap(),
                1,
                "the journal dates a restatement against the records, a v4 journal"
            );
            assert_eq!(
                db.count(
                    "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='record_active_ref'"
                )
                .unwrap(),
                1,
                "the index a found-out turn's rows are read by, a v4 journal"
            );
            assert_eq!(
                db.count("SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('agent_retired', 'store_event')")
                    .unwrap(),
                2,
                "the tables of marks and of events, a v4 journal"
            );
            assert_eq!(
                db.count("SELECT count(*) FROM restatement WHERE after_record IS NULL")
                    .unwrap(),
                2,
                "rows written before the column have none"
            );
            assert_eq!(
                db.count("SELECT count(*) FROM restatement WHERE undone = 0")
                    .unwrap(),
                1
            );
            assert_eq!(
                db.count("SELECT count(*) FROM restatement WHERE undone = 1")
                    .unwrap(),
                1
            );
            assert_eq!(
                db.count("SELECT count(*) FROM restatement_retired")
                    .unwrap(),
                1
            );
            // a row can be marked pending and found by its record
            db.conn
                .execute("UPDATE restatement SET undone = 2 WHERE id = 1", [])
                .unwrap();
            assert_eq!(
                db.count("SELECT count(*) FROM restatement WHERE undone = 2 AND record_id = 1")
                    .unwrap(),
                1
            );
            db.conn
                .execute("UPDATE restatement SET undone = 0 WHERE id = 1", [])
                .unwrap();
        }
    }

    /// Every shape a store older than v5 can have: 1.2.0's schema (v2), v3, the three shapes
    /// earlier builds of v4 wrote (the prompt tables alone, then with the `record_turn`
    /// columns, then with the restatement journal before it could hold a pending row), and v4
    /// as its last build left it. Label, the version it says, its DDL.
    fn older_shapes() -> Vec<(&'static str, &'static str, String)> {
        const PROMPT_TABLES: &str =
            "CREATE TABLE agent_task (hash TEXT NOT NULL, session_id TEXT NOT NULL, \
             PRIMARY KEY (hash, session_id)); \
             CREATE TABLE turn_prompt (hash TEXT NOT NULL, session_id TEXT NOT NULL, \
             transcript_ref TEXT NOT NULL, PRIMARY KEY (hash, transcript_ref));";
        const TURN_COLUMNS: &str = "ALTER TABLE record_turn ADD COLUMN session_id TEXT; \
             ALTER TABLE record_turn ADD COLUMN created_at INTEGER;";
        const JOURNAL: &str =
            "CREATE TABLE restatement (id INTEGER PRIMARY KEY, record_id INTEGER NOT NULL, \
             turn_ref TEXT NOT NULL, new_session TEXT, new_created INTEGER NOT NULL, \
             prior_invalid INTEGER NOT NULL, prior_reason TEXT, prior_by INTEGER, \
             prior_via TEXT, prior_at INTEGER, prior_ref TEXT, prior_session TEXT, \
             prior_created INTEGER NOT NULL, added_turn INTEGER NOT NULL DEFAULT 0, \
             undone INTEGER NOT NULL DEFAULT 0); \
             CREATE TABLE restatement_retired (\
             restatement_id INTEGER NOT NULL REFERENCES restatement(id), \
             record_id INTEGER NOT NULL, PRIMARY KEY (restatement_id, record_id));";
        let v3 = include_str!("testdata/schema_v3.sql");
        vec![
            (
                "2",
                "2",
                include_str!("testdata/schema_1_2_0.sql").to_string(),
            ),
            ("3", "3", v3.to_string()),
            ("4, prompt tables", "4", format!("{v3}{PROMPT_TABLES}")),
            (
                "4, turn columns",
                "4",
                format!("{v3}{PROMPT_TABLES}{TURN_COLUMNS}"),
            ),
            (
                "4, journal",
                "4",
                format!("{v3}{PROMPT_TABLES}{TURN_COLUMNS}{JOURNAL}"),
            ),
            ("4", "4", include_str!("testdata/schema_v4.sql").to_string()),
        ]
    }

    /// WP-13. `meta.muninn_version` named the build that created the store for ever. A write
    /// open by another build says which build wrote it last.
    #[test]
    fn a_write_open_stamps_the_build_that_wrote_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("m.db");
        let db = Db::open(&p, Mode::ReadWrite).unwrap();
        db.meta_set("muninn_version", "1.0.0").unwrap();
        drop(db);
        let db = Db::open(&p, Mode::ReadOnly).unwrap();
        assert_eq!(
            db.meta_get("muninn_version").unwrap(),
            Some("1.0.0".into()),
            "a read open writes nothing"
        );
        drop(db);
        let db = Db::open(&p, Mode::ReadWrite).unwrap();
        assert_eq!(
            db.meta_get("muninn_version").unwrap(),
            Some(crate::VERSION.into())
        );
    }

    /// WP-11. The Markdown mirror is written again only when records changed, so a store
    /// made by an earlier build would keep files in that build's shape for as long as nothing
    /// was said. The open that moves its schema asks for the mirror once; a later open does
    /// not ask again.
    #[test]
    fn a_store_whose_schema_moved_gets_its_mirror_written_again_once() {
        for (version, stamp, schema) in older_shapes() {
            let tmp = tempfile::tempdir().unwrap();
            let p = tmp.path().join("m.db");
            {
                let c = Connection::open(&p).unwrap();
                c.execute_batch(&schema).unwrap();
                c.execute(
                    "INSERT INTO meta(key, value) VALUES ('schema_version', ?1) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    [stamp],
                )
                .unwrap();
            }
            let db = Db::open(&p, Mode::ReadWrite).unwrap();
            assert_eq!(
                db.meta_get("records_changed_since_project").unwrap(),
                Some("1".into()),
                "from v{version}"
            );
            db.meta_set("records_changed_since_project", "0").unwrap();
            drop(db);
            let db = Db::open(&p, Mode::ReadWrite).unwrap();
            assert_eq!(
                db.meta_get("records_changed_since_project").unwrap(),
                Some("0".into()),
                "from v{version}, second open"
            );
        }
    }

    /// v5 takes out the embedding sidecar: no store in use ever held a vector. A store of any
    /// older schema loses the table and the two `meta` keys on its first write open, keeps
    /// every record as it was, and a second open changes nothing.
    #[test]
    fn an_older_store_loses_the_sidecar_table_and_keeps_its_records() {
        for (version, stamp, schema) in older_shapes() {
            let tmp = tempfile::tempdir().unwrap();
            let p = tmp.path().join("m.db");
            {
                let c = Connection::open(&p).unwrap();
                c.execute_batch(&schema).unwrap();
                c.execute(
                    "INSERT INTO meta(key, value) VALUES ('schema_version', ?1) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    [stamp],
                )
                .unwrap();
                c.execute_batch(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                       VALUES(1,'decision','s','is','we pool with pgbouncer','we pool with pgbouncer','user_said',3,'s','h',1); \
                     INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at,invalid,invalid_reason) \
                       VALUES(2,'decision','t','is','logs go to stderr','logs go to stderr','user_said',3,'s','h2',1,1,'revoked'); \
                     INSERT INTO record_vec(record_id, dim, vec, model_id, created_at) VALUES (1, 2, x'0000', 'm', 1); \
                     INSERT INTO meta(key, value) VALUES ('embed_model', 'm'), ('embed_at_ms', '1');",
                )
                .unwrap();
            }
            for open in 0..2 {
                let db = Db::open(&p, Mode::ReadWrite).unwrap();
                assert_eq!(
                    db.schema_version().unwrap(),
                    SCHEMA_VERSION,
                    "from v{version}"
                );
                assert_eq!(
                    db.count(
                        "SELECT count(*) FROM sqlite_master WHERE name = 'record_vec' \
                         OR sql LIKE '%record_vec%'"
                    )
                    .unwrap(),
                    0,
                    "the table is gone, from v{version}, open {open}"
                );
                assert_eq!(
                    db.count(
                        "SELECT count(*) FROM meta WHERE key IN ('embed_model', 'embed_at_ms')"
                    )
                    .unwrap(),
                    0,
                    "and so are its keys, from v{version}"
                );
                assert_eq!(
                    (
                        db.count("SELECT count(*) FROM record WHERE invalid = 0")
                            .unwrap(),
                        db.count("SELECT count(*) FROM record WHERE invalid = 1")
                            .unwrap()
                    ),
                    (1, 1),
                    "no record is retired or deleted, from v{version}"
                );
                db.quick_check().unwrap();
            }
        }
    }

    /// v5 takes out the symbol graph, the directory and symbol cues and the turn context. A
    /// store of any older schema loses the graph's three tables and `turn_context`, and every
    /// cue group that held a directory or a symbol cue goes whole: a group is a conjunction,
    /// and one left with half its conditions would fire on what remains. Event cues and
    /// groups without such a cue stay, the table refuses the two kinds from then on, and no
    /// record is touched.
    #[test]
    fn an_older_store_loses_the_symbol_graph_and_the_directory_and_symbol_cues() {
        for (version, stamp, schema) in older_shapes() {
            let tmp = tempfile::tempdir().unwrap();
            let p = tmp.path().join("m.db");
            {
                let c = Connection::open(&p).unwrap();
                c.execute_batch(&schema).unwrap();
                c.execute(
                    "INSERT INTO meta(key, value) VALUES ('schema_version', ?1) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    [stamp],
                )
                .unwrap();
                c.execute_batch(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,anchor_path,session_id,dedup_hash,created_at) VALUES \
                       (1,'invariant','a','must','never push to main','never push to main','user_said',3,'src/a/b.rs','s','h1',1), \
                       (2,'deadend','b','tried_and_failed','jwt in a cookie','jwt in a cookie','user_said',3,'src/auth/jwt.rs','s','h2',1), \
                       (3,'decision','c','is','we pool with pgbouncer','we pool with pgbouncer','user_said',3,'src/db/pool.rs','s','h3',1); \
                     INSERT INTO cue(record_id, kind, key, grp) VALUES \
                       (1,'event','session_start',0), (1,'event','post_compact',1), (1,'dir','src/a',2), (1,'symbol','push_main',3), \
                       (2,'event','pre_edit',0), (2,'dir','src/auth',0), (2,'symbol','verify_token',1), \
                       (3,'dir','src/db',0), (3,'keyword','pooling',1), (3,'keyword','pgbouncer',2), (3,'symbol','Pool',2); \
                     INSERT INTO symbol(qualified_name, short_name, kind, path, line, file_hash) VALUES ('m::f','f','fn','src/a/b.rs',1,'x'); \
                     INSERT INTO symbol_ref(path, line, from_name, to_name, ref_kind) VALUES ('src/a/b.rs',2,'g','f','call'); \
                     INSERT INTO symbol_file(path, file_hash, lang, defs, refs, indexed_at) VALUES ('src/a/b.rs','x','rust',1,1,1); \
                     INSERT INTO turn_context(session_id, kind, key, seen_at) VALUES ('s','file','src/a/b.rs',1); \
                     INSERT INTO meta(key, value) VALUES ('symbols_indexed_ms', '1');",
                )
                .unwrap();
            }
            for open in 0..2 {
                let db = Db::open(&p, Mode::ReadWrite).unwrap();
                assert_eq!(
                    db.schema_version().unwrap(),
                    SCHEMA_VERSION,
                    "from v{version}"
                );
                assert_eq!(
                    db.count(
                        "SELECT count(*) FROM sqlite_master WHERE tbl_name IN \
                         ('symbol', 'symbol_ref', 'symbol_file', 'turn_context')"
                    )
                    .unwrap(),
                    0,
                    "the graph and the turn context are gone, with their indexes, from v{version}, open {open}"
                );
                assert_eq!(
                    db.count("SELECT count(*) FROM meta WHERE key = 'symbols_indexed_ms'")
                        .unwrap(),
                    0
                );
                let cues: Vec<(i64, String, String)> = db
                    .conn
                    .prepare("SELECT record_id, kind, key FROM cue ORDER BY record_id, key")
                    .unwrap()
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .unwrap()
                    .flatten()
                    .collect();
                assert_eq!(
                    cues,
                    vec![
                        (1, "event".to_string(), "post_compact".to_string()),
                        (1, "event".to_string(), "session_start".to_string()),
                        (3, "keyword".to_string(), "pooling".to_string()),
                    ],
                    "from v{version}, open {open}"
                );
                for kind in ["dir", "symbol"] {
                    assert!(
                        db.conn
                            .execute(
                                "INSERT INTO cue(record_id, kind, key, grp) VALUES (1, ?1, 'k', 9)",
                                [kind]
                            )
                            .is_err(),
                        "a {kind} cue is refused, from v{version}"
                    );
                }
                assert_eq!(
                    db.count(
                        "SELECT count(*) FROM sqlite_master WHERE type = 'index' \
                         AND name IN ('cue_lookup', 'cue_record')"
                    )
                    .unwrap(),
                    2,
                    "the cue table keeps its indexes, from v{version}"
                );
                assert_eq!(
                    db.count("SELECT count(*) FROM record WHERE invalid = 0")
                        .unwrap(),
                    3,
                    "no record is retired or deleted, from v{version}"
                );
                db.quick_check().unwrap();
            }
        }
    }

    /// The graph was most of a store's file, and dropping its tables frees the pages without
    /// shrinking the file. The migration leaves a note, and the write path outside any hook
    /// gives the pages back once: a store that was never migrated, or already compacted, is
    /// left alone.
    #[test]
    fn the_file_shrinks_once_after_the_graph_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("m.db");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(include_str!("testdata/schema_v4.sql"))
                .unwrap();
            c.execute(
                "INSERT INTO meta(key, value) VALUES ('schema_version', '4') \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )
            .unwrap();
            c.execute_batch(
                "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                   VALUES(1,'decision','s','is','we pool with pgbouncer','we pool with pgbouncer','user_said',3,'s','h',1); \
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 4000) \
                 INSERT INTO symbol(qualified_name, short_name, kind, path, line, file_hash) \
                   SELECT 'module::function_number_' || i, 'function_number_' || i, 'fn', \
                          'src/some/long/path/file_' || i || '.rs', i, 'hash' FROM n;",
            )
            .unwrap();
        }
        let before = std::fs::metadata(&p).unwrap().len();
        let db = Db::open(&p, Mode::ReadWrite).unwrap();
        assert!(
            db.compact_if_pending().unwrap(),
            "the migration left a note"
        );
        let after = std::fs::metadata(&p).unwrap().len();
        assert!(after * 2 < before, "{before} bytes before, {after} after");
        assert!(!db.compact_if_pending().unwrap(), "once");
        assert_eq!(
            db.count("SELECT count(*) FROM record WHERE invalid = 0")
                .unwrap(),
            1
        );
        db.quick_check().unwrap();
        drop(db);
        let again = Db::open(&p, Mode::ReadWrite).unwrap();
        assert!(!again.compact_if_pending().unwrap());

        let fresh = Db::open(&tmp.path().join("new.db"), Mode::ReadWrite).unwrap();
        assert!(
            !fresh.compact_if_pending().unwrap(),
            "a store made by this build has nothing to give back"
        );
    }

    #[test]
    fn a_v2_or_v3_store_gains_the_tables_of_who_wrote_a_prompt() {
        for (version, schema) in [
            ("2", include_str!("testdata/schema_1_2_0.sql")),
            ("3", include_str!("testdata/schema_v3.sql")),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let p = tmp.path().join("m.db");
            {
                let c = Connection::open(&p).unwrap();
                c.execute_batch(schema).unwrap();
                c.execute(
                    "INSERT INTO meta(key, value) VALUES ('schema_version', ?1) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    [version],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(1,'decision','s','is','we pool with pgbouncer','we pool with pgbouncer','user_said',3,'s','h',1)",
                    [],
                )
                .unwrap();
            }
            let db = Db::open(&p, Mode::ReadWrite).unwrap();
            assert_eq!(
                db.schema_version().unwrap(),
                SCHEMA_VERSION,
                "from v{version}"
            );
            for table in [
                "agent_task",
                "turn_prompt",
                "record_turn",
                "restatement",
                "restatement_retired",
                "agent_retired",
                "store_event",
            ] {
                assert_eq!(
                    db.count(&format!(
                        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='{table}'"
                    ))
                    .unwrap(),
                    1,
                    "{table} from v{version}"
                );
            }
            assert_eq!(
                db.count("SELECT count(*) FROM sqlite_master WHERE type='index' AND name='restatement_record'")
                    .unwrap(),
                1,
                "the journal's record index, from v{version}"
            );
            assert_eq!(
                db.count(
                    "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='record_active_ref'"
                )
                .unwrap(),
                1,
                "the index a found-out turn's rows are read by"
            );
            assert_eq!(
                db.count("SELECT count(*) FROM pragma_table_info('restatement') WHERE name = 'after_record'")
                    .unwrap(),
                1,
                "the journal dates a restatement against the records, from v{version}"
            );
            assert_eq!(
                db.count("SELECT count(*) FROM record WHERE invalid=0")
                    .unwrap(),
                1,
                "the record survives the migration from v{version}"
            );
            assert_eq!(
                db.count(
                    "SELECT count(*) FROM pragma_table_info('record_turn') \
                     WHERE name IN ('session_id', 'created_at')"
                )
                .unwrap(),
                2,
                "record_turn can undo a restatement, from v{version}"
            );
            drop(db);
            let again = Db::open(&p, Mode::ReadWrite).unwrap();
            assert_eq!(again.schema_version().unwrap(), SCHEMA_VERSION);
        }
    }

    #[test]
    fn a_1_2_0_store_is_migrated_with_retirement_columns_and_a_coherent_index() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("m.db");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(include_str!("testdata/schema_1_2_0.sql"))
                .unwrap();
            c.execute(
                "INSERT INTO meta(key, value) VALUES ('schema_version', '2')",
                [],
            )
            .unwrap();
            for (id, obj) in [(1, "we pool with pgbouncer"), (2, "logs go to stderr")] {
                c.execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,'decision',?2,'is',?2,?2,'user_said',3,'s',?2,1)",
                    rusqlite::params![id, obj],
                )
                .unwrap();
            }
            // what 1.2.0's restore pass did: retire, then put back by hand on top of the trigger
            c.execute_batch(
                "UPDATE record SET invalid=1, invalid_reason='superseded' WHERE id=1; \
                 UPDATE record SET invalid=0, invalid_reason=NULL WHERE id=1; \
                 INSERT INTO record_fts(rowid, subject, object, body) SELECT id, subject, object, body FROM record WHERE id=1; \
                 UPDATE meta SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT) WHERE key='fts_rows';",
            )
            .unwrap();
            let drift: String = c
                .query_row("SELECT value FROM meta WHERE key='fts_rows'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(
                drift, "3",
                "the 1.2.0 shape: two active rows, a counter of three"
            );
        }
        let db = Db::open(&p, Mode::ReadWrite).unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(db.meta_get("fts_rows").unwrap().as_deref(), Some("2"));
        let hits = db
            .count("SELECT count(*) FROM record_fts WHERE record_fts MATCH 'pgbouncer'")
            .unwrap();
        assert_eq!(hits, 1, "the restored row is in the index once");
        assert_eq!(
            db.count(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='record_turn'"
            )
            .unwrap(),
            1,
            "the table of earlier turns exists"
        );
        db.conn
            .execute(
                "UPDATE record SET invalid=1, invalid_reason='revoked' WHERE id=2",
                [],
            )
            .unwrap();
        let (at, via): (Option<i64>, Option<String>) = db
            .conn
            .query_row(
                "SELECT invalidated_at, invalidated_via FROM record WHERE id=2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(at.is_some_and(|t| t > 1_700_000_000_000), "dated: {at:?}");
        assert_eq!(
            via.as_deref(),
            Some("revoked"),
            "and its cause defaults to the reason"
        );
        // a retirement that names its own cause keeps it
        db.conn
            .execute(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_via='said' WHERE id=1",
                [],
            )
            .unwrap();
        let via: String = db
            .conn
            .query_row("SELECT invalidated_via FROM record WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(via, "said");
        // and putting a record back clears both
        db.conn
            .execute(
                "UPDATE record SET invalid=0, invalid_reason=NULL WHERE id=1",
                [],
            )
            .unwrap();
        let (at, via): (Option<i64>, Option<String>) = db
            .conn
            .query_row(
                "SELECT invalidated_at, invalidated_via FROM record WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((at, via), (None, None));
        db.quick_check().unwrap();
        // the migration runs once: a second open changes nothing
        drop(db);
        let db = Db::open(&p, Mode::ReadWrite).unwrap();
        assert_eq!(db.meta_get("fts_rows").unwrap().as_deref(), Some("1"));
        let hits = db
            .count("SELECT count(*) FROM record_fts WHERE record_fts MATCH 'pgbouncer'")
            .unwrap();
        assert_eq!(hits, 1);
    }

    /// WP-08. `doctor` compares the index counter with the active rows and, when they differ,
    /// now sends the reader to `maintain`, which has to mend it: rebuild the index from the
    /// servable rows and set the counter to what it holds. A coherent store is left alone.
    #[test]
    fn a_drifted_index_is_rebuilt_and_a_coherent_one_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        db.conn.execute(
            "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
             VALUES('decision','retry.policy','is','exponential','we use exponential backoff','user_said',3,'s1','h1',1)",
            [],
        ).unwrap();
        assert_eq!(db.repair_fts_if_drifted().unwrap(), None);
        db.meta_set("fts_rows", "5").unwrap();
        assert_eq!(db.repair_fts_if_drifted().unwrap(), Some(1));
        assert_eq!(db.meta_get("fts_rows").unwrap().as_deref(), Some("1"));
        let n = db
            .count("SELECT count(*) FROM record_fts WHERE record_fts MATCH 'exponential'")
            .unwrap();
        assert_eq!(n, 1);
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
