//! `muninn` — one binary for the CLI and every hook entry point.

mod codex_install;
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
    /// Show what the prompt hook would deliver for a prompt: the same path, the same blocks
    Recall {
        /// Leave out what this session has already been delivered, as the hook does
        #[arg(long)]
        session: Option<String>,
        prompt: Vec<String>,
    },
    /// Export records as JSONL (active only unless --all)
    Export {
        #[arg(long)]
        all: bool,
        /// Output file (default .muninn/export-<epoch>.jsonl)
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Import records from a JSONL file or a directory of Markdown files with frontmatter.
    /// Records arrive as `imported`, trust 0, unless --trust-origin
    Import {
        path: PathBuf,
        /// The file is your own export: keep the origin (and so the trust) each record states
        #[arg(long)]
        trust_origin: bool,
    },
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
    /// Read or set a project setting (`prompt-delivery on|off`, `subagent-delivery on|off`)
    Config { key: String, value: Option<String> },
    /// Scan the project's configuration for the three published defect classes:
    /// unpinned MCP servers, over-broad Bash allow rules, skills that pre-approve a shell
    ScanConfig,
    /// Keep a statement the user made, as a record of this kind. It is the user's (trust 3)
    /// only when the user typed it word for word in this session; otherwise it is kept as the
    /// agent's wording, at trust 0
    Remember {
        /// The statement, word for word
        #[arg(required = true)]
        statement: Vec<String>,
        #[arg(long, value_parser = ["decision", "invariant", "correction", "deadend"])]
        kind: String,
        /// The session it was said in (default: the harness's, `CLAUDE_CODE_SESSION_ID`)
        #[arg(long)]
        session: Option<String>,
        /// The session's transcript, when the session log does not name it
        #[arg(long)]
        transcript: Option<PathBuf>,
    },
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
            boot_file,
            no_boot_block: _,
            check_budget,
        } => {
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
                // Which binary produced this report. A plugin keeps its own copy and a
                // developer builds another; twice in one day this project read a report that
                // described a build two hours older than the one just compiled, and nothing
                // in the report said so. The hook path does not pay for this — `doctor` is
                // the live report and runs once, by hand.
                let who = std::env::current_exe().ok().map(|p| {
                    let when = std::fs::metadata(&p)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| muninn_core::recall::date_time_of(d.as_millis() as i64))
                        .unwrap_or_default();
                    (p.display().to_string(), when)
                });
                // `None`: no Codex here. Otherwise which way its hooks reach this project.
                let codex_route = codex_install::route(&paths.root);
                if cli.json {
                    output::json(&serde_json::json!({
                        "summary": report.summary(),
                        "checks": report.checks,
                        "codex_hooks": codex_route.as_ref().map(codex_install::Route::name),
                        "binary": who.as_ref().map(|(p, _)| p),
                        "binary_built": who.as_ref().map(|(_, t)| t),
                    }));
                } else {
                    output::out(&report.summary());
                    if let Some((p, t)) = &who {
                        output::out(&format!("this report is from {p}, built {t}"));
                    }
                    // Which way Muninn's hooks reach a Codex session here. Said in this
                    // report and not in the health line, which a hook builds.
                    use codex_install::Route;
                    match &codex_route {
                        None => {}
                        Some(Route::None) => output::out(
                            "  codex: found on this machine, and this project has no Muninn hooks: \
                             `muninn init` writes them to .codex/hooks.json (a plugin's binary \
                             leaves them to the Codex plugin)",
                        ),
                        Some(Route::Project) => output::out(
                            "  codex: hooks from this project's .codex/hooks.json",
                        ),
                        Some(Route::Plugin(p)) => output::out(&format!(
                            "  codex: hooks from the Codex plugin ({})",
                            p.display()
                        )),
                        Some(Route::Both(p)) => output::out(&format!(
                            "  codex: hooks from the Codex plugin ({}) and from this project's \
                             .codex/hooks.json, so each runs twice; `muninn clean` takes the \
                             project's out",
                            p.display()
                        )),
                    }
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
                        // of the retired, the ones an agent's statement retired and nobody
                        // gave back; shown only when there are some
                        let unrestored = match health::unrestored(db).len() {
                            0 => String::new(),
                            n => format!(" ({n} by an agent's statement, not restored)"),
                        };
                        // how often `muninn remember` was called, to read its use later
                        let remembered = db
                            .meta_get(muninn_capture::remember::CALLS)
                            .ok()
                            .flatten()
                            .unwrap_or_else(|| "0".into());
                        output::out(&format!("  records: {active} active, {invalid} retained-invalid{unrestored} · rules: {rules} · remember: {remembered} · store: {}", paths.db_path().display()));
                        // When each pass of the write path last ran. These timestamps
                        // have been written since they were added and read by nothing, so a
                        // report could say the store was healthy while a pass had not
                        // run for a week. They are shown, not checked: a stale one is a fact
                        // about a machine, not a fault this file can name.
                        if full {
                            let when = |k: &str| {
                                db.meta_get(k)
                                    .ok()
                                    .flatten()
                                    .and_then(|v| v.parse::<i64>().ok())
                                    .map(muninn_core::recall::date_time_of)
                                    .unwrap_or_else(|| "never".into())
                            };
                            output::out(&format!(
                                "  last run · ingest {} · git {} · anchors {}",
                                when("ingest_watermark_ms"),
                                when("git_watermark_ms"),
                                when("anchors_checked_ms"),
                            ));
                        }
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
        Cmd::Remember {
            statement,
            kind,
            session,
            transcript,
        } => remember_cmd(
            &paths,
            &statement.join(" "),
            &kind,
            session,
            transcript,
            cli.json,
        ),
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
        Cmd::Recall { session, prompt } => {
            let p = prompt.join(" ");
            match Db::open(&paths.db_path(), Mode::ReadOnly) {
                Ok(db) => {
                    let t0 = std::time::Instant::now();
                    let terms = muninn_core::recall::select_terms(&db, &p, 8).unwrap_or_default();
                    // The prompt hook's own selection, gates included, so what this prints is
                    // what a prompt would be given: it used `recall::deliver` and `render`,
                    // which order, cap and head a block differently, and the two returned
                    // different records for 58 of 210 prompts of a 1.2.0 store (R-14).
                    let d =
                        hook::select_for_prompt(&paths, &p, session.as_deref()).map(|s| match s {
                            hook::Selection::Gated(reason) => (
                                Some(reason),
                                muninn_core::cue::Merged {
                                    text: String::new(),
                                    ids: vec![],
                                    tokens: 0,
                                    reasons: vec![],
                                    gated: vec![],
                                },
                            ),
                            hook::Selection::Blocks(m) => (None, m),
                        });
                    match d {
                        Ok((gate, d)) => {
                            if cli.json {
                                output::json(
                                    &serde_json::json!({ "terms": terms, "ids": d.ids, "tokens": d.tokens, "text": d.text, "gated": gate, "ms": t0.elapsed().as_secs_f64() * 1000.0 }),
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
                                    "terms: {}  · {} tokens · {:.2} ms{}",
                                    terms.join(" "),
                                    d.tokens,
                                    t0.elapsed().as_secs_f64() * 1000.0,
                                    gate.map(|g| format!(" · {g}")).unwrap_or_default()
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
        Cmd::Import { path, trust_origin } => match Db::open(&paths.db_path(), Mode::ReadWrite) {
            Ok(db) => match muninn_core::project::import_with(&db, &path, trust_origin) {
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
                    let ctx = cue::TurnContext { event, keywords };
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
                            } else if m.text.is_empty() {
                                // Silence here reads as "nothing is recorded" when it usually
                                // means the command was given no context to evaluate against.
                                output::out(if ctx.keywords.is_empty() {
                                    "no record has a cue for that event: pass `--event \
                                     session_start` or `--event post_compact`, or a `--keyword`"
                                } else {
                                    "no record has a trigger condition matching that context"
                                });
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
                    Some(_) => "SELECT reason, count(*), coalesce(sum(tokens),0), count(DISTINCT session_id || ':' || fired_at) FROM fire_ledger WHERE session_id = ?1 GROUP BY reason ORDER BY 2 DESC",
                    None => "SELECT reason, count(*), coalesce(sum(tokens),0), count(DISTINCT session_id || ':' || fired_at) FROM fire_ledger WHERE ?1 = ?1 GROUP BY reason ORDER BY 2 DESC",
                };
                let sid = session.clone().unwrap_or_default();
                let rows: Vec<(String, i64, i64, i64)> = db
                    .conn
                    .prepare(sql)
                    .and_then(|mut st| {
                        st.query_map([sid.as_str()], |r| {
                            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                        })
                        .map(|it| it.filter_map(|x| x.ok()).collect())
                    })
                    .unwrap_or_default();
                let sessions = db
                    .count("SELECT count(DISTINCT session_id) FROM fire_ledger")
                    .unwrap_or(0);
                let prompts = muninn_core::heartbeat::prompts_recorded(&db);
                if cli.json {
                    output::json(
                        &serde_json::json!({"sessions": sessions, "prompts": prompts, "by_reason": rows.iter().map(|(r, n, t, e)| serde_json::json!({"reason": r, "rows": n, "tokens": t, "events": e})).collect::<Vec<_>>()}),
                    );
                } else {
                    output::out(&format!(
                        "ledger: {sessions} session(s), {prompts} prompt hook(s) recorded"
                    ));
                    output::out("  events  records    tokens  reason");
                    for (r, e, n, t, cut) in ledger_lines(&rows) {
                        let cut = if cut > 0 {
                            format!(" ({cut} record(s) left out)")
                        } else {
                            String::new()
                        };
                        output::out(&format!("  {e:>6}  {n:>7}  {t:>8}  {r}{cut}"));
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
                // tool-time cues went in 2.0.0 and nothing reads the key: writing it would
                // tell the person it is on
                Some(_) if key == "cues" => {
                    output::err(
                        "muninn config: `cues` was removed in 2.0.0 with the tool-time cues; nothing reads it",
                    );
                    1
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
    };
    std::process::exit(code);
}

/// The ledger's text lines. A row of `fire_ledger` is one delivered record, so `count(*)`
/// is records, not deliveries; events are the distinct (session, time) pairs. The budget
/// gate writes its count into the reason (`gated:budget:37`), which split the report into
/// one line per count: those are folded into one line, with the records they left out.
fn ledger_lines(rows: &[(String, i64, i64, i64)]) -> Vec<(String, i64, i64, i64, i64)> {
    let mut out: Vec<(String, i64, i64, i64, i64)> = Vec::new();
    for (reason, n, t, e) in rows {
        let (key, cut) = match reason.rsplit_once(':') {
            Some((head, num)) if head.ends_with("gated:budget") => match num.parse::<i64>() {
                Ok(k) => (head.to_string(), k * e),
                Err(_) => (reason.clone(), 0),
            },
            _ => (reason.clone(), 0),
        };
        match out.iter_mut().find(|l| l.0 == key) {
            Some(l) => {
                l.1 += e;
                l.2 += n;
                l.3 += t;
                l.4 += cut;
            }
            None => out.push((key, *e, *n, *t, cut)),
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
mod ledger_tests {
    use super::ledger_lines;

    #[test]
    fn budget_gate_counts_fold_into_one_line() {
        let rows = vec![
            ("literal:gated:budget:4".to_string(), 8, 0, 2),
            ("literal:gated:budget:34".to_string(), 32, 0, 1),
            ("literal:literal".to_string(), 9, 900, 3),
        ];
        let l = ledger_lines(&rows);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0], ("literal:gated:budget".to_string(), 3, 40, 0, 42));
        assert_eq!(l[1], ("literal:literal".to_string(), 3, 9, 900, 0));
    }
}

#[cfg(test)]
mod remedy_tests {
    use clap::CommandFactory;

    /// WP-08. The health report's RED lines tell the reader what to run, and four of them
    /// named a flag `muninn` does not have (`doctor --rebuild-fts`, `doctor --enforce-caps`,
    /// `doctor --ingest`, `import --from-records`), so the advice ended in `unexpected
    /// argument`. Every `muninn <command> [--flag]` the report can print must parse.
    #[test]
    fn every_remedy_the_health_report_names_is_a_real_command() {
        let src = include_str!("../../muninn-core/src/health.rs");
        let mut seen = 0;
        for snippet in src.split('`').skip(1).step_by(2) {
            let Some(rest) = snippet.strip_prefix("muninn ") else {
                continue;
            };
            seen += 1;
            // every import the report advises restores the user's own records, which a plain
            // import would bring back as `imported`, trust 0
            if rest.starts_with("import") {
                assert!(
                    rest.contains("--trust-origin"),
                    "`{snippet}` would drop the user's records to trust 0"
                );
            }
            // a placeholder stands for what the reader fills in
            let argv: Vec<&str> = std::iter::once("muninn")
                .chain(rest.split_whitespace().map(|w| match w {
                    "<id>" => "1",
                    w if w.starts_with('<') => "x",
                    w => w,
                }))
                .collect();
            if let Err(e) = super::Cli::command().try_get_matches_from(&argv) {
                panic!("`{snippet}` does not parse: {e}");
            }
        }
        assert!(seen >= 5, "the report's remedies were found: {seen}");
    }
}

/// Why `--transcript` was not taken as the session.
const NOT_IN_LOG: &str = "the session log does not name that transcript for that session";

/// `muninn remember`. The session is the one named, or the harness's own; its transcript is the
/// one named, or the one the session log holds for it. Nothing here reads the statement's words
/// to find either.
fn remember_cmd(
    paths: &ProjectPaths,
    statement: &str,
    kind: &str,
    session: Option<String>,
    transcript: Option<PathBuf>,
    json: bool,
) -> i32 {
    if !paths.is_initialised() {
        output::err(&format!(
            "muninn: not initialised in {} (run `muninn init`)",
            paths.root.display()
        ));
        return 1;
    }
    let session = session.or_else(|| {
        std::env::var("CLAUDE_CODE_SESSION_ID")
            .ok()
            .filter(|s| !s.trim().is_empty())
    });
    // A file is the session only when a hook noted it for that session: the flags choose
    // among what the session log holds and never add to it.
    let mut refused = false;
    let source: Option<(PathBuf, String)> = match (transcript, session) {
        (Some(t), s) => {
            let named = sessions::noted_as(paths, &t);
            let pick = match s {
                Some(s) => named.into_iter().find(|(sid, _)| *sid == s),
                None => {
                    let mut ids: Vec<&String> = named.iter().map(|(sid, _)| sid).collect();
                    ids.dedup();
                    if ids.len() == 1 {
                        named.into_iter().next()
                    } else {
                        None
                    }
                }
            };
            refused = pick.is_none();
            pick.map(|(sid, p)| (p, sid))
        }
        (None, Some(s)) => sessions::transcript_of(paths, &s).map(|t| (t, s)),
        (None, None) => None,
    };
    let others: Vec<PathBuf> = sessions::pending(paths)
        .into_iter()
        .map(|(_, p)| p)
        .filter(|p| source.as_ref().is_none_or(|(t, _)| t != p))
        .collect();
    let others: Vec<&std::path::Path> = others.iter().map(PathBuf::as_path).collect();
    let db = match Db::open(&paths.db_path(), Mode::ReadWrite) {
        Ok(db) => db,
        Err(e) => {
            output::err(&format!("muninn remember: {e}"));
            return 1;
        }
    };
    let src = source.as_ref().map(|(t, s)| (t.as_path(), s.as_str()));
    match muninn_capture::remember::remember_with(&db, statement, kind, src, &others) {
        Ok(mut r) => {
            if refused {
                r.lower = Some(NOT_IN_LOG);
            }
            // A record stored outside an ingest: its file is written now, and what it
            // retired or restated is the next `maintain`'s, which is asked for here. An
            // ingest used to ask on every call, whether it had written or not.
            let _ = db.meta_set("records_changed_since_project", "1");
            let _ = muninn_core::project::project(paths, &db, &[r.id]);
            if json {
                output::json(&r);
            } else {
                let what = match r.outcome {
                    "deduped" => "already on record as",
                    "restated" => "on record again as",
                    _ => "remembered as",
                };
                let mut line = format!(
                    "{what} #{} ({}, origin {}, trust {})",
                    r.id, r.kind, r.origin, r.trust
                );
                if let Some(why) = r.lower {
                    line.push_str(&format!(": kept as the agent's wording, because {why}"));
                }
                // on record is not the same as current: a retired record is not served
                if r.retired {
                    line.push_str(&match r.heir {
                        Some(heir) => format!(", retired (#{heir})"),
                        None => ", retired".to_string(),
                    });
                }
                output::out(&line);
            }
            0
        }
        Err(e) => {
            output::err(&format!("muninn remember: {e}"));
            1
        }
    }
}
