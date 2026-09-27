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
                muninn_capture::sole_anchor(&files),
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
                    muninn_capture::sole_anchor(&files),
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
    let push = |w: &str, out: &mut Vec<String>| {
        if w.chars().count() >= 3 && w.chars().any(|c| c.is_alphabetic()) {
            let w = w.to_lowercase();
            if !out.contains(&w) {
                out.push(w);
            }
        }
    };
    for raw in line.split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))) {
        let w = raw.trim_matches(|c: char| !c.is_alphanumeric());
        push(w, out);
        // A record's words come from `topic_words`, which keeps only alphanumerics, so it
        // holds `async std` where the manifest line holds `async-std`. Keeping the joined
        // form alone meant a hyphenated value — `async-std`, `axe-core`, `date-fns` — never
        // matched the decision that named it, in either direction. Both forms go in.
        if w.contains(['-', '.', '_', '/']) {
            for part in w.split(['-', '.', '_', '/']) {
                push(part, out);
            }
        }
    }
}

#[derive(Default)]
struct Hunk {
    out: Vec<String>,
    inn: Vec<String>,
    was: String,
    line: String,
    outs: usize,
    ins: usize,
}

/// A hunk where one line became one line — what a changed value looks like in a diff.
struct Swap {
    out: Vec<String>,
    inn: Vec<String>,
    /// the line that went, trimmed
    was: String,
    /// the line that replaced it, trimmed: the literal the project now holds
    line: String,
    file: String,
}

