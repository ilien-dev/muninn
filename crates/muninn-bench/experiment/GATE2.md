# Gate 2 — does literal episode delivery by hook change the outcome? (measured 2026-09-13)

**Result: PASS.** On the five non-inferable tasks, the literal arm passed 19/25 cells and
the no-memory arm 2/25; the length-matched irrelevant control passed 4/25.

| contrast (non-inferable tasks) | point estimate | 95 % bootstrap CI (10 000, stratified by task) |
|---|---|---|
| literal − off | +0.680 | [+0.560, +0.800] |
| control − off | +0.080 | [−0.040, +0.200] |

Decision rule (PREREGISTRATION.md): (1) literal − off > 0 with a CI excluding 0 — met;
(2) control − off has a CI including 0, or a point estimate below half the literal effect
— both met. Model: claude-sonnet-5, 5 runs, task×arm order randomised per run, one fresh
git worktree and one fresh store per cell, executable oracles, no judge model. 90 cells,
$27.63, 0 errors, 0 timeouts (one control cell first recorded as an error was the agent running out of turns; since 2026-09-14 it is scored as a fail — see the amendment in `PREREGISTRATION.md`).

## Per task

| task | off | literal | control | what the oracle checks |
|---|---|---|---|---|
| engine-s12-why-responder | 2/5 | 5/5 | 4/5 | §12 no longer says the why responder is out of v1 and names `muninn why` as MVP |
| fact-userprompt-p95 | 0/5 | 5/5 | 0/5 | the measured p95 before (10.27 ms) and after (1.82 ms) the term-selection fix |
| fact-sessionstart-gate | 0/5 | 5/5 | 0/5 | SessionStart p50 before (11.9 ms) and after (1.05 ms) moving quick_check off the read path |
| fact-corpus-fetch-limits | 0/5 | 4/5 | 0/5 | 12 visits / 60 s / domain, 900 s cooldown, jsdelivr and statically mirrors |
| fact-real-transcript-ingest | 0/5 | 0/5 | 0/5 | 21 MB, 30 turns, 20 episodes, 30 ms |
| docs-grammars-control (inferable sanity) | 5/5 | 5/5 | 5/5 | a grammar table derivable from the repository |

Every number in the fact tasks exists only in the seeded transcript: absent from the
repository at `base_ref` and from `git log` (checked by grep before the run); the prompts
never state them and ask the agent to write "not found" rather than invent.

## Four levels

| level | off | literal | control |
|---|---|---|---|
| stored (episodes in the seeded store) | 82 | 82 | 82 (+183 in the foreign store) |
| delivered per cell (tokens, mean; records) | 0 | 627 · 3.5 | 539 · length-matched, foreign project |
| management (UserPromptSubmit p95, max over cells) | 0.07 ms | 1.36 ms | 1.75 ms |
| outcome (non-inferable pass) | 2/25 | 19/25 | 4/25 |
| turns per cell (mean) · wall time · cost | 19.5 · 77 s · $0.321 | 12.5 · 49 s · $0.245 | 20.4 · 80 s · $0.348 |

The literal arm is also the cheapest: fewer turns, because the agent stops searching.

## What the failures say

- `fact-real-transcript-ingest`, 0/5 in every arm: the prompt reads as "measure the
  ingest" and every agent tried to build and run `muninn ingest`; `cargo build` is
  blocked by the cell's permission mode, and the agents reported "not measured" instead
  of using the delivered episode (which carried all four numbers). Task wording, not
  retrieval: the same block was delivered in the pilot, where one cell used it.
- `engine-s12-why-responder` is only partly non-inferable (off 2/5 with the fair oracle):
  the repository's research files let a careful agent reconstruct the decision. It stays
  in the reported set because it was pre-registered for run 2.
- `fact-corpus-fetch-limits` literal 4/5: one cell wrote "60-second window" as "per
  minute" phrasing the oracle did not accept; scored as the oracle says.
- The control arm's 4/25 are s12 (4/5): irrelevant blocks do not help the fact tasks
  (0/15) and the s12 gain is within what the off arm shows.

## Reading the result honestly

- The tasks are fact recall (a number the earlier session measured) plus one decision
  task. They test the mechanism the plan needs — the agent receives and uses evidence it
  cannot reconstruct — not DreamBench-SWE's "hidden constraint changes the code" shape
  ([K2], hidden oracles not public). The external reference, 0.117 → 0.456, is a
  different benchmark and not comparable.
- One seed transcript (this project's own, frozen before the tasks were written, sha256
  in PREREGISTRATION.md); one model; one harness. Replication on Codex is pending.
- The transcript file is readable from the cells' home directory. Six cells (all on the
  ingest task) mention `~/.claude/projects` in their final answer: they looked for the
  transcript to measure it, not for the numbers. No cell in any arm produced a
  non-inferable number without a delivered block.
- Run 1 (75 cells, $15.74) failed the same rule with an invalid instrument; it is
  reported in full in PREREGISTRATION.md and `results/run1/`.

## Raw data

`results/run2/`: results.jsonl (every cell), summary.md, diffs/ (every cell's patch),
logs/ (the model's final message per cell), tasks.json (the exact prompts and oracles).
Reproduce: `muninn-bench experiment --jobs 3` with the seed at
`~/.local/share/muninn-bench/seed-14a6ab47-frozen.jsonl`; `--rescore` re-runs the
oracles on the saved patches without model calls.

## Note added 2026-09-14 — the control arm and `muninn why`

A Codex replication found that a `control` agent can reach the real memory by running `muninn
why` itself, because only prompt delivery was swapped to the foreign store (`PREREGISTRATION.md`,
2026-09-14). This grid is not affected: `muninn why` was not an allowed tool in run 2 (it was added
during Gate 3), and the control arm passed 0/15 fact cells. The runner now gives the control arm a
store that is foreign in every channel.
