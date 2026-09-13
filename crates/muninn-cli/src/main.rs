//! `muninn` — one binary for the CLI and every hook entry point.

mod compile_cmd;
mod delivery;
mod hook;
mod init;
mod maintain;
mod output;
mod pretooluse;

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
        /// Do not touch CLAUDE.md / AGENTS.md
        #[arg(long)]
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
    /// Ask why: literal records with lineage (Phase 4)
    Why { query: Vec<String> },
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
    /// Symbol graph maintenance (Phase 5)
    Symbols,
}

fn not_yet(what: &str, phase: &str) -> i32 {
    output::err(&format!(
        "muninn: `{what}` arrives in {phase}; see the plan's hard gates"
    ));
    2
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
            refresh,
            codex,
            no_boot_block,
            check_budget,
        } => {
            if check_budget {
                let b = init::check_budget();
                if cli.json {
                    output::json(&serde_json::json!({
                        "chars": b.chars, "est_tokens": b.est_tokens, "exact_tokens": b.exact_tokens, "ok": b.ok,
                        "max_chars": muninn_core::caps::BOOT_BLOCK_MAX_CHARS, "max_tokens": muninn_core::caps::BOOT_BLOCK_MAX_TOKENS
                    }));
                } else {
                    output::out(&format!(
                        "boot block: {} chars, ~{} tokens (estimate){} — {}",
                        b.chars,
                        b.est_tokens,
                        b.exact_tokens
                            .map(|t| format!(", {t} tokens (cl100k)"))
                            .unwrap_or_default(),
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
                        no_boot_block,
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
                                output::out(&format!(
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
        Cmd::Why { .. } => not_yet("why", "Phase 4"),
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
        Cmd::Symbols => not_yet("symbols", "Phase 5"),
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
