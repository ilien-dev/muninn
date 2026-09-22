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



## What the floor costs, measured after the registration

Not part of the registered decision rule — run after it, on a condition it did not name, and
reported because it goes against the change. The same replica **without** the commits, which
is the condition the live grid scores 18/27 on:

| build | answered | retired | records / task | tokens / task |
|---|---|---|---|---|
| base (`07911e8`) | 5/9 | 5/9 | 1.8 | 109 |
| relevance floor 0.5 | **4/9** | 5/9 | 1.3 | 80 |

**The floor loses an answer here.** On `revoke-internal-http` the base block holds three
records and the one stating "https everywhere" is not the first; the floor cuts it for scoring
under half of the first. That is the floor's shape: it protects the top of the block and will
drop a correct record that ranks below a better-matching one.

Read the absolute numbers with care — they are much worse than the live grid's plain condition
for a reason this harness cannot fix. In the grid the model's own acknowledgement restates the
change ("Got it — switching TLS backend from openssl to rustls"), and that sentence is a
record; here every acknowledgement is the fixed line `Noted.`, so the only text carrying the
new value is the user's message. The store is thinner than the grid's, which is why base
answers 5 of 9 rather than 9 of 9. The comparison between the two builds on that identical
store still holds.

## Raw data

`results-base.json`, `results-anchor.json`, `results-floor50.json` — the registered condition;
`results-base-nocommits.json`, `results-floor50-nocommits.json` — the check above.
