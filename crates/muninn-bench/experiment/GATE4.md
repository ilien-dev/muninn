# Gate 4 — F3: cue-anchored delivery, compaction survival, PM-Bench (measured 2026-09-13; PM-Bench rounds 4–5 the same day)

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

### §1, second grid — query expansion through the symbol graph (`results/gate4-cues-v2/`)

Post-gate change: the read hook adds to the lexical query the definitions of files
whose path contains a prompt word (`expand_terms`), so "the embedding crate" reaches a
record that only says `embed_pending`. Pre-registered in `PREREGISTRATION.md` (after
launch, before any result was read). Same eight tasks and seed, claude-sonnet-5,
3 runs, 72 cells, $17.57, 1 error re-run; cells confined by a PreToolUse deny.

| arm | pass | tokens delivered (mean) | turns (mean) | cost/cell |
|---|---|---|---|---|
| lexical-plain (no cues, no expansion) | 10/24 | 503 | 10.5 | $0.231 |
| lexical (no cues, expansion) | 13/24 | 522 | 12.0 | $0.260 |
| literal (cues + expansion) | 15/24 | 741 | 11.0 | $0.242 |

| contrast | point | 95 % cluster-bootstrap CI (by task) |
|---|---|---|
| lexical − lexical-plain | +0.125 | [+0.000, +0.292] |
| literal − lexical | +0.083 | [−0.083, +0.292] |
| literal − lexical-plain | +0.208 | [−0.042, +0.417] |

Per task, expansion turned `cue-busy-cap` 0/3 → 2/3 and `cue-azure-pattern` 1/3 → 2/3;
the other six are unchanged. Retired records served: 0. The lower bound sits at
exactly zero, so by the rule it ships as an **opt-in** (`muninn config expand on`,
`MUNINN_EXPAND=1`) and the experiments keep it on in the arms that name it. Cues on
top of expansion repeat the first grid's figure (+0.083); the verdict on cues stands.

### Boot-block vehicle — file vs SessionStart hook (`results/boot-vehicle/`)

