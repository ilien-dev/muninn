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

## Gate 3 — revocation grid (pre-registered 2026-09-13, before any grid cell)

Question: does the F1 filter (invalid never served) change what the agent does when a
recorded policy was retired, compared with the same records served with invalidation
off (the render-matched control [X1]) and with no memory?

Design: `revocation/scenarios.py` → `seed.jsonl` (20 records: ten policies, each with a
retired value; eight with a replacement, two revoked without one) and
`tasks-revocation.json` (one task per policy: write `docs/policies/<slug>.md` stating
the current recorded decision, "nothing recorded" if none; never name retired
alternatives). Every value is absent from the repository at `base_ref` (git grep).
Cells: single-commit archive of `base_ref`, store outside the checkout, no Markdown
mirror, seeded with the frozen transcript plus `seed.jsonl`. Arms: `off` (no memory),
`unfiltered` (all 20 records active, same block layout, conflict marking off),
`literal` (filter on: 10 active). 3 runs, order randomised. Models: claude-sonnet-5
first; claude-haiku-4-5 and Codex gpt-5.6-sol as further families if the harness holds.

Metrics per cell: pass (oracle: replacement present when one exists, retired value
absent); unsafe (retired value present in the file); retired-served (fire_ledger rows
whose record is invalid; must be 0 in `literal` by construction, checked).

Decision rule (plan, Gate 3): literal − unfiltered > 0 with a 95 % bootstrap CI that
excludes 0 on the eight replacement scenarios; retired-served = 0; unsafe rate in
`literal` ≤ 2.2 % (the published stale_guard figure [K7]; the script itself is not
run here). The two no-replacement scenarios are reported separately (they are where
`off` can pass by saying "nothing recorded").

Pilots (3 scenarios × 3 arms × 1 run, three iterations, $4.4 total) found and fixed
before this registration: cells could read the store's Markdown mirror and the SQLite
file inside the worktree (retired flags included) — the store now lives outside the
checkout with no mirror; a git worktree exposed the real repository's later commits —
cells are now single-commit archives; two seeded records cited a commit hash that a
cell cannot have — those origins are now user_said/review_accepted. Pilot 3 (after the
fixes): off 1/3, unfiltered 1/3, literal 3/3, retired-served 0.
- Codex as a third family: parked. With codex-cli 0.154.0, hooks passed as `-c
  hooks.*` overrides never fired in `codex exec` (bisected with echo / touch / a script:
  no side effect, no injected context), and a project `.codex/hooks.json` made the
  run hang until the timeout (a trust prompt with no TTY is the likely cause). The
  runner keeps the `codex` harness for when this is resolved; Gate 3 is measured on
  Claude Code with two model families (sonnet, haiku) and reported as such.
