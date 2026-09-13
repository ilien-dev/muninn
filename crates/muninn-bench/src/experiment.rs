//! Four-arm experiment runner (Gate 2). See experiment/PREREGISTRATION.md.
//! Every cell: fresh worktree at base_ref, fresh store seeded from prior-session
//! transcripts, one `claude -p` run with the hooks wired through `--settings`,
//! then the task's executable oracle. Nothing is scored by a model.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize, Clone)]
pub struct Task {
    pub id: String,
    #[serde(default)]
    pub inferable: bool,
    pub prompt: String,
    pub oracle: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub repo: String,
    pub base_ref: String,
    pub model: String,
    pub runs: usize,
    pub arms: Vec<String>,
    #[serde(default = "d_turns")]
    pub max_turns: usize,
    #[serde(default = "d_timeout")]
    pub timeout_s: u64,
    pub seed_transcripts: Vec<String>,
    #[serde(default)]
    pub control_transcripts: Vec<String>,
    /// JSONL records imported into every cell's store after the transcripts (the
    /// revocation grid seeds retired and replacement facts this way). The `unfiltered`
    /// arm imports them with every `invalid` flag cleared: same records, same layout,
    /// invalidation off — the render-matched control [X1].
    #[serde(default)]
    pub seed_records: Option<String>,
    /// `claude` (default) or `codex`: the harness that runs each cell. Hooks, arms,
    /// oracles and the store are identical; only the agent process differs.
    #[serde(default = "d_harness")]
    pub harness: String,
    pub tasks: Vec<Task>,
}

fn d_harness() -> String {
    "claude".into()
}

/// `codex exec` takes its hooks as `-c` overrides (TOML inline tables), so a cell needs
/// no file in the worktree and no persisted hook trust.
fn codex_hook_overrides(muninn: &Path) -> Vec<String> {
    let bin = muninn.to_string_lossy();
    let one = |key: &str, ev: &str, timeout: u64, is_async: bool| {
        format!(
            "hooks.{key}=[{{hooks=[{{type=\"command\",command=\"{bin} hook {ev}\",timeout={timeout}{}}}]}}]",
            if is_async { ",async=true" } else { "" }
        )
    };
    vec![
        one("session_start", "SessionStart", 5, false),
        one("user_prompt_submit", "UserPromptSubmit", 2, false),
        one("stop", "Stop", 30, false),
        one("session_end", "SessionEnd", 5, false),
    ]
}

/// Reduce `codex exec --json` events to the shape the cell expects: the final agent
/// message, a turn count (command executions + 1) and token usage; no USD figure.
fn codex_summary(jsonl: &str) -> serde_json::Value {
    let mut result = String::new();
    let mut commands = 0u64;
    let mut usage = serde_json::Value::Null;
    let mut error: Option<String> = None;
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match v["type"].as_str() {
            Some("item.completed") => match v["item"]["type"].as_str() {
                Some("agent_message") => {
                    result = v["item"]["text"].as_str().unwrap_or("").to_string()
                }
                Some("command_execution") => commands += 1,
                Some("error") => {
                    let m = v["item"]["message"].as_str().unwrap_or("");
                    if !m.contains("bypass-hook-trust") {
                        error = Some(m.to_string());
                    }
                }
                _ => {}
            },
            Some("turn.completed") => usage = v["usage"].clone(),
            Some("turn.failed") | Some("error") => {
                error = Some(
                    v["error"]["message"]
                        .as_str()
                        .or(v["message"].as_str())
                        .unwrap_or("turn failed")
                        .to_string(),
                )
            }
            _ => {}
        }
    }
    serde_json::json!({
        "result": result,
        "num_turns": commands + 1,
        "usage": usage,
        "is_error": error.is_some(),
        "error": error,
        "harness": "codex",
    })
}
fn d_turns() -> usize {
    25
}
fn d_timeout() -> u64 {
    600
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Cell {
    pub run: usize,
    pub task: String,
    pub inferable: bool,
    pub arm: String,
    pub order: usize,
    pub status: String, // pass | fail | error
    pub oracle_exit: Option<i32>,
    pub claude_exit: Option<i32>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<u64>,
    pub duration_ms: u128,
    pub delivered_tokens: i64,
    pub delivered_records: i64,
    pub hook_p95_ms: Option<f64>,
    pub stored_episodes: i64,
    /// Deliveries of records that were retired in the store (must be 0 with the filter).
    #[serde(default)]
    pub served_invalid: i64,
    pub error: Option<String>,
}

fn expand(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h).join(rest);
        }
    }
    PathBuf::from(p)
}

fn transcripts(list: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in list {
        let p = expand(p);
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                let mut v: Vec<PathBuf> = rd
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|x| x.extension().is_some_and(|e| e == "jsonl"))
                    .collect();
                v.sort();
                out.extend(v);
            }
        } else if p.is_file() {
            out.push(p);
        }
    }
    out
}

