# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Rule for this file
Stay under 1 000 tokens (cl100k); CI enforces it (`muninn-cli` test `claude_md_within_budget`). To add a line, cut one. Only what applies to every task belongs here; the rest lives in `docs/`, `design/ENGINE.md` or a skill and is referenced by path, never copied.

## What Muninn is
A local, deterministic memory engine for coding agents: one Rust binary, SQLite + FTS5, no server, no model or LLM in the hook read path (single-digit ms). Three functions, each behind a hard evidence gate: F1 filter (retired facts are never served), F2 rule compiler (CLAUDE.md rules → permission rules and deny hooks), F3 delivery (cue-anchored memory under a 700-token budget). Ships as a Claude Code plugin (`plugin/`) and a Codex hooks file (`codex/`).

The design is fixed by `research/CONCLUSION.md` and `research/00-evidence-log.md`; decisions cite evidence ids such as `[K2]`. `design/ENGINE.md` is the spec, `docs/scope.md` lists what is deliberately not done. Neither is re-litigated without a new measurement.

## Map
`crates/muninn-core` store, `schema.sql`, recall, cues, filter, health gate · `muninn-cli` the `muninn` binary: commands and every hook entry (`hook.rs`) · `muninn-capture` transcript and git capture, typed extraction, redaction · `muninn-compile` F2 · `muninn-embed` model2vec sidecar, write path only · `muninn-symbols` tree-sitter graph · `muninn-why` routed responder · `muninn-bench` latency contracts and the experiment runner; gate reports and raw results under `crates/muninn-bench/experiment/`.

## Commands
```sh
cargo build --release
cargo test --workspace --features exact-tokens
cargo test -p muninn-core recall::           # one module or one test, by name
MUNINN_FAULT_REPS=200 cargo test -p muninn-cli --release --test fault
cargo fmt --all && cargo clippy --workspace --all-targets --features exact-tokens -- -D warnings
cargo run --release -p muninn-bench -- perf --strict
cargo run --release -p muninn-cli --features exact-tokens -- init --check-budget
```
CI runs exactly these; a change is finished when all of them pass.

## Invariants
- Read hooks open the store `query_only`, always exit 0, never load the model. Only `Stop`, `SessionEnd` and `maintain` write, inside `write_tx()`.
- Nothing enters the hot path without a `perf --strict` figure. Records are retired (`invalid=1`), never deleted. Trust derives from `origin`, never from wording.
- Every number in docs is measured or cited by evidence id; estimates are labelled. Never describe the output of a command that did not run.
- Experiments: pre-register in `experiment/PREREGISTRATION.md` before any cell runs; cells stay confined to their checkout; raw results are committed under `results/`.
- Commit as code@ilien.dev; never push.

## This repository dogfoods Muninn
`.muninn/` is a live store (plugin installed from the local marketplace). Never edit it by hand or commit its database and logs. `muninn status` is the first diagnostic; run `muninn why "<question>"` before overturning a recorded decision.

## Shell gotcha
To stop a background run, `kill` the pids from `pgrep -f "[p]attern"` in a command that contains nothing else: a plain pattern matches the shell's own command line and kills the session.
