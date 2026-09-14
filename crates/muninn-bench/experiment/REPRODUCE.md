# Reproducing every number

One command per published figure. Each entry says what the command needs, what it prints,
and where the frozen inputs live. Commands run from the repository root after
`cargo build --release`. Grids that call a model need a Claude Code login (`claude`) or a
Codex login (`codex`); the cost and wall time quoted are what the recorded runs measured.

## Inputs that are public and inputs that are not

| input | where | public |
|---|---|---|
| scenarios, seed records, oracles, task prompts | `revocation/`, `cues/`, `tasks.json` | yes |
| the experiment runner and every analysis script | `../src/experiment.rs`, `revocation/analyze.py`, `pmbench/*.py` | yes |
| raw results of every grid (cell logs, patches, score files) | `results/` | yes |
| PM-Bench (benchmark, scorer, released week) | `github.com/genglinliu/PMBench` at commit `e1093c4` | yes |
| the frozen seed transcript used by Gates 2–4 and the boot/cue grids | outside the repository, sha256 `223dcf4d…fa895`, 22 559 233 bytes | **no** — this project's own private sessions |
| pre-registration timestamps | `PREREGISTRATION.md` (git history) and `prereg-stamps/*.ots` (OpenTimestamps, Bitcoin-anchored) | yes |

Grids marked *private seed* below can be re-run only with a seed transcript of your own;
the public-seed Gate 3 grid exists for that reason.

## Latency and budget contracts (no model)

```sh
cargo run --release -p muninn-bench -- perf --strict          # hook p95s at 20 000 records, fails above the contract
cargo run --release -p muninn-cli --features exact-tokens -- init --check-budget   # boot block ≤ 1 000 tokens
MUNINN_FAULT_REPS=200 cargo test -p muninn-cli --release --test fault             # 15 fault scenarios × 200
```

## Gate 1 — rule classifier (no model)

```sh
./target/release/muninn-bench rules --holdout crates/muninn-bench/corpora/claude-md/holdout2.txt --labels crates/muninn-bench/corpora/claude-md/labels2.jsonl
```
Corpus, labels and report: `../corpora/claude-md/GATE1.md`.

## Gate 2 — literal delivery (private seed, claude-sonnet-5, 90 cells)

```sh
./target/release/muninn-bench experiment --config crates/muninn-bench/experiment/tasks.json --jobs 3 --out <dir>
./target/release/muninn-bench experiment --rescore --out <dir>
```
Report `GATE2.md`; raw `results/run2/`. Codex replication: `--config crates/muninn-bench/experiment/tasks-gate2-codex.json` (`results/gate2-codex/`).

## Gate 3 — F1 filter

Private seed, two families (`GATE3.md`, `results/gate3-sonnet/`, `results/gate3-haiku/`):
```sh
./target/release/muninn-bench experiment --config crates/muninn-bench/experiment/revocation/tasks-revocation.json --jobs 3 --out <dir>
python3 crates/muninn-bench/experiment/revocation/analyze.py <dir>
```
Codex / gpt-5.6-sol (pre-registered, `results/gate3-codex/`):
```sh
./target/release/muninn-bench experiment --config crates/muninn-bench/experiment/revocation/tasks-revocation-codex.json --jobs 3 --out <dir>
```
Public seed, no private input at all (pre-registered, `results/gate3-public/`):
```sh
./target/release/muninn-bench experiment --config crates/muninn-bench/experiment/revocation/tasks-revocation-public.json --jobs 3 --out <dir>
```
Public seed on three external repositories chosen by rule (`results/gate3-ext-{gin,vue,python}-{codex,sonnet}/`):
```sh
gh repo clone gin-gonic/gin <dir>/gin -- --depth 1        # likewise vuejs/vue, TheAlgorithms/Python; commits in the task files
# edit "repo" in each task file to your clone path, then:
./target/release/muninn-bench experiment --config crates/muninn-bench/experiment/revocation/tasks-external-gin-codex.json --jobs 3 --out <dir>
```

## Gate 4 §1 — cues, expansion, boot vehicle (private seed)

```sh
./target/release/muninn-bench experiment --config crates/muninn-bench/experiment/cues/tasks-cues.json --jobs 3 --out <dir>            # first grid
./target/release/muninn-bench experiment --config crates/muninn-bench/experiment/results/gate4-cues-v2/tasks.json --jobs 3 --out <dir>  # expansion
./target/release/muninn-bench experiment --config crates/muninn-bench/experiment/results/boot-vehicle/tasks.json --jobs 3 --out <dir>   # boot vehicle
./target/release/muninn-bench experiment --config crates/muninn-bench/experiment/cues/tasks-cues-v2-rep5.json --jobs 3 --out <dir>      # five-run replication
./target/release/muninn-bench experiment --config crates/muninn-bench/experiment/cues/tasks-boot-vehicle-rep5.json --jobs 3 --out <dir> # five-run replication
```

## Gate 4 §2 — compaction survival (no model)

```sh
python3 crates/muninn-bench/experiment/decay_probe.py --reps 100      # 10 invariants, 100 forced compactions; writes results/decay-probe/
```

## Gate 4 §3 — PM-Bench

Rounds 4–7 (`GATE4.md`, `results/pmbench/round4..7/`):
```sh
bash crates/muninn-bench/pmbench/run_round4.sh <PMBench checkout> <out dir> 3 claude-sonnet-5
python3 crates/muninn-bench/pmbench/aggregate_round4.py <out dir>
```
Round 8 — held-out weeks, store ablation, second family, one bridge (`results/pmbench/round8-*/`):
```sh
python3 crates/muninn-bench/pmbench/run_round8.py --pmbench <PMBench checkout> --out <dir> \
    --prereg-commit f173f2dc75625b89f7b0543004ed7cf44c51b399 --model claude-sonnet-5 --bridge claude --port 30002
python3 crates/muninn-bench/pmbench/run_round8.py --pmbench <PMBench checkout> --out <dir-codex> \
    --prereg-commit f173f2dc75625b89f7b0543004ed7cf44c51b399 --model gpt-5.6-sol --bridge codex --port 30003
python3 crates/muninn-bench/pmbench/analyze_round8.py sonnet=<dir> codex=<dir-codex>
```
Round 9 — shadow store (`results/pmbench/round9-*/`): the same launcher with `--shadow --weeks heldout --arms muninn_store,plain_store`.
The held-out seeds are a function of the pre-registration commit hash (`run_round8.py`,
`heldout_seeds`); the launcher regenerates and validates them with PM-Bench's own tools and
writes `weeks/MANIFEST.json` with their hashes. Grids resume with `--resume` (complete runs kept, partial ones removed, refused if a frozen hash changed); experiment grids re-run infrastructure errors with `--rerun-errors --config <the grid's own file>` and rescore with `--rescore --config <same>` (the runner refuses another file). Every run writes a `*.manifest.json` with the
hashes of the scaffold, the scorer, the scenario, the muninn binary and the bridge's canary
answer.

## Checking the pre-registration timestamps

```sh
ots verify crates/muninn-bench/experiment/prereg-stamps/<file>.ots     # needs a Bitcoin node or the public calendars
git log --format='%H %cI %s' -- crates/muninn-bench/experiment/PREREGISTRATION.md
```
An `.ots` file is "pending" until the calendar's Bitcoin transaction confirms (hours); `ots
upgrade <file>.ots` completes it afterwards.
