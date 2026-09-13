# Gate 4 — F3: cue-anchored delivery, compaction survival, PM-Bench (measured 2026-09-13)

Gate 4 has three conditions. Two are measured below; the third (PM-Bench) is measured
in the same document once its replicate runs finish.

## §1 Cues vs lexical-only (`results/gate4-cues/`): NOT DISTINGUISHABLE — condition not met

Eight decisions anchored to eight files (one per crate), each naming a token absent
from the repository; one task per file, naming the area but never the file; the record
shares as few words as possible with the task. claude-sonnet-5, 3 runs, 96 cells,
$28.03, 1 error (control).

| arm | pass | cells where a dir/symbol cue fired | tokens delivered (mean) |
|---|---|---|---|
| off | 0/24 | — | 0 |
| lexical (cues off) | 12/24 | 0 | 609 |
| literal (cues + lexical, tool-time delivery on) | 14/24 | 15/24 | 1 187 |
| control (length-matched irrelevant) | 2/23 | 0 | 1 039 |

| contrast | point | 95 % bootstrap CI |
|---|---|---|
| literal − lexical | +0.083 | [−0.083, +0.250] |
| literal − off | +0.583 | [+0.458, +0.708] |
| lexical − off | +0.500 | [+0.375, +0.625] |
| control − off | +0.087 | [+0.000, +0.174] |

Pre-registered rule: literal − lexical > 0 with a CI excluding 0. The CI includes 0.
Lexical recall of the prompt already finds the anchored record in half the cells (the
rewritten prompts still share enough vocabulary with the records); the cues fired in 15
of 24 literal cells and added two passes. The length control gains +0.087 with a lower
bound at exactly 0, so part of the literal arm's extra 580 tokens per cell is length.

Consequence, as the plan states it: F3 ships as reinjection on compaction and at
session start (event cues, §2) plus lexical recall; dir/symbol cue delivery at prompt
and tool time stays in the engine, measured, **off by default** (`muninn init --cues`,
`muninn config cues on`, or `MUNINN_CUES=1`). The experiments keep measuring the full
mechanism (`MUNINN_CUES=1` in every arm with Muninn).

What would change the verdict: a repository whose files carry decisions that the
prompt vocabulary cannot reach — the case F3 was designed for — or more runs (the point
estimate is positive). Neither was available here; this grid used this repository and
three runs.

## §2 Compaction survival (decay probe): 100 %

Ten invariants seeded, 100 forced compactions (`PostCompact` hook, epoch bump each
time): 100/100 compactions delivered all ten (1 000/1 000 facts), 1.4 ms median / 2.5 ms
max per hook, no model involved. Published without-harness figure: 106/108 losses [K1];
constraint violations 30–59 % after compaction, 0 % when the constraint survives [C2].
Condition met by the mechanism; the without-Muninn line is cited, not re-measured.

## §3 PM-Bench [V1]

Setup: the released benchmark (`genglinliu/PMBench`, v9 week, 80 steps) run against
claude-sonnet-5 through an OpenAI-compatible bridge over `claude -p`
(`pmbench/claude_bridge.py`, temperature 0, 256 max tokens); three scaffolds: the
paper's `single_baseline`, its `todo_ledger`, and `muninn_ledger`
(`pmbench/run_muninn_ledger.py`: the same ledger, persisted as Muninn records with
`after`/`keyword` cues on the simulated clock, and at each step the fired intentions
injected as DUE NOW on top of the ledger). Line to beat: 65.1 % set-F1 (the paper's best,
a GPT-5.4 agent).

Round 1 (one run each, first scaffold version that replaced the ledger view with the
fired items):

| scaffold | hit | set precision | set recall | set F1 |
|---|---|---|---|---|
| single_baseline | 49.4 % | 74.1 % | 49.4 % | 59.3 % |
| todo_ledger | 48.1 % | 86.7 % | 48.1 % | 61.9 % |
| muninn_ledger (v1) | 44.4 % | 73.5 % | 44.4 % | 55.4 % |

Round 2 (`muninn_ledger` v2 keeps the full ledger view and adds the fired items on top;
two more runs of each baseline; one baseline run crashed inside the benchmark's own
parser on an odd model reply and is excluded):

| scaffold | runs | set F1 per run | mean |
|---|---|---|---|
| single_baseline | 3 | 59.3 · 58.2 · 65.1 | 60.9 % |
| todo_ledger | 3 | 61.9 · 66.2 · 57.4 | 61.8 % |
| muninn_ledger v2 | 3 | 63.0 · 58.6 · 60.3 | 60.6 % |
| muninn_ledger v1 (ledger view replaced) | 1 | 55.4 | — |

Round 3 (`muninn_ledger` v3, exploratory, not pre-registered: the fired items moved
from the ledger message to the head of the step message, for recency; three runs,
same bridge, `claude -p` now run in a bare directory so no project settings or store
reach the model under test):

| scaffold | runs | set F1 per run | mean | set recall | set precision |
|---|---|---|---|---|---|
| muninn_ledger v3 (DUE NOW next to the step) | 3 | 55.3 · 55.9 · 59.7 | 57.0 % | 40.7–45.7 % | 81.0–89.2 % |

Worse than v2 by 3.6 points on the mean, with recall down (v2: 48–49 %) and precision
up: next to the step, the fired list narrows what the model acts on instead of adding to
it. The scaffold shipped is v2; v3 is kept only as this row (`results/pmbench/v3/`).

Run-to-run spread on this model is ±4 points at temperature 0, larger than any
difference between scaffolds. **Condition not met**: no scaffold reaches 65.1 % on
average, and the Muninn scaffold is not distinguishable from the paper's own ledger.
What the week exercises is the model's own intention discipline (the JSON ledger it
rewrites each step); Muninn's trigger delivery fired correctly (DUE NOW blocks appear
on every due step in the prompt logs) and did not change the outcome. Raw data:
`results/pmbench/` (score logs and trajectories; `pmbench/` has the bridge and the
scaffold to reproduce them).

## Verdict

- §1 cues vs lexical: not distinguishable → dir/symbol cue delivery ships off by default.
- §2 compaction survival: 100/100 → event reinjection ships on by default.
- §3 PM-Bench: below the line, and equal to the paper's ledger → not claimed.

As the plan states for this outcome, F3 in the MVP is reinjection on compaction and at
session start, plus lexical recall; the rest of F3 stays measurable and switchable.
