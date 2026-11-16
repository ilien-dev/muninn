//! muninn-bench: performance regressions and, later, the four-arm experiment.
//!
//! `perf` regenerates a deterministic synthetic store, measures the hook binary
//! end to end (process start → exit) and the cue-evaluation strategies that fixed
//! the design ([M2]: normalised dir-ancestor lookup vs GLOB), and fails with
//! `--strict` when a contract is exceeded. Every number printed is measured here,
//! on this machine, now.

mod enforce;
mod experiment;
mod replay;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use muninn_core::{Db, Mode};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

#[derive(Parser)]
#[command(name = "muninn-bench")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Rule compiler over a corpus of CLAUDE.md/AGENTS.md files: stats, sampling, evaluation (Gate 1)
    Rules {
        /// Directory of corpus files
        #[arg(long, default_value = "crates/muninn-bench/corpora/claude-md/files")]
        dir: PathBuf,
        /// Write a random sample of candidates to label (JSONL) instead of stats
        #[arg(long)]
        sample: Option<PathBuf>,
        #[arg(long, default_value_t = 100)]
        n: usize,
        #[arg(long, default_value_t = 7)]
        seed: u64,
        /// Evaluate against a labelled JSONL (fields: text, label, class?)
        #[arg(long)]
        labels: Option<PathBuf>,
        /// List every enforceable classification (pattern, source, text) for inspection
        #[arg(long)]
        list: bool,
        /// Restrict to files listed (one name per line) — the held-out set for the gate
        #[arg(long)]
        holdout: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        /// Exit non-zero when the Gate 1 contract is not met (precision >= 0.90, recall >= 0.70)
        #[arg(long)]
        strict: bool,
    },
    /// Compiled controls against hand-labelled tool calls (Gate 5a): does the control
    /// refuse what its rule forbids, and nothing else? No model.
    Enforce {
        /// Hand-labelled cases (JSONL): rule text, tool call, and the verdict the rule calls for
        #[arg(
            long,
            default_value = "crates/muninn-bench/corpora/claude-md/enforce_cases.jsonl"
        )]
        cases: PathBuf,
        /// muninn binary (default: next to this one)
        #[arg(long)]
        muninn: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Four-arm experiment (Gate 2): see experiment/PREREGISTRATION.md in
    /// https://github.com/ilien-dev/muninn-experiments, which also holds the task files
    Experiment {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, default_value = "crates/muninn-bench/experiment/out")]
        out: PathBuf,
        /// Print the cell plan and exit
        #[arg(long)]
        dry_run: bool,
        /// One run, arms off+literal, first non-inferable task — validates the pipeline and measures cost per cell
        #[arg(long)]
        pilot: bool,
        #[arg(long)]
        runs: Option<usize>,
        #[arg(long)]
        model: Option<String>,
        /// Cells to run concurrently (each has its own worktree and store)
        #[arg(long, default_value_t = 1)]
        jobs: usize,
        /// Re-run only the cells recorded as `error` in <out>/results.jsonl
        #[arg(long)]
        rerun_errors: bool,
        /// Re-run every oracle on the saved patches (after an oracle amendment); no model calls
        #[arg(long)]
        rescore: bool,
        #[arg(long)]
        muninn: Option<PathBuf>,
    },
    /// A labelled fixture (extraction or delivery) run against this build: prints the counts
    /// its section of the usage-v4 experiment reads. See `fixtures/replay/` for the format
    Replay {
        /// The fixture: a JSON file whose paths are relative to itself
        fixture: PathBuf,
        /// Read only the items that carry this split
        #[arg(long)]
        split: Option<String>,
        /// muninn binary a delivery fixture asks (default: next to this one)
        #[arg(long)]
        muninn: Option<PathBuf>,
    },
    /// Performance contracts
    Perf {
        /// Fail on any contract violation
        #[arg(long)]
        strict: bool,
        /// Records in the synthetic store. The default is the schema's own cap
        /// (`caps::MAX_ACTIVE_RECORDS`): a latency contract that only holds below the size
        /// the store is allowed to reach is not a contract, and `docs/claims.md` publishes
        /// these figures as measured at 20 000. At 5 000 the gate passed while SessionStart
        /// was 10.5 ms at the cap, against its 10 ms limit.
        #[arg(long, default_value_t = 20_000)]
        records: usize,
        /// Hook invocations per measurement
        #[arg(long, default_value_t = 300)]
        runs: usize,
        /// Path to the muninn binary (default: sibling of this binary)
        #[arg(long)]
        muninn: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

/// Deterministic LCG so every run measures the same data.
pub struct Rng(pub u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn pick<'a>(&mut self, v: &'a [&'a str]) -> &'a str {
        v[(self.next() as usize) % v.len()]
    }
}

/// A Zipf-distributed synthetic vocabulary: 30 frequent domain words plus 2 000
/// rare identifiers, so posting lists look like real code discussion, not like a
/// corpus where every record contains every term.
fn word(rng: &mut Rng) -> String {
    let r = rng.next() % 100;
    if r < 55 {
        WORDS[(rng.next() as usize) % WORDS.len()].to_string()
    } else {
        format!(
            "{}{}",
            WORDS[(rng.next() as usize) % WORDS.len()],
            (rng.next() % 2000)
        )
    }
}

const WORDS: &[&str] = &[
    "retry",
    "backoff",
    "jwt",
    "session",
    "cache",
    "webhook",
    "idempotent",
    "migration",
    "schema",
    "index",
    "handler",
    "router",
    "middleware",
    "timeout",
    "queue",
    "worker",
    "cursor",
    "pagination",
    "token",
    "refresh",
    "sqlite",
    "postgres",
    "redis",
    "grpc",
    "http",
    "serde",
    "config",
    "env",
    "flag",
    "cli",
];
/// The directories the synthetic records are anchored under.
const DIRS: &[&str] = &[
    "src/auth",
    "src/api",
    "src/db",
    "src/webhooks",
    "src/cli",
    "crates/core",
    "crates/cli",
    "tests",
    "docs",
    "scripts",
];

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 * p) as usize).min(v.len() - 1)]
}

