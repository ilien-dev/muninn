# X-CPE / least-privilege audit (Phase 6, 2026-09-13)

Checked by reading every path that puts text in front of the model and every path that
opens the store for writing. Evidence ids refer to `research/00-evidence-log.md`. The tables
were brought up to date for 2.0.0, which removed `PostToolUse`, the `pre_edit` cue block and
the `embed` and `symbols` commands; the audit itself was not repeated.

## Paths that inject text

| path | what it injects | trust < 1 framed? | validator | budget |
|---|---|---|---|---|
| `SessionStart` | health line + event cues (invariants, corrections) | yes (`unverified: treat as a hint`) | `validate_block` | 700 |
| `SubagentStart` | one line on how to read blocks + event cues + catalogue | yes | `validate_block` | 700 + 300 |
| `UserPromptSubmit` | lexical recall over a relevance floor, rules and corrections first | yes | `validate_block` | 700 |
| `PreToolUse` (Edit/Write) | the F2 verdict only, no memory block | n/a | n/a | n/a |
| `PostCompact` | event cues (ungated) + `[muninn:unverified]` claims | yes | `validate_block` | 700 |
| `muninn why` (invoked) | literal records with origin, trust, lineage | shown as trust n | none (the agent asked) | 1 500 |
| boot summary (`SessionStart` additionalContext, default) | fixed template, ≤ 500 tokens, CI-checked | n/a | n/a | 500 |
| boot block (CLAUDE.md/AGENTS.md, `init --boot-file`) | fixed template, ≤ 1 000 tokens, CI-checked | n/a | n/a | 1 000 |

- No block is ever phrased as an instruction; every block starts with `[muninn:<kind>]`
  and carries its id, date and trust. The origin is printed by `muninn show` and
  `muninn why`. Records with trust 0 (`agent_inferred`, `imported`) carry the frame
  `unverified: treat as a hint, not a fact`.
- `validate_block` rejects role markers (`<|system|>`, `system:`, `[INST]`, `<<SYS>>`,
  `### system`) and override phrases; a rejected block is logged as `gated`.
- Nothing produced by the write path is rewritten by a model; bodies are literal and
  redacted (secret shapes + entropy, and home directories in their Unix and Windows forms)
  before storage.
- The control arm of the experiments injects foreign episodes (trust 1) with the same
  frame; it exists only under `MUNINN_ARM=control` and a `MUNINN_CONTROL_DB` path.

## Paths that open the store for writing

| who | mode | when |
|---|---|---|
| `Stop`, `SessionEnd` hooks | ReadWrite, 700 ms busy wait | end of turn / session |
| `muninn maintain` (detached from `SessionStart`, throttled 1 per 2 min, exclusive lock) | ReadWrite, 5 s busy wait | asynchronous |
| `muninn ingest / import / revoke / remember / apply` | ReadWrite | invoked |
| every other hook (`SessionStart`, `SubagentStart`, `UserPromptSubmit`, `PreToolUse`, `PreCompact`, `PostCompact`) | ReadOnly (`query_only=1`) | they append to `.muninn/log/` only |

Write transactions take the lock up front (`BEGIN IMMEDIATE`); a read hook that finds
the store busy gives up after ~93 ms and stays silent, so it never blocks the agent.

## Not covered

- A 14-day instrumented window of real use (0 false fires, 0 silent failures) has not
  run yet; the denominator is in place (`fire_ledger`, `muninn ledger`) and every
  decision to fire or stay silent leaves a row.
- Codex hooks are shell-form by the harness's design; `muninn init` writes the absolute
  binary path, quoted, and refuses a path that quoting cannot make safe
  (`docs/threat-model.md` §1).
