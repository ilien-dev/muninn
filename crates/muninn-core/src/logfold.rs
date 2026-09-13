//! Append-only JSONL logs that read hooks write and the write path folds into the
//! store (heartbeats, deliveries). A fold never renames or truncates the file other
//! processes are appending to: it reads the complete lines past a watermark kept in
//! `meta`, inside the write transaction that serialises folds, and moves the watermark
//! in that same transaction. A line appended at any moment is read by this fold or by
//! the next one, and a fold that fails to commit leaves its lines for the next.
//!
//! The file is rotated only once a committed fold has consumed `ROTATE_BYTES` of it:
//! it becomes `<name>.1`, which the next folds read first, so a line written through a
//! descriptor opened before the rename is still folded.

use crate::error::{Error, Result};
use crate::sanitize::from_bytes_lossy;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Size of folded lines after which the live file is rotated.
pub const ROTATE_BYTES: u64 = 4 << 20;
/// At most this much is read per file per fold; the rest waits for the next fold.
const MAX_READ: u64 = 64 << 20;

pub struct Taken {
    pub lines: Vec<String>,
    /// A `<name>.folding` file left by the rename-based fold of earlier versions:
    /// its lines are in `lines`; remove it once the transaction has committed.
    pub legacy: Option<PathBuf>,
}

impl Taken {
    pub fn finish(self) {
        if let Some(p) = self.legacy {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn rotated(log: &Path) -> PathBuf {
    log.with_extension("jsonl.1")
}

fn key(log: &Path) -> String {
    format!(
        "fold:{}",
        log.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    )
}

/// Watermarks `(live, rotated)` in bytes.
fn marks(conn: &rusqlite::Connection, log: &Path) -> (u64, u64) {
    conn.query_row("SELECT value FROM meta WHERE key=?1", [key(log)], |r| {
        r.get::<_, String>(0)
    })
    .ok()
    .and_then(|v| {
        let (a, b) = v.split_once(',')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    })
    .unwrap_or((0, 0))
}

/// Complete lines of a regular file from `off`; returns the offset after the last
/// newline read. A missing file, a device or a file shorter than `off` (replaced)
/// reads from its start or not at all.
fn read_from(path: &Path, off: u64, out: &mut Vec<String>) -> Result<u64> {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(Error::io(path, e)),
    };
    let md = f.metadata().map_err(|e| Error::io(path, e))?;
    if !md.is_file() {
        return Ok(off);
    }
    let off = if md.len() < off { 0 } else { off };
    f.seek(SeekFrom::Start(off))
        .map_err(|e| Error::io(path, e))?;
    let mut buf = Vec::new();
    f.take(MAX_READ)
        .read_to_end(&mut buf)
        .map_err(|e| Error::io(path, e))?;
    // a line still being written has no newline yet: it belongs to the next fold
    let Some(end) = buf.iter().rposition(|b| *b == b'\n') else {
        return Ok(off);
    };
    out.extend(
        from_bytes_lossy(&buf[..=end])
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string),
    );
    Ok(off + end as u64 + 1)
}

/// Take the lines appended since the last fold. Call inside the write transaction
/// `tx` and commit it before calling `Taken::finish`.
pub fn take(tx: &rusqlite::Transaction<'_>, log: &Path) -> Result<Taken> {
    let (mut live, mut rot) = marks(tx, log);
    let rot_path = rotated(log);
    let mut lines = Vec::new();
    // the rotated file first: its tail may hold lines written after the rename
    if rot_path.exists() {
        rot = read_from(&rot_path, rot, &mut lines)?;
    }
    if live >= ROTATE_BYTES {
        let is_file = std::fs::symlink_metadata(log).is_ok_and(|m| m.is_file());
        if is_file && std::fs::rename(log, &rot_path).is_ok() {
            rot = live;
            live = 0;
            rot = read_from(&rot_path, rot, &mut lines)?;
        }
    }
    live = read_from(log, live, &mut lines)?;
    let legacy = log.with_extension("jsonl.folding");
    let legacy = if legacy.is_file() {
        let _ = read_from(&legacy, 0, &mut lines)?;
        Some(legacy)
    } else {
        None
    };
    tx.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key(log), format!("{live},{rot}")],
    )?;
    Ok(Taken { lines, legacy })
}

/// Lines appended since the last fold, without moving the watermark (a reader that
/// adds them to what the store already holds).
pub fn pending(conn: &rusqlite::Connection, log: &Path) -> Vec<String> {
    let (live, rot) = marks(conn, log);
    let mut lines = Vec::new();
    let _ = read_from(&rotated(log), rot, &mut lines);
    let _ = read_from(log, live, &mut lines);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Db, Mode};
    use std::io::Write;

    fn append(p: &Path, s: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    #[test]
    fn watermark_partial_lines_and_rotation() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let log = tmp.path().join("x.jsonl");
        append(&log, "a\nb\npart");
        let tx = db.write_tx().unwrap();
        let t = take(&tx, &log).unwrap();
        tx.commit().unwrap();
        assert_eq!(t.lines, vec!["a", "b"]);
        assert_eq!(pending(&db.conn, &log), Vec::<String>::new());

        // the torn line completes; nothing is read twice
        append(&log, "ial\nc\n");
        assert_eq!(pending(&db.conn, &log), vec!["partial", "c"]);
        let tx = db.write_tx().unwrap();
        assert_eq!(take(&tx, &log).unwrap().lines, vec!["partial", "c"]);
        tx.commit().unwrap();

        // a fold that does not commit leaves its lines for the next one
        append(&log, "d\n");
        let tx = db.write_tx().unwrap();
        assert_eq!(take(&tx, &log).unwrap().lines, vec!["d"]);
        drop(tx);
        let tx = db.write_tx().unwrap();
        assert_eq!(take(&tx, &log).unwrap().lines, vec!["d"]);
        tx.commit().unwrap();

        // past ROTATE_BYTES the folded file becomes x.jsonl.1; a line written through
        // an old descriptor after the rename is still folded
        let big = "y".repeat(1023) + "\n";
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        for _ in 0..(ROTATE_BYTES / 1024 + 1) {
            f.write_all(big.as_bytes()).unwrap();
        }
        let tx = db.write_tx().unwrap();
        let n = take(&tx, &log).unwrap().lines.len();
        tx.commit().unwrap();
        assert_eq!(n as u64, ROTATE_BYTES / 1024 + 1);
        append(&log, "e\n");
        let tx = db.write_tx().unwrap();
        assert_eq!(take(&tx, &log).unwrap().lines, vec!["e"]);
        tx.commit().unwrap();
        assert!(rotated(&log).exists());
        f.write_all(b"late\n").unwrap();
        append(&log, "f\n");
        let tx = db.write_tx().unwrap();
        assert_eq!(take(&tx, &log).unwrap().lines, vec!["late", "f"]);
        tx.commit().unwrap();
    }
}
