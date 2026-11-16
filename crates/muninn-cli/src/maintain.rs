//! `muninn maintain`: the asynchronous write path that runs outside any hook —
//! fold the hook logs, resume a pending ingest from its watermark, capture commits and
//! reverts from git since the last run (ENGINE.md §3), project Markdown. Spawned detached by
//! SessionStart; run inline by Stop.

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
    pub anchors_retired: usize,
    pub cues_derived: usize,
    /// Records retired because the repository stopped holding the value they named.
    pub values_retired: usize,
    /// Typed records retired because a compaction summary, not the user, put them on record.
    pub summary_records_retired: usize,
    /// Records put back after a bug up to 1.1.0 retired them, or after 1.2.0's commit-diff
    /// rule retired a record that had already been put back once.
    pub records_restored: usize,
    /// The full-text index disagreed with the active rows and was rebuilt.
    pub index_rebuilt: bool,
    /// Records dated past tomorrow in the wrong unit, brought back to their own date.
    pub dates_repaired: usize,
    /// The file was rewritten without the pages of the tables schema v5 dropped.
    pub store_compacted: bool,
    /// Lines taken out of the session log: repeats of a session and transcript already noted.
    pub session_lines_dropped: usize,
    /// Rows older than thirty days taken out of the `heartbeat` table.
    pub heartbeats_dropped: usize,
    pub ms: u128,
}

