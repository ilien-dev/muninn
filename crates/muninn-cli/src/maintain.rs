//! `muninn maintain`: the asynchronous write path that runs outside any hook —
//! fold the hook logs, resume a pending ingest from its watermark, capture commits and
//! reverts from git since the last run (ENGINE.md §3), project Markdown, then let the
//! embedding sidecar catch up. Spawned detached by SessionStart; run inline by Stop.

use crate::output;
use muninn_core::db::now_ms;
use muninn_core::{Db, Mode, ProjectPaths};
use serde::Serialize;
use std::process::Command;

#[derive(Debug, Default, Serialize)]
pub struct MaintainStats {
    pub ingested: usize,
    pub commits: usize,
    pub reverts: usize,
    pub projected: usize,
    pub embedded: usize,
    pub anchors_retired: usize,
    pub ms: u128,
}

fn git(root: &std::path::Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Commits since the watermark become `decision` records (commit_linked, trust 2);
/// reverts retire the decision they undo and leave a `deadend`.
pub fn capture_git(paths: &ProjectPaths, db: &Db) -> muninn_core::Result<(usize, usize)> {
    let since_ms: i64 = db
        .meta_get("git_watermark_ms")?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let since = format!("--since=@{}", since_ms / 1000);
    let now = now_ms();
    let Some(log) = git(
        &paths.root,
        &[
            "log",
            &since,
            "--format=%x1e%h%x1f%s%x1f%ct%x1f%an",
            "--name-only",
            "--no-merges",
            "-n",
            "500",
        ],
    ) else {
        return Ok((0, 0));
    };
    let mut commits = 0usize;
    let mut reverts = 0usize;
    let tx = db.conn.unchecked_transaction()?;
    for block in log.split('\x1e').filter(|b| !b.trim().is_empty()) {
        let mut lines = block.lines();
        let Some(head) = lines.next() else { continue };
        let mut f = head.split('\x1f');
        let (Some(hash), Some(subject), Some(ct)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let files: Vec<String> = lines
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .take(20)
            .map(String::from)
            .collect();
        let created: i64 = ct.parse::<i64>().map(|s| s * 1000).unwrap_or(now);
        let short: String = hash.chars().take(7).collect();
        let body = muninn_capture::redact::redact(&format!(
            "commit {short}: {subject}\nfiles: {}\n",
            files.join(", ")
        ));
        let body: String = body
            .chars()
            .take(muninn_core::caps::MAX_BODY_CHARS)
            .collect();
        let object: String = subject.chars().take(160).collect();
        let hash_d = blake3::hash(format!("decision|commit:{short}|is|{object}|{body}").as_bytes())
            .to_hex()
            .to_string();
        let n = tx.execute(
            "INSERT OR IGNORE INTO record(kind, subject, relation, object, body, origin, trust, anchor_path, session_id, transcript_ref, dedup_hash, created_at) \
             VALUES('decision', ?1, 'is', ?2, ?3, 'commit_linked', 2, ?4, 'git', ?5, ?6, ?7)",
            rusqlite::params![
                format!("commit:{short}"),
                object,
                body,
                files.first(),
                format!("git:{hash}"),
                hash_d,
                created
            ],
        )?;
        if n == 1 {
            commits += 1;
        }
        // "Revert \"<subject>\"": the reverted decision is retired, the dead end kept
        if let Some(rest) = subject.strip_prefix("Revert \"") {
            let orig = rest.trim_end_matches('"');
            let retired = tx.execute(
                "UPDATE record SET invalid=1, invalid_reason='reverted' WHERE invalid=0 AND kind='decision' AND subject LIKE 'commit:%' AND object=?1",
                [orig],
            )?;
            let de_body = format!("reverted by {short}: {orig}\n");
            let de_hash = blake3::hash(
                format!("deadend|revert:{short}|tried_and_failed|{orig}|{de_body}").as_bytes(),
            )
            .to_hex()
            .to_string();
            tx.execute(
                "INSERT OR IGNORE INTO record(kind, subject, relation, object, body, origin, trust, anchor_path, session_id, transcript_ref, dedup_hash, created_at) \
                 VALUES('deadend', ?1, 'tried_and_failed', ?2, ?3, 'tool_observed', 1, ?4, 'git', ?5, ?6, ?7)",
                rusqlite::params![
                    format!("revert:{short}"),
                    orig.chars().take(160).collect::<String>(),
                    de_body,
                    files.first(),
                    format!("git:{hash}"),
                    de_hash,
                    created
                ],
            )?;
            reverts += retired.max(1);
        }
    }
    tx.commit()?;
    db.meta_set("git_watermark_ms", &now.to_string())?;
    Ok((commits, reverts))
}

/// One writer at a time: a second `maintain` finds the lock held and exits at once.
fn lock(paths: &ProjectPaths) -> Option<std::fs::File> {
    let p = paths.log_dir().join("maintain.lock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&p)
        .ok()?;
    match f.try_lock() {
        Ok(()) => Some(f),
        Err(_) => None,
    }
}

pub fn run(paths: &ProjectPaths, json: bool) -> i32 {
    let t0 = std::time::Instant::now();
    let mut st = MaintainStats::default();
    if !paths.is_initialised() {
        return 0;
    }
    let Some(_guard) = lock(paths) else {
        if json {
            output::json(&serde_json::json!({"skipped": "another maintain is running"}));
        }
        return 0;
    };
    let db = match Db::open(&paths.db_path(), Mode::ReadWrite) {
        Ok(db) => db,
        Err(e) => {
            output::err(&format!("muninn maintain: {e}"));
            return 1;
        }
    };
    if let Err(e) = muninn_core::heartbeat::fold_into_db(paths, &db) {
        output::err(&format!("muninn maintain: heartbeat fold: {e}"));
    }
    if let Err(e) = crate::delivery::fold_into_db(paths, &db) {
        output::err(&format!("muninn maintain: delivery fold: {e}"));
    }
    // resume: the last transcript from its watermark (a hook that expired left it)
    if let Ok(Some(p)) = db.meta_get("last_transcript_path") {
        let p = std::path::PathBuf::from(p);
        if p.is_file() {
            match muninn_capture::ingest::ingest_transcript_with(&db, &p, "resume", Some(paths)) {
                Ok(s) => {
                    st.ingested += s.inserted;
                    st.projected += s.projected;
                }
                Err(e) => output::err(&format!("muninn maintain: ingest: {e}")),
            }
        }
    }
    match muninn_core::filter::validate_anchors(paths, &db) {
        Ok(n) => st.anchors_retired = n,
        Err(e) => output::err(&format!("muninn maintain: anchors: {e}")),
    }
    match capture_git(paths, &db) {
        Ok((c, r)) => {
            st.commits = c;
            st.reverts = r;
        }
        Err(e) => output::err(&format!("muninn maintain: git: {e}")),
    }
    if st.commits + st.reverts > 0
        || db
            .meta_get("records_changed_since_render")
            .ok()
            .flatten()
            .as_deref()
            == Some("1")
    {
        match muninn_core::project::project(paths, &db, &[]) {
            Ok(n) => {
                st.projected = n;
                let _ = db.meta_set("records_changed_since_render", "0");
            }
            Err(e) => output::err(&format!("muninn maintain: project: {e}")),
        }
    }
    // the sidecar catches up here, never in a read hook; no model → stays cold
    if let Ok(emb) = muninn_embed::Embedder::load_default() {
        match muninn_embed::embed_pending(&db, &emb, false) {
            Ok(s) => st.embedded = s.embedded,
            Err(e) => output::err(&format!("muninn maintain: embed: {e}")),
        }
    }
    let _ = db.record_quick_check(3_600_000);
    st.ms = t0.elapsed().as_millis();
    if json {
        output::json(&st);
    } else {
        output::out(&format!(
            "maintain: {} ingested, {} commit(s), {} revert(s), {} anchor(s) retired, {} file(s) projected, {} embedded, {} ms",
            st.ingested, st.commits, st.reverts, st.anchors_retired, st.projected, st.embedded, st.ms
        ));
    }
    0
}

/// Spawn only if the last spawn is older than `min_age_s` (stamp file in the log dir,
/// the one place a read hook may write).
pub fn spawn_detached_throttled(paths: &ProjectPaths, min_age_s: u64) {
    let stamp = paths.log_dir().join("maintain.stamp");
    if let Ok(md) = std::fs::metadata(&stamp) {
        if let Ok(age) = md
            .modified()
            .and_then(|m| m.elapsed().map_err(std::io::Error::other))
        {
            if age.as_secs() < min_age_s {
                return;
            }
        }
    }
    let _ = std::fs::write(&stamp, now_ms().to_string());
    spawn_detached(paths);
}

/// Start `muninn maintain` as a detached process (no inherited stdio, own process
/// group) so a read hook returns at once and the harness never waits on the write path.
pub fn spawn_detached(paths: &ProjectPaths) {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.arg("--cwd")
        .arg(&paths.root)
        .arg("maintain")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    let _ = cmd.spawn();
}
