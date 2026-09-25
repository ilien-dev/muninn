//! F1 — invalidation and the read filter (ENGINE.md §5). Coarse by design: a record is
//! active or retired; retired rows stay on disk, leave the index, and are never served
//! unless asked for with `--all`. Three deterministic triggers: supersession (write
//! path), anchor change (here), revert (git capture). Plus an explicit revocation.
//!
//! **Where the filter lives.** Not in this module: in the `served_record` view
//! (`schema.sql`) and in `recall::Hit`, whose only constructor reads from it. A serving
//! path cannot express a retired record, so the guarantee does not depend on any query
//! remembering `WHERE invalid = 0`. `crates/muninn-cli/tests/fault.rs` (s16, s17) asserts
//! it end to end against real hook stdout.
//!
//! The paths that read retired rows on purpose, and must keep doing so:
//!
//! - `muninn why --all` and `muninn export --all` — a person asked, and the row is printed
//!   with `· RETIRED (reason)`;
//! - `lineage` below — ids and `invalid_reason` only, never a body, so `muninn why` can
//!   show what replaced what;
//! - `project::load_all` — the Markdown mirror under `.muninn/records/`, which keeps every
//!   record with `invalid:` in its frontmatter (`MUNINN_NO_PROJECT` switches it off);
//! - the write path: supersession and dedup in `muninn-capture`, `maintain`, and the
//!   anchor validator here;
//! - `capture::inherit_topic`, which copies topic words — never names or values — from a
//!   retired record onto the one that replaced it, so the replacement is findable by the
//!   words it omits.

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
    let tx = db.write_tx()?;
    for (id, path, hash) in rows {
        let now = cache
            .entry(path.clone())
            .or_insert_with(|| file_hash(&paths.source_root(), &path))
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
        db.meta_set("records_changed_since_project", "1")?;
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
        db.meta_set("records_changed_since_project", "1")?;
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
    let mut ids: Vec<i64> = stmt
        .query_map([id], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    ids.extend(judged_conflicts_of(db, id));
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// Active records the write-path judge paired with `id`, in either direction
/// (`judged_conflict`). A store written before the table existed, opened `query_only` by a
/// read hook, has no such table: that reads as no pairs, never as an error.
pub fn judged_conflicts_of(db: &Db, id: i64) -> Vec<i64> {
    let Ok(mut stmt) = db.conn.prepare_cached(
        "SELECT j.new_id FROM judged_conflict j JOIN record o ON o.id = j.new_id \
          WHERE j.old_id = ?1 AND o.invalid = 0 \
         UNION SELECT j.old_id FROM judged_conflict j JOIN record o ON o.id = j.old_id \
          WHERE j.new_id = ?1 AND o.invalid = 0",
    ) else {
        return Vec::new();
    };
    stmt.query_map([id], |r| r.get(0))
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
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
    fn a_judged_pair_is_a_conflict_both_ways_until_one_side_is_retired() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::from_root(tmp.path());
        for d in paths.all_dirs() {
            std::fs::create_dir_all(d).unwrap();
        }
        let db = Db::open(&paths.db_path(), Mode::ReadWrite).unwrap();
        // different subjects: the key-based check alone sees no conflict
        let old = ins(&db, "decision", "queue", "RabbitMQ", None);
        let new = ins(&db, "decision", "bus", "NATS", None);
        assert!(conflicts_of(&db, old).unwrap().is_empty());
        db.conn
            .execute(
                "INSERT INTO judged_conflict(old_id,new_id,p_pick,p_confirm,model,created_at) VALUES(?1,?2,0.9,0.9,'m',1)",
                [old, new],
            )
            .unwrap();
        assert_eq!(conflicts_of(&db, old).unwrap(), vec![new]);
        assert_eq!(conflicts_of(&db, new).unwrap(), vec![old]);
        db.conn
            .execute("UPDATE record SET invalid = 1 WHERE id = ?1", [new])
            .unwrap();
        assert!(conflicts_of(&db, old).unwrap().is_empty());
        // a store from before the table: no pairs, no error
        db.conn.execute_batch("DROP TABLE judged_conflict").unwrap();
        assert!(judged_conflicts_of(&db, old).is_empty());
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

    /// Every file that may read `record` directly, with the reason. A serving path is not on
    /// this list: it reads `served_record`, so it cannot express a retired record (F1).
    /// Adding a file here is a deliberate act — say why, or point the query at the view.
    const MAY_READ_RECORD: &[(&str, &str)] = &[
        (
            "muninn-core/src/db.rs",
            "schema and the FTS-follows-invalid test",
        ),
        (
            "muninn-core/src/filter.rs",
            "writes the invalid bit; lineage walks retired rows",
        ),
        ("muninn-core/src/health.rs", "counts, never text"),
        (
            "muninn-core/src/project.rs",
            "load_all: the Markdown mirror and export --all",
        ),
        (
            "muninn-core/src/cue.rs",
            "derive_missing: ids of active records, no body",
        ),
        ("muninn-core/src/recall.rs", "the IDF denominator: a count"),
        (
            "muninn-judge/src/lib.rs",
            "write path (maintain): reads active rows by id and time to judge them, filters invalid itself",
        ),
        (
            "muninn-embed/src/lib.rs",
            "write path: backfill, dedup and variant retirement",
        ),
        (
            "muninn-capture/src/ingest.rs",
            "write path: dedup, supersession, topic inheritance",
        ),
        ("muninn-cli/src/main.rs", "status counters"),
        ("muninn-cli/src/maintain.rs", "write path: revert detection"),
        (
            "muninn-cli/src/compile_cmd.rs",
            "one rationale id, never expanded to text",
        ),
        (
            "muninn-why/src/lib.rs",
            "muninn why --all, which prints RETIRED rows on purpose",
        ),
        ("muninn-bench/src/main.rs", "offline harness"),
        (
            "muninn-bench/src/experiment.rs",
            "offline harness; audits retired deliveries",
        ),
    ];

    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name()
                    .is_some_and(|n| n == "target" || n == "fixtures")
                {
                    continue;
                }
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// A new query against `record` in a file that is not on the list fails here rather
    /// than in production. Crude on purpose: it does not need to be clever to be useful.
    #[test]
    fn no_undeclared_reads_of_record() {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let mut files = Vec::new();
        walk(&crates, &mut files);
        assert!(files.len() > 20, "found no sources to scan");
        let mut offenders: Vec<String> = Vec::new();
        for f in files {
            let rel = f
                .strip_prefix(&crates)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel.contains("/tests/") || MAY_READ_RECORD.iter().any(|(a, _)| rel == *a) {
                continue;
            }
            let src = std::fs::read_to_string(&f).unwrap_or_default();
            for pat in [
                "FROM record ",
                "JOIN record ",
                "FROM record\\",
                "JOIN record\\",
            ] {
                if src.contains(pat) {
                    offenders.push(format!("{rel}: {pat:?}"));
                    break;
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these files read `record` directly but are not declared in MAY_READ_RECORD; \
             a serving path must read `served_record` instead:\n  {}",
            offenders.join("\n  ")
        );
    }
}
