# Where this left off — 2026-09-22

Everything below is committed. Nothing is in flight that matters: the only
background process at the end was a CI gate, which is re-runnable with the four
commands in `CLAUDE.md`.

## What is claimed now

| | replacement cells passed |
|---|---|
| **Muninn** (`muninn-ship`, the build on `master`) | **48/54** |
| claude-mem 13.24.23 | 25/54 |
| agentmemory 0.9.29 | 1/49 |
| no memory at all (`off`) | 0/54 |

Exact Fisher p = 3.4 × 10⁻⁶ against claude-mem, 2.4 × 10⁻²¹ against agentmemory.
Six runs, 54 cells an arm, size and threshold fixed before any cell ran, fixture
validated by its own `off` control. `results/h2h-v13-final/`.

Not claimed: the plain condition, where decisions never reach the code — 39/54
against 33/54, p = 0.31. Gate 5b on either model family.

Against us: median injected context **2.591 [2.420, 2.739]** times claude-mem's.
Two ways to cut it were measured and both cost answers (a 594-character boot
summary: −6 cells; the per-prompt block off: −4 cells but 1.84× context, shipped
as `muninn config prompt-delivery off`, default on).

## What changed today, in one line each

- `[muninn:catalog]` + `muninn show <id>` — the change that won the grid.
- Two sentences saying a moved file is not a retirement: 39/54 → 51/54.
- A deleted file retires nothing (it used to erase six decisions and their episodes).
- Four scale defects: the hooks were 7–16× over contract at 20 000 records.
- The perf fixture now runs at the schema's cap and contains event cues and retired rows.
- Eight capture guards, all found by reading this project's own store: the harness
  is not the user, what you paste is not what you decided, a denied change is not a
  change, a header is not a statement, `ahora` alone is not a marker, an
  abbreviation does not end a sentence, asking what you meant is not a correction,
  and a long message is not cut through a word.
- `muninn init` now allows `muninn show`; health says when an install is missing it.

## What is measured and closed

- Pairing a replacement with what it replaces by vector: 3 of 42 rank first
  (`loop12/`). Both halves of that route — sentences and names — are shut.
- `semantic_duplicate` removed: never called, and its threshold fires zero times
  on a real store.

## If you pick this up again

The next honest steps, in the order their evidence supports:

1. **The window cost is the open number.** Both obvious doors are measured and
   shut. Anything new has to come from somewhere that is not the instructions and
   not the per-prompt block, and it needs a grid.

   One thing measured and *not* a defect, so nobody spends the day on it again:
   **167 of the 224 records the per-prompt blocks serve were already named in the
   catalogue** (60 cells, `muninn-ship`). That looks like free savings and is not.
   The catalogue gives a line; the block gives the record with its provenance, and
   turning the blocks off was measured at four answers in fifty. What is untested
   is a *shorter* block for a record the catalogue already named — that is a real
   hypothesis and it needs its own arm.
2. **Gate 5b needs size, not another arm.** On haiku it reads 2/24 against 0/24,
   +0.083 [+0.000, +0.167]. A grid four times that size would exclude 0 at the
   same rate. It is deliberately not run: a gate whose size is chosen after seeing
   the effect is what `PREREGISTRATION.md` exists to prevent, so it needs its own
   registration first.
3. **The plain condition is a tie and its failure mode is known**: 14 of 15 failing
   cells served a stale value, which is `[Z5]`. The ack bridge is the only opening
   found and its reach is 27 % of replies when Muninn is the memory in the loop.

`PREREGISTRATION.md` is the file that matters — every arm, its rule written before
its data, and every result including the ones that went against the tool.
