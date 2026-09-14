# Changelog

## 0.1.0 — unreleased (MVP, measured)

Everything below carries a measurement; the reports live under
`crates/muninn-bench/experiment/` and `crates/muninn-bench/corpora/claude-md/`.

- **F2 — rule compiler.** CLAUDE.md / AGENTS.md / `.claude/rules` rules → `deny`/`ask`
  permission rules and PreToolUse conditions, applied only after a diff. Gate 1:
  precision 0.905, recall 0.864 on a clean 100-rule hold-out; corpus of 330 public files,
  92.9 % of rules interpretive-only.
- **Literal capture.** Transcripts (Claude Code JSONL, Codex rollouts) → literal
  episodes (long turns chunked), corrections and invariants (user_said, trust 3),
  commit-linked decisions (trust 2), dead ends (tool_observed, trust 1); user steering
  inside tool results captured; secrets redacted by shape and entropy. Gate 2: with
  literal delivery 19/25 vs 2/25 without, length-matched control 4/25 (+0.68
  [+0.56, +0.80]).
- **F1 — filter.** Supersession, anchor validation, reverts, explicit revoke; retired
  records never served; conflicts served as conflicts. Gate 3, two model families:
  filtered vs render-matched unfiltered +0.22 [+0.11, +0.33] and +0.19 [+0.07, +0.30];
  retired records served 0/180 cells; retired value written 0 % vs 7–22 %.
- **`muninn why`.** Routed, invoked, literal records with provenance, lineage, conflicts
  and a sufficiency marker; lexical + exact-kNN sidecar fused by RRF; 55 ms.
- **Embedding sidecar.** potion-base-8M, checksummed, write path only; 52 ms load,
  8.5 ms per 200 texts, kNN bit-identical 1 000/1 000.
- **Symbol graph.** tree-sitter grammars for Rust, TypeScript/TSX, JavaScript/JSX,
  Python, Go compiled in; 5 000 files in 6.8 s, unchanged re-scan 40 ms; 12.5 MB binary.
- **F3 — cues and compaction.** Cues derived on the write path, indexed evaluation
  (p99 1.5 ms at 15 000 cues), tool-time delivery, event reinjection at session start
  and after compaction (decay probe 100/100), compaction-summary claims checked against
  exit codes. Gate 4 §1: dir/symbol cues vs lexical-only +0.08 [−0.08, +0.25] — not
  distinguishable, shipped off by default (`muninn config cues on`).
- **Reliability.** Read hooks open the store `query_only` and never block past ~93 ms;
  the write path waits (`BEGIN IMMEDIATE`, 700 ms in hooks, 5 s in `maintain`);
  15 fault-injection scenarios × 200 in CI; hook p95 at 20 000 records: SessionStart
  2.1 ms, UserPromptSubmit gated 0.86 ms / full 3.5 ms.
- **Hardening.** Exec-form hooks, checksummed install with Sigstore bundle verification,
  `muninn scan-config` for unpinned MCP servers, over-broad Bash allow rules and skills
  that pre-approve a shell; block validator against role/instruction injection;
  boot block ≤ 1 000 tokens checked in CI.
- **Post-gate improvements (measured).** Every delivered block carries an `evidence:`
  line (transcript and offset); one block per turn; newly embedded records within
  cosine 0.95 of an older active record of the same kind are retired as variants
  (restated summaries measure 0.968, distinct-but-similar turns 0.69–0.89);
  corrections only count a leading "no"; `muninn init` adds allow rules for
  `muninn why`/`muninn status`; query expansion through the symbol graph, +0.125
  [+0.000, +0.292] over plain lexical on the 72-cell cue grid, shipped opt-in
  (`muninn config expand on`); PM-Bench with the fired items next to the step
  measured worse (57.0 % vs 60.6 %) and was not kept. Dogfooding starts in this
  repository (plugin from the local marketplace; the manifest no longer lists the
  standard hook/skill/command directories, which Claude Code loads by itself).
- **Boot summary by hook.** `muninn init` no longer writes into CLAUDE.md / AGENTS.md;
  the SessionStart hook injects a ~425-token summary (capped at 500, CI-checked) and
  the skill keeps the long form. Measured against the file vehicle on 84 paired cells:
  +0.119 [+0.000, +0.262], not inferior. `muninn init --boot-file` restores the file.
- **Log folds without loss.** Heartbeat and delivery logs are folded from a watermark
  kept in the store, inside the write transaction, instead of renaming the live file:
  concurrent SessionEnd hooks lost heartbeats (fault scenario 9 failed in each of three
  200-repetition runs, one of them without the Codex changes; passes now), and a log over the
  64 MB read bound lost its remainder. Files rotate at 4 MB. Per-session delivery
  de-duplication still reads only unfolded lines, the window the gates measured.
- **Codex 0.154 transcripts.** codex-cli 0.154 writes turns as typed items and wraps
  tool calls in code-mode scripts; capture read 0 turns from its rollouts. The parser
  now reads the items (prompts, replies, commands with exit codes, file changes): the
  five rollouts of the probe yield 7 turns.
- **Codex tool-time hooks.** Codex edits files through `apply_patch`; PreToolUse and
  PostToolUse now match it and read every file in the patch, so compiled deny rules,
  pre-edit cues and the turn context cover Codex edits. Confinement denies a relative
  `../` path to a new file. `muninn init --codex` writes the new matchers.
- **Instrument integrity (experiments).** The `off` arm has no store, hooks or `MUNINN_*`
  variables; cells live outside the repository tree; in a cell the PreToolUse hook denies and
  counts store reads and escapes (`deny:store-access`, `deny:escape`). Found by the first
  Codex Gate 3 grid, where a no-memory agent located the store from the engine's source
  (kept as `results/gate3-codex-v1-leaky/`). With the fix: Gate 3 on Codex / gpt-5.6-sol,
  literal 27/27, unfiltered 0/27 (18 retired values written), off 0/27, 0 denials in 90 cells.
- **Compaction reinjection says what it cut.** `decay_probe.py` (committed, one command) gives
  100/100 at 10 invariants; at 20+ the 700-token budget delivers 16, and PostCompact now ends
  with `[muninn:gated] N more … did not fit`.
- **Reproducibility surfaces.** `experiment/REPRODUCE.md` (one command per number),
  `docs/claims.md` (claimed / pending / not claimed), OpenTimestamps proofs of every
  pre-registration commit under `experiment/prereg-stamps/`, per-run manifests and bridge
  canaries for PM-Bench round 8.
- **Not done, on purpose.** See `docs/scope.md`. Codex replication of the gates is
  not measured yet: the harness now runs (hooks fire under `codex exec` 0.154.0 with
  the project `hooks.json`; one smoke cell, `PREREGISTRATION.md`), no grid has. PM-Bench is measured in
  `GATE4.md`: rounds 1–3 below the paper's 65.1 % line (bridge later found to leak the
  user's global CLAUDE.md); rounds 4–5 with Muninn as the typed intention store and an
  isolated bridge reach 96.3–96.7 % set F1 on claude-sonnet-5 (rounds 5 and 7, 3 runs each) against 79.8 % / 77.9 %
  for the paper's own scaffolds on the same model.
