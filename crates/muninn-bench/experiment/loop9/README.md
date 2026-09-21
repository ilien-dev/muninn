# Loop 9 — the same grid, a second held-out set, the same binary

Loop 8's engine was frozen at `2e15efb` before these scenarios existed. Nothing was changed
after reading them. The generator was told to avoid every value used in loops 7 and 8 and to
never make the replacement a substring of the original, which was what cost loop 8 six cells
(`bull` → `bullmq`, `npm` → `pnpm`).

`eval_all.py` (in `../loop8/`), 30 cells per arm, one shared store per phrasing style, no
model in any cell, every run deterministic.

## served_ok — the new value is delivered and the old one is not

| set | order | talk | both, opaque commit | both, commit names the value |
|---|---|---|---|---|
| loop 8 | adjacent | 9/30 | 15/30 | 19/30 |
| loop 8 | blocks | 0/30 | 11/30 | 15/30 |
| loop 9 | adjacent | 8/30 | 16/30 | 30/30 |
| loop 9 | blocks | 1/30 | 15/30 | 30/30 |

## retired_a — the replaced decision is no longer served

| set | order | talk | code (no second message at all) |
|---|---|---|---|
| loop 8 | adjacent | 17/30 | 23/30 |
| loop 8 | blocks | 6/30 | 23/30 |
| loop 9 | adjacent | 17/30 | 29/30 |
| loop 9 | blocks | 5/30 | 29/30 |

## The control that matters most

`--commit-msg opaque` replaces the commit subject `use nats` with `update dependencies`, so
the commit contributes its diff and nothing else. Read the two columns together:

- **Retirement comes from the diff, not from the subject line.** With an opaque subject the
  `code` arm still retires 23/30 and 29/30 — the same numbers as with a subject that names
  the value — while `served_ok` drops to 0/30, because with no second message and no
  informative subject nothing in the store names the new value. The engine knows the old
  decision is dead and has nothing to put in its place, which is the correct state and is
  reported as a miss.
- **The 30/30 is a commit subject doing half the work.** Where the subject names the value,
  it answers the question by itself. Real subjects often do name it; often they do not. The
  honest number for the mechanism is the opaque column: **15-16/30 against 8-9/30 for the
  conversation alone, and 11-15/30 against 0-1/30 when the revision is not adjacent to the
  decision.**

## The precision control

`noise` — commits of the same shape that swap a value none of the decisions mention — retires
**0 of 30** in every condition measured: two sets, two orders, both commit-subject styles.
Eight runs, no false retirement.

## What did not change

`kept_b` is 28-30/30 in every arm, the same as the conversation alone: nothing still true is
lost by adding the second signal.
