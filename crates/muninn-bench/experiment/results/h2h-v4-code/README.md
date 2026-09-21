# Head-to-head v4 — the decisions are also in the code

Pre-registered in `../../PREREGISTRATION.md` ("head-to-head v4"). Same twenty live seeding
sessions per run, same held-out phrasings (`h2h/v2/seed_phrasings.json`), same nine
replacement tasks, same Gate 3 oracles as v1, v2 and v3. What `--code` adds, identically for
every arm: one tracked file per decision in the *seeding* checkout, and a commit that swaps
the value immediately after the message announcing the change, with the subject
`update dependencies` — never naming the new value. Task cells get the base checkout, as in
every other head-to-head: a cell whose repository held the current value would be answerable
by `grep`.

## The registered arms

| arm | replacement pass | retired value written |
|---|---|---|
| claude-mem 13.24.23 | **22/27** | 3/27 |
| muninn-loop8 (`01bf7797`) | **6/27** | 1/27 |

Exact Fisher p = 2.7 × 10⁻⁵. **Muninn lost, and not narrowly.**

## What the same cells say the number is

The decomposition was registered after 16 cells had run and before any further cell was read
for it (`h2h/delivered_vs_used.py`). "Delivered" counts what the hooks injected **and** what
the memory's own tools returned when the agent asked — claude-mem ships an MCP search server,
Muninn allows `muninn why`, and counting only the injection scored four cells where
claude-mem's agent searched its store as "passed without memory".

| arm | used | delivered, not used | passed without memory | not delivered | memory asks / cell | repository looks / cell |
|---|---|---|---|---|---|---|
| claude-mem | 19 | 5 | 3 | 0 | 1.5 | 2.1 |
| muninn-loop8 | 6 | 21 | 0 | 0 | 1.3 | 6.0 |

**Muninn put the current decision in front of the agent in 27 cells out of 27.** The agent
acted on it in 6. The gap is not retrieval, and no amount of improving retrieval moves it.
The last column is the tell: the agent with Muninn went looking in the checkout three times as
often, which is what you do when you do not believe what you were told.

## Three causes, all ours, found by reading those cells

1. **The boot summary forbade the task.** Muninn's own startup note said "Do not … paste
   blocks into files"; every task in this grid is to write the current decision into a file.
   It meant *do not copy a block's header and evidence line into your repository*, and it now
   says that.
2. **A hyphen hid a value from its own decision.** A record's words come from text with
   punctuation stripped (`async std`); the diff's words kept it (`async-std`). They could
   never match, so the commit that replaced async-std with tokio retired nothing and **both
   values were served**. The agent wrote that the decision had been revoked and no replacement
   was recorded — which, given what it was shown, was a reasonable reading.
3. **The `topic:` line restated the retired value.** `topic: backend openssl`, printed under
   "Let's use rustls instead", because the filter that was supposed to keep values out of that
   line only knows values spelled like products. The line is gone; the key still inherits
   every word and is indexed.

## The third arm

`muninn-now`, pre-registered before it was seeded, runs the same cells with all three fixed
and five measured engine changes besides. Its result is appended here when it finishes,
whatever it is, with the note that cause 1 was found while diagnosing this grid — whose task
is exactly the shape that sentence collided with.

## Raw data

`results.jsonl` (one row per cell), `diffs/` (what each cell wrote), `logs/` (each cell's
transcript and the memory's own report), `seeding/` (the twenty sessions per run, with each
arm's settle), `config.json` and `FROZEN.jsonl` (the frozen parameters, one line per launch).
