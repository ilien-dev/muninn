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
    validate_anchors_with(paths, db, || {})
}

/// The validator, with something run between its read and its write (a test's seam).
///
/// It opens by reading the anchored active records, hashes their files with no transaction
/// open, and closes by retiring the ones whose file no longer matches. A record is judged
/// only against a hash taken after the record was read, so one ingested during the work, with
/// the hash of the file as it is then, is not judged at all in this run; and a record is
/// retired at the close only if it still carries the hash that was read, so one re-anchored
/// during the work is left alone. No event on opening: every run reads every active anchored
/// record, so one given back later is judged by the next run.
pub(crate) fn validate_anchors_with(
    paths: &ProjectPaths,
    db: &Db,
    during_the_work: impl FnOnce(),
) -> Result<usize> {
    let rows: Vec<(i64, String, String)> = crate::db::open_pass(db, "anchors", false, |tx| {
        let mut stmt = tx.prepare(
            "SELECT id, anchor_path, anchor_hash FROM record \
             WHERE invalid = 0 AND anchor_path IS NOT NULL AND anchor_hash IS NOT NULL \
             AND kind IN ('decision','invariant','claim','correction')",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    })?;
    let mut cache: HashMap<String, Option<String>> = HashMap::new();
    let changed: Vec<(i64, String)> = rows
        .into_iter()
        .filter(|(_, path, hash)| {
            let now = cache
                .entry(path.clone())
                .or_insert_with(|| file_hash(&paths.source_root(), path));
            now.as_deref() != Some(hash.as_str())
        })
        .map(|(id, _, hash)| (id, hash))
        .collect();
    during_the_work();
    let retired = crate::db::close_pass(db, "anchors", |tx| {
        let mut retired = 0usize;
        for (id, hash) in &changed {
            retired += tx.execute(
                "UPDATE record SET invalid = 1, invalid_reason = 'anchor_changed' \
                 WHERE id = ?1 AND invalid = 0 AND anchor_hash = ?2",
                rusqlite::params![id, hash],
            )?;
        }
        Ok((retired, !changed.is_empty()))
    })?;
    if retired > 0 {
        db.meta_set("records_changed_since_project", "1")?;
    }
    db.meta_set("anchors_checked_ms", &now_ms().to_string())?;
    Ok(retired)
}

/// Explicit revocation by a person: retained, never served. `reason` is free text kept
/// in the retired row's body tail? No — the body is literal; the reason goes to meta.
///
/// A record that is retired already is left exactly as it is, and the answer is "no" as it
/// always was. One thing is written beside it when a replacement holds it retired: that the
/// person revoked it, and when (`agent_retired`, `held_by` -1, with the newest record and
/// journal row of that moment). If what replaced it turns out to be an agent's prompt read as
/// the user's, the undo of that prompt does not give the record back (`muninn-capture`,
/// `agent/undo.rs`). Nothing else reads the note.
pub fn revoke(db: &Db, id: i64, reason: &str) -> Result<bool> {
    let tx = db.write_tx()?;
    let n = tx.execute(
        "UPDATE record SET invalid = 1, invalid_reason = 'revoked' WHERE id = ?1 AND invalid = 0",
        [id],
    )?;
    if n == 1 {
        // only a revoke that retired something: a mistyped id blocks nothing
        crate::db::note_event(&tx, "revoke")?;
    } else {
        tx.execute(
            "INSERT OR REPLACE INTO agent_retired(record_id, held_by, statement, journal) \
             SELECT id, -1, (SELECT coalesce(max(id), 0) FROM record), \
                    (SELECT coalesce(max(id), 0) FROM restatement) \
             FROM record WHERE id = ?1 AND invalid = 1 AND invalid_reason = 'superseded'",
            [id],
        )?;
    }
    tx.commit()?;
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
///
/// So are two active decisions that ingest marked as disagreeing ([`mark_conflict`]): an
/// older one read after a later one that would have replaced it by its words.
pub fn conflicts_of(db: &Db, id: i64) -> Result<Vec<i64>> {
    let mut stmt = db.conn.prepare_cached(
        "SELECT o.id FROM record r JOIN record o ON o.subject = r.subject AND o.relation = r.relation AND o.kind = r.kind \
         WHERE r.id = ?1 AND o.id <> r.id AND o.invalid = 0 AND o.object <> r.object AND r.kind <> 'episode' ORDER BY o.id",
    )?;
    let mut ids: Vec<i64> = stmt
        .query_map([id], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    for other in marked_conflicts(db, id)? {
        if !ids.contains(&other) {
            ids.push(other);
        }
    }
    ids.sort_unstable();
    Ok(ids)
}

fn conflict_key(id: i64) -> String {
    format!("conflict:{id}")
}

/// The active records `id` was marked as disagreeing with, while `id` is active itself. The
/// mark is a `meta` row a record, read by its primary key: one probe for a record that has
/// none, which is every record but a few.
pub fn marked_conflicts(db: &Db, id: i64) -> Result<Vec<i64>> {
    let listed: Option<String> = match db.conn.query_row(
        "SELECT value FROM meta WHERE key = ?1",
        [conflict_key(id)],
        |r| r.get(0),
    ) {
        Ok(v) => Some(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.into()),
    };
    let Some(listed) = listed else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut active = db.conn.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM record a, record b WHERE a.id = ?1 AND b.id = ?2 \
         AND a.invalid = 0 AND b.invalid = 0)",
    )?;
    for other in listed.split(',').filter_map(|i| i.parse::<i64>().ok()) {
        if active.query_row([id, other], |r| r.get::<_, bool>(0))? {
            out.push(other);
        }
    }
    Ok(out)
}

/// Mark two records as disagreeing, each with the other. Inside the caller's write
/// transaction. The mark says nothing once either record is retired, and is never removed.
pub fn mark_conflict(tx: &rusqlite::Connection, a: i64, b: i64) -> Result<()> {
    for (x, y) in [(a, b), (b, a)] {
        let key = conflict_key(x);
        let have: Option<String> =
            match tx.query_row("SELECT value FROM meta WHERE key = ?1", [&key], |r| {
                r.get(0)
            }) {
                Ok(v) => Some(v),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(e.into()),
            };
        let mut ids: Vec<String> = have
            .map(|v| v.split(',').map(str::to_string).collect())
            .unwrap_or_default();
        if !ids.contains(&y.to_string()) {
            ids.push(y.to_string());
            tx.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
                rusqlite::params![key, ids.join(",")],
            )?;
        }
    }
    Ok(())
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
        let events = |what: &str| {
            db.count(&format!(
                "SELECT count(*) FROM store_event WHERE what = '{what}'"
            ))
            .unwrap()
        };
        assert_eq!(validate_anchors(&paths, &db).unwrap(), 0);
        assert_eq!(
            events("anchors"),
            0,
            "a pass that retires nothing leaves no event"
        );
        std::fs::write(tmp.path().join("a.rs"), "v2").unwrap();
        assert_eq!(validate_anchors(&paths, &db).unwrap(), 1);
        assert_eq!(
            events("anchors"),
            1,
            "the pass that retired a record left one"
        );
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
        assert_eq!(
            events("revoke"),
            1,
            "only the revoke that retired a record leaves an event"
        );
        assert!(!revoke(&db, 9999, "no such record").unwrap());
        assert_eq!(events("revoke"), 1, "a mistyped id leaves none");
        // A record a replacement holds retired is not touched and the answer is no, as it
        // always was; that the person revoked it is noted beside it, for the undo of an
        // agent's prompt to read.
        assert!(!revoke(&db, b, "not this one either").unwrap());
        let (invalid, reason, heir): (i64, String, i64) = db
            .conn
            .query_row(
                "SELECT invalid, invalid_reason, invalidated_by FROM record WHERE id=?1",
                [b],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((invalid, reason.as_str(), heir), (1, "superseded", c));
        assert_eq!(events("revoke"), 1);
        assert_eq!(
            db.count("SELECT count(*) FROM agent_retired WHERE held_by = -1")
                .unwrap(),
            1,
            "noted for b, not for the revoked d nor for the id that is no record"
        );
    }

    /// The validator reads its records, then hashes, then retires. A record another session
    /// writes during the hashing, anchored to a file as it is by then, is not judged against a
    /// hash taken before it existed; and a record re-anchored during the hashing is not retired
    /// on the hash it had when it was read. A record whose file did change is retired, and the
    /// pass leaves its event.
    #[test]
    fn the_validator_judges_no_record_against_a_hash_older_than_the_record() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::from_root(tmp.path());
        for d in paths.all_dirs() {
            std::fs::create_dir_all(d).unwrap();
        }
        let db = Db::open(&paths.db_path(), Mode::ReadWrite).unwrap();
        for f in ["a.rs", "b.rs", "c.rs"] {
            std::fs::write(tmp.path().join(f), "v1").unwrap();
        }
        let hash = |f: &str| file_hash(tmp.path(), f).unwrap();
        let changed = ins(&db, "decision", "one", "x", Some(("a.rs", &hash("a.rs"))));
        let reanchored = ins(&db, "decision", "two", "y", Some(("b.rs", &hash("b.rs"))));
        let untouched = ins(&db, "decision", "three", "z", Some(("c.rs", &hash("c.rs"))));
        std::fs::write(tmp.path().join("a.rs"), "v2").unwrap();
        std::fs::write(tmp.path().join("b.rs"), "v2").unwrap();
        let mut late = 0;
        let retired = validate_anchors_with(&paths, &db, || {
            // during the hashing: c.rs is edited and a new record is anchored to it as it is
            // now; the record on b.rs is anchored anew to b.rs as it is now
            std::fs::write(tmp.path().join("c.rs"), "v3").unwrap();
            late = ins(&db, "decision", "four", "w", Some(("c.rs", &hash("c.rs"))));
            db.conn
                .execute(
                    "UPDATE record SET anchor_hash = ?1 WHERE id = ?2",
                    rusqlite::params![hash("b.rs"), reanchored],
                )
                .unwrap();
        })
        .unwrap();
        assert_eq!(retired, 1);
        let invalid = |id: i64| -> i64 {
            db.conn
                .query_row("SELECT invalid FROM record WHERE id=?1", [id], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(
            (
                invalid(changed),
                invalid(reanchored),
                invalid(untouched),
                invalid(late)
            ),
            (1, 0, 0, 0)
        );
        assert_eq!(
            db.count("SELECT count(*) FROM store_event WHERE what = 'anchors'")
                .unwrap(),
            1
        );
        // the next run sees c.rs as it is now: the record read before the edit goes, the one
        // written after it stays
        assert_eq!(validate_anchors(&paths, &db).unwrap(), 1);
        assert_eq!((invalid(untouched), invalid(late)), (1, 0));
    }

    /// `muninn revoke` on a record a replacement had retired changes nothing, and the episode
    /// of that record's turn stays hidden: the read path hides it while the statement's reason
    /// is `superseded` (`recall::stale_turns`).
    #[test]
    fn revoking_a_replaced_record_leaves_it_and_its_twin_as_they_were() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), Mode::ReadWrite).unwrap();
        let ins = |id: i64, kind: &str, text: &str, tref: Option<&str>| {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at,transcript_ref) \
                     VALUES(?1,?2,?3,'is',?4,?4,'user_said',3,'s',?3,1,?5)",
                    rusqlite::params![id, kind, format!("k{id}"), text, tref],
                )
                .unwrap();
        };
        ins(1, "decision", "the pooler is pgpool", Some("t:1"));
        ins(
            2,
            "episode",
            "pooler pooler pooler pooler connection connection pooler pgpool",
            Some("t:1"),
        );
        ins(
            3,
            "decision",
            "the pooler is pgbouncer now, set by hand on every node",
            None,
        );
        for i in 5..12 {
            ins(
                i,
                "decision",
                &format!("filler record number {i} about nothing"),
                None,
            );
        }
        db.conn
            .execute(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=3, invalidated_via='said' WHERE id=1",
                [],
            )
            .unwrap();
        let terms = vec!["pooler".to_string(), "connection".to_string()];
        let served = || -> Vec<i64> {
            let mut ids: Vec<i64> =
                crate::recall::recall(&db, &terms, 8, &std::collections::HashSet::new())
                    .unwrap()
                    .iter()
                    .map(|h| h.id)
                    .collect();
            ids.sort_unstable();
            ids
        };
        let before = served();
        assert!(!before.contains(&2), "the twin is hidden: {before:?}");
        assert!(!revoke(&db, 1, "again").unwrap(), "nothing to revoke");
        let state: (i64, String, i64, String) = db
            .conn
            .query_row(
                "SELECT invalid, invalid_reason, invalidated_by, invalidated_via FROM record WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(state, (1, "superseded".to_string(), 3, "said".to_string()));
        assert_eq!(served(), before, "and the twin is still hidden");
        assert_eq!(
            db.count("SELECT count(*) FROM store_event").unwrap(),
            0,
            "a revoke that retired nothing leaves no event"
        );
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
            "muninn-capture/src/ingest.rs",
            "write path: dedup, supersession, topic inheritance",
        ),
        (
            "muninn-capture/src/agent.rs",
            "write path: records typed from an agent's prompt are retired",
        ),
        (
            "muninn-capture/src/agent/undo.rs",
            "write path: what an agent's prompt retired is given back",
        ),
        (
            "muninn-capture/src/remember.rs",
            "write path: `muninn remember` finds the record a turn already has",
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
        (
            "muninn-why/src/es.rs",
            "muninn why --all: the Spanish forms a retired record's words take, words only",
        ),
        ("muninn-bench/src/main.rs", "offline harness"),
        (
            "muninn-bench/src/replay.rs",
            "offline harness; rewinds a copy of a store to a prompt's moment",
        ),
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
