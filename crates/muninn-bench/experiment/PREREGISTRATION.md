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

## Amendments (recorded before the full run; the decision rule is unchanged)

- 2026-09-12, after the pilot (haiku 1×1×2 and sonnet 1×1×2 on
  `engine-s12-why-responder`, no full-run cell executed): the s12 oracle's second
  condition matched the pristine file through an unrelated fixture line in section 6
  (`v1/sync … muninn why`) and missed a correct edit that wrapped `muninn why` and `v1`
  across a line break. The oracle now scopes all three conditions to section 12 (text
  flattened to one line) and requires `v1` not to be followed by `/`, `.` or an
  alphanumeric. Verified: exit 1 on the pristine file, exit 0 on the pilot `off` edit
  with the sidecar row also fixed. Pilot cells are not part of the reported runs.
- 2026-09-12, during the full run (after cell 4 of 75): the s10 oracle checked, per
  line, that the line naming `napi` also carried a deferral word; a correct edit that
  wrapped "queda diferido" onto the next line scored as fail. s10 and s13 now flatten
  their section to one line before matching (s10: a deferral word within 240 chars of
  `napi`). The model outputs are not touched: every cell's saved patch is re-scored with
  `muninn-bench experiment --rescore` after the run, so all cells are scored by the same
  oracle. Verified: exit 1 on the pristine file for all five oracles; exit 0 on the r0
  literal s10 edit. Three cells (r0 s10-off, s11-control, s12-literal) errored on a stale
  git worktree registration before any model call; they are re-run with
  `--rerun-errors` and counted normally.

## Run 1 (2026-09-12, sonnet, 75 cells, $15.74): FAIL by the rule above, instrument invalid

Result as pre-registered (after `--rescore` with the amended s10/s13 oracles): non-inferable
pass rate off 17/20, literal 17/20, control 18/20; literal − off = 0.000 [−0.150, +0.150].
Raw data: `results/run1/` (results.jsonl, summary.md, every cell's patch, the tasks file).

Why the run does not test the hypothesis:
1. Three of the four "non-inferable" tasks (s10, s11, s13) passed 5/5 in the `off` arm:
   the prompts and the repository (README, plan-derived docs) carried the answer. Ceiling.
2. The s12 oracle's third condition (no "apagado por defecto") scored the correct answer
   as wrong: the plan itself keeps the embedding sidecar off by default pending a
   measurement (Phase 3 sub-gate), so only the "why responder" row was stale. With
   conditions 1–2 only, s12 was literal 5/5, off 3/5, control 3/5 — reported as post hoc,
   not as a result.
3. The `control` arm delivered tokens at prompt time but the fold into `fire_ledger`
   failed on a foreign-key constraint (foreign record ids), so `delivered tokens` read 0
   for that arm. Fixed; the arm's hook p95 (78 ms vs 0.04 ms for `off`) shows it ran.
4. Two capture defects made the literal store nearly empty of the decisive evidence:
   user steering that arrives inside tool results (plan rejections, AskUserQuestion
   answers) was not captured, and long turns (compaction summaries) were cut to their
   first 600 characters. Both fixed before run 1 (steering) and after it (chunking of
   long turns into ≤ 1 800-char literal episodes; the delivered block is now the
   window of the episode with the most query terms instead of its head).

## Run 2 (pre-registered 2026-09-13, before any run-2 cell)

Same design, decision rule, model, runs and arms. Changes, all fixed before running:
- Seed: the project transcript frozen at the byte offset just after its last compaction
  summary (22 559 233 bytes, sha256 223dcf4d…fa895, kept outside the repository), so no
  turn written while designing run 2 is in the store.
- Tasks: s12 with conditions 1–2 only; five fact tasks whose oracle is a measured number
  that appears only in the transcript — verified by grep to be absent from the
  repository at base_ref and from `git log`; the prompt never states the number and asks
  the agent to say "not found" rather than invent. One inferable sanity task kept.
- Non-inferability check: a 1-run pilot (off + literal) is run first; a fact task on
  which `off` passes is dropped before the full run and listed here.
- Known limitation: the transcript file is readable from the cells' home directory; an
  agent that thought of grepping `~/.claude/projects` could find the numbers without
  memory. Run 1 showed no cell doing so; run 2 cells are checked for it in their patches
  and command logs.
- Pilot for run 2 (1 run, off + literal, 14 cells, $4.0): `off` failed all five fact
  tasks and passed s12 (the fair oracle makes s12 partly inferable: 3/5 in run 1); the
  `off` cell for `fact-gate1-first-holdout` found the same figure as a ratio (0.837) in
  GATE1.md, so that fact is in the repository and the task is dropped. Two defects of the
  runner found and fixed before run 2: cells had no boot block (`--no-boot-block`), so
  the agent had no instruction on what a `[muninn:episode]` block is — arms with Muninn
  (literal, control) now get it, `off` does not, as the plan states; and the saved patch
  omitted new files, so `--rescore` could not have scored the fact tasks — patches now
  include untracked files, and each cell's model output is saved under `logs/`.
- Pilot 3 (literal only, four fact tasks, $1.1): 1/4. The cells read `git log` at
  `base_ref = HEAD`, found the run-2 design commit that says the numbers were kept out
  of the repository on purpose, and rejected the delivered numbers as a trap; one cell
  also found the number inside a unit test string added with the passage-selection fix.
  Fixes: `base_ref` pinned to `0cb51ab` (the last commit before any run-2 design work;
  the seed transcript is frozen earlier still), the test string changed, and the boot
  block now defines the trust scale (1 = observed in the project's own transcript) so a
  trust-1 block is not read as "low confidence". Whether an agent then uses the number
  or insists on repository corroboration is part of what run 2 measures.
- Pilot 4 (literal only, four fact tasks, base 0cb51ab, $0.8): 2/4 by the oracle, 4/4
  by content — one cell wrote the numbers as "Turns produced: **30**" (the oracle wanted
  "30 turns") and one wrote the file into the main repository by an absolute path that a
  delivered episode carried (`~/Projects/muninn/...`), outside its worktree. Fixes before
  run 2: the fact oracles match the number near its noun on the flattened file, the
  runner appends one neutral sentence to every prompt in every arm ("work only inside
  the current working directory ... never outside it"), and the two files that pilot
  cells had written into the main repository (docs/perf-history.md, a GATE1.md
  section) were removed — they were never in `base_ref`. Model output in this pilot:
  two cells used the delivered number and said so; one wrote the delivered 11.9 ms as
  "unverified" because git log did not corroborate it; one cited the episode as its
  only source and wrote it.

## Run 2 result (2026-09-13): PASS

90 cells, $27.63. Non-inferable: off 2/25, literal 19/25, control 4/24.
literal − off = +0.680 [+0.560, +0.800]; control − off = +0.087 [−0.035, +0.208].
63 cells first errored on the account's session limit (no model call) and were re-run
with `--rerun-errors` after the limit was lifted; one control cell remains `error`
(model error) and is excluded. Full report: GATE2.md; raw data: results/run2/.
