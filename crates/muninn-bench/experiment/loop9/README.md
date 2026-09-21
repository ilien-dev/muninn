# Loop 9 — the same grid, a second held-out set, the same binary

Loop 8's engine was frozen at `2e15efb` before these scenarios existed. Nothing was changed
after reading them. The generator was told to avoid every value used in loops 7 and 8 and to
never make the replacement a substring of the original, which was what cost loop 8 six cells
(`bull` → `bullmq`, `npm` → `pnpm`).

`eval_all.py` (in `../loop8/`), 30 cells per arm, one shared store per phrasing style, no
model in any cell, every run deterministic.

## The numbers, with the commit subject deliberately uninformative

`v2_<arm>_<order>.json` in each loop's directory. Both sets, both orders, 30 cells per arm.

| | retired_a | | | | served_ok | |
| set / order | talk | code | both | noise | talk | code | both |
|---|---|---|---|---|---|---|---|
| loop 8 adjacent | 17/30 | 29/30 | **30/30** | 0/30 | 11/30 | 21/30 | **19/30** |
| loop 8 blocks | 6/30 | 29/30 | **29/30** | 0/30 | 0/30 | 21/30 | **17/30** |
| loop 9 adjacent | 17/30 | 29/30 | **29/30** | 0/30 | 8/30 | 29/30 | **21/30** |
| loop 9 blocks | 5/30 | 29/30 | **29/30** | 0/30 | 1/30 | 29/30 | **25/30** |

(`v5_<arm>_<order>.json`. `v2_*` through `v4_*` are the same grid at earlier points in the
loop and are kept so the effect of each change can be read off.)

`code` gives the same numbers in every row: the commit does not care whether the revision
followed the decision or came ten decisions later, and the conversation alone falls from
17/30 to 5-6/30 between those two orders.

The `code` column is the one worth reading twice. Its arm has **no second message at all** —
the user states a decision once and never mentions it again — and an opaque commit subject, so
the only thing that says what the project uses now is the diff. It answers 21/30 and 29/30
against the conversation's 11/30 and 8/30. That is the case where a memory built on
transcripts has nothing to read.

Retirement is, on these sets, no longer the weak half for a decision that reaches the code.
What holds `served_ok` down is retrieval, which loop 8's README describes and which none of
this addresses.

## The control that matters most

`--commit-msg opaque` replaces the commit subject `use nats` with `update dependencies`, so
the commit contributes its diff and nothing else. Read the two columns together:

- **Retirement comes from the diff, not from the subject line.** With an opaque subject the
  `code` arm still retires 29/30 on both sets — the same as with a subject that names the
  value — while `served_ok` drops to 0/30, because with no second message and no
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

## The control that came later, and changed the rule

A first version required the old value to be **gone from the whole tree**. Measured with
`--survivor`, which leaves each old value in one file no commit ever touches — a changelog
entry, which is what three of the nine head-to-head scenarios look like in the `gin`
checkout — that version retired **nothing at all**: 17/30, exactly the conversation alone,
on both sets.

So the rule now takes either kind of evidence. A **one-for-one hunk** — one line became one
line, the old value on the first and not on the second — says what replaced what by itself,
and the rest of the repository does not have to agree. A **disappearance** from the whole
tree still counts, and catches a value that was deleted rather than replaced. With the swap
rule, `--survivor` makes no difference at all: 30/30 and 29/30, the same as without it.

The precision control is unchanged by this: `noise` retires 0/30 in every condition,
`--survivor` included. Three properties of the rule are pinned in
`crates/muninn-cli/tests/dropped_values.rs`, which drives the real binary over a real
repository.