The shipped default stopped writing the boot block into the user's CLAUDE.md /
AGENTS.md: the SessionStart hook injects a compact summary (~425 estimated tokens,
`plugin/templates/BOOT.hook.md`) at startup, resume, clear and after compaction, and
the `muninn` skill carries the long form. Pre-registered as a non-inferiority test
(`PREREGISTRATION.md`), 14 tasks (Gate 2's six plus the eight cue tasks), 3 runs,
84 cells, claude-sonnet-5, 0 errors.

| arm | pass | tokens delivered (mean) | turns (mean) | cost/cell |
|---|---|---|---|---|
| literal (long block in CLAUDE.md, as in every gate) | 30/42 | 782 | 10.3 | $0.233 |
| literal-hookboot (no file; compact summary by hook) | 35/42 | 776 | 10.9 | $0.239 |

`literal-hookboot − literal`: **+0.119 [95 % CI +0.000, +0.262]** (cluster bootstrap by
task). Not inferior; if anything better: three cue tasks moved from 1/3 to 2–3/3 with
the summary arriving as the first thing in the session instead of inside the
instruction file. Retired records served: 0. The hook vehicle stays the default;
`muninn init --boot-file` remains for users who want the block in the file.

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

## §3 rounds 4–5 — Muninn as the intention store (measured 2026-09-13)

Pre-registered in `PREREGISTRATION.md` ("round 4", "round 5") before any cell ran. Two
findings on the round 1–3 data motivated the rounds:

- **The round 1–3 bridge leaked the user's global `~/.claude/CLAUDE.md`** to the model under
  test (`claude -p` without `--setting-sources ""`; probe answer through the old bridge:
  "Svipall para acceso web; respuesta en español"; the round-2 ledger notes are in Spanish).
  All eleven earlier runs share it. The round-4 bridge passes `--setting-sources ""` (probe:
  "NONE", 527 input tokens) and both baselines were re-measured through it.
- **The v1–v3 scaffold read hidden fields** (`step["time"]`, `step["cues"]`) into the store.
  The round-4 scaffold reads only what the model can see (day plan, vignette, options, step
  menu, and the replies to the `check_time` / `query_state` actions it issues).

**Scaffold** (`pmbench/run_muninn_pis.py`, arm `muninn_store`). The model no longer owns a
ledger. Intentions are typed records in a Muninn store (`after` cues for day and clock,
`keyword` cues for channels); lifecycle is code: add / reschedule / override / cancel / done,
daily re-arm of regular intentions, day-scoped carry of cross-day intentions, same-day expiry.
Per step: one **Form/Revise** call (new text → typed ops), code-issued **observation**
(`check_time` while a time intention is pending; while any intention watches a channel, one
`query_state` per channel), **Filter** = `muninn cues --ungated` with the fake clock set from
the queried time, one **Decide** call (eligible board → menu handles). A clock-matched time
intention the model omits would be added by a token-overlap guard; it never fired (0 events in
6 runs). Round 5 changed one rule: a channel-kind intention is eligible only when some channel
reply is new information (not "(no updates)" and different from that channel's previous reply
the same day). Design after PIS `[V2]`; prompts unchanged between rounds.

**Results** — claude-sonnet-5, temperature 0, isolated invocation, the released v9 week (80
steps, 81 due items), 3 runs per arm:

| arm | set F1 per run | mean (sd) | cross-day miss | update miss | time-modality hit | proactive-monitoring hit | FA/step |
|---|---|---|---|---|---|---|---|
| single_baseline (PM-Bench's own) | 79.7 · 76.7 · 77.2 | **77.9 (1.6)** | 0.0 % | 40.7 % | 70.8 % | 44.4 % | 7.5 % |
| todo_ledger (PM-Bench's own) | 78.3 · 80.0 · 81.1 | **79.8 (1.4)** | 9.5 % | 33.3 % | 63.9 % | 39.3 % | 2.5 % |
| muninn_store, round 4 | 96.2 · 96.2 · 92.4 | **94.9 (2.2)** | 0.0 % | 11.1 % | 100 % | 96.6 % | 2.9 % |
| muninn_store, round 5 | 96.2 · 95.7 · 96.9 | **96.3 (0.6)** | 0.0 % | 11.1 % | 100 % | 100 % | 1.7 % |

Lines from the literature: 65.1 % (the paper's best scaffold, a GPT-5.4 agent `[V1]`) and
82.9 % (PIS on DeepSeek-Chat `[V2]`). Both are other models; PIS's code is not released, so it
was not run on claude-sonnet-5. The within-model comparison is the two rows above.

Where the points come from, on the round-5 traces: all 39 due items that need a channel
query (24 clock, 15 hidden channels) are hit in every run; the remaining errors (4 misses and
1–3 false alarms per run) are judge decisions on ambiguous vignettes ("You rinse a dish" taken as dinner, "your package is
visible behind the counter" not taken as reaching it, a follow-up text read one step late).
The one update miss per run is the same item in all six runs (Friday's dry-cleaning override,
whose new cue arrives in the same sentence as dinner).

Cost: `muninn_store` 167 model calls and 129–133 k estimated input tokens per week, 8.5–9.4
min wall-clock (direct `claude -p`, three runs in parallel); the baselines' surviving prompt
logs show 1.19 M (single, 126 calls) and 1.49 M (ledger, 80 calls) estimated input tokens —
one run each, and the file was shared by three concurrent writers, so indicative only. The
store keeps each call short: the model sees the store, not the growing conversation.

**Data.** `results/pmbench/round4/` and `results/pmbench/round5/`: action logs, score
reports, the scaffold's per-step traces (board, ops, replies, decisions) and final store dumps.
The three baseline runs of each arm were launched in the same second and PM-Bench's runner
names the log by that second, so their action logs overwrote one file; each run's console
output was kept and `pmbench/reconstruct_from_stdout.py` rebuilt the three logs from it. On the
run whose original log survived the rebuilt entries are identical (80/80, both arms); the
collided originals are kept under `original-collided/`. The launcher now staggers launches.

**Decision rule, applied.** Pre-registered: claim only if mean set-F1 ≥ 82.9 % and above both
baselines by more than the round-2 spread (±4). Round 5: 96.3 % vs 79.8 % and 77.9 %. Met.
Round 5 is not below round 4, so the round-5 rule ships as the scaffold. Caveats that stay
attached to the claim: the 82.9 % line is another model; three runs per arm; the scaffold is an
in-loop agent for PM-Bench, not the harness delivery path of F3 — what transfers to F3 is the
mechanism (lifecycle in code, cue firing by the store, the model only forms and decides).

## Verdict

- §1 cues vs lexical: not distinguishable → dir/symbol cue delivery ships off by default.
- §2 compaction survival: 100/100 → event reinjection ships on by default.
- §3 PM-Bench rounds 1–3: below the line, and equal to the paper's ledger → not claimed (bridge later found contaminated, scaffold peeked at hidden fields).
- §3 PM-Bench rounds 4–5: Muninn as the intention store, isolated bridge — 96.3 % set F1 (3 runs) vs 79.8 % / 77.9 % for the paper's two scaffolds on the same model, above the 82.9 % PIS line (another model) → claimed, with the caveats in §3 rounds 4–5.

As the plan states for this outcome, F3 in the MVP is reinjection on compaction and at
session start, plus lexical recall; the rest of F3 stays measurable and switchable.
