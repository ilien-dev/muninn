# DreamBench-SWE — status (2026-09-13)

DreamBench-SWE (arXiv 2608.20664, `github.com/iroiro147/dreambench-swe`) is the public
multi-session memory-hygiene benchmark for software agents and the closest match to what
Muninn does (`research/00-evidence-log.md` [K2]). Its published lines: no memory 21/180
(11.7 %), literal event memory 82/180, reference probe 83/180, Mem0 literal 97/180 (53.9 %),
v2.1 successor audit, Codex `gpt-5.5` as the agent.

## What a Muninn number would need, and what is not public

Read from the public checkout (`d340bb2`) and the v2.1 release artifact:

- The scored unit is session 3 of a three-session trap, judged by a hidden pytest oracle
  (`experiments/env/oracles/<seq>/s3_test.py`). The oracles are excluded from the public
  package (`scripts/package_artifact.py`). **0 of 180 cells can be scored locally.**
- 25 of the 60 traps start from repository commits that the public `build_env.py` cannot
  regenerate (`load_env.py` maps 5 of the 11 `initial_commit` values). **35 of 60 traps can be
  materialised, none scored.**
- Memory is a harness-side `MemoryPolicy` (`src/benchmarks/baselines.py`): `write(trajectory)`
  after each scored session, `read(task)` → at most 6 items / 1 200 tokens pasted into the
  agent prompt. A Muninn condition is a `MemoryPolicy` subclass registered in
  `run_bench._policy_for_condition` and `run_grid.VALID_CONDITIONS`; Claude Code hooks do not
  take part, and the agent is fixed to Codex `gpt-5.5` in Docker for comparability.
- Statistics: exact sign-flip test paired by trap with Holm correction over a fixed family,
  each condition against a concurrent no-memory baseline (`analyze_confirmatory_v2.py`).

## What is honest to say

No DreamBench-SWE number for Muninn exists and none can be produced without the authors'
private package. The only public subset with executable oracles is the 24-task v0 pilot
(`experiments/env/tasks.jsonl`); a number on it would not be comparable with the paper and is
not reported as one.

## Request to the authors (drafted, not sent)

Sent by the maintainer, not by an automated process. Text in `REQUEST.md`. Asks for:
the reviewer package (`--private`: oracles + reference solutions), the fixture repositories
for the 11 initial commits, the v2.1 synthetic conformance corpus, and a pre-registered slot
for an external `MuninnPolicy` condition run with a concurrent B0 on their infrastructure —
or acceptance of a pull request that adds the policy so they can run it.
