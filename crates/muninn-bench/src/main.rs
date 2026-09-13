//! muninn-bench: performance regressions and, later, the four-arm experiment.
//!
//! `perf` regenerates a deterministic synthetic store, measures the hook binary
//! end to end (process start → exit) and the cue-evaluation strategies that fixed
//! the design ([M2]: normalised dir-ancestor lookup vs GLOB), and fails with
//! `--strict` when a contract is exceeded. Every number printed is measured here,
//! on this machine, now.

mod experiment;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use muninn_core::{Db, Mode};
use rusqlite::Connection;
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
    },
    /// Four-arm experiment (Gate 2): see experiment/PREREGISTRATION.md
    Experiment {
        #[arg(long, default_value = "crates/muninn-bench/experiment/tasks.json")]
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
        #[arg(long)]
        muninn: Option<PathBuf>,
    },
    /// Performance contracts
    Perf {
        /// Fail on any contract violation
        #[arg(long)]
        strict: bool,
        /// Records in the synthetic store
        #[arg(long, default_value_t = 5000)]
        records: usize,
        /// Cues in the synthetic store
        #[arg(long, default_value_t = 15000)]
        cues: usize,
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

fn populate(root: &Path, records: usize, cues: usize) -> Result<()> {
    std::fs::create_dir_all(root.join(".git"))?;
    let m = root.join(".muninn");
    for d in ["", "records", "compiled", "log", "compact"] {
        std::fs::create_dir_all(m.join(d))?;
    }
    let db = Db::open(&m.join("muninn.db"), Mode::ReadWrite)?;
    let mut rng = Rng(42);
    let t = Instant::now();
    let tx = db.conn.unchecked_transaction()?;
    {
        let mut st = tx.prepare(
            "INSERT INTO record(kind,subject,relation,object,body,origin,trust,anchor_path,session_id,dedup_hash,created_at) \
             VALUES(?1,?2,?3,?4,?5,'commit_linked',2,?6,'bench',?7,?8)",
        )?;
        let mut cue = tx.prepare("INSERT INTO cue(record_id,kind,key,grp) VALUES(?1,?2,?3,0)")?;
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
            let id = tx.last_insert_rowid();
            // dir cue on every record; symbol cue on half; extra dir cues to reach the target
            cue.execute(rusqlite::params![id, "dir", dir])?;
            if i % 2 == 0 {
                cue.execute(rusqlite::params![id, "symbol", rng.pick(WORDS)])?;
            }
        }
        let mut n = db
            .conn
            .query_row("SELECT count(*) FROM cue", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as usize;
        let mut i = 0usize;
        while n < cues {
            let id = (rng.next() as usize % records) as i64 + 1;
            cue.execute(rusqlite::params![
                id,
                "dir",
                format!("{}/{}", rng.pick(DIRS), rng.pick(WORDS))
            ])?;
            n += 1;
            i += 1;
            if i > cues * 2 {
                break;
            }
        }
    }
    tx.commit()?;
    db.conn
        .execute_batch("INSERT INTO record_fts(record_fts) VALUES('optimize');")?;
    db.meta_set(
        "bench_populated_ms",
        &(t.elapsed().as_secs_f64() * 1000.0).to_string(),
    )?;
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

/// Re-creation of the cue benchmark from [M2]: GLOB per touched file vs
/// normalised dir-prefix ancestor lookup on an indexed table.
type P3 = (f64, f64, f64);

fn cue_eval(root: &Path, runs: usize) -> Result<(P3, P3)> {
    let conn = Connection::open(root.join(".muninn/muninn.db"))?;
    conn.execute_batch("PRAGMA query_only=1;")?;
    let mut rng = Rng(7);
    // a turn context: 20 touched files, 50 symbols
    let files: Vec<String> = (0..20)
        .map(|_| {
            format!(
                "{}/{}/{}.rs",
                rng.pick(DIRS),
                rng.pick(WORDS),
                rng.pick(WORDS)
            )
        })
        .collect();
    let syms: Vec<String> = (0..50).map(|_| rng.pick(WORDS).to_string()).collect();

    // strategy A: GLOB per file against every dir cue
    let mut glob_st =
        conn.prepare("SELECT record_id FROM cue WHERE kind='dir' AND ?1 GLOB (key || '*')")?;
    let mut sym_st = conn.prepare("SELECT record_id FROM cue WHERE kind='symbol' AND key=?1")?;
    let mut ta = Vec::with_capacity(runs);
    for _ in 0..runs {
        let t = Instant::now();
        let mut hits = 0usize;
        for f in &files {
            hits += glob_st.query_map([f], |r| r.get::<_, i64>(0))?.count();
        }
        for s in &syms {
            hits += sym_st.query_map([s], |r| r.get::<_, i64>(0))?.count();
        }
        std::hint::black_box(hits);
        ta.push(t.elapsed().as_secs_f64() * 1000.0);
    }

    // strategy B: normalised ancestors, exact indexed lookups
    let mut anc_st = conn.prepare("SELECT record_id FROM cue WHERE kind='dir' AND key=?1")?;
    let mut tb = Vec::with_capacity(runs);
    for _ in 0..runs {
        let t = Instant::now();
        let mut hits = 0usize;
        let mut seen = std::collections::HashSet::new();
        for f in &files {
            let mut p = Path::new(f).parent();
            while let Some(d) = p {
                let key = d.to_string_lossy();
                if key.is_empty() {
                    break;
                }
                if seen.insert(key.to_string()) {
                    hits += anc_st
                        .query_map([key.as_ref()], |r| r.get::<_, i64>(0))?
                        .count();
                }
                p = d.parent();
            }
        }
        for s in &syms {
            hits += sym_st.query_map([s], |r| r.get::<_, i64>(0))?.count();
        }
        std::hint::black_box(hits);
        tb.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    Ok((
        (
            pct(&mut ta.clone(), 0.5),
            pct(&mut ta.clone(), 0.95),
            pct(&mut ta, 0.99),
        ),
        (
            pct(&mut tb.clone(), 0.5),
            pct(&mut tb.clone(), 0.95),
            pct(&mut tb, 0.99),
        ),
    ))
}

fn ingest_200(root: &Path) -> Result<f64> {
    let db = Db::open(&root.join(".muninn/muninn.db"), Mode::ReadWrite)?;
    let mut rng = Rng(99);
    let t = Instant::now();
    let tx = db.conn.unchecked_transaction()?;
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
            muninn,
        } => {
            let bin = muninn.unwrap_or_else(|| {
                let mut p = std::env::current_exe().unwrap();
                p.set_file_name("muninn");
                p
            });
            anyhow::ensure!(bin.exists(), "muninn binary not found at {}", bin.display());
            experiment::run(&config, &out, &bin, dry_run, pilot, runs, model, jobs)
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
        } => rules_cmd(&dir, sample, n, seed, labels, list, holdout, json),
        Cmd::Perf {
            strict,
            records,
            cues,
            runs,
            muninn,
            json,
        } => {
            let bin = muninn.unwrap_or_else(|| {
                let mut p = std::env::current_exe().unwrap();
                p.set_file_name("muninn");
                p
            });
            anyhow::ensure!(bin.exists(), "muninn binary not found at {} (build it with `cargo build --release -p muninn-cli`)", bin.display());
            let tmp = tempfile_dir()?;
            let root = tmp.as_path();
            let t = Instant::now();
            populate(root, records, cues)?;
            let populate_ms = t.elapsed().as_secs_f64() * 1000.0;
            let cwd = root.to_string_lossy().to_string();
            let ss = run_hook(
                &bin,
                root,
                "SessionStart",
                &serde_json::json!({"session_id":"b","cwd":cwd}),
                runs,
            )?;
            let ups_gated = run_hook(
                &bin,
                root,
                "UserPromptSubmit",
                &serde_json::json!({"session_id":"b","cwd":cwd,"prompt":"ok thanks"}),
                runs,
            )?;
            let ups = run_hook(
                &bin,
                root,
                "UserPromptSubmit",
                &serde_json::json!({"session_id":"b","cwd":cwd,"prompt":"implement retry backoff for the webhook handler with idempotent tokens"}),
                runs,
            )?;
            let (glob, anc) = cue_eval(root, runs)?;
            let ingest_ms = ingest_200(root)?;
            let contracts = vec![
                Contract {
                    name: "hook SessionStart p95",
                    value: ss.1,
                    limit: 10.0,
                    unit: "ms",
                },
                Contract {
                    name: "hook UserPromptSubmit gated p95",
                    value: ups_gated.1,
                    limit: 1.0,
                    unit: "ms",
                },
                Contract {
                    name: "hook UserPromptSubmit full p95",
                    value: ups.1,
                    limit: 10.0,
                    unit: "ms",
                },
                Contract {
                    name: "cue eval (ancestor lookup) p99",
                    value: anc.2,
                    limit: 3.0,
                    unit: "ms",
                },
                Contract {
                    name: "ingest 200 records",
                    value: ingest_ms,
                    limit: 500.0,
                    unit: "ms",
                },
            ];
            let failed: Vec<&Contract> = contracts.iter().filter(|c| c.value > c.limit).collect();
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "records": records, "cues": cues, "runs": runs, "populate_ms": populate_ms,
                        "hook": { "SessionStart": ss, "UserPromptSubmit_gated": ups_gated, "UserPromptSubmit_full": ups },
                        "cue_eval": { "glob": glob, "ancestor": anc },
                        "ingest_200_ms": ingest_ms,
                        "contracts": contracts.iter().map(|c| serde_json::json!({"name": c.name, "value": c.value, "limit": c.limit, "ok": c.value <= c.limit})).collect::<Vec<_>>(),
                    })
                );
            } else {
                println!("muninn-bench perf · {records} records, {cues} cues, {runs} runs · store built in {populate_ms:.0} ms");
                println!("level: management (hook cost). stored/delivered/outcome levels arrive with the experiment.");
                println!(
                    "{:<36} {:>8} {:>8} {:>8}",
                    "measurement", "p50", "p95", "p99"
                );
                for (n, v) in [
                    ("hook SessionStart", ss),
                    ("hook UserPromptSubmit (gated)", ups_gated),
                    ("hook UserPromptSubmit (full)", ups),
                    ("cue eval GLOB per file", glob),
                    ("cue eval ancestor lookup", anc),
                ] {
                    println!("{n:<36} {:>7.3}ms {:>7.3}ms {:>7.3}ms", v.0, v.1, v.2);
                }
                println!(
                    "{:<36} {:>7.1}ms",
                    "ingest 200 records (tx + fts)", ingest_ms
                );
                println!(
                    "cue speedup ancestor vs GLOB (p50): {:.1}x",
                    glob.0 / anc.0.max(0.001)
                );
                println!();
                for c in &contracts {
                    println!(
                        "{} {:<36} {:.3} {} (limit {})",
                        if c.value <= c.limit { "ok  " } else { "FAIL" },
                        c.name,
                        c.value,
                        c.unit,
                        c.limit
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
