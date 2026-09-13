//! F1 — invalidation and the read filter (ENGINE.md §5). Coarse by design: a record is
//! active or retired; retired rows stay on disk, leave the index, and are never served
//! unless asked for with `--all`. Three deterministic triggers: supersession (write
//! path), anchor change (here), revert (git capture). Plus an explicit revocation.

use crate::db::now_ms;
use crate::paths::ProjectPaths;
use crate::{Db, Result};
use std::collections::HashMap;
use std::path::Path;

pub fn file_hash(root: &Path, rel: &str) -> Option<String> {
    let p = if Path::new(rel).is_absolute() {
        std::path::PathBuf::from(rel)
    } else {
        root.join(rel)
    };
    let bytes = std::fs::read(p).ok()?;
    Some(blake3::hash(&bytes).to_hex().to_string())
}

/// Anchor validator: an active anchored record whose file content no longer hashes to
/// `anchor_hash` is retired with `anchor_changed`. The new value is never guessed
/// [K11]. Records whose file is gone are retired the same way. Returns the count.
pub fn validate_anchors(paths: &ProjectPaths, db: &Db) -> Result<usize> {
    let mut stmt = db.conn.prepare(
        "SELECT id, anchor_path, anchor_hash FROM record \
         WHERE invalid = 0 AND anchor_path IS NOT NULL AND anchor_hash IS NOT NULL \
         AND kind IN ('decision','invariant','claim','correction')",
    )?;
    let rows: Vec<(i64, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .filter_map(|r| r.ok())
        .collect();
    let mut cache: HashMap<String, Option<String>> = HashMap::new();
    let mut retired = 0usize;
    let tx = db.conn.unchecked_transaction()?;
    for (id, path, hash) in rows {
        let now = cache
            .entry(path.clone())
            .or_insert_with(|| file_hash(&paths.root, &path))
            .clone();
        if now.as_deref() != Some(hash.as_str()) {
            tx.execute(
                "UPDATE record SET invalid = 1, invalid_reason = 'anchor_changed' WHERE id = ?1 AND invalid = 0",
                [id],
            )?;
            retired += 1;
        }
    }
    tx.commit()?;
    if retired > 0 {
        db.meta_set("records_changed_since_render", "1")?;
    }
    db.meta_set("anchors_checked_ms", &now_ms().to_string())?;
    Ok(retired)
}

/// Explicit revocation by a person: retained, never served. `reason` is free text kept
/// in the retired row's body tail? No — the body is literal; the reason goes to meta.
pub fn revoke(db: &Db, id: i64, reason: &str) -> Result<bool> {
    let n = db.conn.execute(
        "UPDATE record SET invalid = 1, invalid_reason = 'revoked' WHERE id = ?1 AND invalid = 0",
        [id],
    )?;
    if n == 1 {
        db.meta_set(
            &format!("revoked:{id}"),
            &format!("{}|{}", now_ms(), reason),
        )?;
        db.meta_set("records_changed_since_render", "1")?;
    }
    Ok(n == 1)
}

/// Lineage of a record: what it superseded (backwards) and what superseded it
/// (forwards), as `(id, invalid_reason)` pairs, oldest first. The record itself is
/// included.
pub fn lineage(db: &Db, id: i64) -> Result<Vec<(i64, Option<String>)>> {
    let mut back = Vec::new();
    let mut cur = id;
    for _ in 0..32 {
        let prev: Option<i64> = db
            .conn
            .query_row(
                "SELECT id FROM record WHERE invalidated_by = ?1 ORDER BY id DESC LIMIT 1",
                [cur],
                |r| r.get(0),
            )
            .ok();
        let Some(p) = prev else { break };
        back.push(p);
        cur = p;
    }
    back.reverse();
    let mut fwd = Vec::new();
    let mut cur = id;
    for _ in 0..32 {
        let next: Option<i64> = db
            .conn
            .query_row(
                "SELECT invalidated_by FROM record WHERE id = ?1",
                [cur],
                |r| r.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten();
        let Some(n) = next else { break };
        if n == cur {
            break;
        }
        fwd.push(n);
        cur = n;
    }
    let mut ids = back;
    ids.push(id);
    ids.extend(fwd);
    let mut out = Vec::with_capacity(ids.len());
    for i in ids {
        let reason: Option<String> = db
            .conn
            .query_row(
                "SELECT invalid_reason FROM record WHERE id = ?1",
                [i],
                |r| r.get(0),
            )
            .unwrap_or(None);
        out.push((i, reason));
    }
    Ok(out)
}

/// Active records that share (subject, relation) with `id` but hold a different
/// object: an unresolved conflict [N4]. Both are served, both marked; ranking never
/// picks one.
pub fn conflicts_of(db: &Db, id: i64) -> Result<Vec<i64>> {
    let mut stmt = db.conn.prepare(
        "SELECT o.id FROM record r JOIN record o ON o.subject = r.subject AND o.relation = r.relation AND o.kind = r.kind \
         WHERE r.id = ?1 AND o.id <> r.id AND o.invalid = 0 AND o.object <> r.object AND r.kind <> 'episode' ORDER BY o.id",
    )?;
    let ids: Vec<i64> = stmt
        .query_map([id], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Mode;

    fn ins(db: &Db, kind: &str, subject: &str, object: &str, anchor: Option<(&str, &str)>) -> i64 {
        db.conn
            .execute(
                "INSERT INTO record(kind,subject,relation,object,body,origin,trust,anchor_path,anchor_hash,session_id,dedup_hash,created_at) \
                 VALUES(?1,?2,'is',?3,?3,'user_said',3,?4,?5,'s',?6,1)",
                rusqlite::params![kind, subject, object, anchor.map(|a| a.0), anchor.map(|a| a.1), format!("{kind}{subject}{object}")],
            )
            .unwrap();
        db.conn.last_insert_rowid()
    }

    #[test]
    fn anchors_revocation_lineage_conflicts() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::from_root(tmp.path());
        for d in paths.all_dirs() {
            std::fs::create_dir_all(d).unwrap();
        }
        let db = Db::open(&paths.db_path(), Mode::ReadWrite).unwrap();
        std::fs::write(tmp.path().join("a.rs"), "v1").unwrap();
        let h = file_hash(tmp.path(), "a.rs").unwrap();
        let a = ins(&db, "decision", "retry", "linear", Some(("a.rs", &h)));
        // untouched file: stays
        assert_eq!(validate_anchors(&paths, &db).unwrap(), 0);
        std::fs::write(tmp.path().join("a.rs"), "v2").unwrap();
        assert_eq!(validate_anchors(&paths, &db).unwrap(), 1);
        let reason: String = db
            .conn
            .query_row("SELECT invalid_reason FROM record WHERE id=?1", [a], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(reason, "anchor_changed");
        // lineage through invalidated_by
        let b = ins(&db, "invariant", "k", "one", None);
        let c = ins(&db, "invariant", "k", "two", None);
        db.conn
            .execute("UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?1 WHERE id=?2", [c, b])
            .unwrap();
        let l = lineage(&db, c).unwrap();
        assert_eq!(l.iter().map(|x| x.0).collect::<Vec<_>>(), vec![b, c]);
        assert_eq!(l[0].1.as_deref(), Some("superseded"));
        // conflict: two active records, same key, different object
        let d = ins(&db, "invariant", "k", "three", None);
        assert_eq!(conflicts_of(&db, c).unwrap(), vec![d]);
        assert!(revoke(&db, d, "user said so").unwrap());
        assert!(conflicts_of(&db, c).unwrap().is_empty());
        assert!(!revoke(&db, d, "again").unwrap());
    }
}
