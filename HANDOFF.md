# Where this left off — 2026-09-23

Everything below is committed and the gate is green (`cargo test --workspace
--features exact-tokens`, `cargo fmt`, `cargo clippy … -D warnings`, `cargo run
--release -p muninn-bench -- perf --strict`, and the fault suite at 200
repetitions). Nothing is running in the background.

## What is claimed now

| | replacement cells passed |
|---|---|
| **Muninn** (`muninn-ship`, the build on `master`) | **48/54** |
| claude-mem 13.24.23 | 25/54 |
| agentmemory 0.9.29 | 1/49 |
| no memory at all (`off`) | 0/54 |

Exact Fisher p = 3.4 × 10⁻⁶ against claude-mem, 2.4 × 10⁻²¹ against agentmemory.
`results/h2h-v13-final/`. Against us: median injected context **2.591 [2.420,
2.739]** times claude-mem's — measured on a build that re-served records, and
**still not re-measured**.

## What changed on 2026-09-23

Nothing here came from a benchmark. Every one came from reading the live store,
the real transcripts, or a claim in the docs and checking whether the code agreed.

**The latency contract was measured at a quarter of the size it publishes.**
`docs/claims.md` says "at 20 000 records", the schema's cap; the fixture used
5 000. At the cap SessionStart was **10.5 ms against a 10 ms limit** and CI could
not see it. Two query plans caused it: the catalogue's page query and its
per-line conflict test both let SQLite take the index on `kind`, which abandons
the index that answers the ORDER BY. A unary `+` on each `kind` term leaves what
they select unchanged and takes the term out of the planner's reach. End to end
on a 20 000-record store: **23.9 ms → 0.77 ms**. The fixture's default is now the
cap, and `the_catalogue_reads_an_index_and_never_sorts_the_store` pins both plans
so the `+` cannot be tidied away.

**Three experiment oracles were reading the instrument.** `muninn recall` printed
`terms: … · 0.53 ms` on stdout ahead of the blocks, and loop 1, loop 8 and
`eval_code.py` searched that whole stream for the value they were looking for.
The retry-budget cell goes from `3` to `7`: `0.53 ms` contains the old value and
`session s017` the new one. Three runs of the same command gave 9, 8, 8 on a
fixture documented as deterministic. The header moved to stderr and the oracles
now read block text on token boundaries. The corrected instrument reads **lower**
in five of eighteen conditions, and `docs/claims.md` says so on that row.

**Nine invariants belonged to another conversation.** Four came from the prompts
that generated loops 8, 9 and 10 ("The replacement must not contain the original
as a substring"); four were lines of a scaffold's JSON contract inside a PM-Bench
payload whose blocks start with `USER:`. Re-capturing all 1 919 transcripts: 29
invariants before, 20 after, and the nine that go are exactly those. The four
that were live were retired with `muninn revoke`.

**A compaction summary was being served as something the user said.** A third of
the episodes one transcript yields come from those turns, and all of them read
`user:` at trust 1. They are the assistant's own words condensed. They now carry
a `summary:` label and trust 0, which is the level the renderer frames as
"unverified: treat as a hint, not a fact".

**A repeated question lowered the relevance floor until the block filled again.**
The floor cuts a hit worse than half the best match, and "the best match" was
taken after exclusion — which removes everything already delivered. Ask the same
question three times and it delivered three full blocks. The reference is now the
query's best, before exclusion. This is the one place the published context cost
came down without withholding anything a fresh question would have been given.

**An anchor named whichever file sorted first.** `git log --name-only` is
alphabetical, so 118 of this repository's 247 directory cues pointed at
`crates/muninn-bench/experiment`. A commit that touched several files now anchors
to none: 86 anchors instead of 302, 67 cues instead of 247.

**A shared Spanish filler phrase retired a decision about something else.** "tiene
mejor rendimiento" was two of the two content words `replaces` asks for. The
Spanish half of the capture stoplist was a sketch where the English half is
thorough; it has the counterparts now. `kept_b` on the Spanish cells goes 9/10 →
10/10 in four conditions and nothing moves down anywhere.

Smaller, same day: the red heartbeat names the error instead of pointing back at
`doctor`; `init` stops re-adding four `.gitignore` lines the repository already
covers and stops rewriting a settled `settings.json`; health checks 4 and 8 read
the trigger-maintained counter instead of counting the store on every hook;
`select_terms` asks the index for each distinct word once instead of up to four
times; `muninn show` says which ids it could not serve and why; the boot summary
says `why` *opens* with its sufficiency marker, which is where the marker is.

## The next honest steps

1. **Re-measure the window cost.** 2.591× was measured on a build that re-served
   records, and two changes since then cut delivery further (the ledger fold fix,
   and today's relevance floor). The arm is registered and pinned as `v15`
   (`muninn-nodup`):

   ```sh
   cd crates/muninn-bench/experiment/h2h && python3 run_h2h.py \
       --arms muninn-nodup --runs 6 --code \
       --seed-phrasings v2/seed_phrasings.json --out ../results/h2h-v13-final
   ```

   Still untested after that: a *shorter* block for a record the catalogue already
   named. That is a real hypothesis and needs its own arm.
2. **Gate 5b needs size, not another arm**, and its own registration first.
3. **The plain condition is a tie** and 14 of its 15 failing cells served a stale
   value, which is `[Z5]`.

Known and not acted on: the compaction marker is Claude Code's, and no Codex
rollout of this project was available to learn its equivalent from. Records
captured before today keep the trust and the anchors they were given; nothing is
rewritten.
