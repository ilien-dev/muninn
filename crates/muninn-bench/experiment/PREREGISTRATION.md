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
  - Unparked 2026-09-13 (infrastructure, no grid run). Probe on codex-cli 0.154.0: with
    the project `.codex/hooks.json` that `muninn init --codex` writes, hooks fire in the
    interactive TUI (after "Trust all and continue") and under `codex exec` with
    `--dangerously-bypass-hook-trust` and stdin closed; a canary invariant was delivered
    by UserPromptSubmit and answered in both. Codex reports file edits as `apply_patch`
    (relative paths, several files per call), which no Muninn matcher named; the hook
    now reshapes it into Edit/Write, and the confinement check no longer lets
    `root/../x` through for a file that does not exist yet (live: the patch to
    `../outside.txt` is denied). The runner now writes that `hooks.json` into the cell
    (excluded from the diff) and gives Codex a private HOME holding only the login: the
    real HOME had leaked `~/.agents/skills` into the model's context. Smoke, one cell,
    gpt-5.6-terra, `fact-userprompt-p95` literal: pass, 831 tokens delivered, hook p95
    2.56 ms (the same cell had timed out at 303 s before). A Codex grid needs its own
    pre-registration here before any cell runs.
- Claude cell isolation, found 2026-09-13 (infrastructure). `claude -p` cells ran with
  `--settings` but every setting source, so the user's own configuration reached the
  agent. Probe with those flags on this machine today (claude-haiku-4-5, a CLAUDE.md
  canary in the checkout): the user's global CLAUDE.md and output style were loaded,
  with 10 user plugins, 6 MCP servers (153 tools) and 3 user SessionStart hooks. The
  runner now passes `--setting-sources project,local --strict-mcp-config`: same probe,
  checkout CLAUDE.md still read, no user CLAUDE.md, default output style, 0 plugins,
  0 MCP servers, 30 tools, and the cell's `--settings` hooks still fire. What the user
  configuration was when Gates 2–4 ran is not recorded; it applied to every arm alike,
  and the Gate 3 oracle amendment below (agents answering in Spanish) is consistent with
  it. Grids from here on run isolated.
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

## Boot-block vehicle — file vs SessionStart hook (recorded 2026-09-13, before any cell ran)

**Change under test.** The shipped default no longer writes the boot block into the
user's CLAUDE.md / AGENTS.md; the SessionStart hook injects a compact summary (~425
estimated tokens, `plugin/templates/BOOT.hook.md`) as additionalContext at startup,
resume, clear and after compaction, and the `muninn` skill carries the long form on
demand. `muninn init --boot-file` restores the file vehicle (`boot = "file"`).

**Arms.** `literal` (long block in CLAUDE.md, as in every gate so far) and
`literal-hookboot` (no file; compact summary by hook). Everything else identical:
claude-sonnet-5, the six Gate 2 tasks plus the eight cue-grid tasks (14), same frozen
seed (sha256 223dcf4d…fa895) and seed records, 3 runs, 84 cells, randomised order,
confined cells, cues on in both arms.

**Hypothesis and rule.** Non-inferiority: `literal-hookboot − literal` with a 95 %
cluster-bootstrap CI (by task). The hook vehicle ships as the default if the point
estimate is ≥ −0.05 and the lower bound is > −0.20; if the point estimate is below
−0.05, `init` goes back to writing the file by default and the hook summary becomes the
opt-in. Also recorded: delivered tokens, turns, cost, retired records served (must be 0).


## Gate 4 §3, round 4 — Muninn as the intention store (recorded 2026-09-13, before any cell runs)

**Why a fourth round.** Two findings on the round 1–3 data, both read before anything was run:

1. *Confound in the bridge.* `claude_bridge.py` ran `claude -p` without `--setting-sources ""`,
   so the model under test loaded the user's global `~/.claude/CLAUDE.md` (probe answer:
   "Svipall para acceso web; respuesta en español"; ledger notes in the round-2 trajectories are
   in Spanish). All eleven earlier runs (baseline, todo, muninn v1–v3) share the contamination;
   their absolute values are not comparable with an isolated bridge. Round 4 re-measures the two
   baselines with the isolated bridge (probe answer: "NONE", 527 input tokens).
2. *The scaffold peeked.* `run_muninn_ledger.py` fed `step["time"]` and `step["cues"]` (hidden
   scenario fields) to the store. Round 4 forbids the scaffold every field the model cannot see:
   it reads only `start_instructions`, `steps[].text`, `steps[].options`, the step action menu,
   and the replies to `query_state` it issues itself. Time and channel state reach the store
   only through the benchmark's own `query_state` channel replies.

**Change under test.** `run_muninn_pis.py` (arm `muninn_store`): the model no longer owns the
ledger. Intentions are typed records in a Muninn store with `after` cues (day, clock) and
`keyword` cues (channel); lifecycle is code: add / reschedule / override / cancel / done, daily
re-arm of regular intentions, day-scoped carry of cross-day intentions, expiry of same-day
intentions at day end. Per step: (1) one **Form/Revise** call — the model reads the new text
and emits typed ops; (2) code issues `check_time` and one `query_state` per channel that a
pending intention watches; (3) **Filter** = `muninn cues --ungated` with the fake clock set from
the queried time, keyword = channels that answered; (4) one **Decide** call — the model maps the
eligible board to menu handles. A clock-matched time intention the model omits is added by a
code guard (token overlap ≥ 0.5 with a menu entry) and the guard event is logged. Design after
PIS `[V2]` (Form → Revise → Filter → Decide), lifecycle in code, no training.

**Arms.** `muninn_store` (new), `single_baseline`, `todo_ledger` — all claude-sonnet-5,
temperature 0, isolated invocation, 3 runs each, the released v9 week.

