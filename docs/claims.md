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
| Literal memory delivered by hook changes what the agent does | pass 19/25 vs 2/25 without memory; literal − off +0.68 [+0.56, +0.80]; length-matched irrelevant control +0.08 [−0.04, +0.20] | Gate 2, claude-sonnet-5, 90 cells, private seed; Codex / gpt-5.6-sol replication 10/25 vs 5/25 (exact p = 0.108; the two recoverable facts 10/10 vs 0/10, p = 2.2 × 10⁻⁴) | tasks written by the authors on the authors' repository; facts only reachable through the seeded transcript (checked by grep); on the second family the overall effect is much weaker, one task lost to an oracle that wants the command name in a Spanish-language answer |
| Records marked retired are never served, and that changes outcomes | filtered − render-matched unfiltered +0.22 [+0.11, +0.33] (sonnet), +0.19 [+0.07, +0.30] (haiku), 27/27 vs 0/27 on gpt-5.6-sol via Codex (Fisher p = 5 × 10⁻¹⁶); retired served 0/900 cells; retired value written 0 % vs 7.4 % / 22.2 % / 66.7 % | Gate 3, three families and two harnesses, 270 cells, private seed; plus a public-seed grid anyone can re-run (sonnet, 90 cells: 27/27 vs 1/27, +0.96 [+0.89, +1.00]); plus the same public-seed grid on three external repositories chosen by rule (Go, TypeScript, Python): gpt-5.6-sol 80/81 vs 0/81 (retired value written 0/81 vs 64/81), claude-sonnet-5 81/81 vs 3/81 (0/81 vs 15/81) | **the seed arrives with the retired records already marked** (`invalid: true` in `revocation/seed.jsonl`, honoured by `muninn import`): Gate 3 measures what serving only current records does, not whether Muninn detects on its own that a decision was replaced — that is the pre-registered head-to-head below; the control keeps layout, budget and record count identical (render-matched); the first Codex grid was invalid (instrument) and is published as such; without transcript noise the effect is larger, so the noisy private-seed figures are the ones to quote |
| Invariants survive compaction, up to the turn budget | 100/100 forced compactions delivered all 10 invariants (1 000/1 000 facts); 1.2 ms median per hook; with 20, 40 or 80 invariants the 700-token budget delivers 16 and the hook now says how many it cut | Gate 4 §2, `decay_probe.py`, no model | mechanism test; the without-Muninn loss rate is cited from the literature, not re-measured; the budget is a design decision and the limit travels with the claim |
| Read hooks are cheap | SessionStart p95 2.1 ms, UserPromptSubmit gated 0.86 ms / full 3.5 ms at 20 000 records | `perf --strict`, one machine | one machine; the contract fails CI when exceeded |
| PM-Bench: the typed-intention mechanism beats the paper's scaffolds on the same model, on weeks nobody tuned on, in two model families | three held-out weeks generated from seeds fixed by a commit hash: claude-sonnet-5 91.4 % vs 80.6 % / 78.7 %; gpt-5.6-sol 97.5 % vs 80.6 % / 80.4 %; exact stratified permutation p ≤ 5/8 000 for every baseline contrast in both families; development week 97.3 % and 98.3 % | Gate 4 §3 round 8, 96 counted runs, temperature 0 on Claude (none available on Codex), one isolated bridge per family for every arm | the identical scaffold on a plain dict store scores the same: round 9 found 0 differing boards in 1 458 and traced round 8's sonnet gap to one day-1 typing decision that later landed on both arms (Fisher p = 0.264), so the claim is about the mechanism, which Muninn implements exactly, not about the engine; the worst-case rule failed on sonnet in one week (4 of 9 runs at 84.6–87.8); scaffold with lifecycle in code, not the harness delivery path; the 82.9 % PIS line is another model |

## Pending (pre-registered, running or queued)

| question | grid | what changes the public wording |
|---|---|---|
| Does Muninn detect, from ordinary conversation and with no labels, that a decision was replaced — and do claude-mem and agentmemory? | head-to-head, same live seeding sessions for every tool (pre-registered). v1 (seed wording, 3 runs) measured: Muninn as registered 12/27 vs claude-mem 26/27 (p = 4.6 × 10⁻⁵, claude-mem better); the loop-5 build 27/27 vs 26/27 (tie, p = 1), agentmemory 12/27, no memory 1/27 — but the loops were checked on that wording. v2 (held-out wording, 3 runs): the loop-5 build 17/27, claude-mem 14/27 (tie, Holm p = 0.58), agentmemory 9/27 (tie, Holm p = 0.11), agentmemory with injection 1/27 (Muninn better, Holm p = 1.4 × 10⁻⁵), no memory 0/27 | only v2 supports a public comparison, and on it Muninn ties claude-mem; Muninn's answers mention the retired value more often (11/27 vs 2/27, secondary figure, not the registered outcome) |

## Not claimed

- **Better than the harness's native memory.** Not measured; the evidence log says why it
  cannot be with the data that exists (`research/CONCLUSION.md`).
- **Better than Mem0, Rekal, agentmemory or any other product.** No head-to-head on the same
  harness has been run. Vendor LoCoMo / LongMemEval figures are not comparable and are not
  cited as comparisons.
- **DreamBench-SWE.** The confirmatory traps need the authors' private oracles (request drafted). The
  public 24-task pilot was run under the benchmark's own harness and does not discriminate: without
  memory the agent passed 48/48 sessions (`dreambench/STATUS.md`).
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
