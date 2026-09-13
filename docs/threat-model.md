# Threat model

Muninn injects text into a coding agent's context and writes files into the
repository. Both are attack surfaces. This document lists the classes we defend
against, what the defence is, and what stays out of scope. Evidence ids refer to
`research/00-evidence-log.md`.

## 1. Hook hijacking (HookPry) [X2]

**Threat.** A shell-form hook (`"command": "some string"`) is parsed by a shell.
Anything that can influence the string (a repository file, an environment
variable, an interpolated `${...}` placeholder) can run arbitrary commands. The
published attack succeeds 92.5 % of the time and endpoint defenders recalled 0 %.

**Defence.**
- Claude Code hooks are exec-form only: `command` plus an `args` array, no shell.
- The binary is pinned by version in `plugin.json` and verified by checksum on
  install (`scripts/install.sh`).
- No `${...}` other than `${CLAUDE_PLUGIN_ROOT}`, which Claude Code substitutes
  as a plain string in exec form.
- Codex hooks are shell-form by the harness's own design; `muninn init --codex`
  writes the absolute binary path and refuses paths with spaces or shell
  metacharacters.

## 2. Cross-context prompt injection through memory (X-CPE / M-CPE) [W2]

**Threat.** A memory that stores untrusted text (tool output, imported files,
agent inferences) and later injects it as context is a delayed prompt-injection
channel. The stored text may impersonate a system or user role.

**Defence.**
- Trust is derived from `origin`, never from wording [W3]. `agent_inferred` and
  `imported` records are trust 0 and are only delivered with an explicit
  provenance frame.
- Every delivered block is framed as evidence ("origin: …, trust n"), never as
  an instruction.
- The block validator (Phase 5) rejects any block containing role-like
  sequences (`system:`, `user:`, `assistant:`, `<|`, `Human:`) or imperative
  instruction patterns.
- Hard budget: ≤ 700 tokens per turn. A poisoned store cannot flood the context.

## 3. Insecure or stale memory driving unsafe actions [W1] [K7] [N4]

**Threat.** A remembered decision that was later revoked, or a remembered
pattern that is insecure, is served as if current.

**Defence.**
- Coarse invalidation: `invalid=1` rows are retained and never served (F1).
- Anchor hashes: a fact tied to a file is retired when the file changes; the
  new value is never guessed [K11].
- Conflicts are served as conflicts, never resolved by ranking [N4].
- Revoked-policy grid (Phase 4 gate) measures "revoked fact served" and must be 0.

## 4. Supply chain of the generated configuration [2609.07360]

**Threat.** Compiled permission rules or hooks that are over-broad
(`Bash(x:*)`), unpinned, or pre-approve shell.

**Defence.** `muninn apply` shows a diff, requires confirmation, writes only
inside `muninn:begin/end` markers, and a scanner (Phase 6) rejects the three
published defect classes before anything is applied.

## 5. Read path privilege

The read hooks (`SessionStart`, `UserPromptSubmit`, `PreCompact`,
`PostCompact`, `InstructionsLoaded`) open the store with `query_only=1` and
`SQLITE_OPEN_READ_ONLY`. They cannot write the database even if compromised.
Heartbeats go to an append-only log file. Only `Stop`, `SessionEnd` and
`PostToolUse` open the store read-write.

## Out of scope for the MVP

- Team transport (shared stores across machines). Requires signed provenance
  and admission control [C5]. Not built.
- Protection against a malicious harness. If Claude Code or Codex is itself
  compromised, the hook contract is meaningless.
- Encryption at rest. The store lives inside the repository's working tree with
  the repository's own permissions; `.muninn/muninn.db*` is git-ignored.
