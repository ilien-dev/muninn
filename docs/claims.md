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
| Literal memory delivered by hook changes what the agent does | pass 19/25 vs 2/25 without memory; literal − off +0.68 [+0.56, +0.80]; length-matched irrelevant control +0.09 [−0.04, +0.21] | Gate 2, claude-sonnet-5, 90 cells, private seed | one model; tasks written by the authors on the authors' repository; facts only reachable through the seeded transcript (checked by grep) |
| Retired facts are never served, and that changes outcomes | filtered − render-matched unfiltered +0.22 [+0.11, +0.33] (sonnet), +0.19 [+0.07, +0.30] (haiku), 27/27 vs 0/27 on gpt-5.6-sol via Codex (Fisher p = 5 × 10⁻¹⁶); retired served 0/270 cells; retired value written 0 % vs 7.4 % / 22.2 % / 66.7 % | Gate 3, three families and two harnesses, 270 cells, private seed | the control keeps layout, budget and record count identical (render-matched); the first Codex grid was invalid (instrument) and is published as such; a public-seed grid is pre-registered and pending |
| Invariants survive compaction, up to the turn budget | 100/100 forced compactions delivered all 10 invariants (1 000/1 000 facts); 1.2 ms median per hook; with 20, 40 or 80 invariants the 700-token budget delivers 16 and the hook now says how many it cut | Gate 4 §2, `decay_probe.py`, no model | mechanism test; the without-Muninn loss rate is cited from the literature, not re-measured; the budget is a design decision and the limit travels with the claim |
| Read hooks are cheap | SessionStart p95 2.1 ms, UserPromptSubmit gated 0.86 ms / full 3.5 ms at 20 000 records | `perf --strict`, one machine | one machine; the contract fails CI when exceeded |
| PM-Bench: the typed-intention store beats the paper's scaffolds on the same model | set F1 96.3 % (round 5) and 96.7 % (round 7) vs 79.8 % (todo ledger) and 77.9 % (single baseline), 3 runs each, claude-sonnet-5, temperature 0 | Gate 4 §3 rounds 4–7 | development week only until round 8 reports; scaffold with lifecycle in code, not the harness delivery path; the 82.9 % PIS line is another model; round-8 store ablation decides whether the sentence says "Muninn" or "the mechanism Muninn implements" |

## Pending (pre-registered, running or queued)

| question | grid | what changes the public wording |
|---|---|---|
| Does the PM-Bench figure hold on weeks nobody tuned on? | round 8, 3 held-out weeks, seeds from the pre-registration commit hash | R1 fails → "development week only" |
| Is it the store or the scaffold? | round 8, `plain_store` ablation | R2 within ±2 points → "the mechanism, which Muninn implements" |
| Second model family on PM-Bench | round 8 on gpt-5.6-sol via Codex | a family where the effect fails is reported as such |
| Reproducible by outsiders | Gate 3 with a public records-only seed | none; it exists so the grid can be re-run |
| Two decisions whose CI touched zero | five-run replications of boot vehicle and query expansion | replication decides default vs opt-in vs removal |

## Not claimed

- **Better than the harness's native memory.** Not measured; the evidence log says why it
  cannot be with the data that exists (`research/CONCLUSION.md`).
- **Better than Mem0, Rekal, agentmemory or any other product.** No head-to-head on the same
  harness has been run. Vendor LoCoMo / LongMemEval figures are not comparable and are not
  cited as comparisons.
- **DreamBench-SWE.** The benchmark's hidden oracles are not public; see the experiment
  README for the status of that item.
- **Cue-anchored delivery (dir/symbol cues) helps.** Measured, not distinguishable from lexical
  recall (+0.08 [−0.08, +0.25]); shipped off by default.
- **Any figure on a repository that is not this one.** All harness grids run on this
  repository at a frozen commit; generalisation to other codebases is not measured.
- **PM-Bench through the actual hook delivery path.** The PM-Bench scaffold is an in-loop
  agent that uses the store's CLI; what transfers to the plugin is the mechanism, and the
  claim says so.
