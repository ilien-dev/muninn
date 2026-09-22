# Loop 11 — what the `--code` condition puts in the block

Pre-registered in `../../PREREGISTRATION.md` ("loop 11: what the code condition puts in the
block"). No model runs anywhere in this harness: `loop11/eval_crowding.py` rebuilds the head-
to-head v4 `--code` store from the frozen phrasings and the same swap commits, then asks each
of the nine replacement tasks its own cell prompt through `muninn recall`.

## Why it exists

Five builds were measured on one grid of 27 live cells, at roughly five hours a grid, and four
of the five hypotheses were wrong. Counting what those cells were actually given said where to
look:

| condition | arm | records / cell | commit records / cell | of those, about the cell's own topic |
|---|---|---|---|---|
| nocode | muninn-loop8 | 4.0 | 0.2 | 0 of 6 |
| code | muninn-loop8 | 7.9 | 5.4 | 0 of 145 |
| code | muninn-now | 8.0 | 5.4 | 12 of 147 |
| code | muninn-kinds | 7.8 | 5.3 | 12 of 144 |
| code | muninn-why | 8.0 | 5.7 | 12 of 155 |
| code | muninn-quiet | 7.3 | 4.0 | 12 of 108 |

A hypothesis about what the block *contains* does not need an agent to test it.

## Results

| build | answered | retired | rank 1 | records / task | off-topic commit records / task | tokens / task |
|---|---|---|---|---|---|---|
| base (`07911e8`) | 9/9 | 0/9 | 9/9 | 6.3 | 4.4 | 409 |
| rarest-term anchor | 8/9 | 0/9 | 8/9 | 1.6 | 0.0 | 96 |
| relevance floor 0.5 | 9/9 | 0/9 | 9/9 | 2.2 | 0.6 | 139 |

The base block is two-thirds padding: a question about the compression codec selects the terms
`compression, value, decisions, decision`, and four records **about other decisions** match on
the last three alone, because every commit record Muninn writes reads
`config/decisions/<id>.json now reads "value": …`. They score −3.09 to −2.68 against the
answer's −6.39 and take four of six slots.

**The anchor was rejected.** Requiring the question's rarest word is standard practice and
takes off-topic to zero, and it loses an answer: the rarest word of *"…current recorded
decision on the cache eviction policy"* is `policy`, which in this store occurs in exactly one
place — the unrelated "https everywhere" pair. One accidental rare word vetoes the right
record.

**The floor was carried.** A hit scoring worse than half the first hit's bm25 is not served.
Where it does not help is where the answer is not clearly the best match: `revoke-license` has
no commit record of its own, its answer scores −3.31 against −3.09 for the intruders, and
everything is within half of everything.

## What this is not

A replica, not the grid: the assistant's acknowledgements are a fixed synthetic line rather
than a model's, so these counts are not the grid's counts. It measures one build against
another on an identical store. On this repository's own store the floor is **inert** — five
real questions return the same three records and the same token counts with it and without
it, because a 700-token budget already stops at three strong blocks. It bites only where many
records tie weakly, which is what capturing commits creates.

## Raw data

`results-base.json`, `results-anchor.json`, `results-floor50.json` — one row per task.
