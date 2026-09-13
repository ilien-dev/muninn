//! `muninn` — one binary for the CLI and every hook entry point.

mod compile_cmd;
mod hook;
mod init;
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
    /// Export records as JSONL (Phase 3)
    Export,
    /// Import records from JSONL or Markdown (Phase 3)
    Import,
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
    /// Embedding sidecar maintenance (Phase 3)
    Embed,
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
        Cmd::Export => not_yet("export", "Phase 3"),
        Cmd::Import => not_yet("import", "Phase 3"),
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
        Cmd::Embed => not_yet("embed", "Phase 3"),
        Cmd::Symbols => not_yet("symbols", "Phase 5"),
    };
    std::process::exit(code);
}
