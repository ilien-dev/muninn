# Pre-registration — Gate 2: does literal episode delivery improve multi-session outcomes?

Registered 2026-09-12, before any arm was run. Frozen with the commit that adds this file.

## Question

Does delivering literal episodes from earlier sessions by hook (arm `literal`) raise the
pass rate of tasks that depend on non-inferable evidence from those sessions, compared
with no memory (arm `off`), and is the effect due to the content rather than to the
presence of extra tokens (arm `control`, length-matched irrelevant episodes)?

## Design

- Harness: Claude Code `-p` (non-interactive), Codex as a replication only if the gate
  passes on Claude Code. Native memory disabled (`autoMemoryEnabled: false`).
- Model: fixed per run (`tasks.json.model`); the same model in every arm.
- Arms (identical hooks, identical prompt; only `MUNINN_ARM` differs):
  1. `off` — hooks run, deliver nothing.
  2. `literal` — episodes from the seeded prior sessions, lexical recall, ≤ 700 tokens.
  3. `control` — the same number of tokens the literal arm would have delivered, filled
     with episodes from an unrelated project's store (`MUNINN_CONTROL_DB`).
  Arm 3 of the plan (`literal + filter`) is run in Phase 4, not here.
- Unit: one (task, arm, run) cell in a fresh git worktree at `base_ref`, a fresh store
  seeded with the prior-session transcripts, one `claude -p` invocation, then the task's
  oracle command; exit 0 = pass.
- Runs: ≥ 5 per arm; task×arm order randomised per run with a recorded seed.
- Oracle: an executable check written before the runs (grep/test on the worktree). No
  judge model. No manual scoring.
- Exclusions: a cell whose `claude -p` exits non-zero or times out is recorded as
  `error` and excluded from the pass rate but counted in the report.

## Tasks

`tasks.json`. Every task's correct outcome is (a) determined by evidence that exists only
in the seeded prior-session transcripts and (b) contradicted or unstated in the
repository at `base_ref`, so that an agent without memory cannot infer it. One task is
marked `inferable: true` as a sanity control: it should pass in every arm.

## Metrics (four levels)

- stored: episodes in the seeded store;
- delivered: tokens delivered per cell (`fire_ledger`), by arm;
- management: hook wall time p95 per cell (heartbeats);
- outcome: pass rate per arm on non-inferable tasks.

## Decision rule

Gate 2 passes iff, on non-inferable tasks:
1. pass(literal) − pass(off) > 0 with a 95 % bootstrap CI (10 000 resamples over cells,
   stratified by task) that does not include 0; and
2. pass(control) − pass(off) has a 95 % CI that includes 0, or its point estimate is
   less than half of the literal effect.
If (1) fails, the project reduces to F2. If (1) passes but (2) fails, the effect is
attributed to length and the gate fails.

External reference: DreamBench-SWE v2.1 successor audit — no memory 21/180 (0.117),
deterministic verbatim event memory 82/180 (0.456) [K2]. Its hidden oracles are not
public, so it cannot be run here; the numbers are a reference, not a comparison.

## What is reported regardless of outcome

`results.jsonl` (every cell), `summary.md`, cost per cell, and the exclusion ledger.
