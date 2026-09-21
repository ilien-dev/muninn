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
- Each release publishes one plugin bundle per platform with the binary built from
  that tag already in `plugin/bin/`, and stamps the tag's version into
  `plugin.json` and `marketplace.json`, so the plugin version names the binary it
  ships with. Every asset carries a published sha256 and a keyless Sigstore
  signature; `scripts/install.sh` verifies the checksum always and the signature
  whenever `cosign` is installed.
- No `${...}` other than `${CLAUDE_PLUGIN_ROOT}`, which Claude Code substitutes
  as a plain string in exec form.
- Codex hooks are shell-form by the harness's own design. `muninn init --codex`
  writes the absolute binary path **single-quoted**, so a space or a metacharacter
  in it is literal, and refuses outright a path carrying a single quote or a
  control character, which quoting cannot make safe
  (`init::shell_quote_binary`, tested in `init::tests`).

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
- The block validator (`muninn_core::cue::validate_block`) rejects any block
  containing role-like sequences (`<|system|>`, `system:`, `[INST]`, `<<SYS>>`,
  `### system`) or instruction-override patterns ("ignore previous
  instructions", "you must now", ...); a rejected block counts as `gated`.
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

## 6. The repository as an input to invalidation

**Threat.** Since the fourth invalidation trigger (ENGINE.md §5), `maintain` reads the diffs
of the commits it captures and retires a decision whose value a commit took out of the code.
Anyone who can land a commit can therefore retire a record: a one-line change that swaps a
value is enough, and a repository with untrusted contributors has a path to making the
assistant forget a decision it should have kept.

**What bounds it.** Retirement is not deletion and never was: the record stays on disk, leaves
the index, and `muninn why --all` shows it with `invalidated_by` pointing at the record the
commit corroborated. Nothing is created from a diff — a removed word only matters when an
active record already named it, and the replacement is always a record that already exists —
so a commit cannot *insert* a belief, only withdraw one. The write path runs outside every
hook, and no read hook executes git.

**What does not bound it.** There is no signature check and no notion of a trusted author:
`git log` is read as the repository presents it. A repository whose commits you do not control
is a repository whose memory you do not fully control either. That is the same trust boundary
as the code itself — an attacker who can land a commit can change what the code does, which is
strictly worse than changing what the memory serves — and it is stated here rather than
defended, because nothing in the MVP defends it.

## Out of scope for the MVP

- Team transport (shared stores across machines). Requires signed provenance
  and admission control [C5]. Not built.
- Protection against a malicious harness. If Claude Code or Codex is itself
  compromised, the hook contract is meaningless.
- Encryption at rest. The store lives inside the repository's working tree with
  the repository's own permissions; `.muninn/muninn.db*` is git-ignored.

## 4. Configuration defects in generated and hand-written config [2609.07360]

**Threat.** Agent configurations in the wild carry three recurring defects: MCP
servers launched without a pinned version (`npx pkg`, `pkg@latest`), over-broad
`Bash(x:*)` allow rules (`Bash(*)`, `Bash(sudo:*)`, `Bash(rm -rf:*)`, `Bash(curl:*)`),
and skills or commands whose `allowed-tools` pre-approve a shell. Muninn writes
permission rules itself (F2), so it must not add to that population.

**Defence.**
- F2 emits `deny` and `ask` rules only, never `allow`.
- `muninn scan-config` finds the three classes in `.claude/settings.json`,
  `.claude/settings.local.json`, `.mcp.json`, `.claude/commands/*.md`,
  `.claude/skills/*/SKILL.md` and in `.muninn/compiled/permissions.json`; `muninn apply`
  runs it before writing and prints the findings. Exit code 2 when anything is found.
