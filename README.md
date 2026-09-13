# Muninn

Local, deterministic memory engine for coding agents. Ships as a Claude Code
plugin and a Codex hooks file, one Rust binary, no server, no model and no LLM
in the read path. Hooks cost single-digit milliseconds.

Three functions, each behind a hard evidence gate before it ships:

- **F1 filter** — superseded, revoked, anchor-changed and reverted facts are
  retained but never served.
- **F2 compiler** — rules in `CLAUDE.md` / `AGENTS.md` / `.claude/rules/` become
  permission rules and `PreToolUse` deny hooks the harness enforces, with a
  coverage report. Applied only after you see the diff.
- **F3 delivery** — memory arrives by permanent trigger conditions (directory,
  symbol, event), under a budget of 700 tokens per turn, silent by default.

Also in the MVP: an embedding sidecar (never in the hot path), a tree-sitter
symbol graph (Rust, TypeScript/TSX, JavaScript/JSX, Python, Go), a routed
`muninn why` responder, and a ≤ 1 000-token boot block for the agent.

## Status

Phases 0–2 complete.
- Gate 1 (F2 compiler) passed on a clean held-out set: precision 0.905, recall 0.864
  (`crates/muninn-bench/corpora/claude-md/GATE1.md`; corpus now 330 files).
- Gate 2 (literal episode delivery by hook) passed: on non-inferable tasks, no memory
  2/25, literal 19/25, length-matched irrelevant control 4/24; literal − off = +0.68
  [+0.56, +0.80], control − off = +0.09 [−0.04, +0.21]; 90 cells, sonnet, $27.63
  (`crates/muninn-bench/experiment/GATE2.md`, pre-registration and both runs in
  `experiment/`). Run 1 failed with an invalid instrument and is reported in full.
- Phase 3 (real engine): typed capture (corrections, invariants, commit-linked
  decisions, dead ends), supersession, caps, Markdown projection + export/import,
  `muninn maintain` (git capture, resume), embedding sidecar (potion-base-8M,
  checksummed, write path only, exact kNN), 10-check gate, 15 fault scenarios. The
  literal arm re-run on the real engine: 21/25 vs 19/25 on the throwaway store
  (`crates/muninn-bench/experiment/PHASE3.md`).
Phase 4 (F1 filter + `muninn why`) is next.
See `PLAN` in the repository description and `design/ENGINE.md`.

## Install (development)

```sh
cargo build --release -p muninn-cli
# Claude Code plugin: point the plugin at the binary
mkdir -p plugin/bin && cp target/release/muninn plugin/bin/
claude plugin add ./plugin        # or add the marketplace once published
# In your project:
muninn init             # creates .muninn/, disables native memory for this project,
                        # inserts the boot block into CLAUDE.md and AGENTS.md
muninn init --codex     # also writes .codex/hooks.json for Codex
muninn status           # MUNINN 10/10 GREEN
```

`muninn init --keep-native` leaves the harness's own memory on.
`muninn clean --yes` undoes everything `init` touched.

## Layout

```
crates/muninn-core      store, schema, sanitisation, heartbeat, health gate
crates/muninn-cli       the `muninn` binary: commands and hook entry points
crates/muninn-capture   transcript & git capture (Phase 2)
crates/muninn-compile   F2 rule compiler (Phase 1)
crates/muninn-embed     embedding sidecar (Phase 3)
crates/muninn-symbols   tree-sitter symbol graph (Phase 5)
crates/muninn-why       routed why responder (Phase 4)
crates/muninn-bench     performance contracts, experiment harness
plugin/                 Claude Code plugin (hooks.json, skill, commands, boot block)
codex/                  Codex hooks.json template
design/ENGINE.md        engine specification
research/               evidence log, conclusion, dossier, benchmarks
docs/                   threat model, portable format
```

## Verify

```sh
cargo test --workspace --features exact-tokens
MUNINN_FAULT_REPS=200 cargo test -p muninn-cli --release --test fault
cargo run --release -p muninn-cli --features exact-tokens -- init --check-budget
cargo run --release -p muninn-bench -- perf --strict
```

## Research

The design is fixed by `research/CONCLUSION.md` and the evidence log
`research/00-evidence-log.md` (window: June–September 2026). The dossier
`research/dossier.html` is the same material as a page. Every number in the
docs is either measured in `crates/muninn-bench` or cited by evidence id.