fn run_ok(cmd: &mut Command) -> Result<String> {
    let out = cmd
        .output()
        .with_context(|| format!("spawning {:?}", cmd))?;
    if !out.status.success() {
        bail!("{:?} failed: {}", cmd, String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// `boot`: write the ≤1000-token boot block into the cell's CLAUDE.md — every arm that
/// has Muninn pays for it (plan, Phase 0 §10-bis); the `off` arm has no Muninn.
fn seed_store(muninn: &Path, dir: &Path, seeds: &[PathBuf], boot: bool) -> Result<i64> {
    seed_store_arm(muninn, dir, seeds, boot, None, "literal")
}

fn seed_store_arm(
    muninn: &Path,
    dir: &Path,
    seeds: &[PathBuf],
    boot: bool,
    records: Option<&Path>,
    arm: &str,
) -> Result<i64> {
    seed_store_full(muninn, dir, seeds, boot, records, arm, None)
}

/// `source`: the checkout whose symbol graph goes into the store before the seeded
/// records are imported, so their symbol cues come from the graph, not the fallback.
#[allow(clippy::too_many_arguments)]
fn seed_store_full(
    muninn: &Path,
    dir: &Path,
    seeds: &[PathBuf],
    boot: bool,
    records: Option<&Path>,
    arm: &str,
    source: Option<&Path>,
) -> Result<i64> {
    // init touches no file by default; the boot block (file or hook) is the cell's
    // business, decided by its arm
    let _ = boot;
    let args = vec!["--cwd", dir.to_str().unwrap(), "init", "--keep-native"];
    run_ok(Command::new(muninn).env("MUNINN_ROOT", dir).args(&args))?;
    for s in seeds {
        run_ok(Command::new(muninn).env("MUNINN_ROOT", dir).args([
            "--cwd",
            dir.to_str().unwrap(),
            "ingest",
            s.to_str().unwrap(),
        ]))?;
    }
    if let Some(src) = source {
        run_ok(
            Command::new(muninn)
                .env("MUNINN_ROOT", dir)
                .env("MUNINN_SOURCE_ROOT", src)
                .args(["--cwd", dir.to_str().unwrap(), "symbols"]),
        )?;
    }
    if let Some(r) = records {
        run_ok(Command::new(muninn).env("MUNINN_ROOT", dir).args([
            "--cwd",
            dir.to_str().unwrap(),
            "import",
            r.to_str().unwrap(),
        ]))?;
    }
    // no Markdown mirror in a cell: the agent must get memory through the hooks only
    let _ = std::fs::remove_dir_all(dir.join(".muninn/records"));
    let _ = std::fs::remove_file(dir.join(".muninn/index.md"));
    let db = rusqlite::Connection::open(dir.join(".muninn/muninn.db"))?;
    if arm == "unfiltered" {
        // invalidation off: every seeded record is active and indexed
        db.execute("UPDATE record SET invalid = 0, invalid_reason = NULL, invalidated_by = NULL WHERE invalid = 1", [])?;
    }
    Ok(
        db.query_row("SELECT count(*) FROM record WHERE invalid=0", [], |r| {
            r.get(0)
        })?,
    )
}

fn settings_json(muninn: &Path) -> serde_json::Value {
    let bin = muninn.to_string_lossy().to_string();
    let h = |ev: &str, timeout: u64| serde_json::json!([{ "hooks": [{ "type": "command", "command": bin, "args": ["hook", ev], "timeout": timeout }] }]);
    serde_json::json!({
        "autoMemoryEnabled": false,
        "hooks": {
            "SessionStart": h("SessionStart", 5),
            "UserPromptSubmit": h("UserPromptSubmit", 5),
            // confinement (MUNINN_CONFINE_ROOT): an edit outside the checkout is denied
            "PreToolUse": [{ "matcher": "Edit|Write|MultiEdit|NotebookEdit", "hooks": [{ "type": "command", "command": bin, "args": ["hook", "PreToolUse"], "timeout": 5 }] }],
            "PostToolUse": [{ "matcher": "Bash|Edit|Write|MultiEdit|Read", "hooks": [{ "type": "command", "command": bin, "args": ["hook", "PostToolUse"], "timeout": 5 }] }],
            "Stop": h("Stop", 30),
            "SessionEnd": h("SessionEnd", 5)
        }
    })
}

fn pct(v: &mut [f64], p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(v[((v.len() as f64 * p) as usize).min(v.len() - 1)])
}

fn measure_store(dir: &Path) -> (i64, i64, Option<f64>) {
    let Ok(db) = rusqlite::Connection::open(dir.join(".muninn/muninn.db")) else {
        return (0, 0, None);
    };
    let tokens: i64 = db
        .query_row(
            "SELECT coalesce(sum(tokens),0) FROM fire_ledger WHERE tokens > 0",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let recs: i64 = db
        .query_row(
            "SELECT count(*) FROM fire_ledger WHERE record_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    // delivery.jsonl may not have been folded if Stop did not fire; count it too
    let mut extra_tokens = 0i64;
    let mut extra_recs = 0i64;
    if let Ok(s) = std::fs::read_to_string(dir.join(".muninn/log/delivery.jsonl")) {
        for l in s.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(l) {
                if v["reason"]
                    .as_str()
                    .map(|r| !r.starts_with("off") && !r.starts_with("silence"))
                    .unwrap_or(false)
                {
                    extra_tokens += v["tokens"].as_i64().unwrap_or(0);
                    extra_recs += v["ids"].as_array().map(|a| a.len() as i64).unwrap_or(0);
                }
            }
        }
    }
    let mut ms: Vec<f64> = Vec::new();
    if let Ok(mut st) =
        db.prepare("SELECT ms FROM heartbeat WHERE hook='UserPromptSubmit' AND ms IS NOT NULL")
    {
        if let Ok(rows) = st.query_map([], |r| r.get::<_, f64>(0)) {
            ms.extend(rows.filter_map(|r| r.ok()));
        }
    }
    if let Ok(s) = std::fs::read_to_string(dir.join(".muninn/log/heartbeat.jsonl")) {
        for l in s.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(l) {
                if v["ev"] == "finish" && v["hook"] == "UserPromptSubmit" {
                    if let Some(m) = v["ms"].as_f64() {
                        ms.push(m);
                    }
                }
            }
        }
    }
    (tokens + extra_tokens, recs + extra_recs, pct(&mut ms, 0.95))
}

#[allow(clippy::too_many_arguments)]
fn run_cell(
    cfg: &Config,
    muninn: &Path,
    repo: &Path,
    seeds: &[PathBuf],
    control_db: Option<&Path>,
    task: &Task,
    arm: &str,
    run: usize,
    order: usize,
    work: &Path,
) -> Cell {
    let t0 = Instant::now();
    let mut cell = Cell {
        run,
        task: task.id.clone(),
        inferable: task.inferable,
        arm: arm.into(),
        order,
        status: "error".into(),
        oracle_exit: None,
        claude_exit: None,
        cost_usd: None,
        num_turns: None,
        duration_ms: 0,
        delivered_tokens: 0,
        delivered_records: 0,
        hook_p95_ms: None,
        stored_episodes: 0,
        served_invalid: 0,
        error: None,
    };
    let dir = work.join(format!("cell-r{run}-{}-{arm}", task.id));
    // the store lives outside the checkout: an agent that reads its own working tree
    // must not find the database (retired flags included) around the hooks
    let store = work
        .parent()
        .unwrap_or(work)
        .join("stores")
        .join(format!("store-r{run}-{}-{arm}", task.id));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&store);
    let res: Result<()> = (|| {
        // a single-commit repository from `git archive`: no branch, tag, reflog or
        // later commit of the real repository is reachable from inside the cell
        std::fs::create_dir_all(&dir)?;
        let archive = Command::new("git")
            .current_dir(repo)
            .args(["archive", "--format=tar", &cfg.base_ref])
            .output()
            .context("git archive")?;
        anyhow::ensure!(
            archive.status.success(),
            "git archive {} failed",
            cfg.base_ref
        );
        let mut tar = Command::new("tar")
            .current_dir(&dir)
            .args(["-x"])
            .stdin(Stdio::piped())
            .spawn()
            .context("tar")?;
        std::io::Write::write_all(tar.stdin.as_mut().unwrap(), &archive.stdout)?;
        drop(tar.stdin.take());
        anyhow::ensure!(tar.wait()?.success(), "tar failed");
        for a in [
            vec!["init", "-q"],
            vec!["add", "-A"],
            vec![
                "-c",
                "user.email=cell@muninn",
                "-c",
                "user.name=cell",
                "commit",
                "-q",
                "-m",
                "base",
            ],
        ] {
            run_ok(Command::new("git").current_dir(&dir).args(&a))?;
        }
        std::fs::create_dir_all(store.join(".git"))?;
        let seed_records = cfg.seed_records.as_deref().map(expand);
        cell.stored_episodes = seed_store_full(
            muninn,
            &store,
            seeds,
            false,
            seed_records.as_deref(),
            arm,
            Some(&dir),
        )?;
        // the write path once, so the store starts warm (anchors, cues, sidecar)
        let _ = run_ok(
            Command::new(muninn)
                .env("MUNINN_ROOT", &store)
                .env("MUNINN_SOURCE_ROOT", &dir)
                .env("MUNINN_NO_PROJECT", "1")
                .args(["--cwd", store.to_str().unwrap(), "maintain"]),
        );
        // the boot block goes into the checkout (every arm with Muninn), from the
        // plugin template of the repository under test
        // `<arm>-hookboot`: same arm, the compact summary injected by the SessionStart
        // hook instead of the long block in the file (the shipped default)
        let hookboot = arm.ends_with("-hookboot");
        let base_arm = arm.strip_suffix("-hookboot").unwrap_or(arm);
        if base_arm != "off" && !hookboot {
            let tpl = repo.join("plugin/templates/CLAUDE.muninn.md");
            if let Ok(t) = std::fs::read_to_string(&tpl) {
                for f in ["CLAUDE.md", "AGENTS.md"] {
                    let _ = std::fs::write(dir.join(f), &t);
                }
            }
        }
        let settings = store.join("claude-settings.json");
        std::fs::write(
            &settings,
            serde_json::to_string_pretty(&settings_json(muninn))?,
        )?;
        let prompt = format!("{}\n\nWork only inside the current working directory (this repository checkout); write files by paths relative to it and never outside it.", task.prompt);
        let mut cmd = if cfg.harness == "codex" {
            let mut c = Command::new("codex");
            c.current_dir(&dir).args([
                "exec",
                "--json",
                "-C",
                dir.to_str().unwrap(),
                "-s",
                "workspace-write",
                "--skip-git-repo-check",
                "--dangerously-bypass-hook-trust",
                "-c",
                "features.hooks=true",
                "-m",
                &cfg.model,
            ]);
            for o in codex_hook_overrides(muninn) {
                c.args(["-c", &o]);
            }
            c.arg(&prompt);
            c
        } else {
            let mut c = Command::new("claude");
            c.current_dir(&dir)
                .args(["-p", &prompt, "--model", &cfg.model, "--output-format", "json", "--max-turns", &cfg.max_turns.to_string(), "--permission-mode", "acceptEdits", "--settings", settings.to_str().unwrap()])
                .args(["--allowedTools", "Read,Edit,Write,MultiEdit,Grep,Glob,Bash(git diff *),Bash(git log *),Bash(git status *),Bash(cat *),Bash(grep *),Bash(awk *),Bash(sed -n *),Bash(head *),Bash(tail *),Bash(wc *),Bash(ls *),Bash(muninn why *),Bash(muninn status *),Bash(muninn why:*),Bash(muninn status:*)"]);
            c
        };
        // `muninn why` / `muninn status` are part of the product the boot block
        // describes: the binary's directory goes on PATH for the agent's own shell
        let path = std::env::var("PATH").unwrap_or_default();
        let bin_dir = muninn
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        cmd.env("PATH", format!("{bin_dir}:{path}"))
            .env("MUNINN_NO_PROJECT", "1")
            .env("MUNINN_SOURCE_ROOT", &dir)
            .env("MUNINN_CONFINE_ROOT", &dir)
            .env("MUNINN_ARM", base_arm)
            .env(
                "MUNINN_BOOT",
                if base_arm == "off" {
                    "off"
                } else if hookboot {
                    "hook"
                } else {
                    "file"
                },
            )
            .env("MUNINN_ROOT", &store)
            .env_remove("CLAUDECODE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(c) = control_db {
            cmd.env("MUNINN_CONTROL_DB", c);
        }
        // query expansion is an opt-in in the shipped default (GATE4.md §1, second
        // grid); the arms that carry it say so explicitly
        if base_arm == "lexical" {
            cmd.env("MUNINN_NO_CUES", "1").env("MUNINN_EXPAND", "1");
        } else if base_arm == "lexical-plain" {
            cmd.env("MUNINN_NO_CUES", "1").env("MUNINN_EXPAND", "0");
        } else {
            cmd.env("MUNINN_EXPAND", "1");
            // experiments measure the full mechanism; the shipped default keeps
            // dir/symbol cues off (GATE4.md)
            cmd.env("MUNINN_CUES", "1");
        }
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().context("spawning the harness")?;
        let deadline = Instant::now() + Duration::from_secs(cfg.timeout_s);
        // stdout/stderr are drained by threads: a harness that spawns grandchildren
        // (codex → node → vendor binary) must not be able to hold the pipe open past
        // the deadline, and a chatty --json stream must not fill the pipe buffer
        let mut so = child.stdout.take().expect("piped stdout");
        let mut se = child.stderr.take().expect("piped stderr");
        let so_t = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = std::io::Read::read_to_end(&mut so, &mut b);
            b
        });
        let se_t = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = std::io::Read::read_to_end(&mut se, &mut b);
            b
        });
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break Some(s);
            }
            if Instant::now() > deadline {
                // the whole process group, not just the direct child
                let _ = Command::new("kill")
                    .args(["-9", "--", &format!("-{}", child.id())])
                    .output();
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        let out = std::process::Output {
            status: status
                .unwrap_or_else(|| child.wait().unwrap_or(std::process::ExitStatus::default())),
            stdout: so_t.join().unwrap_or_default(),
            stderr: se_t.join().unwrap_or_default(),
        };
        let raw = String::from_utf8_lossy(&out.stdout).to_string();
        let stdout = if cfg.harness == "codex" {
            codex_summary(&raw).to_string()
        } else {
            raw
        };
        // the model's own account of the cell, for audit (what it read, what it claimed)
        let logs = work.parent().unwrap_or(work).join("logs");
        let _ = std::fs::create_dir_all(&logs);
        let _ = std::fs::write(logs.join(format!("r{run}-{}-{arm}.json", task.id)), &stdout);
        let _ = std::fs::write(
            logs.join(format!("r{run}-{}-{arm}.stderr", task.id)),
            &out.stderr,
        );
        match status {
            None => {
                cell.error = Some("timeout".into());
            }
            Some(s) => {
                cell.claude_exit = s.code();
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&stdout) {
                    cell.cost_usd = v["total_cost_usd"].as_f64();
                    cell.num_turns = v["num_turns"].as_u64();
                    if v["is_error"].as_bool().unwrap_or(false) {
                        cell.error = Some(format!(
                            "claude is_error: {}",
                            v["result"]
                                .as_str()
                                .unwrap_or("")
                                .chars()
                                .take(200)
                                .collect::<String>()
                        ));
                    }
                } else {
                    cell.error = Some(format!(
                        "claude exit {:?}: {}",
                        s.code(),
                        String::from_utf8_lossy(&out.stderr)
                            .chars()
                            .take(300)
                            .collect::<String>()
                    ));
                }
            }
        }
        // make sure the write path ran even if the harness skipped SessionEnd
        let _ = Command::new(muninn)
            .env("MUNINN_ROOT", &store)
            .current_dir(&dir)
            .args(["hook", "SessionEnd"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .and_then(|mut c| {
                if let Some(mut si) = c.stdin.take() {
                    let _ = si.write_all(
                        format!("{{\"session_id\":\"cell\",\"cwd\":\"{}\"}}", dir.display())
                            .as_bytes(),
                    );
                }
                c.wait()
            });
        // keep the diff for audit before the worktree goes away (new files included)
        let _ = Command::new("git")
            .current_dir(&dir)
            .args([
                "add",
                "-A",
                "--",
                ".",
                ":(exclude).muninn",
                ":(exclude)CLAUDE.md",
                ":(exclude)AGENTS.md",
                ":(exclude).claude",
                ":(exclude).gitignore",
            ])
            .output();
        if let Ok(d) = Command::new("git")
            .current_dir(&dir)
            .args(["diff", "--cached", "--no-color"])
            .output()
        {
            let diffs = work.parent().unwrap_or(work).join("diffs");
            let _ = std::fs::create_dir_all(&diffs);
            let _ = std::fs::write(
                diffs.join(format!("r{run}-{}-{arm}.patch", task.id)),
                &d.stdout,
            );
        }
        // the cell's delivery and turn-context logs, for audit (which cue fired, when)
        {
            let logs = work.parent().unwrap_or(work).join("logs");
            let _ = std::fs::create_dir_all(&logs);
            for (src, suffix) in [
                ("delivery.jsonl", "delivery.jsonl"),
                ("turn_context.jsonl", "context.jsonl"),
            ] {
                let from = store.join(".muninn/log").join(src);
                if from.exists() {
                    let _ = std::fs::copy(
                        &from,
                        logs.join(format!("r{run}-{}-{arm}.{suffix}", task.id)),
                    );
                }
            }
        }
        // the folded ledger too: every fire and silence with its reason
        if let Ok(db) = rusqlite::Connection::open(store.join(".muninn/muninn.db")) {
            if let Ok(mut st) = db
                .prepare("SELECT fired_at, record_id, tokens, reason FROM fire_ledger ORDER BY id")
            {
                let rows: Vec<serde_json::Value> = st
                    .query_map([], |r| {
                        Ok(serde_json::json!({"at": r.get::<_, i64>(0)?, "id": r.get::<_, Option<i64>>(1)?, "tokens": r.get::<_, i64>(2)?, "reason": r.get::<_, String>(3)?}))
                    })
                    .map(|it| it.filter_map(|x| x.ok()).collect())
                    .unwrap_or_default();
                let logs = work.parent().unwrap_or(work).join("logs");
                let _ = std::fs::write(
                    logs.join(format!("r{run}-{}-{arm}.ledger.jsonl", task.id)),
                    rows.iter()
                        .map(|v| v.to_string() + "\n")
                        .collect::<String>(),
                );
            }
        }
        let (tok, recs, p95) = measure_store(&store);
        cell.delivered_tokens = tok;
        cell.delivered_records = recs;
        cell.hook_p95_ms = p95;
        cell.served_invalid = rusqlite::Connection::open(store.join(".muninn/muninn.db"))
            .and_then(|db| {
                db.query_row(
                    "SELECT count(*) FROM fire_ledger f JOIN record r ON r.id = f.record_id WHERE r.invalid = 1",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap_or(0);
        if cell.error.is_none() {
            let o = Command::new("sh")
                .current_dir(&dir)
                .args(["-c", &task.oracle])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()?;
            cell.oracle_exit = o.status.code();
            cell.status = if o.status.success() {
                "pass".into()
            } else {
                "fail".into()
            };
        }
        Ok(())
    })();
    if let Err(e) = res {
        cell.error = Some(format!("{e:#}"));
        cell.status = "error".into();
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&store);
    cell.duration_ms = t0.elapsed().as_millis();
    cell
}

fn bootstrap_diff(cells: &[Cell], a: &str, b: &str, iters: usize, seed: u64) -> (f64, f64, f64) {
    // stratified by task: resample within each task's cells for each arm
    let mut tasks: Vec<String> = cells.iter().map(|c| c.task.clone()).collect();
    tasks.sort();
    tasks.dedup();
    let pick = |arm: &str, t: &str| -> Vec<f64> {
        cells
            .iter()
            .filter(|c| c.arm == arm && c.task == t && c.status != "error")
            .map(|c| if c.status == "pass" { 1.0 } else { 0.0 })
            .collect()
    };
    let mut rng = crate::Rng(seed);
    let mut diffs = Vec::with_capacity(iters);
    let point = {
        let (mut sa, mut na, mut sb, mut nb) = (0.0, 0, 0.0, 0);
        for t in &tasks {
            for x in pick(a, t) {
                sa += x;
                na += 1;
            }
            for x in pick(b, t) {
                sb += x;
                nb += 1;
            }
        }
        if na == 0 || nb == 0 {
            return (f64::NAN, f64::NAN, f64::NAN);
        }
        sa / na as f64 - sb / nb as f64
    };
    for _ in 0..iters {
        let (mut sa, mut na, mut sb, mut nb) = (0.0, 0, 0.0, 0);
        for t in &tasks {
            let va = pick(a, t);
            let vb = pick(b, t);
            for _ in 0..va.len() {
                sa += va[(rng.next() as usize) % va.len()];
                na += 1;
            }
            for _ in 0..vb.len() {
                sb += vb[(rng.next() as usize) % vb.len()];
                nb += 1;
            }
        }
        if na > 0 && nb > 0 {
            diffs.push(sa / na as f64 - sb / nb as f64);
        }
    }
    diffs.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let lo = diffs[(diffs.len() as f64 * 0.025) as usize];
    let hi = diffs[((diffs.len() as f64 * 0.975) as usize).min(diffs.len() - 1)];
    (point, lo, hi)
}

pub fn summary(cells: &[Cell], cfg: &Config) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "# Gate 2 experiment — {} cells, model {}, {} run(s)\n\n",
        cells.len(),
        cfg.model,
        cfg.runs
    ));
    s.push_str("| arm | non-inferable pass | inferable pass | errors | delivered tokens (mean) | hook p95 ms (max) | cost/cell (mean) |\n|---|---|---|---|---|---|---|\n");
    for arm in &cfg.arms {
        let a: Vec<&Cell> = cells.iter().filter(|c| &c.arm == arm).collect();
        let ni: Vec<&Cell> = a
            .iter()
            .copied()
            .filter(|c| !c.inferable && c.status != "error")
            .collect();
        let inf: Vec<&Cell> = a
            .iter()
            .copied()
            .filter(|c| c.inferable && c.status != "error")
            .collect();
        let errs = a.iter().filter(|c| c.status == "error").count();
        let pr = |v: &[&Cell]| {
            if v.is_empty() {
                "n/a".to_string()
            } else {
                format!(
                    "{}/{} ({:.0}%)",
                    v.iter().filter(|c| c.status == "pass").count(),
                    v.len(),
                    100.0 * v.iter().filter(|c| c.status == "pass").count() as f64 / v.len() as f64
                )
            }
        };
        let tok = if a.is_empty() {
            0.0
        } else {
            a.iter().map(|c| c.delivered_tokens as f64).sum::<f64>() / a.len() as f64
        };
        let p95 = a
            .iter()
            .filter_map(|c| c.hook_p95_ms)
            .fold(0.0f64, f64::max);
        let cost = {
            let v: Vec<f64> = a.iter().filter_map(|c| c.cost_usd).collect();
            if v.is_empty() {
                0.0
            } else {
                v.iter().sum::<f64>() / v.len() as f64
            }
        };
        s.push_str(&format!(
            "| {arm} | {} | {} | {errs} | {tok:.0} | {p95:.2} | ${cost:.3} |\n",
            pr(&ni),
            pr(&inf)
        ));
    }
    let ni: Vec<Cell> = cells.iter().filter(|c| !c.inferable).cloned().collect();
    if cfg.arms.contains(&"literal".to_string()) && cfg.arms.contains(&"off".to_string()) {
        let (p, lo, hi) = bootstrap_diff(&ni, "literal", "off", 10_000, 42);
        s.push_str(&format!(
            "\nliteral − off (non-inferable): {p:+.3} [95% CI {lo:+.3}, {hi:+.3}]\n"
        ));
        if cfg.arms.contains(&"control".to_string()) {
            let (pc, loc, hic) = bootstrap_diff(&ni, "control", "off", 10_000, 43);
            s.push_str(&format!(
                "control − off (non-inferable): {pc:+.3} [95% CI {loc:+.3}, {hic:+.3}]\n"
            ));
            let pass1 = !p.is_nan() && lo > 0.0;
            let pass2 = pc.is_nan() || (loc <= 0.0 && hic >= 0.0) || pc < p / 2.0;
            s.push_str(&format!(
                "\nGate 2: {}\n",
                if pass1 && pass2 {
                    "PASS"
                } else if ni.is_empty() {
                    "NOT RUN"
                } else {
                    "FAIL (see PREREGISTRATION.md decision rule)"
                }
            ));
        }
    }
    s.push_str("\n## Per task\n\n| task | inferable | ");
    for arm in &cfg.arms {
        s.push_str(&format!("{arm} | "));
    }
    s.push('\n');
    s.push_str("|---|---|");
    for _ in &cfg.arms {
        s.push_str("---|");
    }
    s.push('\n');
    for t in &cfg.tasks {
        s.push_str(&format!("| {} | {} | ", t.id, t.inferable));
        for arm in &cfg.arms {
            let v: Vec<&Cell> = cells
                .iter()
                .filter(|c| c.task == t.id && &c.arm == arm)
                .collect();
            let p = v.iter().filter(|c| c.status == "pass").count();
            let e = v.iter().filter(|c| c.status == "error").count();
            s.push_str(&format!(
                "{p}/{}{} | ",
                v.len(),
                if e > 0 {
                    format!(" ({e} err)")
                } else {
                    String::new()
                }
            ));
        }
        s.push('\n');
    }
    let total_cost: f64 = cells.iter().filter_map(|c| c.cost_usd).sum();
    let served: i64 = cells.iter().map(|c| c.served_invalid).sum();
    if cells.iter().any(|c| c.stored_episodes > 0) {
        s.push_str(&format!(
            "\nRetired records delivered (all arms, all cells): {served}.\n"
        ));
    }
    s.push_str(&format!("\nTotal model cost: ${total_cost:.2}. Errors are excluded from pass rates and listed in results.jsonl.\n"));
    s
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    config: &Path,
    out_dir: &Path,
    muninn: &Path,
    dry_run: bool,
    pilot: bool,
    runs_override: Option<usize>,
    model_override: Option<String>,
    jobs: usize,
    rerun_errors: bool,
    rescore: bool,
) -> Result<()> {
    let text =
        std::fs::read_to_string(config).with_context(|| format!("reading {}", config.display()))?;
    let mut cfg: Config = serde_json::from_str(&text)?;
    if let Some(r) = runs_override {
        cfg.runs = r;
    }
    if let Some(m) = model_override {
        cfg.model = m;
    }
    if pilot {
        cfg.runs = 1;
        cfg.arms = vec!["off".into(), "literal".into()];
        cfg.tasks = cfg
            .tasks
            .iter()
            .filter(|t| !t.inferable)
            .take(1)
            .cloned()
            .collect();
    }
    let repo =
        std::fs::canonicalize(expand(&cfg.repo).as_path()).unwrap_or_else(|_| expand(&cfg.repo));
    let repo = if repo.is_relative() {
        std::env::current_dir()?.join(repo)
    } else {
        repo
    };
    let seeds = transcripts(&cfg.seed_transcripts);
    anyhow::ensure!(!seeds.is_empty(), "no seed transcripts found");
    std::fs::create_dir_all(out_dir)?;
    let out_dir = &std::fs::canonicalize(out_dir)?;
    let work = out_dir.join("work");
    std::fs::create_dir_all(&work)?;

    // plan
    let mut plan: Vec<(usize, String, String)> = Vec::new();
    for run in 0..cfg.runs {
        let mut cells: Vec<(String, String)> = cfg
            .tasks
            .iter()
            .flat_map(|t| cfg.arms.iter().map(move |a| (t.id.clone(), a.clone())))
            .collect();
        let mut rng = crate::Rng(1000 + run as u64);
        for i in (1..cells.len()).rev() {
            let j = (rng.next() as usize) % (i + 1);
            cells.swap(i, j);
        }
        plan.extend(cells.into_iter().map(|(t, a)| (run, t, a)));
    }
    println!(
        "plan: {} cells ({} tasks × {} arms × {} runs), model {}, base {} of {}",
        plan.len(),
        cfg.tasks.len(),
        cfg.arms.len(),
        cfg.runs,
        cfg.model,
        cfg.base_ref,
        repo.display()
    );
    println!("seed transcripts: {}", seeds.len());
    let results_path = out_dir.join("results.jsonl");
    if rescore {
        // Re-run every task's oracle on the saved per-cell patch applied to a pristine
        // worktree at base_ref. Used when an oracle is amended after cells ran: the
        // model output is untouched, only the scoring is redone, for every cell alike.
        let dir = work.join("rescore");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = Command::new("git")
            .current_dir(&repo)
            .args(["worktree", "prune"])
            .output();
        run_ok(Command::new("git").current_dir(&repo).args([
            "worktree",
            "add",
            "--detach",
            dir.to_str().unwrap(),
            &cfg.base_ref,
        ]))?;
        let prev = std::fs::read_to_string(&results_path).unwrap_or_default();
        let mut cells: Vec<Cell> = prev
            .lines()
            .filter_map(|l| serde_json::from_str::<Cell>(l).ok())
            .collect();
        let mut changed = 0usize;
        for c in cells.iter_mut() {
            if c.status == "error" {
                continue;
            }
            let Some(task) = cfg.tasks.iter().find(|t| t.id == c.task) else {
                continue;
            };
            let patch = out_dir
                .join("diffs")
                .join(format!("r{}-{}-{}.patch", c.run, c.task, c.arm));
            let _ = Command::new("git")
                .current_dir(&dir)
                .args(["checkout", "--", "."])
                .output();
            let _ = Command::new("git")
                .current_dir(&dir)
                .args(["clean", "-fdq"])
                .output();
            let body = std::fs::read(&patch).unwrap_or_default();
            if !body.is_empty() {
                let ok = Command::new("git")
                    .current_dir(&dir)
                    .args(["apply", "--whitespace=nowarn", patch.to_str().unwrap()])
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                if !ok {
                    println!(
                        "rescore: patch did not apply for r{}-{}-{}; status kept",
                        c.run, c.task, c.arm
                    );
                    continue;
                }
            }
            let o = Command::new("sh")
                .current_dir(&dir)
                .args(["-c", &task.oracle])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .output()?;
            let status = if o.status.success() { "pass" } else { "fail" };
            if status != c.status {
                println!(
                    "rescore: r{}-{}-{} {} → {}",
                    c.run, c.task, c.arm, c.status, status
                );
                changed += 1;
            }
            c.oracle_exit = o.status.code();
            c.status = status.into();
        }
        let _ = Command::new("git")
            .current_dir(&repo)
            .args(["worktree", "remove", "--force", dir.to_str().unwrap()])
            .output();
        let mut f = std::fs::File::create(&results_path)?;
        for c in &cells {
            writeln!(f, "{}", serde_json::to_string(c)?)?;
        }
        let s = summary(&cells, &cfg);
        std::fs::write(out_dir.join("summary.md"), &s)?;
        println!("rescore: {} cell(s), {changed} changed\n\n{s}", cells.len());
        return Ok(());
    }
    if rerun_errors {
        let prev = std::fs::read_to_string(&results_path).unwrap_or_default();
        let kept: Vec<Cell> = prev
            .lines()
            .filter_map(|l| serde_json::from_str::<Cell>(l).ok())
            .filter(|c| c.status != "error")
            .collect();
        let done: std::collections::HashSet<(usize, String, String)> = kept
            .iter()
            .map(|c| (c.run, c.task.clone(), c.arm.clone()))
            .collect();
        plan.retain(|c| !done.contains(c));
        let mut f = std::fs::File::create(&results_path)?;
        for c in &kept {
            writeln!(f, "{}", serde_json::to_string(c)?)?;
        }
        println!(
            "rerun-errors: {} cell(s) kept, {} to run",
            kept.len(),
            plan.len()
        );
    }
    if dry_run {
        for (i, (r, t, a)) in plan.iter().enumerate() {
            println!("  {i:>3}  run {r}  {t:<32} {a}");
        }
        return Ok(());
    }

    // control store: built once
    let control_db = if cfg.arms.iter().any(|a| a == "control") {
        let cdir = out_dir.join("control-store");
        let _ = std::fs::remove_dir_all(&cdir);
        std::fs::create_dir_all(cdir.join(".git"))?;
        let ct = transcripts(&cfg.control_transcripts);
        anyhow::ensure!(!ct.is_empty(), "control arm needs control_transcripts");
        let n = seed_store(muninn, &cdir, &ct, false)?;
        println!(
            "control store: {n} episodes from {} transcript(s)",
            ct.len()
        );
        Some(cdir.join(".muninn/muninn.db"))
    } else {
        None
    };

    let results = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&results_path)?;
    // Cells are independent (own worktree, own store); `jobs` of them run at once.
    // Results are appended in completion order; `order` keeps the planned position.
    let prior: Vec<Cell> = if rerun_errors {
        std::fs::read_to_string(&results_path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Cell>(l).ok())
            .collect()
    } else {
        Vec::new()
    };
    let state = std::sync::Mutex::new((results, prior));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let jobs = jobs.max(1).min(plan.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| loop {
                let order = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some((run, tid, arm)) = plan.get(order) else {
                    break;
                };
                let task = cfg.tasks.iter().find(|t| &t.id == tid).unwrap();
                println!("[{:>3}/{}] run {run} {tid} {arm} …", order + 1, plan.len());
                let cell = run_cell(
                    &cfg,
                    muninn,
                    &repo,
                    &seeds,
                    control_db.as_deref(),
                    task,
                    arm,
                    *run,
                    order,
                    &work,
                );
                println!(
                    "[{:>3}/{}] run {run} {tid} {arm} → {} ({:.0}s, {} tok, ${:.3}){}",
                    order + 1,
                    plan.len(),
                    cell.status,
                    cell.duration_ms as f64 / 1000.0,
                    cell.delivered_tokens,
                    cell.cost_usd.unwrap_or(0.0),
                    cell.error
                        .as_ref()
                        .map(|e| format!(" — {e}"))
                        .unwrap_or_default()
                );
                let mut g = state.lock().unwrap();
                let (results, cells) = &mut *g;
                if let Ok(line) = serde_json::to_string(&cell) {
                    let _ = writeln!(results, "{line}");
                }
                cells.push(cell);
                let s = summary(cells, &cfg);
                let _ = std::fs::write(out_dir.join("summary.md"), &s);
            });
        }
    });
    let cells = state.into_inner().unwrap().1;
    println!("\n{}", summary(&cells, &cfg));
    Ok(())
}
