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
  literal delivery 19/25 vs 2/25 without, length-matched control 4/24 (+0.68
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
- **Not done, on purpose.** See `docs/scope.md`. Codex replication of the gates is
  parked (hooks did not fire under `codex exec` 0.154.0). PM-Bench is measured in
  `GATE4.md` (below the paper's 65.1 % line on claude-sonnet-5 in round 1).
