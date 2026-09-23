//! `muninn` — one binary for the CLI and every hook entry point.

mod compile_cmd;
mod delivery;
mod hook;
mod init;
mod maintain;
mod output;
mod pretooluse;
mod scan;
mod sessions;

use clap::{Parser, Subcommand};
use muninn_core::{health, Db, Mode, ProjectPaths};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "muninn",
    version,
    about = "Local deterministic memory engine for coding agents"
)]
struct Cli {
    /// Emit JSON instead of text
    #[arg(long, global = true)]
    json: bool,
    /// Project directory (default: current directory, resolved to the repo root)
    #[arg(long, global = true)]
    cwd: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create .muninn/ for this project, disable native memory, insert the boot block
    Init {
        /// Leave the harness's native memory enabled
        #[arg(long)]
        keep_native: bool,
        /// Rewrite the boot block even if present
        #[arg(long)]
        refresh: bool,
        /// Also write .codex/hooks.json pointing at this binary
        #[arg(long)]
        codex: bool,
        /// Turn on dir/symbol cue delivery at prompt and tool time (off by default; see GATE4.md)
        #[arg(long)]
        cues: bool,
        /// Write the long boot block into CLAUDE.md / AGENTS.md (default: the
        /// SessionStart hook injects a compact summary and no file is touched)
        #[arg(long)]
        boot_file: bool,
        /// Accepted for compatibility; the default already touches no file
        #[arg(long, hide = true)]
        no_boot_block: bool,
        /// Only measure the boot block against its budget and exit
        #[arg(long)]
        check_budget: bool,
    },
    /// Remove .muninn/ and undo what init changed (asks unless --yes)
    Clean {
        #[arg(long)]
        yes: bool,
    },
    /// Health line and counters
    Status,
    /// Full health report
    Doctor,
    /// Hook entry point: reads the harness JSON on stdin
    Hook { event: String },
    /// Ingest a transcript file now (what Stop/SessionEnd do automatically)
    Ingest { transcript: PathBuf },
    /// Show what the read path would deliver for a prompt
    Recall { prompt: Vec<String> },
    /// Export records as JSONL (active only unless --all)
    Export {
        #[arg(long)]
        all: bool,
        /// Output file (default .muninn/export-<epoch>.jsonl)
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Import records from a JSONL file or a directory of Markdown files with frontmatter
    Import { path: PathBuf },
    /// Run the asynchronous write path now: fold logs, resume ingest, capture git, project Markdown
    Maintain,
    /// Ask why: literal records with provenance, lineage, conflicts and a sufficiency marker
    Why {
        query: Vec<String>,
        /// Include retired records
        #[arg(long)]
        all: bool,
        /// Maximum records
        #[arg(long, default_value_t = 8)]
        limit: usize,
        /// Output budget in tokens (estimate)
        #[arg(long, default_value_t = 1500)]
        budget: usize,
    },
    /// Print the records with these ids: the pull half of the session catalogue
    Show {
        ids: Vec<i64>,
        /// Output budget in tokens (estimate)
        #[arg(long, default_value_t = 700)]
        budget: usize,
    },
    /// Evaluate the trigger conditions for a context and print the delivery (F3, invoked form)
    Cues {
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value = "prompt")]
        event: String,
        #[arg(long = "file")]
        files: Vec<String>,
        #[arg(long = "symbol")]
        symbols: Vec<String>,
        #[arg(long = "keyword")]
        keywords: Vec<String>,
        /// Ignore the ledger (deliver even if already delivered this session)
        #[arg(long)]
        ungated: bool,
    },
    /// The delivery denominator: fires, silences and gated deliveries by reason, from the fire ledger
    Ledger {
        /// Only this session
        #[arg(long)]
        session: Option<String>,
    },
    /// Read or set a project setting (`cues on|off`, `prompt-delivery on|off`)
    Config { key: String, value: Option<String> },
    /// Scan the project's configuration for the three published defect classes:
    /// unpinned MCP servers, over-broad Bash allow rules, skills that pre-approve a shell
    ScanConfig,
    /// Retire a record by hand (retained, never served)
    Revoke {
        id: i64,
        #[arg(long, default_value = "user")]
        reason: String,
    },
    /// Compile CLAUDE.md/AGENTS.md rules into enforceable controls (writes .muninn/compiled/, applies nothing)
    Compile {
        /// Recompile even if sources are unchanged
        #[arg(long)]
        force: bool,
    },
    /// Apply compiled controls to .claude/settings.json after showing the diff
    Apply {
        /// Remove exactly what a previous apply added
        #[arg(long)]
        revert: bool,
        /// Apply without asking
        #[arg(long)]
        yes: bool,
    },
    /// Embedding sidecar: embed pending records (--rebuild re-embeds everything, --status reports)
    Embed {
        #[arg(long)]
        rebuild: bool,
        #[arg(long)]
        status: bool,
    },
    /// Symbol graph: index changed files (--rebuild re-indexes all, --status reports, --lookup <name>)
    Symbols {
        #[arg(long)]
        rebuild: bool,
        #[arg(long)]
        status: bool,
        #[arg(long)]
        lookup: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    let cwd = cli
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let paths = ProjectPaths::resolve(&cwd);
    let code = match cli.cmd {
        Cmd::Init {
            keep_native,
            cues,
            refresh,
            codex,
            boot_file,
            no_boot_block: _,
            check_budget,
        } => {
            if cues {
                // Merge, never overwrite: config.json also carries `boot` and `expand`.
                let _ = init::set_config(&paths.muninn_dir, "cues", true.into());
            }
            {
                if check_budget {
                    let b = init::check_budget();
                    if cli.json {
                        output::json(&serde_json::json!({
                            "chars": b.chars, "est_tokens": b.est_tokens, "exact_tokens": b.exact_tokens, "ok": b.ok,
                            "hook_chars": b.hook_chars, "hook_tokens": b.hook_tokens, "hook_max_tokens": muninn_core::caps::BOOT_HOOK_MAX_TOKENS,
                            "max_chars": muninn_core::caps::BOOT_BLOCK_MAX_CHARS, "max_tokens": muninn_core::caps::BOOT_BLOCK_MAX_TOKENS
                        }));
                    } else {
                        output::out(&format!(
                            "boot block (file): {} chars, ~{} tokens (estimate){}; hook summary: {} chars, ~{} tokens (max {}) — {}",
                            b.chars,
                            b.est_tokens,
                            b.exact_tokens
                                .map(|t| format!(", {t} tokens (cl100k)"))
                                .unwrap_or_default(),
                            b.hook_chars,
                            b.hook_tokens,
                            muninn_core::caps::BOOT_HOOK_MAX_TOKENS,
                            if b.ok { "within budget" } else { "OVER BUDGET" }
                        ));
                    }
                    if b.ok {
                        0
                    } else {
                        1
                    }
                } else {
                    match init::run(
                        &paths,
                        init::InitOpts {
                            keep_native,
                            refresh,
                            codex,
                            boot_file,
                        },
                        cli.json,
                    ) {
                        Ok(()) => 0,
                        Err(e) => {
                            output::err(&format!("muninn init: {e:#}"));
                            1
                        }
                    }
                }
            }
        }
        Cmd::Clean { yes } => match init::clean(&paths, yes, cli.json) {
            Ok(()) => 0,
            Err(e) => {
                output::err(&format!("muninn clean: {e:#}"));
                1
            }
        },
        Cmd::Status | Cmd::Doctor => {
            let full = matches!(cli.cmd, Cmd::Doctor);
            if !paths.is_initialised() {
                output::err(&format!(
                    "muninn: not initialised in {} (run `muninn init`)",
                    paths.root.display()
                ));
                1
            } else {
                let db = Db::open(&paths.db_path(), Mode::ReadOnly);
                let report = match &db {
                    Ok(db) => health::run(&paths, Some(db), None, full),
                    Err(e) => health::run(&paths, None, Some(e), full),
                };
                if cli.json {
                    output::json(
                        &serde_json::json!({ "summary": report.summary(), "checks": report.checks }),
                    );
                } else {
                    output::out(&report.summary());
                    if full {
                        for c in &report.checks {
                            output::out(&format!(
                                "  {:>1}. {:<10} {:<5} {}{}",
                                c.id,
                                c.name,
                                format!("{:?}", c.status).to_lowercase(),
                                c.detail,
                                c.fix
                                    .as_ref()
                                    .map(|f| format!("  → {f}"))
                                    .unwrap_or_default()
                            ));
                        }
                    }
                    if let Ok(db) = &db {
                        let active = db
                            .count("SELECT count(*) FROM record WHERE invalid=0")
                            .unwrap_or(0);
                        let invalid = db
                            .count("SELECT count(*) FROM record WHERE invalid=1")
                            .unwrap_or(0);
                        let rules = db.count("SELECT count(*) FROM rule").unwrap_or(0);
                        output::out(&format!("  records: {active} active, {invalid} retained-invalid · rules: {rules} · store: {}", paths.db_path().display()));
                    }
                }
                if report.is_green() {
                    0
                } else {
                    1
                }
            }
        }
        Cmd::Hook { event } => hook::run(&event, cli.cwd),
        Cmd::Ingest { transcript } => match Db::open(&paths.db_path(), Mode::ReadWrite)
            .map_err(anyhow::Error::from)
            .and_then(|db| {
                muninn_capture::ingest::ingest_transcript_with(
                    &db,
                    &transcript,
                    "manual",
                    Some(&paths),
                )
                .map_err(anyhow::Error::from)
            }) {
            Ok(st) => {
                if cli.json {
                    output::json(&st)
                } else {
                    output::out(&format!(
                        "ingested {} record(s) ({} episodes, {} decisions, {} dead ends, {} corrections, {} invariants; {} duplicates, {} superseded) from {} turn(s), offset {}→{}; {} file(s) projected",
                        st.inserted, st.episodes, st.decisions, st.deadends, st.corrections, st.invariants, st.duplicates, st.superseded, st.turns, st.from_offset, st.to_offset, st.projected
                    ))
                };
                0
            }
            Err(e) => {
                output::err(&format!("muninn ingest: {e:#}"));
                1
            }
        },
        Cmd::Show { ids, budget } => {
            match Db::open(&paths.db_path(), Mode::ReadOnly) {
                Ok(db) => {
                    let d = muninn_core::recall::show(&db, &ids, budget).unwrap_or(
                        muninn_core::recall::Delivery {
                            text: String::new(),
                            ids: vec![],
                            tokens: 0,
                        },
                    );
                    if cli.json {
                        output::json(
                            &serde_json::json!({ "ids": d.ids, "tokens": d.tokens, "text": d.text }),
                        );
                    } else if d.text.is_empty() {
                        // an id that is not served is a retired or absent record, and saying
                        // so is the point: silence here would read as "nothing is recorded"
                        println!("no served record with that id (it may have been retired — `muninn why --all` shows those)");
                    } else {
                        print!("{}", d.text);
                    }
                }
                Err(e) => output::err(&format!("muninn: show: {e}")),
            }
            0
        }
        Cmd::Recall { prompt } => {
            let p = prompt.join(" ");
            match Db::open(&paths.db_path(), Mode::ReadOnly) {
                Ok(db) => {
                    let t0 = std::time::Instant::now();
                    let terms = muninn_core::recall::select_terms(&db, &p, 8).unwrap_or_default();
                    let d = muninn_core::recall::deliver(&db, &p, &Default::default());
                    match d {
                        Ok(d) => {
                            if cli.json {
                                output::json(
                                    &serde_json::json!({ "terms": terms, "ids": d.ids, "tokens": d.tokens, "text": d.text, "ms": t0.elapsed().as_secs_f64() * 1000.0 }),
                                )
                            } else {
                                // The header is about the run, not the delivery, and it goes
                                // to stderr so that stdout is exactly what the agent would be
                                // given. It carries a wall-clock figure, and three experiment
                                // harnesses match the value they are looking for against this
                                // whole stream: loop 8's retry-budget cell read the `3` out of
                                // `0.53 ms` as the old value being served and flipped between
                                // runs of a fixture documented as deterministic.
                                output::err(&format!(
                                    "terms: {}  · {} tokens · {:.2} ms",
                                    terms.join(" "),
                                    d.tokens,
                                    t0.elapsed().as_secs_f64() * 1000.0
                                ));
                                output::out(&d.text)
                            }
                            0
                        }
                        Err(e) => {
                            output::err(&format!("muninn recall: {e}"));
                            1
                        }
                    }
                }
                Err(e) => {
                    output::err(&format!("muninn recall: {e}"));
                    1
                }
            }
        }
        Cmd::Export { all, out } => match Db::open(&paths.db_path(), Mode::ReadOnly) {
            Ok(db) => {
                let out = out.unwrap_or_else(|| {
                    paths
                        .root
                        .join(".muninn")
                        .join(format!("export-{}.jsonl", muninn_core::db::now_ms()))
                });
                match muninn_core::project::export_jsonl(&db, &out, all) {
                    Ok(n) => {
                        output::out(&format!("exported {n} record(s) to {}", out.display()));
                        0
                    }
                    Err(e) => {
                        output::err(&format!("muninn export: {e}"));
                        1
                    }
                }
            }
            Err(e) => {
                output::err(&format!("muninn export: {e}"));
                1
            }
        },
        Cmd::Import { path } => match Db::open(&paths.db_path(), Mode::ReadWrite) {
            Ok(db) => match muninn_core::project::import(&db, &path) {
                Ok(st) => {
                    let _ = muninn_core::project::project(&paths, &db, &[]);
                    output::out(&format!(
                        "imported {} of {} ({} duplicates, {} rejected)",
                        st.inserted, st.read, st.duplicates, st.rejected
                    ));
                    0
                }
                Err(e) => {
                    output::err(&format!("muninn import: {e}"));
                    1
                }
            },
            Err(e) => {
                output::err(&format!("muninn import: {e}"));
                1
            }
        },
        Cmd::Maintain => maintain::run(&paths, cli.json),
        Cmd::Why {
            query,
            all,
            limit,
            budget,
        } => {
            let q = query.join(" ");
            if q.trim().is_empty() {
                output::err("muninn why: give a question or a record id");
                2
            } else {
                match Db::open(&paths.db_path(), Mode::ReadOnly) {
                    Ok(db) => match muninn_why::answer(&db, &q, all, limit, budget) {
                        Ok(a) => {
                            if cli.json {
                                output::json(&a);
                            } else {
                                output::out(&muninn_why::render(&a));
                            }
                            if a.sufficient {
                                0
                            } else {
                                3
                            }
                        }
                        Err(e) => {
                            output::err(&format!("muninn why: {e}"));
                            1
                        }
                    },
                    Err(e) => {
                        output::err(&format!("muninn why: {e}"));
                        1
                    }
                }
            }
        }
        Cmd::Cues {
            session,
            event,
            files,
            symbols,
            keywords,
            ungated,
        } => {
            use muninn_core::cue;
            let session = session.unwrap_or_else(|| "cli".into());
            match Db::open(&paths.db_path(), Mode::ReadOnly) {
                Ok(db) => {
                    let exclude = if ungated {
                        Default::default()
                    } else {
                        delivery::delivered_ids(&paths, &db, &session)
                    };
                    let ctx = cue::TurnContext {
                        files,
                        symbols,
                        event,
                        keywords,
                    };
                    match cue::evaluate(&db, &ctx, &exclude) {
                        Ok(hits) => {
                            let ids: Vec<i64> = hits.iter().map(|h| h.record_id).collect();
                            let recs = cue::hits_for(&db, &ids).unwrap_or_default();
                            let m = cue::merge(
                                &hits,
                                &recs,
                                &[],
                                &[],
                                muninn_core::caps::BUDGET_TURN_TOKENS,
                                &[],
                            );
                            if !m.ids.is_empty() {
                                delivery::append(
                                    &paths,
                                    &delivery::Line {
                                        at: muninn_core::db::now_ms(),
                                        session: session.clone(),
                                        arm: "literal".into(),
                                        ids: m.ids.clone(),
                                        tokens: m.tokens,
                                        reason: "cue:cli".into(),
                                    },
                                );
                            }
                            if cli.json {
                                output::json(
                                    &serde_json::json!({"text": m.text, "ids": m.ids, "tokens": m.tokens, "reasons": m.reasons, "gated": m.gated}),
                                );
                            } else {
                                output::out(&m.text);
                            }
                            0
                        }
                        Err(e) => {
                            output::err(&format!("muninn cues: {e}"));
                            1
                        }
                    }
                }
                Err(e) => {
                    output::err(&format!("muninn cues: {e}"));
                    1
                }
            }
        }
        Cmd::Ledger { session } => match Db::open(&paths.db_path(), Mode::ReadOnly) {
            Ok(db) => {
                let sql = match &session {
                    Some(_) => "SELECT reason, count(*), coalesce(sum(tokens),0) FROM fire_ledger WHERE session_id = ?1 GROUP BY reason ORDER BY 2 DESC",
                    None => "SELECT reason, count(*), coalesce(sum(tokens),0) FROM fire_ledger WHERE ?1 = ?1 GROUP BY reason ORDER BY 2 DESC",
                };
                let sid = session.clone().unwrap_or_default();
                let rows: Vec<(String, i64, i64)> = db
                    .conn
                    .prepare(sql)
                    .and_then(|mut st| {
                        st.query_map([sid.as_str()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                            .map(|it| it.filter_map(|x| x.ok()).collect())
                    })
                    .unwrap_or_default();
                let sessions = db
                    .count("SELECT count(DISTINCT session_id) FROM fire_ledger")
                    .unwrap_or(0);
                let prompts = db
                    .count("SELECT count(*) FROM heartbeat WHERE hook = 'UserPromptSubmit'")
                    .unwrap_or(0);
                if cli.json {
                    output::json(
                        &serde_json::json!({"sessions": sessions, "prompts": prompts, "by_reason": rows.iter().map(|(r, n, t)| serde_json::json!({"reason": r, "rows": n, "tokens": t})).collect::<Vec<_>>()}),
                    );
                } else {
                    output::out(&format!(
                        "ledger: {sessions} session(s), {prompts} prompt hook(s) recorded"
                    ));
                    for (r, n, t) in rows {
                        output::out(&format!("  {n:>6}  {t:>7} tok  {r}"));
                    }
                }
                0
            }
            Err(e) => {
                output::err(&format!("muninn ledger: {e}"));
                1
            }
        },
        Cmd::Config { key, value } => {
            // `prompt-delivery` and `prompt_delivery` are the same setting: the flag reads
            // one spelling and a person types the other
            let key = key.replace('-', "_");
            let p = paths.muninn_dir.join("config.json");
            let mut v: serde_json::Value = std::fs::read_to_string(&p)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            match value {
                None => {
                    output::out(&format!(
                        "{key} = {}",
                        v.get(&key)
                            .map(|x| x.to_string())
                            .unwrap_or_else(|| "unset".into())
                    ));
                    0
                }
                Some(val) => {
                    let parsed = match val.to_ascii_lowercase().as_str() {
                        "on" | "true" | "1" => serde_json::Value::Bool(true),
                        "off" | "false" | "0" => serde_json::Value::Bool(false),
                        _ => serde_json::Value::String(val.clone()),
                    };
                    v[&key] = parsed;
                    let _ = std::fs::create_dir_all(&paths.muninn_dir);
                    match std::fs::write(
                        &p,
                        serde_json::to_string_pretty(&v).unwrap_or_default() + "\n",
                    ) {
                        Ok(()) => {
                            output::out(&format!("{key} = {}", v[&key]));
                            0
                        }
                        Err(e) => {
                            output::err(&format!("muninn config: {e}"));
                            1
                        }
                    }
                }
            }
        }
        Cmd::ScanConfig => {
            let f = scan::scan(&paths);
            if cli.json {
                output::json(&f);
            } else {
                output::out(&scan::render(&f));
            }
            if f.is_empty() {
                0
            } else {
                2
            }
        }
        Cmd::Revoke { id, reason } => match Db::open(&paths.db_path(), Mode::ReadWrite) {
            Ok(db) => match muninn_core::filter::revoke(&db, id, &reason) {
                Ok(true) => {
                    let _ = muninn_core::project::project(&paths, &db, &[id]);
                    output::out(&format!("record #{id} retired (revoked: {reason})"));
                    0
                }
                Ok(false) => {
                    output::err(&format!("record #{id} is not active"));
                    1
                }
                Err(e) => {
                    output::err(&format!("muninn revoke: {e}"));
                    1
                }
            },
            Err(e) => {
                output::err(&format!("muninn revoke: {e}"));
                1
            }
        },
        Cmd::Compile { force } => match compile_cmd::run_compile(&paths, force, cli.json) {
            Ok(()) => 0,
            Err(e) => {
                output::err(&format!("muninn compile: {e:#}"));
                1
            }
        },
        Cmd::Apply { revert, yes } => match compile_cmd::run_apply(&paths, revert, yes, cli.json) {
            Ok(()) => 0,
            Err(e) => {
                output::err(&format!("muninn apply: {e:#}"));
                1
            }
        },
        Cmd::Embed { rebuild, status } => embed_cmd(&paths, rebuild, status, cli.json),
        Cmd::Symbols {
            rebuild,
            status,
            lookup,
        } => symbols_cmd(&paths, rebuild, status, lookup, cli.json),
    };
    std::process::exit(code);
}

fn embed_cmd(paths: &ProjectPaths, rebuild: bool, status: bool, json: bool) -> i32 {
    let db = match Db::open(
        &paths.db_path(),
        if status {
            Mode::ReadOnly
        } else {
            Mode::ReadWrite
        },
    ) {
        Ok(db) => db,
        Err(e) => {
            output::err(&format!("muninn embed: {e}"));
            return 1;
        }
    };
    if status {
        let active = db
            .count("SELECT count(*) FROM record WHERE invalid=0")
            .unwrap_or(0);
        let vecs = db
            .count("SELECT count(*) FROM record_vec v JOIN record r ON r.id=v.record_id WHERE r.invalid=0")
            .unwrap_or(0);
        let model = db.meta_get("embed_model").ok().flatten();
        let dir = muninn_embed::model_dir();
        if json {
            output::json(
                &serde_json::json!({"active": active, "embedded": vecs, "model": model, "model_dir": dir}),
            );
        } else {
            output::out(&format!(
                "embed: {vecs}/{active} active records embedded · model {} · dir {}",
                model.as_deref().unwrap_or("none"),
                dir.map(|d| d.display().to_string())
                    .unwrap_or_else(|| "not found".into())
            ));
        }
        return 0;
    }
    let emb = match muninn_embed::Embedder::load_default() {
        Ok(e) => e,
        Err(e) => {
            output::err(&format!("muninn embed: {e}"));
            return 2;
        }
    };
    match muninn_embed::embed_pending(&db, &emb, rebuild) {
        Ok(st) => {
            if json {
                output::json(&st);
            } else {
                output::out(&format!(
                    "embedded {} of {} pending · model {} · load {:.1} ms · encode {:.1} ms",
                    st.embedded, st.pending_before, st.model_id, st.load_ms, st.encode_ms
                ));
            }
            0
        }
        Err(e) => {
            output::err(&format!("muninn embed: {e}"));
            1
        }
    }
}

fn symbols_cmd(
    paths: &ProjectPaths,
    rebuild: bool,
    status: bool,
    lookup: Option<String>,
    json: bool,
) -> i32 {
    let ro = status || lookup.is_some();
    let db = match Db::open(
        &paths.db_path(),
        if ro { Mode::ReadOnly } else { Mode::ReadWrite },
    ) {
        Ok(db) => db,
        Err(e) => {
            output::err(&format!("muninn symbols: {e}"));
            return 1;
        }
    };
    if let Some(name) = lookup {
        return match muninn_symbols::lookup(&db, &name) {
            Ok(rows) => {
                if json {
                    output::json(&rows);
                } else if rows.is_empty() {
                    output::out(&format!("no definition of `{name}` in the index"));
                } else {
                    for r in rows {
                        output::out(&format!(
                            "{}:{} {} {}",
                            r.path, r.line, r.kind, r.qualified_name
                        ));
                    }
                    if let Ok(refs) = muninn_symbols::referrers(&db, &name, 20) {
                        for (p, l, from) in refs {
                            output::out(&format!(
                                "  ← {p}:{l}{}",
                                from.map(|f| format!(" in {f}")).unwrap_or_default()
                            ));
                        }
                    }
                }
                0
            }
            Err(e) => {
                output::err(&format!("muninn symbols: {e}"));
                1
            }
        };
    }
    if status {
        let files = db.count("SELECT count(*) FROM symbol_file").unwrap_or(0);
        let defs = db.count("SELECT count(*) FROM symbol").unwrap_or(0);
        let refs = db.count("SELECT count(*) FROM symbol_ref").unwrap_or(0);
        let partial = db
            .count("SELECT count(*) FROM symbol_file WHERE partial=1")
            .unwrap_or(0);
        if json {
            output::json(
                &serde_json::json!({"files": files, "defs": defs, "refs": refs, "partial": partial}),
            );
        } else {
            output::out(&format!("symbols: {files} file(s), {defs} definition(s), {refs} reference(s), {partial} with syntax errors"));
        }
        return 0;
    }
    match muninn_symbols::rebuild(&db, &paths.source_root(), rebuild) {
        Ok(st) => {
            if json {
                output::json(&st);
            } else {
                output::out(&format!(
                    "symbols: {} file(s) seen, {} indexed, {} unchanged/skipped · {} defs, {} refs, {} partial · {:.0} ms",
                    st.files_seen, st.files_indexed, st.files_skipped, st.defs, st.refs, st.partial, st.ms
                ));
            }
            0
        }
        Err(e) => {
            output::err(&format!("muninn symbols: {e}"));
            1
        }
    }
}
