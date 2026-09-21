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
    pub symbols_indexed: usize,
    pub cues_derived: usize,
    pub variants_retired: usize,
    /// Records retired because the repository stopped holding the value they named.
    pub values_retired: usize,
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

/// `--since` for a watermark, or `None` when there is not one yet.
///
/// git parses `@0` as *now*, not as the epoch — an invalid approxidate falls back to the
/// current time — so a fresh store asked for `--since=@0` was told the repository had no
/// history at all, and only ever captured commits made after its first `maintain`. With no
/// watermark the range is simply left open and `-n` bounds it.
fn since_arg(since_ms: i64) -> Option<String> {
    let secs = since_ms / 1000;
    (secs > 0).then(|| format!("--since=@{}", secs.saturating_sub(1)))
}

/// Commits since the watermark become `decision` records (commit_linked, trust 2);
/// reverts retire the decision they undo and leave a `deadend`.
pub fn capture_git(paths: &ProjectPaths, db: &Db) -> muninn_core::Result<(usize, usize)> {
    // where the code is, which is not always where the store is: `MUNINN_SOURCE_ROOT`
    // separates them, and symbols, anchors and `scan` have always followed it. Git did not,
    // so a store kept outside its checkout read the history of the wrong directory.
    let root = paths.source_root();
    let since_ms: i64 = db
        .meta_get("git_watermark_ms")?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let now = now_ms();
    let mut args: Vec<String> = vec!["log".into()];
    args.extend(since_arg(since_ms));
    args.extend(
        [
            "--format=%x1e%h%x1f%s%x1f%ct%x1f%an",
            "--name-only",
            "--no-merges",
            "-n",
            "500",
        ]
        .map(String::from),
    );
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some(log) = git(&root, &argv) else {
        return Ok((0, 0));
    };
    let mut commits = 0usize;
    let mut reverts = 0usize;
    let tx = db.write_tx()?;
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

/// Identifier-like tokens of one diff line, lowercased: the words a manifest, an import
/// or a configuration key is made of. Deliberately generous — the set is only ever used
/// to *look up* values the store already holds, never to create one.
fn code_tokens(line: &str, out: &mut Vec<String>) {
    for raw in line.split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))) {
        let w = raw.trim_matches(|c: char| !c.is_alphanumeric());
        if w.chars().count() >= 3 && w.chars().any(|c| c.is_alphabetic()) {
            let w = w.to_lowercase();
            if !out.contains(&w) {
                out.push(w);
            }
        }
    }
}

/// What one commit did to the words of the code.
#[derive(Default)]
struct Commit {
    removed: Vec<String>,
    added: Vec<String>,
    /// hunks where one line became one line: (words out, words in)
    swaps: Vec<(Vec<String>, Vec<String>)>,
}

/// How many diff lines one `maintain` reads once it is caught up. A commit that rewrites a
/// lock file must not turn the write path into a linear scan of the repository's history.
const MAX_DIFF_LINES: usize = 4_000;

/// The first pass over a repository Muninn has never read reaches much further back: a store
/// installed into a project with history should learn what that project stopped using, and it
/// pays for it exactly once. Measured: with seventy commits between the swaps and the first
/// `maintain`, the 50-commit window costs two of the thirty held-out retirements.
const FIRST_PASS_COMMITS: usize = 500;
const FIRST_PASS_DIFF_LINES: usize = 40_000;

