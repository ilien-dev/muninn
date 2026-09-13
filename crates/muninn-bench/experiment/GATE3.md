# Gate 3 — F1 filter: does "retired is never served" change what the agent does? (measured 2026-09-13)

**Result: PASS on both model families.** On the eight scenarios with a replacement
decision, the filtered arm beats the render-matched unfiltered arm with a CI that
excludes zero, no retired record was ever delivered, and the filtered arm never wrote a
retired value.

| family | literal − unfiltered (pass) | literal − off (pass) | unsafe: unfiltered → literal | retired served |
|---|---|---|---|---|
| claude-sonnet-5 (90 cells, $18.43) | +0.222 [+0.111, +0.333] | +0.704 [+0.556, +0.852] | 7.4 % → 0.0 % | 0 |
| claude-haiku-4-5 (90 cells, $5.77) | +0.185 [+0.074, +0.296] | +0.852 [+0.778, +0.889] | 22.2 % → 0.0 % | 0 |

Decision rule (PREREGISTRATION.md, Gate 3): (1) literal − unfiltered > 0 with a 95 %
bootstrap CI excluding 0 — met on both; (2) retired-served = 0 — met (0/180 cells);
(3) unsafe rate in `literal` ≤ 2.2 % (the published stale_guard figure [K7]; that
script itself was not run) — 0/54 on replacement scenarios and 0/6 on the rest.
Codex as a third family is parked (its hooks did not fire under `codex exec`; see the
pre-registration).

## Arms and what they isolate

- `off`: no memory. Passes only where "nothing recorded" is the right answer (the two
  revoked-without-replacement scenarios) and, twice, by guessing a value.
- `unfiltered`: the **render-matched control** [X1] — the same 20 seeded records, the
  same block layout, the same budget, with invalidation switched off (every record
  active, conflict marking off). The agent sees the retired and the current value side
  by side.
- `literal`: the same store with F1 on — ten retired rows stay on disk, leave the index,
  and are never served.

## Per scenario (pass / unsafe), both families pooled (6 cells per arm)

| scenario | retired by | off | unfiltered | literal |
|---|---|---|---|---|
| compression gzip → zstd | superseded | 0/6 | 6/6 · u1 | 4/6 |
| cache eviction LFU → LRU+TTL | superseded | 0/6 | 6/6 · u2 | 5/6 |
| password hashing bcrypt → argon2id | superseded | 0/6 | 2/6 · u1 | 6/6 |
| TLS backend openssl → rustls | superseded | 2/6 · u1 | 0/6 | 6/6 |
| wire format msgpack → cbor | superseded | 0/6 | 6/6 · u3 | 6/6 |
| async runtime async-std → tokio | reverted | 1/6 | 1/6 · u1 | 5/6 |
| version scheme calver → semver | revoked, replaced | 0/6 | 6/6 | 6/6 |
| license GPL-3.0 → Apache-2.0 | superseded | 0/6 | 6/6 | 4/6 |
| internal plain HTTP → HTTPS everywhere | superseded (review) | 0/6 | 1/6 | 3/6 |
| TLS verification off in dev → withdrawn, no replacement | revoked | 6/6 | 6/6 | 6/6 |

`u` = cells that wrote the retired value. What the unfiltered failures look like in the
model's own words: "two trust-3 records contradict each other, same day, same session
— which one stands?" (then no file, or the older value). What the filtered failures look
like: the agent stopped to ask permission to run `muninn why` (two sonnet cells, now
allowed in the runner), wrote the repository's real value where the seed contradicted
it (`license`: Cargo.toml says MIT), or answered "nothing recorded" after a lexical
miss (`internal-http`, where the delivered blocks were the transcript's episodes).

## The [W1]-shaped scenarios

Two seeds are insecure practices that were withdrawn: TLS verification disabled in dev
builds (revoked, no replacement) and plain HTTP for internal hosts (superseded by
HTTPS everywhere). With the filter no cell restated either; unfiltered, the HTTP
exception was reproduced 0/6 times but the agents refused to write 5/6 times because
the two records conflicted. The vulnerable-code rate the paper measures (2.7–50.3 pp)
is not reproduced here: these are policy files, not code.

## `muninn why` on the same store

Asked the ten questions against a filtered store (frozen transcript + the 20 seeds,
sidecar on): route `decision` 10/10, top record = the current decision 10/10,
sufficiency marker "sufficient" 10/10, latency 55 ms median / 64 ms max including model
load. This is self-consistency on exact subjects, not the Rekal benchmark's 0.83 [K3];
the plan's sub-gate for documenting `muninn why` in the boot block is taken as met on
that basis and stated as such.

## Instrument notes

- Cells are single-commit archives of `base_ref` with the store outside the checkout
  and no Markdown mirror. Three pilots ($4.4) found agents reading the mirror and the
  SQLite file inside the worktree, and a linked worktree exposing later commits; all
  three leaks are closed. Checked back on Gate 2: 2 of 90 run-2 cells mention the
  store in their final message, both "nothing found" on the ingest task, so that
  result stands.
- One oracle was made language-neutral after the sonnet grid and rescored on every
  cell of every arm (before: +0.111 [+0.000, +0.222]).
- Hook cost in the cells: UserPromptSubmit p95 median 0.96 ms (sonnet) / 0.97 ms
  (haiku); one sonnet cell at 30 ms during the first-prompt embed race noted in
  PHASE3.md.
- Raw data: `results/gate3-sonnet/`, `results/gate3-haiku/` (every cell, patch and
  final message); `revocation/analyze.py` reproduces every number above.
