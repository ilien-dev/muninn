# What Muninn claims in public, and what it does not

One row per claim that may appear in a README, a post or a talk. A claim is made only when
its row here names a measurement with raw data in the repository and a decision rule written
before the data. Rows under *Not claimed* are things a reader might expect and that the
evidence does not support; they are listed so that nobody has to discover them.

Conventions: point estimate [95 % CI]; CIs are bootstrap over cells, clustered by task where
the pre-registration says so; "cells" are one model invocation on one task in one arm. Every
grid, its pre-registration and its raw data are indexed in
`crates/muninn-bench/experiment/README.md`; the commands are in `REPRODUCE.md` there.

## Claimed

| claim | figure | grid | caveats that travel with the claim |
|---|---|---|---|
| Literal memory delivered by hook changes what the agent does | pass 19/25 vs 2/25 without memory; literal − off +0.68 [+0.56, +0.80]; length-matched irrelevant control +0.08 [−0.04, +0.20] | Gate 2, claude-sonnet-5, 90 cells, private seed | one model; tasks written by the authors on the authors' repository; facts only reachable through the seeded transcript (checked by grep) |
| Retired facts are never served, and that changes outcomes | filtered − render-matched unfiltered +0.22 [+0.11, +0.33] (sonnet), +0.19 [+0.07, +0.30] (haiku), 27/27 vs 0/27 on gpt-5.6-sol via Codex (Fisher p = 5 × 10⁻¹⁶); retired served 0/630 cells; retired value written 0 % vs 7.4 % / 22.2 % / 66.7 % | Gate 3, three families and two harnesses, 270 cells, private seed; plus a public-seed grid anyone can re-run (sonnet, 90 cells: 27/27 vs 1/27, +0.96 [+0.89, +1.00]); plus the same public-seed grid on three external repositories chosen by rule (Go, TypeScript, Python; gpt-5.6-sol): 80/81 vs 0/81, retired value written 0/81 vs 64/81 | the control keeps layout, budget and record count identical (render-matched); the first Codex grid was invalid (instrument) and is published as such; without transcript noise the effect is larger, so the noisy private-seed figures are the ones to quote |
| Invariants survive compaction, up to the turn budget | 100/100 forced compactions delivered all 10 invariants (1 000/1 000 facts); 1.2 ms median per hook; with 20, 40 or 80 invariants the 700-token budget delivers 16 and the hook now says how many it cut | Gate 4 §2, `decay_probe.py`, no model | mechanism test; the without-Muninn loss rate is cited from the literature, not re-measured; the budget is a design decision and the limit travels with the claim |
| Read hooks are cheap | SessionStart p95 2.1 ms, UserPromptSubmit gated 0.86 ms / full 3.5 ms at 20 000 records | `perf --strict`, one machine | one machine; the contract fails CI when exceeded |
| PM-Bench: the typed-intention mechanism beats the paper's scaffolds on the same model, on weeks nobody tuned on, in two model families | three held-out weeks generated from seeds fixed by a commit hash: claude-sonnet-5 91.4 % vs 80.6 % / 78.7 %; gpt-5.6-sol 97.5 % vs 80.6 % / 80.4 %; exact stratified permutation p ≤ 5/8 000 for every baseline contrast in both families; development week 97.3 % and 98.3 % | Gate 4 §3 round 8, 96 counted runs, temperature 0 on Claude (none available on Codex), one isolated bridge per family for every arm | the identical scaffold on a plain dict store scores the same (+0.4 on gpt-5.6-sol) or higher (−5.0 on sonnet, traced to one day-1 typing decision), so the claim is about the mechanism, not the engine; the worst-case rule failed on sonnet in one week (4 of 9 runs at 84.6–87.8); scaffold with lifecycle in code, not the harness delivery path; the 82.9 % PIS line is another model |

## Pending (pre-registered, running or queued)

| question | grid | what changes the public wording |
|---|---|---|
| Do the two stores ever disagree on the same operations? | round 9, shadow store inside muninn runs (no extra model call), 18 runs | any disagreement → the round-8 gap stays attributed to the store |

## Not claimed

- **Better than the harness's native memory.** Not measured; the evidence log says why it
  cannot be with the data that exists (`research/CONCLUSION.md`).
- **Better than Mem0, Rekal, agentmemory or any other product.** No head-to-head on the same
  harness has been run. Vendor LoCoMo / LongMemEval figures are not comparable and are not
  cited as comparisons.
- **DreamBench-SWE.** The benchmark's hidden oracles are not public; see the experiment
  README for the status of that item.
- **Query expansion through the symbol graph helps.** +0.125 on three runs did not replicate:
  −0.125 [−0.275, −0.025] on five, −0.031 [−0.125, +0.062] pooled. Withdrawn.
- **The SessionStart summary is better than the block in CLAUDE.md.** The first grid's +0.119
  did not replicate (+0.000 [−0.114, +0.129]); pooled +0.045 [−0.045, +0.152]. What is claimed is
  only that it is not worse within that precision, which is why it can stay the default.
- **Cue-anchored delivery (dir/symbol cues) helps.** Measured, not distinguishable from lexical
  recall (+0.08 [−0.08, +0.25]); shipped off by default.
- **Any Gate 2 or PM-Bench figure on a repository that is not this one.** Gate 3 has been run on
  three external repositories; Gate 2's facts live in this project's own transcript and cannot be
  moved, and PM-Bench has no repository.
- **PM-Bench through the actual hook delivery path.** The PM-Bench scaffold is an in-loop
  agent that uses the store's CLI; what transfers to the plugin is the mechanism, and the
  claim says so.
