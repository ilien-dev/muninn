# Gate 2 experiment

Pre-registered in `PREREGISTRATION.md`. Tasks in `tasks.json`. Runner:

```sh
cargo build --release -p muninn-cli -p muninn-bench
./target/release/muninn-bench experiment --dry-run              # the cell plan
./target/release/muninn-bench experiment --pilot --model claude-haiku-4-5-20251001   # 2 cells: plumbing + cost per cell
./target/release/muninn-bench experiment                        # full run (tasks.json: model, runs, arms)
```

Each cell: `git worktree add` at `base_ref` → `muninn init` + `muninn ingest` of the
seed transcripts into a store private to that worktree (`MUNINN_ROOT`) → `claude -p`
with the hooks wired through `--settings` and `MUNINN_ARM` set → the task's oracle
command → worktree removed. Results in `out/results.jsonl`, summary in `out/summary.md`
(re-written after every cell, so a partial run is still readable).

The `control` arm needs `control_transcripts`: sessions from an unrelated project. They
never leave the machine; the control store is rebuilt from them at run start.