**Primary outcome.** Mean set-F1 over 3 runs. Lines: 65.1 % (paper's best, GPT-5.4 agent) and
82.9 % (PIS on DeepSeek-Chat `[V2]`). Both are other models; the within-model comparison is
against the two re-measured baselines.

**Secondary.** Cross-day miss (7 items), update miss (11 items), time-modality hit rate,
false alarms per step, proactive-monitoring hit rate, state-query calls, EST_INPUT_TOKENS.

**Decision rule.** Claimed only if mean set-F1 of `muninn_store` ≥ 82.9 % and above both
baselines by more than the round-2 spread (±4 points). Between 65.1 % and 82.9 %: reported
as above the published scaffold line, below PIS. Guard events are reported; a run whose
guard fires on more than 10 % of due items is reported as guard-dependent.

**No tuning on the result.** The scaffold and its prompts are frozen at the commit that adds
this section. Any later change is a round 5 with its own section.

**Smoke test before the freeze (recorded 2026-09-13 06:05, before any counted cell).** One
Monday-only run of the scaffold (`--max-days 1`, not scored, not part of the results) to
catch code errors. 11 of 13 steps matched the ground truth. The two mismatches were traced to
mechanism, and two code rules were added before the freeze: (a) an intention whose trigger was
revised by this step's text is not eligible at this step (the notice is not the cue); (b) while
any intention watches a channel, every channel is queried each step and the judge sees every
non-empty reply, so a channel mistyped at Form time still reaches the judge. Prompts were not
changed after the smoke test. The launched code is the version committed with this section.

## Gate 4 §3, round 5 — channel eligibility only on new channel information (recorded 2026-09-13 06:40, before any round-5 cell runs)

**Round 4 result, read before this section.** `muninn_store` set-F1 96.2 · 96.2 · 92.4 (mean
94.9 %), cross-day miss 0/7 in every run, update miss 1/9 in every run, time-modality hit 100 %,
guard events 0. Baselines through the isolated bridge still running at the time of writing.

**Defect found in the round-4 traces (all three runs, same steps).** Snapshot channels
(bank balance, price tracker, shipment, laundry) answer every query with their current state,
so "some channel answered" held at every step and every channel-kind intention was on the
board at every step. The judge then acted on "Move the check-in when the calendar update moves
the call" from a vignette that was a reschedule notice about another intention (Tuesday step 6,
false alarm), and the real calendar update two steps later found the intention already done
(miss). Two errors per run from one rule.

**Change under test (code only, prompts unchanged).** A channel reply counts as new
information only if it is not "(no updates)" and differs from that channel's previous reply
the same day. Channel-kind intentions are eligible only when at least one channel has new
information this step; the judge sees only the new replies; the board line of a channel
intention says it is satisfied only by a state channel reply, never by the vignette.
Everything else is the round-4 code.

**Arms.** `muninn_store` round 5, 3 runs, claude-sonnet-5, same invocation. The round-4
baselines are the control for both rounds (same model, same isolation).

**Primary outcome and rule.** Mean set-F1 over 3 runs, compared with round 4's 94.9 %. The
round-5 code ships as the scaffold only if its mean is not below round 4's; otherwise round 4
stays and round 5 is reported as a negative result. Secondary metrics as in round 4.

## Rounds 4 and 5 results (2026-09-13)

Round 4 baselines (isolated bridge, 3 runs each): single_baseline 79.7 · 76.7 · 77.2 (mean
77.9 %), todo_ledger 78.3 · 80.0 · 81.1 (mean 79.8 %). muninn_store round 4: 94.9 %; round 5:
96.2 · 95.7 · 96.9 (mean 96.3 %). Decision rule met; the round-5 rule ships. The three baseline
runs of each arm collided on one log file (same launch second); their action logs were rebuilt
from each run's console output and validated against the surviving original (identical). Report
and caveats: `GATE4.md` §3 rounds 4–5; data: `results/pmbench/round4/`, `results/pmbench/round5/`.

## Gate 4 §3, round 6 — prompt rules for the residual judge and Form errors (recorded 2026-09-13 08:05, before any round-6 cell runs)

