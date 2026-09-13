# Experiments and gates — the reproducibility kit

Every gate in the plan is measured here with an executable oracle, pre-registered in
`PREREGISTRATION.md` (amendments included, in order), and reported with its raw data.

| gate | question | report | raw data |
|---|---|---|---|
| 1 | does the deterministic rule classifier reach 90 % precision? | `../corpora/claude-md/GATE1.md` | `../corpora/claude-md/` |
| 2 | does literal episode delivery by hook change the outcome? | `GATE2.md` | `results/run1/` (invalid instrument), `results/run2/` |
| 3 (Phase 3 acceptance) | is the real engine not inferior to the throwaway store? | `PHASE3.md` | `results/run3-literal-real-engine/` |
| 3 | does the F1 filter change what the agent does? | `GATE3.md` | `results/gate3-sonnet/`, `results/gate3-haiku/` |
| 4 §1 | do cue-anchored deliveries beat lexical-only? | `GATE4.md` | `results/gate4-cues/` |
| 4 §2 | do invariants survive compaction? | `PREREGISTRATION.md` (decay probe) | reproducible in one command, no model |
| 4 §3 | PM-Bench | `GATE4.md` | `results/pmbench/` |

## Runner

```sh
cargo build --release -p muninn-cli -p muninn-bench
./target/release/muninn-bench experiment --dry-run                       # the cell plan
./target/release/muninn-bench experiment --config <tasks.json> --jobs 3  # a grid
./target/release/muninn-bench experiment --rerun-errors --out <dir>      # infrastructure errors only
./target/release/muninn-bench experiment --rescore --out <dir>           # re-run every oracle on the saved patches
```

Each cell: a single-commit `git archive` of `base_ref` (no branch, reflog or later commit
reachable) → a store **outside** the checkout (`MUNINN_ROOT`), seeded with the frozen
transcripts, the checkout's symbol graph and, for a grid, `seed_records` (JSONL with
explicit cues) → the boot block in the checkout for arms with Muninn → `claude -p` with
the hooks wired through `--settings`, `MUNINN_ARM`, `MUNINN_SOURCE_ROOT`,
`MUNINN_NO_PROJECT` (no Markdown mirror an agent could read) → the task's oracle →
patch, model message, delivery log and fire ledger saved under `<out>/logs/` and
`<out>/diffs/`. Cells run concurrently (`--jobs`), each in its own process group with
drained pipes and a hard timeout.

Arms: `off` (no memory), `literal` (the shipped engine), `control` (length-matched
irrelevant content from `control_transcripts`), `unfiltered` (same records, invalidation
off — the render-matched control of Gate 3), `lexical` (cues off — Gate 4).

Grids: `revocation/scenarios.py` (Gate 3) and `cues/scenarios.py` (Gate 4) generate
their seeds and tasks; `revocation/analyze.py` computes pass / unsafe / retired-served
and the bootstrap CIs. `pmbench/` holds the OpenAI-compatible bridge over `claude -p`
and the Muninn ledger scaffold for PM-Bench.

The seed transcript (this project's own, frozen before any task was written) lives
outside the repository; its size and sha256 are in `PREREGISTRATION.md`.