fn git(root: &std::path::Path, args: &[&str]) -> Option<String> {
    hold_git_for_a_test(args);
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

/// A seam for one test (`write_path.rs`): before git runs, leave a note naming the subcommand
/// and wait until the note is taken away, so the test can try the store's write lock exactly
/// then. Debug builds only, and only when the variable names a directory.
fn hold_git_for_a_test(args: &[&str]) {
    #[cfg(debug_assertions)]
    if let Ok(dir) = std::env::var("MUNINN_TEST_HOLD_GIT") {
        let note = std::path::Path::new(&dir).join("held");
        if std::fs::write(&note, args.first().copied().unwrap_or_default()).is_ok() {
            for _ in 0..3000 {
                if !note.exists() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = args;
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
    // separates them, and anchors and `scan` have always followed it. Git did not,
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
            // the body comes last and ends at \x1d: it can span lines, and it is where `git
            // revert` names the commit it reverts by hash
            "--format=%x1e%h%x1f%s%x1f%ct%x1f%an%x1f%b%x1d",
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
    // Oldest first. `git log` lists newest first, so a commit and its revert read in one pass
    // met the revert before the commit was on record, and the revert retired nothing (or an
    // earlier commit that happened to share the subject).
    let mut blocks: Vec<&str> = log.split('\x1e').filter(|b| !b.trim().is_empty()).collect();
    blocks.reverse();
    for block in blocks {
        let (head, rest) = block.split_once('\x1d').unwrap_or((block, ""));
        let mut f = head.split('\x1f');
        let (Some(hash), Some(subject), Some(ct)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let message_body = f.nth(1).unwrap_or("");
        let files: Vec<String> = rest
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .take(20)
            .map(String::from)
            .collect();
        let created: i64 = ct
            .parse::<i64>()
            .ok()
            .and_then(|s| muninn_core::db::sane_created_at(s.saturating_mul(1000), now))
            .unwrap_or(now);
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
        // "Revert \"<subject>\"": every record of the reverted commit is retired, the dead end
        // kept, and what those records had retired is put back
        if let Some(rest) = subject.strip_prefix("Revert \"") {
            let orig = rest.trim_end_matches('"');
            let retired = retire_reverted(&tx, &short, message_body, orig, created)?;
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
    // `Stop` runs this too, outside `maintain`: what it stored or retired has no file yet,
    // and no ingest says so for it any more
    if commits + reverts > 0 {
        db.meta_set("records_changed_since_project", "1")?;
    }
    Ok((commits, reverts))
}

/// Retire every record of the commit a revert undid, and put back what those records retired.
///
/// The commit is found by the hash `git revert` writes into the message body; matching by
/// subject, which 1.2.0 did, retired a later re-application of the same change instead of the
/// commit that was reverted. The subject is the fallback for a revert written by hand, cut as
/// the record's object is cut, and then only the newest such commit made before the revert.
/// A reverted commit's records are its own entry from `git log` and the observation its swap
/// wrote; anything that observation had retired was retired by a change the revert took back.
fn retire_reverted(
    tx: &rusqlite::Connection,
    revert_short: &str,
    message_body: &str,
    orig: &str,
    created: i64,
) -> muninn_core::Result<usize> {
    let target: Option<String> = message_body
        .split("This reverts commit ")
        .nth(1)
        .map(|t| {
            t.chars()
                .take_while(char::is_ascii_hexdigit)
                .take(7)
                .collect::<String>()
        })
        .filter(|h| h.len() == 7);
    muninn_core::db::note_event(tx, "revert")?;
    let ids: Vec<i64> = match &target {
        Some(h) => tx
            .prepare(
                "SELECT id FROM record WHERE invalid=0 AND origin='commit_linked' \
                 AND (subject = 'commit:' || ?1 OR substr(transcript_ref, 1, 11) = 'git:' || ?1)",
            )?
            .query_map([h], |r| r.get(0))?
            .flatten()
            .collect(),
        None => {
            let object: String = orig.chars().take(160).collect();
            tx.prepare(
                "SELECT id FROM record WHERE invalid=0 AND kind='decision' AND subject LIKE 'commit:%' \
                 AND object=?1 AND created_at <= ?2 ORDER BY created_at DESC, id DESC LIMIT 1",
            )?
            .query_map(rusqlite::params![object, created], |r| r.get(0))?
            .flatten()
            .collect()
        }
    };
    let via = format!("revert:{revert_short}");
    let mut n = 0;
    for id in &ids {
        n += tx.execute(
            "UPDATE record SET invalid=1, invalid_reason='reverted', invalidated_via=?2 WHERE id=?1 AND invalid=0",
            rusqlite::params![id, via],
        )?;
        tx.execute(
            "UPDATE record SET invalid=0, invalid_reason=NULL, invalidated_by=NULL \
             WHERE invalid=1 AND invalid_reason='superseded' AND invalidated_by=?1",
            [id],
        )?;
    }
    Ok(n)
}

/// The commits whose diff says nothing about a value any more: the ones a later commit
/// reverted, and the reverts themselves. `capture_git` has already marked both by the time
/// the diffs are read.
fn reverted_or_reverts(db: &Db) -> muninn_core::Result<std::collections::HashSet<String>> {
    let mut st = db.conn.prepare(
        "SELECT substr(subject, 8, 7) FROM record WHERE origin='commit_linked' AND invalid_reason='reverted' AND subject LIKE 'commit:%' \
         UNION SELECT substr(subject, 8, 7) FROM record WHERE kind='deadend' AND subject LIKE 'revert:%'",
    )?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.flatten().collect())
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

/// A file a package manager writes: `Cargo.lock`, `package-lock.json`, `pnpm-lock.yaml`,
/// `bun.lockb`, `gradle.lockfile`, `go.sum`, … — by its name's shape, so that `blocklist.json`
/// or `clock.toml` is not one.
fn is_lock_file(name: &str) -> bool {
    let stem_ends_lock = name
        .split('.')
        .next()
        .is_some_and(|stem| stem == "lock" || stem.ends_with("-lock") || stem.ends_with("_lock"));
    let ext_is_lock = name
        .split('.')
        .skip(1)
        .any(|part| matches!(part, "lock" | "lockb" | "lockfile"));
    stem_ends_lock || ext_is_lock || matches!(name, "go.sum" | "npm-shrinkwrap.json")
}

/// Is this tracked file one where a line *is* a value — a manifest or a configuration file —
/// rather than prose or code, where a line that changes is a sentence or a statement being
/// rewritten? WP-02: on one real store the swap rule had read 174 one-line hunks as changed
/// values, most of them source lines and prose sentences, and a single reworded README line
/// retired seven records in a synthetic repository. A lock file is a manifest nobody writes by
/// hand, so it says nothing about what was decided.
fn is_config_file(path: &str) -> bool {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();
    if is_lock_file(&name) {
        return false;
    }
    const NAMES: [&str; 14] = [
        "dockerfile",
        "containerfile",
        "makefile",
        "gemfile",
        "pipfile",
        "procfile",
        "go.mod",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        ".tool-versions",
        ".nvmrc",
        ".python-version",
    ];
    if NAMES.contains(&name.as_str()) || name.starts_with(".env") {
        return true;
    }
    if name.starts_with("requirements") && name.ends_with(".txt") {
        return true;
    }
    const EXT: [&str; 13] = [
        "json",
        "jsonc",
        "json5",
        "toml",
        "yaml",
        "yml",
        "ini",
        "cfg",
        "conf",
        "properties",
        "env",
        "csproj",
        "tf",
    ];
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| EXT.contains(&ext))
}

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
/// A seam for one test (`dropped_values.rs`): between the pass's read and its write, say so
/// and wait until told to go on, so that an ingest can be run exactly there. Debug builds
/// only, and only when the variable names a directory.
fn hold_for_a_test() {
    #[cfg(debug_assertions)]
    if let Ok(dir) = std::env::var("MUNINN_TEST_HOLD_COMMIT_PASS") {
        let dir = std::path::Path::new(&dir);
        let _ = std::fs::write(dir.join("read"), "");
        for _ in 0..600 {
            if dir.join("go").exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

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
                created_at: ct
                    .parse::<i64>()
                    .ok()
                    .and_then(|s| muninn_core::db::sane_created_at(s.saturating_mul(1000), now))
                    .unwrap_or(now),
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
    // a change that was reverted did not happen, and the revert only puts back what was
    let skip = reverted_or_reverts(db)?;
    commits.retain(|c| !c.removed.is_empty() && !skip.contains(&c.hash));
    if commits.is_empty() {
        return Ok(0);
    }
    // The pass opens with its event and its read of the active records in one transaction
    // (`db::open_pass`): a record an agent's prompt retired before this is not in the read, and
    // the event keeps the undo of that prompt from giving it back; one retired after it is in
    // the read. (`muninn-capture`, `agent/undo.rs`.)
    //
    // the values the store actually holds, so the diff's vocabulary is never the subject
    let live: Vec<(i64, String, i64, bool)> = muninn_core::db::open_pass(
        db,
        "commits",
        true,
        |tx| {
            let mut st = tx.prepare(
            "SELECT id, object, created_at, kind = 'decision' FROM record WHERE invalid=0 AND length(body) <= 700 AND ( \
                 (kind='decision' AND origin IN ('user_said','review_accepted')) \
                 OR kind='episode') \
             ORDER BY created_at DESC, id DESC LIMIT 400",
        )?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            Ok(rows.flatten().collect())
        },
    )?;
    // One hunk is evidence about one value, and it goes to one statement: the newest decision
    // that named it, or failing one the newest episode (records are walked decisions first,
    // each newest first). 1.2.0 let a single hunk retire 55 records on one store.
    let mut used: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    // every content word of the statement, not only the ones that look like a product name —
    // `nock` and `bash` are values too, and nothing about their spelling says so — with the
    // name-shaped ones kept apart, because they are exempt from the commonness test below
    let words_of: Vec<(i64, Vec<String>, Vec<String>)> = live
        .iter()
        .filter(|(_, _, _, d)| *d)
        .chain(live.iter().filter(|(_, _, _, d)| !*d))
        .map(|(id, obj, _, _)| {
            (
                *id,
                muninn_capture::extract::topic_words(obj),
                muninn_capture::extract::name_tokens(obj),
            )
        })
        .collect();
    let object_of = |id: i64| -> String {
        live.iter()
            .find(|(i, _, _, _)| *i == id)
            .map(|(_, o, _, _)| o.clone())
            .unwrap_or_default()
    };
    let created_of = |id: i64| -> i64 {
        live.iter()
            .find(|(i, _, _, _)| *i == id)
            .map_or(0, |(_, _, c, _)| *c)
    };
    let mut gone_cache: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    let mut common_cache: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    // The work, with no transaction open: which records the commits speak about. It asks git
    // up to two questions a record (`git grep`, tens of milliseconds each on Windows), and
    // until now it asked them inside the closing transaction, so a `Stop` that met a long
    // `maintain` waited out its 700 ms and failed with `database is locked` (WP-19). What it
    // decides is kept as a plan and the transaction below only writes it.
    //
    // A statement the plan retires takes the episodes of its turns with it, so those are
    // marked here as they will be retired there: they claim no hunk and write no observation.
    let mut planned: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut plan: Vec<Retirement> = Vec::new();
    let episodes_of_its_turns = format!(
        "SELECT id FROM record WHERE invalid=0 AND {} AND length(body) <= 700 AND kind='episode'",
        muninn_capture::ingest::turns_of("?1")
    );
    for (id, words, names) in &words_of {
        // retired earlier in this pass, as the episode of a turn whose decision went, or by
        // something else since the read
        let still_live: bool = !planned.contains(id)
            && db
                .conn
                .query_row("SELECT invalid = 0 FROM record WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .unwrap_or(false);
        if !still_live {
            continue;
        }
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
        //
        // Only a commit made after the statement says anything about it. An earlier commit took
        // the word out before it was said, so the person said it knowing what the code held.
        let born = created_of(*id);
        let later = || commits.iter().filter(|c| c.created_at > born);
        let quantities = muninn_capture::extract::quantity_slots(&object_of(*id));
        // WP-02. A swap speaks only when the hunk is a value changing: a line of a manifest or
        // a configuration file, or a quantity in its slot. Any other one-line hunk is prose or
        // code being rewritten, and 1.2.0 read those too.
        //
        // An episode still takes part. The analysis proposed decisions only; on a design grid
        // in loop 8's shape where the old value survives in another file and the statement was
        // not typed as a decision, that left six of eight such statements active and served
        // (24/24 retired on 1.2.0, 18/24 with decisions only), and the config-file guard alone
        // already keeps a prose edit out. Decisions go first, so an episode is chosen only when
        // no decision named the value.
        let swap = commits
            .iter()
            .enumerate()
            .filter(|(_, c)| c.created_at > born)
            .find_map(|(ci, c)| {
                let (si, sw) = c.swaps.iter().enumerate().find(|(si, sw)| {
                    if used.contains(&(ci, *si)) {
                        return false;
                    }
                    let value = is_config_file(&sw.file)
                        && words
                            .iter()
                            .any(|w| sw.out.contains(w) && !sw.inn.contains(w));
                    value || {
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
                Some((ci, si, c, sw))
            });
        let claim = swap.map(|(ci, si, _, _)| (ci, si));
        let swap = swap.map(|(_, _, c, sw)| (c, sw));
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
                let Some((c, gone)) = later().find_map(|c| {
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
        // A replacement comes after what it replaces and names what the commit brought in:
        // sharing any word with the added lines was enough before, and an unrelated older record
        // was then shown as the current word on the subject.
        let heir: Option<i64> = words_of
            .iter()
            .find(|(other, w, _)| {
                other != id
                    && created_of(*other) > born
                    && w.iter().any(|w| added.contains(w) && !words.contains(w))
            })
            .map(|(i, _, _)| *i);
        // What the file now holds, whether or not anyone said it: it now holds this line, and
        // that is an observation with a commit behind it, not a guess — `commit_linked`, trust
        // 2, anchored to the file, keyed on the retired decision's own topic words so the
        // question that used to reach the old answer reaches this one. Written only when a
        // swap retires something, so a repository's ordinary churn creates nothing. It is the
        // heir of what it retired when no statement is (WP-12): 1.2.0 wrote it and linked
        // nothing to it, so 175 retirements on one store pointed at no record at all.
        let observation = swap.as_ref().map(|(_, sw)| {
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
            let hash = blake3::hash(format!("decision|{subject}|is|{object}|{body}").as_bytes())
                .to_hex()
                .to_string();
            Observation {
                subject,
                object,
                body,
                file: sw.file.clone(),
                commit: commit.hash.clone(),
                hash,
                created_at: commit.created_at,
            }
        });
        let via = match &swap {
            Some(_) => format!("swap:{}", commit.hash),
            None => format!("gone:{gone}"),
        };
        if let Some(c) = claim {
            used.insert(c);
        }
        planned.insert(*id);
        // The same turn's literal episode states it too, and serving that would put the
        // dropped value back in front of the agent. Every turn that stated it: a restatement
        // keeps the turns it moved away from.
        if let Ok(mut st) = db.conn.prepare_cached(&episodes_of_its_turns) {
            if let Ok(rows) = st.query_map([id], |r| r.get::<_, i64>(0)) {
                planned.extend(rows.flatten());
            }
        }
        plan.push(Retirement {
            id: *id,
            heir,
            observation,
            via,
        });
    }
    hold_for_a_test();
    // The pass closes in one transaction (`db::close_pass`) that only writes the plan. A record
    // of the read that is no longer live when it opens was retired during the work by something
    // else, a worker's prompt for one, and the pass cannot judge it any more: that alone leaves
    // the closing event, which keeps the undo of that prompt from giving the record back to a
    // pass that will not see it again.
    muninn_core::db::close_pass(db, "commits", |tx| {
        let read: Vec<String> = live.iter().map(|(id, ..)| id.to_string()).collect();
        let gone_during_the_work: bool = tx.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM record WHERE invalid = 1 AND id IN ({}))",
                read.join(",")
            ),
            [],
            |r| r.get(0),
        )?;
        let mut n = 0;
        for r in &plan {
            let still_live: bool = tx
                .query_row(
                    "SELECT invalid = 0 FROM record WHERE id=?1",
                    [r.id],
                    |row| row.get(0),
                )
                .unwrap_or(false);
            if !still_live {
                continue;
            }
            let observed: Option<i64> = match &r.observation {
                Some(o) => {
                    tx.execute(
                        "INSERT OR IGNORE INTO record(kind, subject, relation, object, body, origin, trust, anchor_path, session_id, transcript_ref, dedup_hash, created_at) \
                         VALUES('decision', ?1, 'is', ?2, ?3, 'commit_linked', 2, ?4, 'git', ?5, ?6, ?7)",
                        rusqlite::params![
                            o.subject,
                            o.object,
                            o.body,
                            o.file,
                            format!("git:{}", o.commit),
                            o.hash,
                            o.created_at
                        ],
                    )?;
                    tx.query_row(
                        "SELECT id FROM record WHERE dedup_hash = ?1",
                        [&o.hash],
                        |row| row.get(0),
                    )
                    .ok()
                }
                None => None,
            };
            let heir = r.heir.or(observed);
            n += tx.execute(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?2, invalidated_via=?3 \
                 WHERE id=?1 AND invalid=0",
                rusqlite::params![r.id, heir, r.via],
            )?;
            // A typed record of that turn is a separate statement — an invariant or a
            // correction typed from the same message is not the value — and nothing here says
            // it stopped holding: 1.2.0 swept those too when they held the word, which is how a
            // reworded README line retired an invariant.
            tx.execute(
                &format!(
                    "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?2, invalidated_via=?3 \
                     WHERE invalid=0 AND {} AND length(body) <= 700 AND kind='episode'",
                    muninn_capture::ingest::turns_of("?1")
                ),
                rusqlite::params![r.id, heir, r.via],
            )?;
            // a statement that replaced it takes over its topic; the observation already carries
            // the topic minus the value that left, and inheriting would put that value back in
            if let Some(h) = heir.filter(|h| Some(*h) != observed) {
                muninn_capture::ingest::inherit_topic(tx, h)?;
            }
        }
        Ok((n, n > 0 || gone_during_the_work))
    })
}

/// What the commit pass decided for one record, with no transaction open, and writes when it
/// closes: the record to retire, the statement that replaced it when one did, the line the
/// file now holds when a swap said so, and the cause.
struct Retirement {
    id: i64,
    heir: Option<i64>,
    observation: Option<Observation>,
    via: String,
}

/// The record a swap leaves: what the file now reads, with the commit behind it.
struct Observation {
    subject: String,
    object: String,
    body: String,
    file: String,
    commit: String,
    hash: String,
    created_at: i64,
}

/// Heartbeat rows dropped in one run from which the file is rewritten to give their pages
/// back: about 1.3 MB of rows at the 128 bytes a row measured on one store.
const HEARTBEATS_WORTH_COMPACTING: usize = 10_000;

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
    // thirty days of heartbeat rows are kept; a first run on a store in use frees enough
    // pages to be worth giving back, which the compaction further down does
    match muninn_core::heartbeat::prune(&db) {
        Ok(n) => {
            st.heartbeats_dropped = n;
            if n >= HEARTBEATS_WORTH_COMPACTING {
                let _ = db.ask_for_compaction();
            }
        }
        Err(e) => output::err(&format!("muninn maintain: heartbeat prune: {e}")),
    }
    // resume: the last transcript from its watermark (a hook that expired left it)
    if let Ok(Some(p)) = db.meta_get("last_transcript_path") {
        let p = std::path::PathBuf::from(p);
        if p.is_file() {
            match crate::sessions::ingest_one(&db, &p, "resume", paths) {
                Ok(s) => {
                    st.ingested += s.inserted;
                    st.projected += s.projected;
                }
                Err(e) => output::err(&format!("muninn maintain: ingest: {e}")),
            }
        }
    }
    // before the log is read: the lines no reader can tell apart go first
    st.session_lines_dropped = crate::sessions::compact(paths);
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
    match muninn_capture::ingest::retire_summary_records(&db) {
        Ok(n) => st.summary_records_retired = n,
        Err(e) => output::err(&format!("muninn maintain: summary records: {e}")),
    }
    match muninn_capture::ingest::restore_false_retirements(&db) {
        Ok(n) => st.records_restored = n,
        Err(e) => output::err(&format!("muninn maintain: restore: {e}")),
    }
    match muninn_capture::ingest::restore_retired_again(&db) {
        Ok(n) => st.records_restored += n,
        Err(e) => output::err(&format!("muninn maintain: restore again: {e}")),
    }
    // what `doctor` compares, mended here when the two disagree (WP-08)
    match db.repair_fts_if_drifted() {
        Ok(n) => st.index_rebuilt = n.is_some(),
        Err(e) => output::err(&format!("muninn maintain: fts repair: {e}")),
    }
    // and what `doctor` reports as dated in the future, where the unit was wrong (R-17)
    match db.repair_future_dates() {
        Ok(n) => st.dates_repaired = n,
        Err(e) => output::err(&format!("muninn maintain: dates: {e}")),
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
    if st.commits
        + st.reverts
        + st.values_retired
        + st.summary_records_retired
        + st.records_restored
        + st.dates_repaired
        > 0
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
    // The turn-context log an earlier build's PostToolUse wrote (megabytes on a store in
    // use): nothing reads it and nothing trims it any more.
    let _ = std::fs::remove_file(paths.log_dir().join("turn_context.jsonl"));
    if let Ok(n) = muninn_core::cue::derive_missing(&db) {
        st.cues_derived = n;
    }
    // once, after the migration that dropped the symbol graph: here and not on `Stop`,
    // because it rewrites the file
    match db.compact_if_pending() {
        Ok(done) => st.store_compacted = done,
        Err(e) => output::err(&format!("muninn maintain: compact: {e}")),
    }
    let _ = db.record_quick_check(3_600_000);
    // as on `Stop`: recompiled when a source file changed, never applied
    if !crate::compile_cmd::is_up_to_date(paths, &db) {
        if let Err(e) = crate::compile_cmd::compile(paths, &db, false) {
            output::err(&format!("muninn maintain: compile: {e:#}"));
        }
    }
    db.fold_log();
    st.ms = t0.elapsed().as_millis();
    if json {
        output::json(&st);
    } else {
        output::out(&format!(
            "maintain: {} ingested, {} commit(s), {} revert(s), {} anchor(s) retired, {} file(s) projected, {} ms",
            st.ingested, st.commits, st.reverts, st.anchors_retired, st.projected, st.ms
        ));
    }
    0
}

/// Spawn only if the last spawn is older than `min_age_s` (stamp file in the log dir,
/// the one place a read hook may write).
///
/// The process is started on another thread, which the caller joins before it exits: starting
/// one costs about 2 ms on Windows (2.05 ms p50 on a copy of one store), and the hook has its
/// reads to do meanwhile (R-10).
pub fn spawn_detached_throttled(
    paths: &ProjectPaths,
    min_age_s: u64,
) -> Option<std::thread::JoinHandle<()>> {
    let stamp = paths.log_dir().join("maintain.stamp");
    if let Ok(md) = std::fs::metadata(&stamp) {
        if let Ok(age) = md
            .modified()
            .and_then(|m| m.elapsed().map_err(std::io::Error::other))
        {
            if age.as_secs() < min_age_s {
                return None;
            }
        }
    }
    let _ = std::fs::write(&stamp, now_ms().to_string());
    let paths = paths.clone();
    Some(std::thread::spawn(move || spawn_detached(&paths)))
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
        // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP: a hidden console, and a Ctrl+C sent
        // to the harness does not reach it. Not DETACHED_PROCESS: with no console at all,
        // every console child it runs (git) gets a fresh visible window of its own
        cmd.creation_flags(0x0800_0000 | 0x0000_0200);
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

    /// A lock file is told by the shape of its name, not by containing `lock` somewhere.
    #[test]
    fn lock_files_are_not_config_and_names_that_merely_contain_lock_are() {
        for lock in [
            "Cargo.lock",
            "package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "bun.lockb",
            "gradle.lockfile",
            "packages.lock.json",
            "go.sum",
            "deps/poetry.lock",
        ] {
            assert!(!is_config_file(lock), "{lock}");
        }
        for cfg in [
            "config/blocklist.json",
            "clock.toml",
            "deadlock-detector.yaml",
            "Cargo.toml",
            "package.json",
        ] {
            assert!(is_config_file(cfg), "{cfg}");
        }
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