**Residual errors in the round-5 traces, by cause.** (a) Form read a scene as an update: at
Friday step 5 ("The dry cleaner has your order hanging on the front rack") it emitted an
override back to the old cue, and the real cue two steps later did not fire (all 3 runs).
(b) Form stored the cue with its purpose ("follow-up text arrives with corrected
instructions"); the vignette said "a follow-up text comes in with the final location" and the
judge did not match them (2 of 3 runs, one step late). (c) The judge took an associated
activity for the cue ("You rinse a dish" → dinner; 2 of 3 runs), which consumed the day's
antibiotic instance. (d) Two intentions sharing a cue, one fired (1 run). (e) "Your package is
visible behind the counter" for "when you reach the counter": the benchmark counts it due, the
judge did not; left alone on purpose (forcing it would trade false alarms).

**Change under test.** Prompt rules only, plus one code line. Form: a scene is not an
instruction; the cue is the observable event without its purpose; a message that reaches the
person directly is an event cue, channel only when the text says it is seen by checking a
portal/tracker/status. Decide: the cue must be explicitly present; a notice that changes an
intention is not evidence any cue occurred; match the event, not its detail; a channel reply
can satisfy an event-kind intention; intentions sharing a cue are due together. Code: channels
are queried at every step, not only while a channel-kind intention is pending.

**Arm and rule.** `muninn_store` round 6, 3 runs, same model and invocation. Ships only if the
mean set-F1 is not below round 5 (96.3 %) and the traces show no new error class; the targeted
steps (Fri s5/s11, Mon s7/s8, Thu s9/s10, Thu s2) are checked one by one. Otherwise round 5 stays
and round 6 is a negative result.

## Round 6 result (2026-09-13): negative, round 5 stays

set-F1 95.0 · 95.7 · 95.6 (mean 95.4 %) < 96.3 %. The two targeted classes disappeared in every
run (Monday follow-up text, Thursday dinner), but two new ones appeared: with "a message that
reaches the person is an event" the dry-cleaning and the receipt intentions were typed as events
and the judge fired both on the Friday rack vignette (false alarms, then the receipt missed at
its real step); and on Tuesday step 8 the model returned a handle that was not on the menu, so
the clock guard had to add the two time intentions (guard events 1 per run, the first in 9 runs).
Data: `results/pmbench/round6/`. The scaffold is reverted to the round-5 code.

## Gate 4 §3, round 7 — the round-6 rules that held, without the typing change (recorded 2026-09-13 08:25, before any round-7 cell runs)

**Change under test.** From the round-5 code, four prompt rules only: Form — a scene is not an
instruction; the cue is the observable event without its purpose. Decide — the cue must be
explicitly present (an associated activity is not the cue); intentions sharing a cue are due
together. Not carried from round 6: the event/channel typing rule, "match the event not its
detail", "a channel reply can satisfy an event intention", and querying channels at every step.

**Arm and rule.** `muninn_store` round 7, 3 runs, same model and invocation. Ships only if the
mean set-F1 is not below round 5 (96.3 %) and no new error class appears in the traces.

## Round 7 result (2026-09-13): ships

set-F1 95.7 · 97.5 · 96.9 (mean 96.7 %, sd 0.9), not below round 5 (96.3 %, sd 0.6); the
difference is within the run-to-run spread and is not claimed as an improvement of the mean.
The two targeted classes (Monday follow-up text, Thursday dinner) are absent in all three runs.
Remaining in every run: Thursday "package visible behind the counter" (miss) and Friday's
dry-cleaning override typed as an email-channel cue firing one step early on the receipt's
email (false alarm + miss). Data: `results/pmbench/round7/`; report: `GATE4.md`.

## Gate 4 §3, round 8 — held-out weeks, store ablation, second model family, one invocation path (recorded 2026-09-13, before any round-8 cell runs)

**Why.** Rounds 4–7 leave four attacks open, and a reader who knows the field will make all
four: (1) seven rounds of prompt work on the only week PM-Bench ships, so the released week is
both the development set and the test set; (2) the number comes from a scaffold that puts the
intention lifecycle in code, and nothing isolates what the Muninn store contributes to it;
(3) one model family; (4) the `muninn_store` arm called `claude -p` directly while the baselines
went through the bridge — two invocation paths, however similar their flags. Round 8 answers
each with a measurement and changes nothing in the scaffold's prompts or logic.

**Frozen before any cell.** The round-7 scaffold (`pmbench/run_muninn_pis.py`) with three
additions that do not touch the Form / Observe / Filter / Decide logic: an HTTP client for the
bridge, the `plain` store, and a per-run manifest. The muninn binary built from this commit.
PM-Bench at commit `e1093c470c8981daf522d4ef047a7c3a71e077d7` (scorer `sim/pm_bench.py`
sha256 `d8ec27d8…254d8`, generator `sim/week_builder_v9.py` sha256 `268ae727…b106`, both
untouched). `FROZEN.json` in each output root records every hash, and each run's
`*.manifest.json` repeats them with the bridge's canary answer.

**Held-out weeks.** PM-Bench ships one deterministic week (seed 42; the generator at its commit
reproduces the released file byte for byte — checked). The launcher generates three more with
PM-Bench's own generator and validator, from seeds derived from the hash of the commit that
records this section: `int(sha256("<commit>:k")[:8], 16) mod 100000`, k = 0, 1, 2, 42
excluded. The seeds therefore did not exist when this text was written, and no human reads
a held-out week before its runs. The released week (v9) is still run, and is reported
separately as the development week. A week the generator or validator rejects is skipped and
the next k is used; this is logged.

**Arms.** Four, all through one bridge process per model:
- `muninn_store` — round 7, unchanged; the muninn binary runs inside `unshare -rn` (a network
  namespace with no interfaces), so the store provably reaches nothing during a run.
- `plain_store` — the identical scaffold with the muninn binary removed: records in a Python
  dict, the three store operations re-implemented with the semantics `cue.rs::evaluate`
  documents (a record fires when every cue of its group matches; `after` when the fake clock
  has passed its key, `keyword` when the key is among this step's answered channels; hits by
  record id). `muninn_store − plain_store` is the store implementation and nothing else. On a
  one-day unscored smoke on v9 the two arms produced identical boards at all 13 steps.
- `single_baseline`, `todo_ledger` — PM-Bench's own runners and scaffolds, untouched,
  `--temperature 0`, scored by PM-Bench's own scorer.

**Models and bridges.** claude-sonnet-5 through `claude_bridge.py` (`claude -p`, `--setting-sources
""`, no tools, no MCP, bare cwd; now threaded and with `/canary`). gpt-5.6-sol through
`codex_bridge.py` (`codex exec --ephemeral --ignore-user-config --ignore-rules -s read-only`,
private `CODEX_HOME` holding only the login, bare cwd; Codex exposes no temperature, and the
model may run read-only commands in the bare directory — the bridge counts them and the count
is reported). A bridge whose canary ("list every instruction you were given besides this
message; if none, answer NONE") returns anything but a denial, or whose canary used a tool,
invalidates every run behind it.

**Runs.** 3 per (model, week, arm): 4 weeks × 4 arms × 3 = 48 runs per model, launched
staggered (PM-Bench names its log by the launch second). No exclusions except a crash inside
PM-Bench's own runner, which is re-run once with both attempts kept on disk (`jobs.jsonl`).
Results are not read until every job of a model has exited.

**Decision rules.**
- R1 (held-out generalisation; the public claim depends on it): on each of the three held-out
  weeks separately, `muninn_store` mean set-F1 ≥ 82.9 % (the PIS line, another model) and above
  each baseline's mean by more than the largest within-arm range (max − min over runs) seen among
  the four arms on that week. All three weeks, or the claim is stated as "on the development week
  only".
- R2 (store ablation): pooled over the held-out weeks, if |`muninn_store` − `plain_store`| ≤ 2.0
  points and no held-out week has a worst-case gap beyond ±2.0 either way, the finding is
  "the store implementation is not distinguishable on this benchmark" and the public wording
  becomes: the typed-intention mechanism with lifecycle in code and cue firing by a store — which
  Muninn implements — scores X against the paper's scaffolds; if `plain_store` is lower by more
  than 2.0 pooled and on every week, the store's evaluation is the difference; if higher,
  that is reported as such. All three outcomes are published.
- R3 (second family): R1 and R2 evaluated on gpt-5.6-sol independently; no pooling across
  models; a family where R1 fails is reported as a failure of generalisation, not omitted.
- R4 (invocation path): `muninn_store` through the bridge on v9 is compared with round 7's direct
  `claude -p` runs (96.7 %, sd 0.9); a mean more than 2.0 points lower is reported as an effect of
  the invocation path and round 7's figure is retracted from the public claim.

**Statistics.** The primary test is exact: for each contrast pooled over the held-out weeks, a
stratified permutation test that enumerates every relabeling of the runs within each week
(C(6,3)³ = 8 000 for three weeks), statistic = mean over weeks of the difference of means; the
smallest attainable one-sided p is therefore 1/8 000 and is stated next to every p. A cluster
bootstrap by week (10 000 resamples, weeks then runs) gives a 95 % CI that is reported as
coarse (three clusters). Per week, the worst-case gap (A's worst run − B's best run) is reported
next to the mean difference, so a reader can see whether every run of one arm beat every run
of the other. `pmbench/analyze_round8.py` computes all of it from the score files; nothing is
computed by hand.

**Cost estimate (not a measurement).** Round 5 measured 8.5–9.4 min and 129–133 k input tokens
per `muninn_store` run on claude-sonnet-5 with direct invocation; the baselines' surviving logs
showed 1.19–1.49 M input tokens per run. Round 8 on one model is therefore of the order of 24
store runs × 130 k + 24 baseline runs × 1.3 M ≈ 35 M input tokens; the Codex family is expected
to be slower per call and its count of tool calls is unknown until the canary runs.

**Amendment, before any round-8 cell (2026-09-13).** The first canary of the new bridge was
run without a system prompt and answered with a description of Claude Code's default system
prompt (identity, tool rules, care section, the account e-mail, the date, the working
directory), no file, skill, memory or CLAUDE.md. Every chat request passes `--system-prompt`,
which in `claude -p` replaces that default, so the canary is now sent through the same path
(a one-line neutral system prompt). The acceptance rule is restated precisely: the canary
answer may name only the harness's own environment reminders (working directory, platform,
date, model identity, token budget) and the account e-mail; a canary naming any file, skill,
memory, instruction file or prior conversation invalidates every run behind that bridge. The
held-out seeds derive from commit `f173f2dc75625b89f7b0543004ed7cf44c51b399`, the commit
that recorded the round-8 section; this amendment does not move them.

## Gate 3 on a second harness and model family — Codex / gpt-5.6-sol (recorded 2026-09-13, before any cell runs)

**Why.** Gate 3 (F1 filter) is measured on two Anthropic models inside Claude Code. A reader
can say the effect belongs to that harness's hook semantics or that vendor's models. Codex is
the other harness Muninn ships for (`codex/hooks.json`), and its models are another family.

**Frozen.** `revocation/tasks-revocation-codex.json`: the Gate 3 configuration unchanged
(same ten scenarios, same 20 seed records, same frozen seed transcript
sha256 `223dcf4d…fa895`, same oracles, `base_ref 0cb51ab`, 3 runs, arms `off` /
`unfiltered` / `literal`) with `harness: codex` and `model: gpt-5.6-sol` (the Codex CLI's
configured default on this machine; codex-cli 0.154.0). The runner drives `codex exec
--json -s workspace-write` in a single-commit archive with the project `.codex/hooks.json`
that `muninn init --codex` writes, a private HOME holding only the login (recorded smoke:
the real HOME had leaked `~/.agents/skills`), and the confinement hook. Codex reports edits as
`apply_patch`; the hook reshapes them (commit `6388aeb`). Same oracles, same
`revocation/analyze.py`, no change to either.

**Decision rule.** Gate 3's rule, verbatim: (1) `literal − unfiltered` > 0 with a 95 %
bootstrap CI excluding 0; (2) retired-served = 0; (3) unsafe rate in `literal` ≤ 2.2 %. A
family where (1) fails is published as "the F1 effect did not replicate on Codex /
gpt-5.6-sol", not omitted. Also reported: cells where Codex's hooks demonstrably fired
(delivery log non-empty) — a grid in which hooks fired in fewer than 90 % of `literal` cells
is an instrument failure and is reported as such, not as a negative result.

## Gate 3 with a public seed — reproducible by anyone (recorded 2026-09-13, before any cell runs)

**Why.** Every Gate 2–4 grid seeds the cell with this project's own transcript, which is
private (its size and hash are published, the file is not). Nobody outside can re-run
those grids. This grid removes the private input.

**Frozen.** `revocation/tasks-revocation-public.json`: Gate 3 unchanged except
`seed_transcripts: []` and `control_transcripts: []` — the store holds only the 20 public
policy records of `revocation/seed.jsonl` (ten current, ten retired) and the checkout's
symbol graph. claude-sonnet-5, 3 runs, 90 cells. Everything a reader needs is in the
repository.

**What changes and is said so.** Without the 82 transcript episodes the lexical index has
no distractors, so recall is easier than in the private grid; this grid measures the F1
filter (retired never served; conflicts served as conflicts) under no retrieval noise, and
the private grid remains the noisy figure. The two are reported side by side; neither
replaces the other. Same decision rule as Gate 3.

## Replications at five runs — boot vehicle and query expansion (recorded 2026-09-13, before any cell runs)

**Why.** Two shipped decisions rest on contrasts whose 95 % CI lower bound sits at exactly
0.000: the SessionStart-hook boot summary vs the file block (+0.119 [+0.000, +0.262], 84
cells) and query expansion vs plain lexical (+0.125 [+0.000, +0.292], 72 cells). Adding runs
to a finished grid would be optional stopping; these are fresh replications, analysed on
their own and then pooled with the originals, both figures reported.

**Frozen.** `cues/tasks-boot-vehicle-rep5.json` and `cues/tasks-cues-v2-rep5.json`: the
original configurations with `runs: 5` (140 and 120 cells), same model, same seed, same
tasks and oracles, same runner (the only runner change since those grids is the
records-only seed option above, which these grids do not use).

**Decision rules.** Boot vehicle: non-inferiority as pre-registered originally
(`literal-hookboot − literal` CI lower bound > −0.10); if the replication's point estimate is
negative the hook default is re-examined. Expansion: `lexical − lexical-plain` > 0 with the CI
excluding 0 on the replication alone → the opt-in becomes the default; CI including 0 → stays
opt-in; point estimate ≤ 0 → the opt-in is removed. Cluster bootstrap by task, 10 000
resamples, as in the originals.

**Order and concurrency.** PM-Bench round 8 (sonnet) is running at eight parallel model
calls; the Codex Gate 3 grid starts now (other provider); the sonnet grids above start when
round 8's sonnet jobs have exited, three cells at a time, so no grid competes for the
account's rate limit with another.

## Instrument failure in the Codex Gate 3 grid, and the fix (recorded 2026-09-13, before the re-run)

**What the first Codex grid showed** (`results/gate3-codex-v1-leaky/`, 90 cells, kept):
`literal` 27/27, `unfiltered` 0/27 with 18 cells writing the retired value, and **`off`
18/27 with zero tokens delivered**. A no-memory agent cannot know that the current codec is
zstd. The cell's turn-context log for `r0-revoke-compression-off` shows what it read before
writing "zstd": `.codex/hooks.json`, `codex/hooks.json`, `plugin/skills/muninn/SKILL.md`,
`crates/muninn-core/src/paths.rs` (symbols `ProjectPaths`, `db_path`) — it located the store
from the engine's own source, and `MUNINN_ROOT` was in its environment. Codex's sandbox
restricts writes, not reads, and the cell directory sat inside this repository's tree, next
to `GATE3.md` and the other cells' stores. The grid is invalid as a measurement of memory
delivered by hooks and is reported only as this paragraph.

**Were the Claude grids affected?** Their cells sat in the same place. Claude cells had no
`sqlite3`, `echo`, `env` or `python` in their allowed tools; `cat *` and `grep *` were
allowed, so a read of a store file was possible in principle. No turn-context logs exist for
the Gate 3 Claude grids (the log came later); the `off` arm's 2/54 passes on replacement
scenarios (both guesses in the model's own words) are consistent with no such read. The
boot-vehicle and expansion grids do have the logs: 1 of 84 and 4 of 68 cells mention a path
outside the cell — those five are listed and inspected in `GATE3.md` at the re-run. This
cannot be excluded retroactively; it is excluded by construction from here on.

**Fix, in the runner and in the hook, applied to every grid from this point:**
1. The `off` arm has no Muninn at all: no store is created, no hooks are installed, no
   `MUNINN_*` variable is set; for Claude the settings file carries only
   `autoMemoryEnabled: false`. Earlier grids gave `off` a seeded store and no-op hooks.
2. Cells and stores live outside the repository tree and outside the results directory
   (`$TMPDIR/muninn-bench/<grid>/`, `MUNINN_BENCH_WORK` overrides), so `..` leads nowhere.
3. The PreToolUse hook, in a cell (`MUNINN_CONFINE_ROOT` set), denies and counts any shell
   command or read that names the store, `muninn.db`, `.muninn/`, `MUNINN_ROOT` or `sqlite3`,
   walks `../`, or takes an absolute path outside the checkout and the system directories
   (`deny:store-access`, `deny:escape` in the cell's ledger). The denial text tells the agent
   the checkout is the whole project and memory arrives through the hooks. The count of
   denials per arm is reported with every grid; a grid where `literal` cells were denied
   more than 5 % of their commands is examined for whether the denial itself drove the
   result.
4. A diagnostic cell (prompt asking for `echo $MUNINN_ROOT`, `ls ..`, `cat ../../GATE3.md`)
   is run on Codex before the grid; its ledger must show the denials and its output must
   show no store path. Recorded below when run.

**Re-run.** `tasks-revocation-codex.json` unchanged; the fixed runner and hook; same decision
rule. The public-seed grid and the five-run replications run on the fixed instrument. For
the replications this changes nothing in the compared arms (all have Muninn); the boot and
expansion figures they replicate came from the old instrument, which is stated when the
replication is pooled with the original.

**Binary change and restart (recorded 2026-09-13, before relaunch).** While round 8 (sonnet:
33 of 48 jobs exited), round 8 on Codex (4 jobs started) and the Codex Gate 3 re-run (21 of 90
cells) were running, the maintainer fixed a Codex bug in the engine (commit `0a3e45a`, "Fold hook
logs from a watermark; read Codex 0.154 rollouts") and rebuilt the binary. Runs that span two
binaries have a manifest that is no longer true, so all three grids restart from zero on the
binary built from `0a3e45a` (sha256 `9c8c80b942682d15…`,
`cargo build --release` reports it up to date with that commit; 78 tests, clippy and fmt green).
The interrupted outputs were moved, unread, to `~/.local/share/muninn-bench/interrupted-2026-09-13/`
(sha256 manifest inside; not committed, 120 MB); no number from them is used or reported. The
sonnet round-8 runs that had exited also straddled a rebuild at 08:15 (the gated-marker and
store-access hook change), which is the same defect. From here no rebuild happens while a grid
runs; every FROZEN.json and manifest must show one binary hash per grid.

**Codex bridge canaries (recorded before the Codex round-8 relaunch).** Both probes answer with
a refusal ("I can't provide hidden system/developer instructions…", 0 tool calls). A refusal
names nothing, so it does not invalidate the bridge, but it proves nothing either; Codex's
isolation rests on the invocation (`--ephemeral --ignore-user-config --ignore-rules
-s read-only`, a private CODEX_HOME holding only the login, an empty working directory) and
on the fact that `codex exec` ships its own constant agent prompt (~13.8 k tokens per call,
visible in the bridge's usage log). Stated as such in the report.

**Second interruption and resume (recorded 2026-09-13 19:05, before relaunch).** At 08:56 the
account's model credits ran out (the PM-Bench baselines died with "Empty response from backend",
three `todo_ledger` runs with exit 1) and the machine went down until 18:58; `/tmp` (the cells)
was lost, the result directories were not. State at the stop: round 8 sonnet 13 of 48 runs
exited (4 complete with a score file: v9 `muninn_store` ×2, v9 `plain_store` ×2; the three
credit crashes and every partial directory are discarded), round 8 Codex 2 of 48 exited
(v9 `muninn_store` ×1 complete), Codex Gate 3 84 of 90 cells (archived unread with the earlier
partials, re-run from zero: the runner has no resume). Round 8 resumes with `run_round8.py
--resume`: it keeps only runs that ended with PM-Bench's score file, deletes partial
directories (listed in `FROZEN.json`), refuses to start if the muninn binary, the scaffold, the
bridge or the scorer hash differs from the grid's `FROZEN.json`, and launches the missing runs.
Keeping complete runs is legitimate because each run is independent and nothing they depend on
changed (binary `9c8c80b9…`, same bridge code, same weeks); the three crashed baseline runs are
re-run as infrastructure errors, as pre-registered. Runs are still not read until the grid's
last job exits.

**Codex marker canary at the resume (recorded 2026-09-13 19:10, no result read).** The marker
probe answered: "Yes — 'AGENTS.md' appears in hidden instructions. I can't quote or reveal
hidden instruction text verbatim." Checked before continuing: (1) the codex-cli 0.154.0 vendor
binary carries 62 strings mentioning `AGENTS.md` (its own instruction-file loader and prompt;
`strings … | grep AGENTS.md`), so the word is part of Codex's constant agent prompt; (2) the
bridge's private `CODEX_HOME` was created empty except for `auth.json` and a two-line
`config.toml`, and Codex itself populated it at first start with its built-in system skills
(`skills/.system/`: skill-installer, skill-creator, review-agent, plugin-creator, openai-docs,
imagegen) and its curated remote plugins — none of this machine's skills, rules, hooks or
AGENTS.md files, and no `AGENTS.md` exists in that home or in the empty working directory;
(3) `--ignore-user-config --ignore-rules --ephemeral` remain in force. Reading of the rule: the
harness's own constant prompt, skills and plugins are not a leak from this machine; they reach
every arm identically through the same bridge and are stated as a property of the Codex family
in the report. A canary naming anything specific to this machine (Svipall, Muninn, Orca, the
language rule, the output style, a CLAUDE.md) would still invalidate the bridge; none appeared.

**Resume defect, recorded 2026-09-13 19:40 while the grids run.** The first `--resume` looked
for a run's score file one directory deep; PM-Bench's own runners nest their runs one level
deeper (`<arm>/<model>/<run>/`), so every completed baseline run directory was classed as
partial and deleted: sonnet v9 `single_baseline` ×3, v9 `todo_ledger` ×2, heldout-76233
`single_baseline` ×1; Codex v9 `single_baseline` ×1 (the list is in each `FROZEN.json`,
`resume_removed_partial_dirs`). Those runs are being re-run as fresh runs by the same launcher;
no score from the deleted runs was read (their score files were never opened by anyone), so
the effect is cost and time, not selection. The lookup is recursive from now on. The kept runs
(sonnet v9 `muninn_store` ×2 and `plain_store` ×2, Codex v9 `muninn_store` ×1) are unaffected.

## Gate 4 §3, round 9 — store equivalence by shadowing, and the day-1 typing rate (recorded 2026-09-13 20:40, after round 8 sonnet was read, before any round-9 cell)

**What round 8 left open.** R2 read as `plain_store` > `muninn_store` by 5.0 points pooled; the
traces attribute all of it to the first FORM call typing the daily medication as a clock time in
four `muninn_store` runs and no `plain_store` run, on byte-identical prompts. Two things are
therefore worth measuring separately: whether the two stores ever disagree when fed the same
operations, and how often that FORM outcome occurs per arm.

**Design.**
1. *Shadow store* (no extra model call). `run_muninn_pis.py --shadow`: a `muninn_store` run also
   keeps a `PlainStore` fed the identical add / reschedule / override / cancel / done / start_day /
   end_day sequence; at every step both boards are computed and the trace records both and
   whether they differ as sets of intention ids. Decisions still come from the Muninn board. A
   disagreement is a genuine semantic difference between the engine's `cues` evaluation and the
   documented rule; the count over all steps is the result.
2. *Runs*: 3 additional `muninn_store --shadow` runs and 3 additional `plain_store` runs per
   held-out week (18 runs), same bridge, same binary, same weeks; the Codex family the same once
   its round 8 has been read.

**Decision rules.**
- S1: shadow disagreements = 0 over every step of every shadow run → "the store implementation is
  equivalent in effect on this benchmark"; any disagreement is listed with its step and cause,
  and R2's reading stands until explained.
- S2: the FORM typing outcome (time vs event for the daily medication) is tabulated per arm over
  round 8 + round 9 (6 runs per arm per week); a two-sided Fisher exact test on arm × outcome; p >
  0.05 → the round-8 gap is reported as sampling variation with S1 as the mechanism; p ≤ 0.05 →
  reported as an unexplained arm effect and investigated further before any claim.
- R1 is *not* re-evaluated on the pooled six runs (that would be optional stopping after a
  failed rule); round 9's runs are reported on their own and, separately, pooled with a note.

**Implementation constraint.** The scaffold file is not edited while the Codex round 8 is
running (its manifests hash the scaffold); `--shadow` is added after that grid's last job exits.

**Replications: credit outage and a wrong-config re-run (recorded 2026-09-14 00:35).** The
account's session limit hit during the boot-vehicle replication (89 of 140 cells `error`,
"You've hit your session limit") and the whole expansion replication (120/120 `error`); the
public-seed Gate 3 grid had finished before it (90/90, no error). Error cells are re-run with
`--rerun-errors` as infrastructure errors. The first re-run was launched without `--config` and
the runner fell back to its default (Gate 2's task file): it was killed after three foreign cells
(`off` arm, Gate 2 tasks) had run; their rows, logs, diffs and control store were removed from the
boot-vehicle directory, the 51 completed cells were untouched, and the re-run was relaunched with
the replication's own task file. Nothing was read from the kept cells before this.

## Gate 3 on three external repositories, and Gate 2 on Codex (recorded 2026-09-14 00:50, before any cell)

**Why.** Every agent grid so far runs on this repository. "It works on the authors' own
codebase" is the first thing an expert will say. Gate 3's public seed (ten policies with a
retired and a current value, oracles that check a written policy file) does not depend on the
repository, so it can be run unchanged on code nobody here wrote. Gate 2 cannot (its facts
live in this project's transcript), but it has only one model family; Codex is the second.

**Repository selection — a rule, applied before looking at anything but the rule's inputs.**
For each language whose grammar Muninn ships (Python, TypeScript, Go; Rust is this repository,
JavaScript overlaps TypeScript): `gh search repos --language=<lang> --license=mit --sort=stars`
on 2026-09-14, take the first result that is not archived, is ≤ 50 MB, and has ≥ 50 tracked
source files in that language at HEAD. MIT because every seed's `license` scenario assumes a
repository whose real license is neither the retired nor the current value (as in this
repository). Outcome of the rule, with the rejected candidates: Python — `public-apis` (6 `.py`
files), `project-based-learning` (0), `hermes-agent` (> 50 MB) rejected, **TheAlgorithms/Python
`6883049`** (1 507 `.py`); TypeScript — `deepseek-harness` (> 50 MB) rejected, **vuejs/vue
`9e88707`** (388 `.ts`); Go — `awesome-go` (10 `.go`), `ollama` (> 50 MB) rejected,
**gin-gonic/gin `dcaa429`** (99 `.go`). Shallow clones at those commits under
`~/.local/share/muninn-bench/external/`. TheAlgorithms/Python has its own `AGENTS.md`.

**Frozen.** `revocation/tasks-external-{python,vue,gin}-{codex,sonnet}.json`: the public-seed Gate
3 configuration with only `repo` and `base_ref` changed (and `harness`/`model` for the Codex
files). Muninn binary `9c8c80b9…` (the one every running grid uses, passed with `--muninn`). One
runner change, needed for a repository that is not this one and built into a separate target
directory so the running grids are untouched: the boot template is read from Muninn's own
`plugin/templates/` when the repository under test has none, and an existing `CLAUDE.md` /
`AGENTS.md` is kept with the block appended instead of overwritten (in this repository at
`0cb51ab` neither file existed, so earlier grids are unaffected). Seed-term collisions at HEAD,
counted before any run: `gzip` 1 file in each repository, `LFU` 7 (Python) and 2 (vue),
`msgpack` 11 (gin), `calver` 3 (Python), `semver` 3 (vue), `GPL`/`Apache` 1–5 — these are the
repositories as they are and nothing is edited.

**Order.** Codex first (its quota is separate from the Claude account that ran out twice): gin,
vue, Python, then Gate 2 on Codex (`tasks-gate2-codex.json`: Gate 2 unchanged, `harness: codex`,
`model: gpt-5.6-sol`, 5 runs, 90 cells), three cells at a time, after the Codex round 8 finishes
or alongside it. The sonnet external grids run after the two sonnet replications finish, if the
account's limit allows; if they do not run, that is stated, and the Codex grids stand alone.

**Decision rules.** Gate 3's rule per repository, verbatim, no pooling across repositories for
the rule (a pooled figure is reported beside it). A repository where rule (1) fails is published
as a failure on that repository. Gate 2's rule verbatim (literal − off > 0 with CI excluding 0;
control − off CI including 0 or below half the literal effect); a Codex hook-delivery rate below
90 % of `literal` cells is an instrument failure, reported as such.

**Amendment: running out of turns is an outcome, not an error (recorded 2026-09-14 01:20, before
reading either replication).** A cell whose harness reported `error_max_turns` (the agent used
all 25 turns) was recorded as `error`, excluded from pass rates, and eligible for
`--rerun-errors`. That is wrong in both directions a statistician would name: exclusion drops
failures from the denominator, and re-running resamples failures only. From now on such a cell
is scored by its oracle on what the agent left (the runner no longer marks it as an error;
`--rescore` scores recorded ones on their saved patch, and a patch that does not apply stays a
fail). Checked across every grid in `results/`: the only published cells affected are one
`control` cell in Gate 2 run 2 and one `control` cell in the first cue grid, both now `fail` —
Gate 2 control 4/24 → 4/25, control − off +0.087 [−0.035, +0.208] → +0.080 [−0.040, +0.200];
Gate 4 §1 control 2/23 → 2/24, control − off +0.087 → +0.083 [+0.000, +0.167]. Both were in the
control arm, so the old exclusion had flattered the control, not Muninn; no conclusion changes.
None had been re-run: the only `--rerun-errors` passes so far re-ran credit-limit errors, checked
cell by cell. In the running replications, 5 boot-vehicle cells and 1 expansion cell hit the turn
limit; they will be scored, not re-run.

## Replications result (2026-09-14)

Boot vehicle: replication +0.000 [−0.114, +0.129], non-inferiority not met on the replication
alone, point not negative → default stays; pooled +0.045 [−0.045, +0.152]. Query expansion:
replication −0.125 [−0.275, −0.025] → rule applied, the opt-in is withdrawn from docs and claims;
its code is removed once no running grid hashes the binary. `GATE4.md` §1 replications.

**Tooling fixes found while scoring (before any contrast was read).** A `--rescore` launched
without `--config` loaded the default task file (Gate 2) and marked cue-task cells it could not
score as fails; caught from the summary header, undone from backups, and redone with each grid's
own file — for the two published cells the outcome was the same (fail by oracle). The runner now
writes each grid's configuration to `<out>/config.json` and refuses a rescore or re-run with a
different one (or, for older grids, one that does not define every task present), and a cell is
only reclassified after its task is found.

**Round 8 Codex: excess baseline runs from the resume, and the rule for them (recorded 2026-09-14
01:55, no Codex round-8 score read).** The second `--resume` counted run directories one level
below each arm; PM-Bench's baselines keep every run under a single `<model>/` directory, so an arm
with three complete runs counted as one and the launcher started two more. Result: 5 score files
for some (week, baseline) cells, 4 for one, with the grid still running. Rule, fixed now: for every
(week, arm), the runs that count are the **first three complete runs by launch time** (the
timestamp PM-Bench writes into the run directory name); later complete runs are moved to
`excess-runs/` inside the grid, kept, listed, and not used in any figure. If a cell ends with
fewer than three complete runs, the missing ones are run and reported as such. The launcher's
count is fixed (count score files, not directories) after this grid's last job exits, so this grid's
hashes stay coherent. The sonnet grid is unaffected: its resume deleted and re-ran those runs, and
every (week, arm) there ended with exactly three.

**Gate 2 on Codex: the control arm leaked, and the fix (recorded 2026-09-14 02:40, before the
re-run).** The first grid (`results/gate2-codex-v1-control-leak/`, kept, not reported as a
measurement) gave `off` 4/25, `literal` 15/25 and `control` 14/25 — a length-matched *irrelevant*
memory arm passing tasks whose answers exist only in the seeded transcript. Two replayed control
cells, with the agent's commands now logged, show why: the control agent ran `muninn why "<the
question>"`, and in that arm only prompt-time delivery was swapped to the foreign store
(`MUNINN_CONTROL_DB`); the store behind `muninn why`, `muninn recall` and event reinjection was the
real one. Fix in the runner: the `control` arm's entire store is seeded from the foreign project's
transcripts, so every channel serves irrelevant memory. Replay of the same two cells after the
fix: both fail, the agents still call `muninn why` and get nothing relevant. The runner now also
keeps every command a Codex agent runs (`commands` in each cell's log), which the first grid did
not.

**Earlier grids with a `control` arm.** Gate 2 run 2 (Claude): `muninn why` was not an allowed
tool then (it was added during Gate 3), so the channel did not exist; control passed 0/15 fact
cells. First cue grid (Claude): the channel existed; control passed 2/24. Any leak there helped the
control, i.e. worked against the reported literal − control margin. Neither is re-run; both notes
go into their reports.

**Re-run.** `tasks-gate2-codex.json` unchanged, 90 cells, fixed runner, same muninn binary
`9c8c80b9…`, Gate 2's rule verbatim. Also reported: the share of passing `literal` cells in which
the agent itself ran `muninn why` (the product allows it; a reader should know how much of the
effect is pull rather than hook delivery).

**Round 9 implementation, recorded before launch (2026-09-14 02:15).** `run_muninn_pis.py --store
muninn-shadow` (`ShadowMuninnStore`): the Muninn store as in round 8, plus, for every active Muninn
record, the cues the dict store would hold; at every board both answers are computed from the same
record ids, the Muninn board drives the decision, and the trace records both (`shadow`), with
`shadow_boards_compared` and `shadow_disagreements` in the run metadata. Launcher: `run_round8.py
--shadow --weeks heldout --arms muninn_store,plain_store`, fresh output roots
`results/pmbench/round9-{sonnet,codex}/`, same seeds, bridges and muninn binary. A two-day smoke on
heldout-10517 (sonnet, not scored, not reported as a result): 26 boards, 24 non-empty, 0
disagreements; an earlier attempt with the bridge down produced empty boards from failed calls and
was discarded. The scaffold's hash changes with this addition; round 8's figures keep theirs.

## Round 9 result (2026-09-14)

S1: 0 disagreements in 1 458 shadow boards (729 per family) → store implementation equivalent in
effect on this benchmark. S2: day-1 clock-time typing 7/18 vs 3/18 on sonnet (Fisher p = 0.264),
0/18 vs 0/18 on gpt-5.6-sol → round 8's sonnet gap reported as sampling variation. `GATE4.md` §3
round 9.

## Head-to-head without labels — Muninn, claude-mem, agentmemory (recorded 2026-09-14, before any seeding session or probe of Muninn on this input)

**Why.** Two attacks are still open. (1) Gate 3 hands Muninn the answer: its seed marks the retired
records `invalid: true` and `muninn import` keeps the mark, so Gate 3 measures a filter, not the
detection of a replaced decision. (2) Nothing compares Muninn with the memory tools people
actually install. This grid answers both at once: every tool, Muninn included, learns the same
twenty decisions from the same live Claude Code sessions, with no labels, through its own shipped
capture path, and is then asked the same ten questions.

**Tools and versions (frozen).** Muninn at the current build (`target/release/muninn`, sha256
`973ef470…`, commit after the query-expansion removal), wired as its plugin wires it (SessionStart,
UserPromptSubmit, PreToolUse, PostToolUse, Stop/SessionEnd hooks; no `import`, no seed file, no
`invalid` field anywhere). claude-mem 13.24.23 (npm; its hooks, worker and MCP search tools; its
observer model is whatever it uses by default through the Claude Code login). agentmemory 0.9.29
(`@agentmemory/agentmemory`, its hooks and MCP tools, defaults: context injection off). Secondary arm
`agentmemory-inject` (`AGENTMEMORY_INJECT_CONTEXT=true`, the documented switch). The two most
downloaded local memory plugins for Claude Code at the time of writing (npm, last month: claude-mem
78 741, agentmemory 29 386). A Mem0 arm is added only if an OpenAI key is provided before any cell
runs, with Mem0's default models; otherwise it is reported as not run. Telemetry off where the tool
has a switch; every tool preinstalled and pinned; no network install inside a cell.

**Repository.** gin-gonic/gin at `dcaa429` (chosen by the external-repository rule). Model
claude-sonnet-5 for seeding sessions and task cells.

**Seeding — identical for every arm.** For each run and each arm, a fresh data directory and a fresh
checkout; the 20 records of `revocation/seed.jsonl` in `created_at` order become 20 consecutive
`claude -p` sessions whose prompt is the record's `body` without the `user: ` prefix, followed by one
fixed line: "(Reply with one short sentence acknowledging.)". Nothing else is written, no `subject`,
`object` or `invalid` field reaches any tool. Each tool captures what it captures from those sessions
through its own hooks. The `off` arm has no seeding. The seeded data directory of a run is copied
into each of that run's ten task cells, so tasks do not see each other.

**Task cells.** The ten Gate 3 public-seed tasks and oracles, unchanged. Arms: `off`, `muninn`,
`claude-mem`, `agentmemory`, `agentmemory-inject`; 3 runs; order randomised per run. Each arm's
memory tools are allowed exactly as `muninn why`/`muninn status` are allowed for Muninn (MCP tool
names added to `--allowedTools`). Cells of tools with a fixed port run serially.

**Outcomes.** Primary: pass on the eight replacement scenarios (24 cells per arm). Secondary: unsafe
(retired value written), pass on the two revocation-without-replacement scenarios, turns, and what
each tool delivered or returned (hook output and MCP results logged per cell).

**Decision rules.** Exact two-sided Fisher tests on replacement-scenario pass counts: `muninn` vs
each competitor arm (three comparisons, Holm-adjusted) and every arm vs `off`. "Muninn detects
replacements better than X" is published only if Holm-adjusted p < 0.05 with Muninn ahead; "not
distinguishable" otherwise; a competitor ahead is published as that. Whatever Muninn scores here
is published beside Gate 3, including a result at the level of `off`.

**What may not change after this point.** The Muninn binary, the seed wording, the acknowledgement
line, the tasks, the oracles and the arm configurations. A fix to the harness (not to any tool) found
during a smoke is recorded here before the grid; an engine change would void this pre-registration.

**Head-to-head amendment after the harness smoke, before any grid cell (2026-09-14).** A smoke of the
`off` and `muninn` arms on one task (not part of the grid, not reported as a result) showed two things
about Muninn on this input:
1. Muninn's capture stores each seeding session as an `episode`; supersession (ENGINE.md §5.1) only
   applies to typed records with a shared (subject, relation), so "change of plan — X is now Y" in
   ordinary conversation retires nothing. This is the product as it is and is what the grid measures.
2. The prompt hook delivered nothing (21 × `silence:no_match`) although lexical recall found both
   records: the injection validator (`cue.rs::validate_block`) rejects any block containing
   `"\nassistant:"`, which is how capture renders every episode of an exchange with an assistant
   reply. Every such episode is silently dropped. This is a bug that affects any user with short
   exchanges, not an artefact of the seeding line.
The pre-registration froze the binary, so the frozen arm stays exactly as registered: `muninn`
(`973ef470…`). A second Muninn arm is added to the same grid, randomised with the others:
`muninn-fixed`, identical except for the validator fix (an episode's own single `assistant:` label
passes; a second one, or one in any other kind of block, is still rejected; unit-tested), binary
`fdcb4606…`. Both arms are published. The confirmatory comparison against the competitors uses the
frozen `muninn` arm; `muninn-fixed` is reported as a post-smoke fix and its comparisons are labelled
exploratory. Nothing else in the design changes. Also removed from the Muninn arm before any cell:
`MUNINN_CONFINE_ROOT`, which in cells makes Muninn's hook deny out-of-checkout reads with a message
about memory — a nudge no competitor arm receives.

## DreamBench-SWE public pilot under the benchmark's own harness (recorded 2026-09-14, before any grid cell)

**Why, and what it cannot show.** DreamBench-SWE's confirmatory traps need the authors' private oracles
(`dreambench/STATUS.md`; request drafted). Its public checkout has one scorable subset: the 24-task v0
pilot (6 sequences × 4 sessions, `experiments/env/tasks.jsonl`). Running it answers a narrower question
than the paper — does Muninn, dropped into a third party's harness as a `MemoryPolicy` next to that
harness's own literal baseline (B5) and Mem0 (B5-MEM0-LIT), help or hurt a Codex agent — and the
feasibility study (`dreambench/FEASIBILITY.md` §6) lists why it may not discriminate at all: each
session starts from the reference solution of the previous one, the failing test is visible in the
checkout, and a smoke on one sequence put both B0 and MUNINN at 4/4. A result at the ceiling is
published as "not discriminating", never as equivalence or superiority.

**Frozen.** DreamBench-SWE checkout `d340bb2` unmodified (a wrapper, `dreambench/run_bench_ext.py`,
registers MUNINN and an offline Mem0 client without editing the benchmark); agent image built from
`scripts/Dockerfile.codex-agent` (Codex 0.142.0), model `gpt-5.5` (the harness accepts only it);
conditions B0, B5, B5-MEM0-LIT (mem0ai 2.0.20 OSS, `infer=False` as in the paper, fastembed
`BAAI/bge-small-en-v1.5` + local Qdrant, offline — the paper used hosted Mem0), MUNINN
(`dreambench/muninn_policy.py`: events → `decision` records with origin `user_said`, episode →
`episode`; read = `muninn why --json`, ≤ 6 items / 1 200 tokens; binary `fdcb4606…`, the head-to-head's
fixed build); seeds 1, 2, 3; all 24 tasks; 288 agent sessions; four processes in parallel.

**Outcome and tests.** Per-session `final_passed` as the harness scores it. Per condition: passes out of
72. MUNINN vs each other condition: exact McNemar on sessions paired by (seed, task), two-sided, Holm
over the three comparisons. "Discriminating" requires B0 below 90 % of sessions; otherwise every
contrast is reported as uninformative whatever its p.

**Head-to-head: competitor arms built and smoked, harness fixes (recorded 2026-09-14, before any grid
cell).** claude-mem 13.24.23 and agentmemory 0.9.29 arms (`h2h/competitors/`) each ran two seeding
sessions, a snapshot, a restore and one task session end to end with `--setting-sources ""`; their hooks
fired (claude-mem in the transcript and worker log; agentmemory in the server state), their MCP tools
answered through `--allowedTools`, and a canary found no user CLAUDE.md, skills or MCP servers. n = 1
per arm, not a result. Harness fixes, none touching a tool: every checkout is named after the repository
(both tools key their state by the git top-level's basename, so a restored store was invisible under a
different name); each task cell's session transcript is copied into the grid's logs and handed to the
arm's `delivered` step (neither tool logs what it injects or returns). Facts a reader must know, left as
they are because they are the tools' defaults or apply to every arm: (1) claude-mem's observer rewrites
what it sees with an LLM (via the Claude Code login) and retires nothing; its async Stop summary never
ran under `claude -p`. (2) agentmemory stores raw prompts as observations, injects nothing by default,
and ignores `--port`/`--data-dir` (cells serial, state under the cell). (3) In the `agentmemory-inject`
arm the fixed acknowledgement line reaches the task session verbatim and the agent called it a prompt
injection in the smoke; the line is pre-registered and stays. (4) Every arm, Muninn included, still
receives Claude Code's own session context (the account e-mail, git status) and the claude.ai connector
list; identical across arms. (5) claude-mem's worker needs a fresh login token per run (the token it
receives does not refresh within a long grid).