fn populate(root: &Path, records: usize) -> Result<()> {
    std::fs::create_dir_all(root.join(".git"))?;
    let m = root.join(".muninn");
    for d in ["", "records", "compiled", "log", "compact"] {
        std::fs::create_dir_all(m.join(d))?;
    }
    let db = Db::open(&m.join("muninn.db"), Mode::ReadWrite)?;
    let mut rng = Rng(42);
    let t = Instant::now();
    let tx = db.write_tx()?;
    {
        // Decisions and invariants are the person's, as most typed records of a store are: the
        // catalogue lists what the person stated and leaves out what a commit observed, so with
        // every record `commit_linked` it listed none of them and the session-start contracts
        // timed the sparse list of episodes instead. Trust follows the origin.
        let mut st = tx.prepare(
            "INSERT INTO record(kind,subject,relation,object,body,origin,trust,anchor_path,session_id,dedup_hash,created_at) \
             VALUES(?1,?2,?3,?4,?5,\
                    CASE WHEN ?1 IN ('decision','invariant') THEN 'user_said' ELSE 'commit_linked' END,\
                    CASE WHEN ?1 IN ('decision','invariant') THEN 3 ELSE 2 END,\
                    ?6,'bench',?7,?8)",
        )?;
        let kinds = ["decision", "deadend", "episode", "claim", "invariant"];
        for i in 0..records {
            let kind = if i % 50 == 0 {
                "invariant"
            } else {
                kinds[(rng.next() as usize) % 4]
            };
            let dir = rng.pick(DIRS);
            let file = format!("{dir}/{}.rs", rng.pick(WORDS));
            let subject = format!("{}.{}", rng.pick(WORDS), rng.pick(WORDS));
            let object = format!(
                "{} {} {}",
                rng.pick(WORDS),
                rng.pick(WORDS),
                rng.pick(WORDS)
            );
            let body: String = (0..40)
                .map(|_| word(&mut rng))
                .collect::<Vec<_>>()
                .join(" ");
            st.execute(rusqlite::params![
                kind,
                subject,
                "is",
                object,
                body,
                file,
                format!("h{i}"),
                1_700_000_000_000i64 + i as i64
            ])?;
            // Every other record used to get a symbol cue with a word drawn here. The draw
            // stays so that the records and the ledger seed below are the ones every earlier
            // build was measured on: the store differs only by the cue rows it no longer has.
            if i % 2 == 0 {
                rng.next();
            }
        }
    }
    tx.commit()?;
    // The ledger of a long session, under the session id the hooks are run with: every read
    // hook asks it what the session was already delivered. The largest session of a 1.2.0 store
    // held 8 336 rows, most of them budget cuts and compaction reinjections, a few deliveries;
    // so does this one. Without it the lookup read an empty ledger and no contract saw its cost.
    {
        let tx = db.write_tx()?;
        {
            let mut st = tx.prepare(
                "INSERT INTO fire_ledger(session_id, compaction_epoch, record_id, fired_at, tokens, reason) \
                 VALUES('b', 0, ?1, ?2, 0, ?3)",
            )?;
            for i in 0..8_336usize {
                let reason = match i % 10 {
                    0..=7 => "literal:gated:budget:54",
                    8 => "literal:cue:event:post_compact",
                    _ => "literal:literal",
                };
                let id = (rng.next() as usize % records) as i64 + 1;
                st.execute(rusqlite::params![
                    id,
                    1_700_000_000_000i64 + i as i64,
                    reason
                ])?;
            }
        }
        tx.commit()?;
    }
    // The index a store actually has, not the one a bulk load leaves.
    //
    // Everything above goes in one transaction and was followed by an `optimize`, which is
    // the tidiest FTS5 index SQLite can produce. Production never sees it: `Stop` writes a
    // handful of records per turn, so a store at the schema's cap is the product of thousands
    // of small commits. Measured on 20 000 records written in 2 000 transactions of ten, the
    // BM25 query costs 8.56 ms against 6.54 ms after an optimize, on an index with 918
    // segments against 416 — and the bulk-loaded fixture was reporting 1.4 ms.
    //
    // So the rows are rewritten into the index the way they arrive. The `optimize` stays out:
    // whether the engine should run one is the question the contract is now able to ask.
    {
        let ids: Vec<i64> = db
            .conn
            .prepare("SELECT id FROM record WHERE invalid = 0")?
            .query_map([], |r| r.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        db.conn.execute_batch(
            "INSERT INTO record_fts(record_fts) VALUES('delete-all');              INSERT OR REPLACE INTO meta(key,value) VALUES('fts_rows','0');",
        )?;
        for chunk in ids.chunks(10) {
            let tx = db.write_tx()?;
            {
                let mut st = tx.prepare(
                    "INSERT INTO record_fts(rowid, subject, object, body)                      SELECT id, subject, object, body FROM record WHERE id = ?1",
                )?;
                for id in chunk {
                    st.execute([id])?;
                }
            }
            tx.execute(
                "UPDATE meta SET value = CAST(CAST(value AS INTEGER) + ?1 AS TEXT) WHERE key='fts_rows'",
                [chunk.len() as i64],
            )?;
            tx.commit()?;
        }
    }
    db.meta_set(
        "bench_populated_ms",
        &(t.elapsed().as_secs_f64() * 1000.0).to_string(),
    )?;
    Ok(())
}

/// Round-trip of `muninn --version`: spawn, dynamic link, parse one argument, exit. It does
/// none of Muninn's work, so it is the floor under every hook figure below it — a 13 MB
/// statically linked binary is most of what the gated contract measures, and without this
/// line a failure there cannot be told apart from a slower machine.
fn process_start(bin: &Path, runs: usize) -> Result<(f64, f64, f64)> {
    let mut ts = Vec::with_capacity(runs);
    for _ in 0..runs {
        let t = Instant::now();
        let out = Command::new(bin)
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("spawning {}", bin.display()))?;
        ts.push(t.elapsed().as_secs_f64() * 1000.0);
        anyhow::ensure!(out.status.success(), "muninn --version failed");
    }
    Ok((
        pct(&mut ts.clone(), 0.50),
        pct(&mut ts.clone(), 0.95),
        pct(&mut ts, 0.99),
    ))
}

