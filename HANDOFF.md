# Where this left off — 2026-09-23

Everything below is committed and the gate is green: `cargo test --workspace --features
exact-tokens`, `cargo fmt`, `cargo clippy --workspace --all-targets --features exact-tokens
-- -D warnings`, `cargo run --release -p muninn-bench -- perf --strict`, and the fault suite
at 200 repetitions. Nothing is running in the background.

## What is claimed

**Where a decision is replaced in conversation *and* in the code** — the condition the engine
is built for, and the one where a commit carries the value:

| | replacement cells | retired value written |
|---|---|---|
| **Muninn** | **54 / 54** | **0 / 54** |
| claude-mem 13.24.23 | 25 / 54 | 10 / 54 |
| agentmemory 0.9.29 | 1 / 54 | 1 / 54 |
| no memory | 0 / 54 | 3 / 54 |

Exact Fisher p = 2.0 × 10⁻¹¹. `results/h2h-v17/`, the arm pinned before a cell of it ran,
competitor cells not re-run and pinned to the versions they were measured at.

**Where decisions live only in the conversation** — not claimed, and the figure to read
before installing this. Five phrasing sets generated after the fact, each registered before
it ran, the shipping build measured on all five:

| wording | Muninn | claude-mem |
|---|---|---|
| bare values (`zstd`) | **18 / 54** | **31 / 53** |
| sentences | 53 / 54 | 38 / 54 |
| sentences, comparative | 45 / 54 | 41 / 54 |
| sentences with change verbs | 53 / 54 | 50 / 54 |
| sentences, mixed | 49 / 54 | 46 / 54 |
| **all five** | **218 / 270** | **206 / 269** |

p = 0.25 over the five, so nothing is claimed. It splits: sentences 200/216 against 175/215
(p = 5.5 × 10⁻⁴), bare values 18/54 against 31/53 (p = 0.012). A turn that is one word has no
statement in it for a typed ledger to record. `unsafe` 1/270 against 4/269.

Context: v17 injects 2.413 [2.346, 2.648] times claude-mem's, on an instrument that reads the
previous build at 2.824 where the grid that first reported it read 2.591 — so the figure
offered without a caveat is the between-arm one, 0.876 of the previous build.

## What the day did, in one line each

Twenty-odd defects found by reading the live store, the real transcripts, or a claim in a
document and checking whether the code agreed. The largest:

- The latency gate ran at 5 000 records where it publishes 20 000; at the cap SessionStart was
  10.5 ms against a 10 ms limit. Two query plans, `23.9 ms → 0.77 ms`, and a test that pins
  both plans so the unary `+` cannot be tidied away.
- Three experiment oracles matched their value against `muninn recall`'s own timing line.
- Nine invariants came from fixture-generation prompts and pasted benchmark payloads.
- A compaction summary was captured as something the user said, at trust 1.
- Nothing had ever run `optimize` on the full-text index; 918 segments, 8.56 ms → 6.54 ms.
- Two health checks could not fire: one always cold, one always green.
- `muninn init --codex` and the committed `codex/hooks.json` disagreed on timeouts and on
  quoting the binary path.
- Health check 6 compared two `meta` keys nothing ever wrote; made to fire, it then read the
  projection trigger — which every commit sets — and called a healthy store's render frozen.
  It now compares the render against the pool of invariants and corrections it was drawn from.
- `[muninn:catalog]` closed with "a subject missing from this list has nothing on record" on
  stores holding episodes, which is every store that has ever captured a session.

Reverted after measuring, and why: reading order (null primary, 4/108 identical); a rule that
would have read a bare value as a decision (the assistant's reply said nothing either); the
reply-as-topic bridge (the problem was a layer earlier); a catalogue that fills its remainder
with short episodes.

**That last revert was audited afterwards and its evidence does not hold.** The change fires
only under five typed records; the three fixtures the adverse rule fired on hold seven to
twelve, measured on all thirty seeded stores with `h2h/store_shape.py`, so the code under test
never ran there. Those four fixtures are therefore one build measured against itself in
different grids: −5, −3, 0, −4, pooled 200/216 against 188/216, p = 0.079. Six of the nine
`unsafe` cells that were read as the change tripling the retired value's reach are on `v4`,
where it cannot execute. **Twelve cells in 216 is what two grids differ by when the builds do
not differ at all**, and the published headline is a twelve-cell margin. The revert stands on
its own fixture — `v3`, 27/54 against 18/54, p = 0.118 — and not on the rule that fired.

## If you pick this up

1. **The bare-value condition is the open one**, and part of it was not the ceiling. Probing
   that fixture's store offline found three rules throwing the value away before any ceiling
   applied: `first_line` skipped a one-word line and took the harness's trailing instruction as
   the object of eight of twenty episodes; `ack_replacement` could not read `note` as the noun
   or enter a quoted value; and the rule that types a decision when the reply states the
   replacement shared an eight-character floor with the rule beside it, which `zstd`, `cbor`
   and `gzip` are all under. All three are fixed, and the store's shape moved on four of the
   five fixtures — typed 15 → 17 on `v3`, retired 44 → 46 on `v7`. **Cells are not measured**;
   the harness cannot resolve an effect this size (see the drift figure above) and the change
   is justified as capture reading what the person actually typed.

   What is left there is the ceiling and it is sharper than before: for the compression pair
   the delivery serves `gzip` and not `zstd`, because `gzip`'s turn contains the question's
   words and `zstd`'s does not. No lexical rule links a record whose whole text is one word to
   a question that does not contain it.
2. **`--share-seed` exists and no grid has used it.** Each arm seeding its own store is the
   remaining structural source of the drift above; pairing on the task was tried on all six
   grids that hold both arms and recovers nothing (McNemar and Fisher agree to within 0.05),
   so the analysis side is closed and the store side is not.
3. **Gate 5b still needs size and its own registration.**
4. `PREREGISTRATION.md` is the file that matters: every arm, its rule written before its data,
   every result including the four changes reverted and the one registered rule this project
   refused in writing.