- Oracle amendment after the sonnet grid, applied by `--rescore` to every cell of every
  arm: `revoke-internal-http` required the English phrase "https everywhere"; the three
  filtered cells had written "HTTPS en todo / en todos lados / en todas partes" (the
  agents answer in the user's language) and were scored fail. The oracle now requires
  `https` present and the retired phrase absent. Before: literal − unfiltered +0.111
  [+0.000, +0.222]; after: +0.222 [+0.111, +0.333]. The haiku grid was launched before
  this amendment and is rescored the same way.
- Runner change recorded for future runs only (neither grid has it): the cells' PATH
  now includes the muninn binary and `muninn why` / `muninn status` are allowed tools,
  because two filtered sonnet cells stopped to ask permission to run `muninn why`
  (counted as fail). Both grids ran without it, so the comparison between families is
  under identical conditions.
- Known flaw found in the sonnet grid: `revoke-license` seeds "Apache-2.0" while the
  repository's Cargo.toml says MIT, so a careful agent sees a real conflict between
  memory and code; two filtered cells wrote MIT or refused. Kept in the reported set
  (pre-registered); the per-scenario table shows it.

## Gate 3 result (2026-09-13): PASS

sonnet: literal − unfiltered +0.222 [+0.111, +0.333], retired served 0, unsafe 0 %.
haiku: +0.185 [+0.074, +0.296], retired served 0, unsafe 0 %. Report: GATE3.md.

## Gate 4, condition 1 — cue delivery vs lexical-only (pre-registered 2026-09-13, before any grid cell)

Question: when a recorded decision is anchored to a file and the agent touches that
file, does cue-anchored delivery (dir / symbol / event cues fired by the turn context)
change the outcome beyond what lexical recall of the prompt already gives, and is the
effect content rather than length?

Design: `cues/scenarios.py` → eight decisions anchored to eight files (one per crate
directory) of the repository at `base_ref`, each naming a token absent from the
repository that the change to that file must carry; one task per file naming the file
and the change, never the token, sharing as few words as possible with the record.
Cells as in Gate 3 (single-commit archive, store outside, symbol graph of the checkout
indexed into the store, no mirror). Arms: `off`; `lexical` (Muninn with cues switched
off: `MUNINN_NO_CUES`); `literal` (cues + lexical, the shipped engine); `control`
(length-matched irrelevant content). 3 runs, claude-sonnet-5. Metrics: pass (token in
the edited file); cue fires per cell from the delivery log (`cue:` reasons); tokens.

Decision rule (plan, Gate 4 §1): literal − lexical > 0 with a 95 % bootstrap CI
excluding 0, and control − off with a CI including 0 (or a point estimate below half
the literal effect). If cues do not beat lexical-only, F3 reduces to reinjection on
compaction (decay probe below), as the plan states.

Measured before this registration, no model involved: decay probe — ten invariants,
100 forced compactions (PostCompact hook with epoch bump), 100/100 compactions delivered
all ten (1 000/1 000 facts), hook 1.4 ms median / 2.5 ms max. The published
without-harness figure is 106/108 losses [K1]; it is cited, not re-measured.
- Pilots of the cue grid (three iterations, 22 cells, $4.6) before this registration is
  used: (1) prompts that named the file and records that shared its words were found
  lexically (lexical 2/3) — scenarios rewritten so neither the prompt nor the record
  names the file; (2) cues evaluated only at UserPromptSubmit never fire in a
  single-prompt cell (the file is touched after the only prompt) — delivery now also
  happens at PostToolUse (Read/Grep/Glob/Bash) and PreToolUse (Edit/Write, `pre_edit`),
  both under the ledger and the budget; (3) with that, lexical 1/4, literal 3/4 on four
  scenarios, one run. The grid runs with the engine as committed at that point.

## Gate 4 §1 result (2026-09-13): condition not met

literal − lexical = +0.083 [−0.083, +0.250] on 96 cells; the CI includes 0. Cues fired in
15/24 literal cells. As pre-registered, F3 reduces to event reinjection (session start,
compaction) plus lexical recall; dir/symbol cue delivery stays available, off by
default. Report: GATE4.md; raw data: results/gate4-cues/.

## Gate 4 §3 result (2026-09-13): condition not met

PM-Bench v9 on claude-sonnet-5 through the `claude -p` bridge, set F1: single_baseline
60.9 % (3 runs), todo_ledger 61.8 % (3), muninn_ledger v2 60.6 % (3); spread ±4 points;
the 65.1 % line is not reached. Report: GATE4.md; data: results/pmbench/.

## Gate 4 §1, second grid — symbol-graph query expansion (recorded 2026-09-13, after the grid was launched, before any result was read)

**Change under test.** The read hook expands the lexical query with the definitions of
files whose path contains a prompt word (`expand_terms`, off with `MUNINN_NO_EXPAND`).
Recommendation 4 of the post-gate review: five of the eight cue tasks name an area of
the code ("the embedding crate", "the router") rather than a word the record contains.

**Arms.** `lexical-plain` (no cues, no expansion: the previous `lexical`),
`lexical` (no cues, expansion on), `literal` (dir/symbol cues on, expansion on).
Same eight tasks, same seed (sha256 223dcf4d…fa895), model claude-sonnet-5, 3 runs,
72 cells, randomised order, single-commit archive cells confined by a PreToolUse deny
(the first launch of this grid was stopped and discarded after 16 cells because the cell
settings lacked that hook and one cell wrote into the real repository; nothing from it
is counted).

**Hypothesis and rule.** Expansion helps if `lexical − lexical-plain` has a 95 %
cluster-bootstrap CI (clustered by task) that excludes zero; it is kept on by default if
the CI excludes zero, kept as an opt-in otherwise, and removed if the point estimate is
negative. `literal − lexical` re-tests the §1 condition with expansion in both arms.

**Also recorded.** Delivered tokens, turns and cost per arm; retired records served
(must stay 0).