/// Whether a session-start context holds the catalogue made of typed records: its heading as
/// the normal path writes it, and at least one line naming a decision or a rule. A store whose
/// typed records the catalogue leaves out gets the sparse list of what was said instead, a
/// different path, and the two session-start contracts would then time that one.
fn lists_typed_records(context: &str) -> bool {
    let heading = context.lines().any(|l| {
        l.starts_with("[muninn:catalog]") && l.ends_with("then decisions, each newest first")
    });
    let line = context.lines().any(|l| {
        l.strip_prefix('#')
            .and_then(|rest| rest.split_once(' '))
            .is_some_and(|(id, rest)| {
                id.chars().all(|c| c.is_ascii_digit())
                    && (rest.starts_with("decision \u{b7} ")
                        || rest.starts_with("invariant \u{b7} ")
                        || rest.starts_with("correction \u{b7} "))
            })
    });
    heading && line
}

/// One more run of a session-start hook after its timed runs, refused unless what it timed was
/// the catalogue's normal path.
fn ensure_catalogue(
    bin: &Path,
    root: &Path,
    event: &str,
    payload: &serde_json::Value,
) -> Result<()> {
    let mut child = Command::new(bin)
        .args(["hook", event])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {}", bin.display()))?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())?;
    let out = child.wait_with_output()?;
    let context = serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .ok()
        .and_then(|v| {
            v["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .map(String::from)
        })
        .unwrap_or_default();
    anyhow::ensure!(
        lists_typed_records(&context),
        "{event} on the bench store did not list typed records in its catalogue: the contract would time another path"
    );
    Ok(())
}

fn run_hook(
    bin: &Path,
    root: &Path,
    event: &str,
    payload: &serde_json::Value,
    runs: usize,
) -> Result<(f64, f64, f64)> {
    let input = payload.to_string();
    let mut ts = Vec::with_capacity(runs);
    for _ in 0..runs {
        let t = Instant::now();
        let mut child = Command::new(bin)
            .args(["hook", event])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning {}", bin.display()))?;
        child.stdin.take().unwrap().write_all(input.as_bytes())?;
        let out = child.wait_with_output()?;
        ts.push(t.elapsed().as_secs_f64() * 1000.0);
        if !out.status.success() {
            anyhow::bail!(
                "{event} exited {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    Ok((
        pct(&mut ts.clone(), 0.5),
        pct(&mut ts.clone(), 0.95),
        pct(&mut ts, 0.99),
    ))
}

/// One prompt for each timed run of the full prompt hook, made of words a few records hold.
///
/// The prompt this contract used to run, the same every time, was six of the thirty words the
/// fixture puts in half of all records: no term was selective, recall returned nothing, and
/// the hook ended `silence:no_match` in every run. The contract timed the open, the ledger
/// lookup and an empty recall, and never a block. Each of these asks for three identifiers
/// out of one record's body, so recall finds that record and the hook renders it.
fn full_prompts(root: &Path, runs: usize) -> Result<Vec<String>> {
    let db = Db::open(&root.join(".muninn/muninn.db"), Mode::ReadOnly)?;
    // Records the session was not given yet: one it was given is not offered to it again, and
    // a prompt for it alone would be answered with nothing. That is the ledger seed's records
    // (by the seed's dates, so the list does not depend on what the hooks before this logged)
    // and the invariants, which a session start delivers.
    let fresh: Vec<i64> = db
        .conn
        .prepare(
            "SELECT id FROM record WHERE session_id = 'bench' AND kind <> 'invariant' AND id NOT IN \
             (SELECT record_id FROM fire_ledger WHERE record_id IS NOT NULL \
              AND fired_at BETWEEN 1700000000000 AND 1700000008335) ORDER BY id",
        )?
        .query_map([], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    anyhow::ensure!(
        !fresh.is_empty(),
        "the bench store holds no record to ask for"
    );
    let mut st = db.conn.prepare("SELECT body FROM record WHERE id = ?1")?;
    let mut out = Vec::with_capacity(runs);
    for i in 0..runs {
        // a different record each run, far from the last one
        let id = fresh[(i * 61) % fresh.len()];
        let body: String = st.query_row([id], |r| r.get(0))?;
        let rare: Vec<&str> = body
            .split(' ')
            .filter(|w| w.ends_with(|c: char| c.is_ascii_digit()))
            .take(3)
            .collect();
        out.push(format!(
            "implement {} for the handler with {}",
            rare.first().copied().unwrap_or("retry"),
            rare.get(1..).unwrap_or_default().join(" and ")
        ));
    }
    Ok(out)
}

/// `run_hook` with a prompt of its own for each run, and how many runs were given a block.
fn run_prompts(
    bin: &Path,
    root: &Path,
    session: &str,
    prompts: &[String],
) -> Result<((f64, f64, f64), usize)> {
    let cwd = root.to_string_lossy().to_string();
    let mut ts = Vec::with_capacity(prompts.len());
    let mut delivered = 0usize;
    for prompt in prompts {
        let input =
            serde_json::json!({"session_id": session, "cwd": cwd, "prompt": prompt}).to_string();
        let t = Instant::now();
        let mut child = Command::new(bin)
            .args(["hook", "UserPromptSubmit"])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning {}", bin.display()))?;
        child.stdin.take().unwrap().write_all(input.as_bytes())?;
        let out = child.wait_with_output()?;
        ts.push(t.elapsed().as_secs_f64() * 1000.0);
        if !out.status.success() {
            anyhow::bail!(
                "UserPromptSubmit exited {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        if String::from_utf8_lossy(&out.stdout).contains("[muninn:") {
            delivered += 1;
        }
    }
    Ok((
        (
            pct(&mut ts.clone(), 0.5),
            pct(&mut ts.clone(), 0.95),
            pct(&mut ts, 0.99),
        ),
        delivered,
    ))
}

/// A write hook with one new turn to ingest each run: a turn is appended to the session's
/// transcript, then the hook runs. Wall clock, process start to exit. Refused unless every
/// turn is on record afterwards: a hook that ingested nothing is another path.
fn run_write_hook(bin: &Path, root: &Path, event: &str, runs: usize) -> Result<(f64, f64, f64)> {
    let session = format!("bench-{}", event.to_lowercase());
    let transcript = root.join(format!("{session}.jsonl"));
    let cwd = root.to_string_lossy().to_string();
    let input = serde_json::json!({
        "session_id": session, "cwd": cwd, "transcript_path": transcript, "hook_event_name": event
    })
    .to_string();
    let mut ts = Vec::with_capacity(runs);
    for i in 0..runs {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&transcript)?;
        let at = format!("2026-01-01T{:02}:{:02}:00.000Z", (i / 60) % 24, i % 60);
        writeln!(
            f,
            "{}",
            serde_json::json!({"type": "user", "sessionId": session, "cwd": cwd, "timestamp": at,
                "message": {"role": "user", "content": format!("Bench turn {i} about the {} {} widget, please look at it", WORDS[i % WORDS.len()], i)}})
        )?;
        writeln!(
            f,
            "{}",
            serde_json::json!({"type": "assistant", "sessionId": session, "timestamp": at,
                "message": {"role": "assistant", "content": [{"type": "text", "text": format!("Looked at it, iteration {i}. Nothing to change.")}]}})
        )?;
        drop(f);
        let t = Instant::now();
        let mut child = Command::new(bin)
            .args(["hook", event])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning {}", bin.display()))?;
        child.stdin.take().unwrap().write_all(input.as_bytes())?;
        let out = child.wait_with_output()?;
        ts.push(t.elapsed().as_secs_f64() * 1000.0);
        if !out.status.success() {
            anyhow::bail!(
                "{event} exited {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    let db = Db::open(&root.join(".muninn/muninn.db"), Mode::ReadOnly)?;
    let ingested: i64 = db.conn.query_row(
        "SELECT count(*) FROM record WHERE session_id = ?1",
        [&session],
        |r| r.get(0),
    )?;
    anyhow::ensure!(
        ingested as usize >= runs,
        "{event} left {ingested} record(s) of {runs} turn(s): the contract would time a hook that ingests nothing"
    );
    Ok((
        pct(&mut ts.clone(), 0.5),
        pct(&mut ts.clone(), 0.95),
        pct(&mut ts, 0.99),
    ))
}

/// The size of the rollout the PostCompact contract reads: the median of the 70 Codex rollouts
/// on the machine the usage review read (160,271 bytes; the largest was 108 MB).
const ROLLOUT_BYTES: usize = 160_271;

/// A Codex rollout of `ROLLOUT_BYTES` or a little more, whose last test command exits with
/// `last_test_exit`.
///
/// It is the invented rollout the Codex parser's tests read, its turns repeated until the size
/// is reached, and then once more up to the test run that passes, with that run's exit code set.
fn codex_rollout(last_test_exit: i64) -> String {
    const FIXTURE: &str = include_str!(
        "../../muninn-capture/tests/fixtures/codex/rollout-2026-11-14T10-00-00-019a7c00-0000-7000-8000-000000000001.jsonl"
    );
    const PASSED: &str = "\"exit_code\":0";
    let lines: Vec<&str> = FIXTURE.lines().filter(|l| !l.trim().is_empty()).collect();
    let (meta, turns) = lines.split_first().expect("the fixture has lines");
    let last_pass = turns
        .iter()
        .rposition(|l| l.contains("\"CommandExecution\"") && l.contains(PASSED))
        .expect("the fixture has a command that passed");
    let mut out = String::with_capacity(ROLLOUT_BYTES + FIXTURE.len() * 2);
    out.push_str(meta);
    out.push('\n');
    while out.len() < ROLLOUT_BYTES {
        for l in turns {
            out.push_str(l);
            out.push('\n');
        }
    }
    for l in &turns[..last_pass] {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(&turns[last_pass].replace(PASSED, &format!("\"exit_code\":{last_test_exit}")));
    out.push('\n');
    out
}

/// PostCompact with a summary that claims a result, and a Codex rollout to check it against:
/// the one read hook that parses a transcript. Timed with a rollout whose last test run
/// passed, so the hook has nothing to say; then run once on the same rollout with that run
/// failed, and refused unless the hook says so there and nowhere else. A hook that never read
/// the rollout answers both the same way.
fn run_summary_check(bin: &Path, root: &Path, runs: usize) -> Result<(f64, f64, f64)> {
    let cwd = root.to_string_lossy().to_string();
    let mut flagged = [0usize; 2];
    let mut ts = Vec::with_capacity(runs);
    for (exit, n) in [(0i64, runs), (1, 1)] {
        let rollout = root.join(format!("rollout-bench-{exit}.jsonl"));
        std::fs::write(&rollout, codex_rollout(exit))?;
        let input = serde_json::json!({
            "session_id": "bench-postcompact", "cwd": cwd, "hook_event_name": "PostCompact",
            "transcript_path": rollout, "compact_summary": "All tests pass and the build succeeds."
        })
        .to_string();
        for _ in 0..n {
            let t = Instant::now();
            let mut child = Command::new(bin)
                .args(["hook", "PostCompact"])
                .current_dir(root)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .with_context(|| format!("spawning {}", bin.display()))?;
            child.stdin.take().unwrap().write_all(input.as_bytes())?;
            let out = child.wait_with_output()?;
            if exit == 0 {
                ts.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            anyhow::ensure!(
                out.status.success(),
                "PostCompact exited {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            );
            if String::from_utf8_lossy(&out.stdout).contains("[muninn:unverified]") {
                flagged[exit as usize] += 1;
            }
        }
    }
    anyhow::ensure!(
        flagged == [0, 1],
        "PostCompact flagged the summary in {} of {runs} runs on a rollout whose tests passed and in {} of 1 on one whose tests failed: the contract would time a hook that does not read the rollout",
        flagged[0],
        flagged[1]
    );
    Ok((
        pct(&mut ts.clone(), 0.5),
        pct(&mut ts.clone(), 0.95),
        pct(&mut ts, 0.99),
    ))
}

fn ingest_200(root: &Path) -> Result<f64> {
    let db = Db::open(&root.join(".muninn/muninn.db"), Mode::ReadWrite)?;
    let mut rng = Rng(99);
    let t = Instant::now();
    let tx = db.write_tx()?;
    {
        let mut st = tx.prepare(
            "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
             VALUES('episode',?1,'happened',?2,?3,'tool_observed',1,'ingest',?4,?5)",
        )?;
        for i in 0..200 {
            let body: String = (0..60)
                .map(|_| word(&mut rng))
                .collect::<Vec<_>>()
                .join(" ");
            st.execute(rusqlite::params![
                format!("ep.{i}"),
                format!("summary {i}"),
                body,
                format!("ing{i}-{}", rng.next()),
                1_800_000_000_000i64 + i
            ])?;
        }
    }
    tx.commit()?;
    Ok(t.elapsed().as_secs_f64() * 1000.0)
}

struct Contract {
    name: &'static str,
    value: f64,
    limit: f64,
    unit: &'static str,
    /// The figure includes starting the process. On a machine where `--version` alone takes
    /// longer than the limit (a shared CI runner measured 1.58 ms p95 against the gated
    /// hook's 1 ms), the contract is judged on what Muninn adds over that floor, and the
    /// output says so.
    floor: Option<f64>,
}

impl Contract {
    fn ok(&self) -> bool {
        self.value <= self.limit || self.over_floor_ok()
    }
    fn over_floor_ok(&self) -> bool {
        self.floor
            .is_some_and(|f| f > self.limit && self.value - f <= self.limit)
    }
}

#[allow(clippy::too_many_arguments)]
fn rules_cmd(
    dir: &Path,
    sample: Option<PathBuf>,
    n: usize,
    seed: u64,
    labels: Option<PathBuf>,
    list: bool,
    holdout: Option<PathBuf>,
    json: bool,
    strict: bool,
) -> Result<()> {
    use muninn_compile::{classify::classify, parse::extract, Class};
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    if let Some(h) = &holdout {
        let names: std::collections::HashSet<String> = std::fs::read_to_string(h)?
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        files.retain(|f| names.contains(&f.file_name().unwrap().to_string_lossy().to_string()));
    }
    let mut all = Vec::new();
    for f in &files {
        let content = muninn_core::sanitize::from_bytes_lossy(&std::fs::read(f)?);
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        for c in extract(&name, &content) {
            let cl = classify(&c);
            all.push((c, cl));
        }
    }
    if list {
        for (c, cl) in all
            .iter()
            .filter(|(_, cl)| cl.class != Class::InterpretiveOnly)
        {
            let arts: Vec<String> = cl
                .permissions
                .iter()
                .map(|p| format!("{:?} {}", p.decision, p.render()))
                .chain(cl.hooks.iter().map(|h| {
                    format!(
                        "hook {:?} {}{}",
                        h.decision,
                        h.tool_regex,
                        h.command_regex
                            .as_ref()
                            .map(|c| format!(" ~{c}"))
                            .unwrap_or_default()
                    )
                }))
                .collect();
            println!(
                "{:<26} {}:{}\n    \"{}\"\n    → {}",
                cl.pattern_id,
                c.source_file,
                c.source_line,
                c.text,
                arts.join(" | ")
            );
        }
        return Ok(());
    }
    if let Some(out) = sample {
        // stratified, deterministic: half predicted enforceable (measures precision),
        // half hard negatives — interpretive candidates carrying a negation (measures recall).
        let neg = regex::Regex::new(
            r"(?i)\b(never|do not|don'?t|must not|mustn'?t|should not|forbidden|not allowed)\b",
        )
        .unwrap();
        let mut rng = Rng(seed);
        let mut idx: Vec<usize> = (0..all.len()).collect();
        for i in (1..idx.len()).rev() {
            let j = (rng.next() as usize) % (i + 1);
            idx.swap(i, j);
        }
        let mut chosen: Vec<usize> = Vec::new();
        let want_pos = n / 2;
        for &i in &idx {
            if chosen.len() >= want_pos {
                break;
            }
            if all[i].1.class != Class::InterpretiveOnly {
                chosen.push(i);
            }
        }
        for &i in &idx {
            if chosen.len() >= n {
                break;
            }
            if all[i].1.class == Class::InterpretiveOnly
                && neg.is_match(&all[i].0.text)
                && !chosen.contains(&i)
            {
                chosen.push(i);
            }
        }
        let mut f = std::fs::File::create(&out)?;
        for (k, &i) in chosen.iter().enumerate() {
            let (c, cl) = &all[i];
            writeln!(
                f,
                "{}",
                serde_json::json!({ "id": k, "file": c.source_file, "line": c.source_line, "text": c.text, "predicted": cl.pattern_id, "label": "" })
            )?;
        }
        println!(
            "wrote {} candidates to {} (from {} candidates in {} files)",
            chosen.len(),
            out.display(),
            all.len(),
            files.len()
        );
        return Ok(());
    }
    if let Some(lab) = labels {
        // label ∈ {enforceable, interpretive}
        let text = std::fs::read_to_string(&lab)?;
        let (mut tp, mut fp, mut fn_, mut tn, mut n_l) = (0, 0, 0, 0, 0);
        let mut errors = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: serde_json::Value = serde_json::from_str(line)?;
            let label = v["label"].as_str().unwrap_or("");
            if label.is_empty() {
                continue;
            }
            n_l += 1;
            let t = v["text"].as_str().unwrap_or("");
            let c = muninn_compile::Candidate {
                source_file: v["file"].as_str().unwrap_or("").into(),
                source_line: v["line"].as_u64().unwrap_or(0) as usize,
                source_hash: String::new(),
                text: t.into(),
                heading: None,
            };
            let cl = classify(&c);
            let pred_enf = cl.class != Class::InterpretiveOnly;
            let gold_enf = label == "enforceable";
            match (pred_enf, gold_enf) {
                (true, true) => tp += 1,
                (true, false) => {
                    fp += 1;
                    errors.push(format!(
                        "FP [{}] {}:{} \"{}\"",
                        cl.pattern_id, c.source_file, c.source_line, t
                    ));
                }
                (false, true) => {
                    fn_ += 1;
                    errors.push(format!("FN {}:{} \"{}\"", c.source_file, c.source_line, t));
                }
                (false, false) => tn += 1,
            }
        }
        let precision = if tp + fp == 0 {
            1.0
        } else {
            tp as f64 / (tp + fp) as f64
        };
        let recall = if tp + fn_ == 0 {
            1.0
        } else {
            tp as f64 / (tp + fn_) as f64
        };
        if json {
            println!(
                "{}",
                serde_json::json!({ "labelled": n_l, "tp": tp, "fp": fp, "fn": fn_, "tn": tn, "precision": precision, "recall": recall, "errors": errors })
            );
        } else {
            println!("labelled {n_l}: tp={tp} fp={fp} fn={fn_} tn={tn}  precision={precision:.3} recall={recall:.3}  (gate: precision ≥ 0.90, recall ≥ 0.70)");
            for e in &errors {
                println!("  {e}");
            }
        }
        // `perf --strict` is the precedent: a measured contract that CI can fail on.
        // Without this the Gate 1 numbers were printed and never guarded.
        if strict {
            anyhow::ensure!(
                precision >= 0.90 && recall >= 0.70,
                "Gate 1 not met: precision {precision:.3} (>= 0.90), recall {recall:.3} (>= 0.70)"
            );
        }
        return Ok(());
    }
    let mut by_class = std::collections::BTreeMap::new();
    let mut by_pattern = std::collections::BTreeMap::new();
    for (_, cl) in &all {
        *by_class.entry(cl.class.as_str()).or_insert(0usize) += 1;
        *by_pattern.entry(cl.pattern_id).or_insert(0usize) += 1;
    }
    let interp = *by_class.get("interpretive_only").unwrap_or(&0);
    if json {
        println!(
            "{}",
            serde_json::json!({ "files": files.len(), "candidates": all.len(), "by_class": by_class, "by_pattern": by_pattern, "interpretive_fraction": interp as f64 / all.len().max(1) as f64 })
        );
    } else {
        println!(
            "{} files, {} rule candidates, {:.1}% interpretive only",
            files.len(),
            all.len(),
            100.0 * interp as f64 / all.len().max(1) as f64
        );
        for (k, v) in &by_class {
            println!("  {k:<24} {v}");
        }
        println!("patterns:");
        for (k, v) in &by_pattern {
            println!("  {k:<28} {v}");
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Experiment {
            config,
            out,
            dry_run,
            pilot,
            runs,
            model,
            jobs,
            rerun_errors,
            rescore,
            muninn,
        } => {
            let bin = muninn.unwrap_or_else(sibling_muninn);
            anyhow::ensure!(bin.exists(), "muninn binary not found at {}", bin.display());
            experiment::run(
                &config,
                &out,
                &bin,
                dry_run,
                pilot,
                runs,
                model,
                jobs,
                rerun_errors,
                rescore,
            )
        }
        Cmd::Rules {
            dir,
            sample,
            n,
            seed,
            labels,
            list,
            holdout,
            json,
            strict,
        } => rules_cmd(&dir, sample, n, seed, labels, list, holdout, json, strict),
        Cmd::Enforce {
            cases,
            muninn,
            json,
        } => {
            let bin = muninn.unwrap_or_else(sibling_muninn);
            anyhow::ensure!(bin.exists(), "muninn binary not found at {} (build it with `cargo build --release -p muninn-cli`)", bin.display());
            let work = tempfile_dir()?;
            let r = enforce::run_cmd(&cases, &bin, &work, json);
            std::fs::remove_dir_all(&work).ok();
            r
        }
        Cmd::Replay {
            fixture,
            split,
            muninn,
        } => {
            let bin = muninn.unwrap_or_else(sibling_muninn);
            let work = tempfile_dir()?;
            let r = replay::run(&fixture, split.as_deref(), &bin, &work);
            std::fs::remove_dir_all(&work).ok();
            r
        }
        Cmd::Perf {
            strict,
            records,
            runs,
            muninn,
            json,
        } => {
            let bin = muninn.unwrap_or_else(sibling_muninn);
            anyhow::ensure!(bin.exists(), "muninn binary not found at {} (build it with `cargo build --release -p muninn-cli`)", bin.display());
            let tmp = tempfile_dir()?;
            let root = tmp.as_path();
            let t = Instant::now();
            populate(root, records)?;
            let populate_ms = t.elapsed().as_secs_f64() * 1000.0;
            let cwd = root.to_string_lossy().to_string();
            let start = process_start(&bin, runs)?;
            let ss = run_hook(
                &bin,
                root,
                "SessionStart",
                &serde_json::json!({"session_id":"b","cwd":cwd}),
                runs,
            )?;
            let subagent = run_hook(
                &bin,
                root,
                "SubagentStart",
                &serde_json::json!({"session_id":"b","cwd":cwd}),
                runs,
            )?;
            for event in ["SessionStart", "SubagentStart"] {
                ensure_catalogue(
                    &bin,
                    root,
                    event,
                    &serde_json::json!({"session_id":"b","cwd":cwd}),
                )?;
            }
            let ups_gated = run_hook(
                &bin,
                root,
                "UserPromptSubmit",
                &serde_json::json!({"session_id":"b","cwd":cwd,"prompt":"ok thanks"}),
                runs,
            )?;
            let (ups, delivered) = run_prompts(&bin, root, "b", &full_prompts(root, runs)?)?;
            anyhow::ensure!(
                delivered == runs,
                "the full prompt was given a block in {delivered} of {runs} runs: the contract would time a prompt that delivers nothing"
            );
            // after the rows above, under a session of its own, so none of them sees it
            let post_compact = run_summary_check(&bin, root, runs)?;
            let ingest_ms = ingest_200(root)?;
            // The write hooks last: they fold the logs the read hooks above left, and
            // SessionEnd starts a detached `maintain` each time. Fewer runs: a turn is
            // ingested in each, and the store should not grow past its cap while timed.
            let stop = run_write_hook(&bin, root, "Stop", (runs / 3).max(1))?;
            let session_end = run_write_hook(&bin, root, "SessionEnd", (runs / 10).max(1))?;
            let mut contracts = vec![
                Contract {
                    name: "hook SessionStart p95",
                    value: ss.1,
                    limit: 10.0,
                    unit: "ms",
                    floor: Some(start.1),
                },
                Contract {
                    name: "hook SubagentStart p95",
                    value: subagent.1,
                    limit: 10.0,
                    unit: "ms",
                    floor: Some(start.1),
                },
                Contract {
                    name: "hook UserPromptSubmit gated p95",
                    value: ups_gated.1,
                    limit: 1.0,
                    unit: "ms",
                    floor: Some(start.1),
                },
                Contract {
                    name: "hook UserPromptSubmit full p95",
                    value: ups.1,
                    limit: 10.0,
                    unit: "ms",
                    floor: Some(start.1),
                },
                // The one read hook that parses a transcript had no figure. Limit: twice the
                // largest p95 read, as for the write hooks below. 8.517, 8.380 and 8.568 ms
                // on a Windows desktop where the process starts in 4.9 to 5.2 ms p95
                // (2.920, 2.855 and 2.816 ms under Linux), on a rollout of 160 KB. The parse
                // grows with the rollout, and the largest one seen was 108 MB.
                Contract {
                    name: "hook PostCompact p95 (Codex rollout)",
                    value: post_compact.1,
                    limit: 17.0,
                    unit: "ms",
                    floor: None,
                },
                Contract {
                    name: "ingest 200 records",
                    value: ingest_ms,
                    limit: 500.0,
                    unit: "ms",
                    floor: None,
                },
                // The two write hooks had no contract, and `Stop` grew for a month under
                // none: 219.5 ms with nothing to ingest on a store whose session log had
                // never been compacted (WP-01, WP-19). Limits: twice the largest p95 read.
                // Stop: 60.6, 61.0 and 59.7 ms on a Windows desktop (24.8 and 25.1 under
                // Linux). SessionEnd: 95.5 and 95.0 ms under Linux, where its p50 is 23 ms:
                // each run starts a detached `maintain`, and thirty runs back to back meet
                // the one before (56.1, 56.0 and 55.1 ms on the Windows desktop).
                Contract {
                    name: "hook Stop p95 (one turn)",
                    value: stop.1,
                    limit: 125.0,
                    unit: "ms",
                    floor: None,
                },
                Contract {
                    name: "hook SessionEnd p95 (one turn)",
                    value: session_end.1,
                    limit: 200.0,
                    unit: "ms",
                    floor: None,
                },
            ];
            // The limits were set on a developer machine. CI's shared runners are slower, so
            // CI states how much slack it allows (MUNINN_PERF_LIMIT_SCALE) and the output
            // prints it. Unset, the limits hold as written; a value below 1 is ignored.
            let scale = std::env::var("MUNINN_PERF_LIMIT_SCALE")
                .ok()
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|s| *s >= 1.0)
                .unwrap_or(1.0);
            for c in &mut contracts {
                c.limit *= scale;
            }
            let failed: Vec<&Contract> = contracts.iter().filter(|c| !c.ok()).collect();
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "records": records, "runs": runs, "populate_ms": populate_ms,
                        "process_start": start,
                        "hook": { "SessionStart": ss, "SubagentStart": subagent, "UserPromptSubmit_gated": ups_gated, "UserPromptSubmit_full": ups, "PostCompact_codex_rollout": post_compact, "Stop": stop, "SessionEnd": session_end },
                        "rollout_bytes": codex_rollout(0).len(),
                        "full_prompts_delivered": delivered,
                        "ingest_200_ms": ingest_ms,
                        "limit_scale": scale,
                        "contracts": contracts.iter().map(|c| serde_json::json!({"name": c.name, "value": c.value, "limit": c.limit, "ok": c.ok(), "over_floor": c.over_floor_ok()})).collect::<Vec<_>>(),
                    })
                );
            } else {
                println!("muninn-bench perf · {records} records, {runs} runs · store built in {populate_ms:.0} ms");
                println!("level: management (hook cost). stored/delivered/outcome levels arrive with the experiment.");
                println!(
                    "{:<36} {:>8} {:>8} {:>8}",
                    "measurement", "p50", "p95", "p99"
                );
                for (n, v) in [
                    ("process start (`--version`, no work)", start),
                    ("hook SessionStart", ss),
                    ("hook SubagentStart", subagent),
                    ("hook UserPromptSubmit (gated)", ups_gated),
                    ("hook UserPromptSubmit (full)", ups),
                    ("hook PostCompact (Codex rollout)", post_compact),
                    ("hook Stop (one turn ingested)", stop),
                    ("hook SessionEnd (one turn ingested)", session_end),
                ] {
                    println!("{n:<36} {:>7.3}ms {:>7.3}ms {:>7.3}ms", v.0, v.1, v.2);
                }
                println!(
                    "{:<36} {:>7.1}ms",
                    "ingest 200 records (tx + fts)", ingest_ms
                );
                println!();
                if scale != 1.0 {
                    println!("limits × {scale} for this machine (MUNINN_PERF_LIMIT_SCALE)");
                }
                for c in &contracts {
                    println!(
                        "{} {:<36} {:.3} {} (limit {}){}",
                        if c.ok() { "ok  " } else { "FAIL" },
                        c.name,
                        c.value,
                        c.unit,
                        c.limit,
                        if c.over_floor_ok() {
                            format!(
                                " · over the limit on this machine, where starting the process takes {:.3} ms p95; Muninn adds {:.3} ms",
                                c.floor.unwrap_or_default(),
                                c.value - c.floor.unwrap_or_default()
                            )
                        } else {
                            String::new()
                        }
                    );
                }
            }
            if strict && !failed.is_empty() {
                anyhow::bail!("{} contract(s) exceeded", failed.len());
            }
            Ok(())
        }
    }
}

fn tempfile_dir() -> Result<PathBuf> {
    let base = std::env::temp_dir().join(format!("muninn-bench-{}", std::process::id()));
    if base.exists() {
        std::fs::remove_dir_all(&base)?;
    }
    std::fs::create_dir_all(&base)?;
    Ok(base)
}

/// The `muninn` binary next to this one. `set_file_name("muninn")` dropped the `.exe`, so on
/// Windows `perf`, `enforce` and `experiment` never found the binary they had just been
/// built beside.
fn sibling_muninn() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.set_file_name(format!("muninn{}", std::env::consts::EXE_SUFFIX));
    p
}

#[cfg(test)]
mod tests {
    use super::lists_typed_records;

    /// The rollout the PostCompact contract hands the hook: the size it says, read by the
    /// parser as a Codex session, and its last test command ends as asked. That last exit code
    /// is what the hook's check of a summary turns on, so the contract can tell a run that
    /// read the rollout from one that did not.
    #[test]
    fn the_bench_rollout_is_a_codex_session_whose_last_test_run_ends_as_asked() {
        let dir = std::env::temp_dir().join(format!("muninn-bench-rollout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for exit in [0i64, 1] {
            let text = super::codex_rollout(exit);
            assert!(
                (super::ROLLOUT_BYTES..super::ROLLOUT_BYTES * 2).contains(&text.len()),
                "{} bytes",
                text.len()
            );
            let path = dir.join(format!("rollout-bench-{exit}.jsonl"));
            std::fs::write(&path, &text).unwrap();
            let session = muninn_capture::parse_any(&path, 0).unwrap();
            let last = session
                .turns
                .iter()
                .flat_map(|t| &t.tools)
                .rfind(|c| c.target.contains("test"))
                .expect("a test command");
            assert_eq!(last.exit_code, Some(exit));
            assert!(session.turns.len() > 20, "{} turns", session.turns.len());
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The check that keeps the session-start contracts on the catalogue's normal path: it
    /// passes on a list of typed records and refuses the sparse list of what was said, a
    /// context with no catalogue, and a heading with no line under it.
    #[test]
    fn the_catalogue_check_tells_typed_records_from_the_sparse_list() {
        let typed = "MUNINN 9/9 GREEN\n[muninn:catalog] what is on record: rules that stand and corrections, then decisions, each newest first\n#19951 invariant \u{b7} retry backoff jwt\n#19999 decision \u{b7} cache webhook queue\n\u{2026} and more, not listed\n";
        assert!(lists_typed_records(typed));
        let sparse = "[muninn:catalog] what is on record: rules that stand and corrections, then decisions, then what was said, each newest first\n#19998 episode \u{b7} token refresh sqlite\nThat is all of it.\n";
        assert!(!lists_typed_records(sparse));
        assert!(!lists_typed_records("MUNINN 9/9 GREEN\n"));
        assert!(!lists_typed_records(
            "[muninn:catalog] what is on record: rules that stand and corrections, then decisions, each newest first\n"
        ));
    }
}