/// What one commit did to the words of the code.
#[derive(Default)]
struct Commit {
    hash: String,
    created_at: i64,
    removed: Vec<String>,
    added: Vec<String>,
    swaps: Vec<Swap>,
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

/// How many "is this word still anywhere in the tree?" questions one `maintain` may ask.
/// Each is a `git grep` over the working tree — 36 ms on this repository, more on a large
/// one — and the number of candidate words grows with the store, so without a bound a busy
/// commit range could hold the write path for minutes; `Stop` runs it inline. Records are
/// considered newest first, so the bound drops the oldest questions. The swap rule needs no
/// grep at all and is not limited by this.
const MAX_TREE_CHECKS: usize = 32;

/// How many tracked files may hold a word before it stops being a *value* and starts being
/// vocabulary. A dependency name lives in a manifest and perhaps a lock file; `delivered`,
/// `session` or `path` live everywhere. Without this the rule fired on the first real store
/// it met: a record saying "`delivered` now looks the file up by session id" was retired
/// because a commit's diff happened to change a line containing the word `delivered`.
/// A word the record itself spells like a name — an inner capital, a digit, a dot or a
/// hyphen — is exempt, because that is a value however often it appears.
const MAX_VALUE_FILES: usize = 3;

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
            "--format=%x1e%h%x1f%ct",
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
    let mut hunk = Hunk::default();
    let mut file = String::new();
    // A file whose destination is /dev/null was deleted, and a value that disappeared with
    // its file is not evidence that the decision changed — the file may have been moved,
    // renamed past git's similarity threshold, or split. Retiring on it loses a decision
    // with no heir: measured on the v5 grid, one commit removing `config/decisions/` retired
    // five live decisions, four of them the only record of their answer, and the cells that
    // asked those four questions were then given nothing. False retirement is the worst thing
    // a memory can do, and loop 8's gate is 0/30; a deleted file buys the conservative side.
    let mut file_gone = false;
    let max_lines = if first_pass {
        FIRST_PASS_DIFF_LINES
    } else {
        MAX_DIFF_LINES
    };
    let close_hunk = |commits: &mut Vec<Commit>, hunk: &mut Hunk, file: &str| {
        // a one-for-one hunk is what a changed value looks like: one line became one line.
        // Anything larger is a rewrite, and pairing across it would be guessing.
        if hunk.outs == 1 && hunk.ins == 1 {
            if let Some(c) = commits.last_mut() {
                c.swaps.push(Swap {
                    out: std::mem::take(&mut hunk.out),
                    inn: std::mem::take(&mut hunk.inn),
                    was: std::mem::take(&mut hunk.was),
                    line: std::mem::take(&mut hunk.line),
                    file: file.to_string(),
                });
            }
        }
        *hunk = Hunk::default();
    };
    for line in diff.lines().take(max_lines) {
        if let Some(h) = line.strip_prefix('\u{1e}') {
            close_hunk(&mut commits, &mut hunk, &file);
            let (hash, ct) = h.trim().split_once('\u{1f}').unwrap_or((h.trim(), ""));
            commits.push(Commit {
                hash: hash.chars().take(7).collect(),
                // the record is dated when the code changed, not when Muninn noticed
                created_at: ct.parse::<i64>().map(|s| s * 1000).unwrap_or(now),
                ..Commit::default()
            });
            continue;
        }
        if commits.is_empty() {
            continue;
        }
        if let Some(path) = line.strip_prefix("+++ b/") {
            close_hunk(&mut commits, &mut hunk, &file);
            file = path.trim().to_string();
            file_gone = false;
            continue;
        }
        if line.starts_with("+++ /dev/null") {
            close_hunk(&mut commits, &mut hunk, &file);
            file_gone = true;
            continue;
        }
        if line.starts_with("@@")
            || line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("diff --git")
        {
            close_hunk(&mut commits, &mut hunk, &file);
            continue;
        }
        if let Some(rest) = line.strip_prefix('-') {
            if file_gone {
                continue;
            }
            code_tokens(rest, &mut commits.last_mut().unwrap().removed);
            code_tokens(rest, &mut hunk.out);
            hunk.was = rest.trim().to_string();
            hunk.outs += 1;
        } else if let Some(rest) = line.strip_prefix('+') {
            code_tokens(rest, &mut commits.last_mut().unwrap().added);
            code_tokens(rest, &mut hunk.inn);
            hunk.line = rest.trim().to_string();
            hunk.ins += 1;
        }
    }
    close_hunk(&mut commits, &mut hunk, &file);
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
    // every content word of the statement, not only the ones that look like a product name —
    // `nock` and `bash` are values too, and nothing about their spelling says so — with the
    // name-shaped ones kept apart, because they are exempt from the commonness test below
    let words_of: Vec<(i64, Vec<String>, Vec<String>)> = live
        .iter()
        .map(|(id, obj)| {
            (
                *id,
                muninn_capture::extract::topic_words(obj),
                muninn_capture::extract::name_tokens(obj),
            )
        })
        .collect();
    let object_of = |id: i64| -> String {
        live.iter()
            .find(|(i, _)| *i == id)
            .map(|(_, o)| o.clone())
            .unwrap_or_default()
    };
    let mut n = 0;
    let mut gone_cache: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    let mut common_cache: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    let tx = db.write_tx()?;
    for (id, words, names) in &words_of {
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
        //
        // A quantity is read as a third case inside the swap: the unit stays on both lines and
        // the number is not a word at all, so `3 attempts` → `7 attempts` leaves nothing that
        // went away. The slot is the unit and the value is the number, exactly as in the
        // conversational rule, and the hunk holds both lines.
        let quantities = muninn_capture::extract::quantity_slots(&object_of(*id));
        let swap = commits.iter().find_map(|c| {
            let sw = c.swaps.iter().find(|sw| {
                words
                    .iter()
                    .any(|w| sw.out.contains(w) && !sw.inn.contains(w))
                    || {
                        let (was, now) = (
                            muninn_capture::extract::quantity_slots(&sw.was),
                            muninn_capture::extract::quantity_slots(&sw.line),
                        );
                        quantities.iter().any(|(unit, num)| {
                            was.iter().any(|(u, n)| u == unit && n == num)
                                && now.iter().any(|(u, n)| u == unit && n != num)
                        })
                    }
            })?;
            Some((c, sw))
        });
        let (commit, gone) = match &swap {
            // the word that went — or, on the quantity path where nothing went, the unit,
            // which is the slot. `unwrap_or(&words[0])` would have been an eager index into a
            // vector that can be empty.
            Some((c, sw)) => {
                let w = words
                    .iter()
                    .find(|w| sw.out.contains(w) && !sw.inn.contains(w))
                    .or_else(|| words.iter().find(|w| sw.out.contains(w)))
                    .or_else(|| words.first());
                let Some(w) = w else { continue };
                (*c, w)
            }
            None => {
                let Some((c, gone)) = commits.iter().find_map(|c| {
                    let w = words.iter().find(|w| c.removed.contains(w))?;
                    Some((c, w))
                }) else {
                    continue;
                };
                // still somewhere in the tree? then the project has not dropped it
                let known = gone_cache.get(gone).copied();
                let absent = match known {
                    Some(v) => v,
                    None if gone_cache.len() >= MAX_TREE_CHECKS => continue,
                    None => {
                        let v = git(&root, &["grep", "-F", "-q", "-i", "--", gone]).is_none();
                        gone_cache.insert(gone.to_string(), v);
                        v
                    }
                };
                if !absent {
                    continue;
                }
                (c, gone)
            }
        };
        // the replacement is an existing record that this commit's added lines corroborate —
        // the added line of the swap itself where there was one
        let added: &[String] = match &swap {
            Some((_, sw)) => &sw.inn,
            None => &commit.added,
        };
        // A word the repository uses all over the place is its vocabulary, not the value of
        // a decision. The record's own spelling exempts a name.
        if !names.contains(gone) {
            let too_common = *common_cache.entry(gone.to_string()).or_insert_with(|| {
                let out = git(&root, &["grep", "-F", "-i", "-l", "--", gone]).unwrap_or_default();
                out.lines().filter(|l| !l.trim().is_empty()).count() > MAX_VALUE_FILES
            });
            if too_common {
                continue;
            }
        }
        let heir: Option<i64> = words_of
            .iter()
            .find(|(other, w, _)| other != id && w.iter().any(|w| added.contains(w)))
            .map(|(i, _, _)| *i);
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
            muninn_capture::ingest::inherit_topic(&tx, h)?;
        }
        {
            // What the file now holds, whether or not anyone said it: it now holds this line, and that is
            // an observation with a commit behind it, not a guess — `commit_linked`, trust 2,
            // anchored to the file, keyed on the retired decision's own topic words so the
            // question that used to reach the old answer reaches this one. Written only when a
            // swap actually retired something, so a repository's ordinary churn creates
            // nothing: at most one record per retirement.
            {
                if let Some((_, sw)) = &swap {
                    // every word the swap took out, not only the one that matched first:
                    // `code_tokens` emits a hyphenated value both joined and split, so
                    // `async-std` contributes `async` and `std`, and filtering on the single
                    // `gone` word left keys like `said:change:runtime std` — half the retired
                    // value, as the topic of the record that replaced it.
                    let topic: Vec<&str> = words
                        .iter()
                        .filter(|w| *w != gone && !(sw.out.contains(w) && !sw.inn.contains(w)))
                        .map(String::as_str)
                        .collect();
                    let object = muninn_core::sanitize::truncate_chars(&sw.line, 160).to_string();
                    let body = muninn_capture::redact::redact(&format!(
                        "commit {}: {} now reads {}\n",
                        commit.hash, sw.file, object
                    ));
                    let subject = format!("said:change:{}", topic.join(" "));
                    let hash =
                        blake3::hash(format!("decision|{subject}|is|{object}|{body}").as_bytes())
                            .to_hex()
                            .to_string();
                    tx.execute(
                        "INSERT OR IGNORE INTO record(kind, subject, relation, object, body, origin, trust, anchor_path, session_id, transcript_ref, dedup_hash, created_at) \
                         VALUES('decision', ?1, 'is', ?2, ?3, 'commit_linked', 2, ?4, 'git', ?5, ?6, ?7)",
                        rusqlite::params![
                            subject,
                            object,
                            body,
                            sw.file,
                            format!("git:{}", commit.hash),
                            hash,
                            commit.created_at
                        ],
                    )?;
                }
            }
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
    // Tidy the full-text index. `Stop` writes a handful of records per turn, so a store at
    // the schema's cap is the product of thousands of small commits and its FTS5 index is in
    // as many segments: measured on 20 000 records written in 2 000 transactions of ten, 918
    // segments where a bulk load leaves a few, and a BM25 query costing 8.56 ms against
    // 6.54 ms once merged. Nothing in this engine has ever run this — `optimize` appears in
    // the bench fixture and nowhere else — so the degradation was permanent and grew.
    //
    // It belongs here and not on a read path: 22 ms the first time on that store, and 0 ms
    // every time after, because there is nothing left to merge.
    if let Err(e) = db
        .conn
        .execute_batch("INSERT INTO record_fts(record_fts) VALUES('optimize');")
    {
        output::err(&format!("muninn maintain: fts optimize: {e}"));
    }
    if st.commits + st.reverts + st.values_retired > 0
        || db
            .meta_get("records_changed_since_project")
            .ok()
            .flatten()
            .as_deref()
            == Some("1")
    {
        match muninn_core::project::project(paths, &db, &[]) {
            Ok(n) => {
                st.projected = n;
                let _ = db.meta_set("records_changed_since_project", "0");
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
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.arg("--cwd")
        .arg(&paths.root)
        .arg("maintain")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: no console, and a Ctrl+C sent to
        // the harness does not reach it
        cmd.creation_flags(0x0000_0008 | 0x0000_0200);
        // Windows children inherit every inheritable handle, and the hook's own stdin,
        // stdout and stderr are pipes the harness reads to EOF: the writer would hold them
        // open and the harness would wait for it, 5.7 s on a locked store in CI. The hook's
        // handles stop being inheritable for the length of the spawn.
        let _guard = win::NoInherit::std_handles();
        let _ = cmd.spawn();
    }
    #[cfg(not(windows))]
    let _ = cmd.spawn();
}

#[cfg(windows)]
mod win {
    type Handle = *mut core::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(which: u32) -> Handle;
        fn SetHandleInformation(h: Handle, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 1;
    // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE: (DWORD)-10, -11, -12
    const STD: [u32; 3] = [-10i32 as u32, -11i32 as u32, -12i32 as u32];

    /// Clears the inherit flag on the process's standard handles, and sets it back on drop.
    pub struct NoInherit(Vec<Handle>);

    impl NoInherit {
        pub fn std_handles() -> Self {
            let mut cleared = Vec::new();
            for which in STD {
                // SAFETY: plain Win32 calls on this process's own standard handles; a null
                // or INVALID_HANDLE_VALUE handle is skipped, and a failed call changes nothing
                unsafe {
                    let h = GetStdHandle(which);
                    if !h.is_null()
                        && h as isize != -1
                        && SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0) != 0
                    {
                        cleared.push(h);
                    }
                }
            }
            NoInherit(cleared)
        }
    }

    impl Drop for NoInherit {
        fn drop(&mut self) {
            for &h in &self.0 {
                // SAFETY: the same handles, restored to how they were
                unsafe {
                    SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
                }
            }
        }
    }
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