/// The decisions the code itself left behind (ENGINE.md §5.2).
///
/// Every lexical rule for noticing that a decision was replaced runs out at the same
/// place: 23 of 30 held-out replacements share no content word with the decision they
/// replace [Z5], and neither embeddings nor a stemmer reach them [Z3] [Z4]. "HashiCorp
/// Vault" and "AWS Secrets Manager" have nothing in common *as text*. They have something
/// in common as facts about a repository: one of them is in it, and then it is not.
///
/// So this reads the commits instead of the sentences. A value that the new commits took
/// out of the code, and that no tracked file holds any more, is a value the project has
/// stopped using; the record that named it is retired and points at the record the
/// same commit corroborates. Nothing is invented from the diff: a removed word only
/// matters when an active record already named it, and the replacement is only ever an
/// *existing* record that the same commit's added lines corroborate. A decision that
/// never touched the code — a release cadence, a review policy — is never affected,
/// because its value is not in the diff.
///
/// Write path only. Read hooks never run git.
pub fn capture_dropped_values(paths: &ProjectPaths, db: &Db) -> muninn_core::Result<usize> {
    let root = paths.source_root();
    let since_ms: i64 = db
        .meta_get("values_watermark_ms")?
        .and_then(|s| s.parse().ok())
        // its own watermark: `capture_git` has already moved its one to now by the time
        // this runs, and borrowing it would make the range empty on every call
        .unwrap_or(0);
    let now = now_ms();
    // one second of overlap (`since_arg`): `--since` has second granularity, so a commit
    // made in the same second as the last run would otherwise fall between two windows.
    // Re-reading a commit is free — a record already retired is not retired twice.
    let mut args: Vec<String> = vec!["log".into()];
    args.extend(since_arg(since_ms));
    let first_pass = since_ms == 0;
    let n = if first_pass { FIRST_PASS_COMMITS } else { 50 };
    args.extend(
        [
            "--no-merges",
            "-U0",
            "-p",
            "--no-color",
            "--format=%x1e%h",
            "-n",
        ]
        .map(String::from),
    );
    args.push(n.to_string());
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some(diff) = git(&root, &argv) else {
        return Ok(0);
    };
    db.meta_set("values_watermark_ms", &now.to_string())?;
    // one entry per commit, newest first: what it took out, what it put in, and the swaps —
    // hunks where exactly one line became exactly one other line. Keeping removals and
    // additions per commit is what makes the replacement specific: a range of commits pooled
    // into one bag pairs a value dropped here with a value added over there.
    let mut commits: Vec<Commit> = Vec::new();
    let mut hunk: (Vec<String>, Vec<String>, usize, usize) = (Vec::new(), Vec::new(), 0, 0);
    let max_lines = if first_pass {
        FIRST_PASS_DIFF_LINES
    } else {
        MAX_DIFF_LINES
    };
    let close_hunk = |commits: &mut Vec<Commit>,
                      hunk: &mut (Vec<String>, Vec<String>, usize, usize)| {
        // a one-for-one hunk is what a changed value looks like: one line became one line.
        // Anything larger is a rewrite, and pairing across it would be guessing.
        if hunk.2 == 1 && hunk.3 == 1 {
            if let Some(c) = commits.last_mut() {
                c.swaps
                    .push((std::mem::take(&mut hunk.0), std::mem::take(&mut hunk.1)));
            }
        }
        *hunk = (Vec::new(), Vec::new(), 0, 0);
    };
    for line in diff.lines().take(max_lines) {
        if line.starts_with('\u{1e}') {
            close_hunk(&mut commits, &mut hunk);
            commits.push(Commit::default());
            continue;
        }
        if commits.is_empty() {
            continue;
        }
        if line.starts_with("@@")
            || line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("diff --git")
        {
            close_hunk(&mut commits, &mut hunk);
            continue;
        }
        if let Some(rest) = line.strip_prefix('-') {
            code_tokens(rest, &mut commits.last_mut().unwrap().removed);
            code_tokens(rest, &mut hunk.0);
            hunk.2 += 1;
        } else if let Some(rest) = line.strip_prefix('+') {
            code_tokens(rest, &mut commits.last_mut().unwrap().added);
            code_tokens(rest, &mut hunk.1);
            hunk.3 += 1;
        }
    }
    close_hunk(&mut commits, &mut hunk);
    commits.retain(|c| !c.removed.is_empty());
    if commits.is_empty() {
        return Ok(0);
    }
    // the values the store actually holds, so the diff's vocabulary is never the subject
    let live: Vec<(i64, String)> = {
        let mut st = db.conn.prepare(
            "SELECT id, object FROM record WHERE invalid=0 AND length(body) <= 700 AND ( \
                 (kind='decision' AND origin IN ('user_said','review_accepted')) \
                 OR kind='episode') \
             ORDER BY created_at DESC, id DESC LIMIT 400",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.flatten().collect()
    };
    let words_of: Vec<(i64, Vec<String>)> = live
        .iter()
        // every content word of the statement, not only the ones that look like a product
        // name: `nock` and `bash` are values too, and nothing about their spelling says so
        .map(|(id, obj)| (*id, muninn_capture::extract::topic_words(obj)))
        .collect();
    let mut n = 0;
    let mut gone_cache: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    let tx = db.write_tx()?;
    for (id, words) in &words_of {
        // Two kinds of evidence, either of which is enough.
        //
        // A **swap**: one line became one line, the old value was on the first and is not on
        // the second. The hunk itself says what replaced what, so the rest of the repository
        // does not have to agree — which matters, because a CHANGELOG entry, a lock file or a
        // comment keeps a word alive long after the project stopped using it. Measured: with
        // the old value left in one untouched file, the disappearance test alone retires
        // nothing at all, on either held-out set.
        //
        // A **disappearance**: a commit took the word out and no tracked file holds it any
        // more. Weaker evidence about a stronger fact, and it catches a value that was deleted
        // rather than replaced.
        let swap = commits.iter().find_map(|c| {
            let sw = c
                .swaps
                .iter()
                .find(|(out, inn)| words.iter().any(|w| out.contains(w) && !inn.contains(w)))?;
            Some((c, sw))
        });
        let (commit, gone) = match &swap {
            Some((c, (out, _))) => (*c, words.iter().find(|w| out.contains(w)).unwrap()),
            None => {
                let Some((c, gone)) = commits.iter().find_map(|c| {
                    let w = words.iter().find(|w| c.removed.contains(w))?;
                    Some((c, w))
                }) else {
                    continue;
                };
                // still somewhere in the tree? then the project has not dropped it
                let absent = *gone_cache.entry(gone.to_string()).or_insert_with(|| {
                    git(&root, &["grep", "-F", "-q", "-i", "--", gone]).is_none()
                });
                if !absent {
                    continue;
                }
                (c, gone)
            }
        };
        // the replacement is an existing record that this commit's added lines corroborate —
        // the added line of the swap itself where there was one
        let added: &[String] = match &swap {
            Some((_, (_, inn))) => inn,
            None => &commit.added,
        };
        let heir: Option<i64> = words_of
            .iter()
            .find(|(other, w)| other != id && w.iter().any(|w| added.contains(w)))
            .map(|(i, _)| *i);
        n += tx.execute(
            "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?2 \
             WHERE id=?1 AND invalid=0",
            rusqlite::params![id, heir],
        )?;
        // the same turn's literal episode states it too, and serving that would put the
        // dropped value back in front of the agent
        let tref: Option<String> = tx
            .query_row("SELECT transcript_ref FROM record WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .ok()
            .flatten();
        if let Some(tref) = tref {
            tx.execute(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?2 \
                 WHERE invalid=0 AND transcript_ref=?1 AND length(body) <= 700",
                rusqlite::params![tref, heir],
            )?;
        }
        if let Some(h) = heir {
            // the dropped value is the one word the heir must not restate
            muninn_capture::ingest::inherit_topic_hiding(&tx, h, std::slice::from_ref(gone))?;
        }
    }
    tx.commit()?;
    Ok(n)
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
    // not a hook: it may wait for a concurrent writer instead of dropping work
    let db = match Db::open_with_busy(&paths.db_path(), Mode::ReadWrite, 5_000) {
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
    st.ingested += crate::sessions::ingest_pending(paths, &db);
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
    match capture_dropped_values(paths, &db) {
        Ok(n) => st.values_retired = n,
        Err(e) => output::err(&format!("muninn maintain: dropped values: {e}")),
    }
    if st.commits + st.reverts + st.values_retired > 0
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
    if let Ok(n) = muninn_core::cue::derive_missing(&db) {
        st.cues_derived = n;
    }
    // symbol graph: only files whose content hash changed are re-parsed
    match muninn_symbols::rebuild(&db, &paths.source_root(), false) {
        Ok(s) => st.symbols_indexed = s.files_indexed,
        Err(e) => output::err(&format!("muninn maintain: symbols: {e}")),
    }
    // the sidecar catches up here, never in a read hook; no model → stays cold
    if let Ok(emb) = muninn_embed::Embedder::load_default() {
        match muninn_embed::embed_pending(&db, &emb, false) {
            Ok(s) => {
                st.embedded = s.embedded;
                st.variants_retired = s.variants_retired;
            }
            Err(e) => output::err(&format!("muninn maintain: embed: {e}")),
        }
    }
    let _ = db.record_quick_check(3_600_000);
    st.ms = t0.elapsed().as_millis();
    if json {
        output::json(&st);
    } else {
        output::out(&format!(
            "maintain: {} ingested, {} commit(s), {} revert(s), {} anchor(s) retired, {} file(s) projected, {} embedded ({} variant(s) retired), {} symbol file(s) indexed, {} ms",
            st.ingested, st.commits, st.reverts, st.anchors_retired, st.projected, st.embedded, st.variants_retired, st.symbols_indexed, st.ms
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

#[cfg(test)]
mod tests {
    use super::*;

    /// git reads `@0` as *now*, not as the epoch, so a fresh store — watermark 0 — was told
    /// its repository had no history and captured nothing until its second `maintain`.
    #[test]
    fn no_watermark_means_no_since_at_all() {
        assert_eq!(since_arg(0), None);
        assert_eq!(since_arg(999), None); // under a second is still no watermark
        assert_eq!(since_arg(10_000), Some("--since=@9".into())); // one second of overlap
    }

    #[test]
    fn code_tokens_are_the_words_a_manifest_is_made_of() {
        let mut out = Vec::new();
        code_tokens("  \"nock\": \"^13.5.1\",", &mut out);
        assert!(out.contains(&"nock".to_string()));
        code_tokens("import { setupServer } from 'msw/node'", &mut out);
        assert!(out.contains(&"msw".to_string()) || out.contains(&"msw/node".to_string()));
        // no single letters, no pure punctuation
        assert!(out.iter().all(|w| w.chars().count() >= 3));
    }
}
