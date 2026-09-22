# Changelog

## 1.0.0 — prepared, not yet tagged

- **The agent is shown what is on record, not only what a query returned.** Six builds
  delivered the current decision in 27 head-to-head cells out of 27 and the agent acted on it
  in 4 to 9, writing "no current recorded decision" with the decision in front of it; five of
  those builds changed what Muninn *tells* the agent and none of them moved it. Reading the
  competitor's own cells showed why: it delivers an index of every decision once per session
  and the agent asks for what it wants by id. `[muninn:catalog]` now does the same with no
  model — one line per active decision, standing rule and correction, newest first, with
  `replaces #n` read from `invalidated_by` and `conflict` where two active records disagree,
  under a 300-token budget whose last line says whether the list is complete. `muninn show
  <id> [<id> …]` pulls any entry in full and returns nothing for a retired id. On the shipped
  build: **51 of 54 against claude-mem 13.24.23's 25 and agentmemory 0.9.29's 1**, with a
  no-memory control at 0 of 54 (`results/h2h-v13-final/`).
- **A moved file is not a retirement, and saying so is worth twelve cells.** Two sentences in
  the startup summary took 39 of 54 to 51 of 54 (exact Fisher p = 0.0036) with nothing else
  changed. Eleven of the thirteen remaining failures were an agent reading a removed file as a
  revoked decision. This is the seventh attempt in this project to move a number by changing
  what Muninn says and the first that worked; the six before it are published too.
- **Deleting a file used to delete the memory of it.** Every `-` line of a diff fed the
  retirement rule, a deleted file's included, so one commit that moved a config directory
  retired the current decision *and its source episode* for six topics with no heir — active
  decisions 21 → 16 on the grid that found it. A value that disappears with its file is not
  evidence that the decision changed. Costs nothing: retirement 27/30 and false retirement
  0/30 with the guard and without it.
- **The hooks were 7 to 16 times over contract on a store at the schema's own cap**, and the
  perf gate could not see it: its fixture had no event cue and no retired record, so the path
  that was slow never ran. At 20 000 records SessionStart measured 128 ms against a 10 ms
  contract and the prompt hook 67 ms. Four causes, each measured: the delivery ledger wrote one
  line per gated record (4 200 a prompt, 14 MB in 32 prompts) and re-read them on the next;
  `cue::merge` formatted blocks after the budget was spent; `cue::evaluate` ran two queries per
  candidate; and the session-start cue loaded every invariant in the store to render sixteen.
  Now 8.9 ms and 4.4 ms, with the gate's fixture rebuilt to run at the cap and to contain what
  production contains.
- **The harness is not the user.** Claude Code injects `<task-notification>`,
  `<system-reminder>` and slash-command blocks inside `type: user` lines with no `isMeta` flag
  — 134 and 56 of them in this project's own transcripts — and they were captured as things the
  user said, at trust 3. With the sentinel lines it writes on the user's behalf, that was 40 %
  of the turns this project's own memory held. Both transcript parsers strip them now.
- **What you paste is not what you decided.** Fenced blocks and blockquotes no longer feed the
  typed extraction — one "decision of this project" was a line of claude-mem's output pasted
  into the chat. A denied change (`No cambié nada`) is no longer a change, a sentence ending in
  a colon is no longer a statement, `ahora` alone is no longer a Spanish change marker, and an
  abbreviation's full stop no longer ends a sentence (`CHECKOUT debe llamarse igual (p` was the
  whole of a record). Fourteen decisions on five real transcripts become nine, and every
  held-out figure is unchanged.
- **The assistant's own reply is read when it names both values.** "Got it, switching the TLS
  backend from openssl to rustls" identifies what a replacement replaced where no lexical test
  can, which is the `[Z5]` ceiling. Its reach is measured and small: 27 % of recorded change
  replies in sessions where Muninn was injecting the earlier decision, 10 % with claude-mem,
  and 0 of 45 with no memory in the loop — delivery of the stale record is what makes its
  retirement possible.
- **A block stops when the matches stop.** A hit scoring worse than half the first hit's bm25
  is not served, and a trust-3 record is never cut. It moved no cell and took the window cost
  from 2.14 to 1.84 times claude-mem's before the catalogue put it back up.
- **Measured and not kept:** citing the commit in a catalogue line (43/54 against 41/54,
  p = 0.82, reverted); a 594-character startup summary instead of 1 768 (35/54 against 41/54 —
  the registered rule would have adopted it on a non-significant test and was wrong to);
  turning the per-prompt block off, which reaches 1.84 times the window cost and loses about
  four answers in fifty, so it ships as `muninn config prompt-delivery off` and not as the
  default. Pairing a replacement with what it replaces by vector is now closed on 42 pairs as
  well as on 10: 3 ranked first, where chance is 1.

- **The memory reads the repository, not only the conversation.** Every lexical rule for
  noticing that a decision was replaced runs out in the same place: 23 of 30 held-out
  replacements share no content word with the message that replaces them, and neither
  embeddings nor a stemmer reaches them. `maintain` now reads the diffs of the commits it
  captures. A hunk where one line became one line, with a decision's value on the first and
  not on the second, says what replaced what; so does a value that a commit took out and that
  no tracked file holds any more. The record naming it is retired, what the line became is
  recorded with the commit behind it (`commit_linked`, trust 2, anchored to the file), and
  the question that reached the old answer reaches the new one. Nothing is invented from a
  diff: a removed word only counts if a record already named it, and a swap that retires
  nothing writes nothing. Measured on two held-out sets generated after the engine was frozen,
  with the commit subject deliberately uninformative — retirement 17/30 → 29-30/30, and 5-6/30
  → 29/30 when the revision is not adjacent to the decision; the current answer delivered
  8-11/30 → 19-25/30, and 21-29/30 where the conversation never names the new value at all.
  False retirement 0/30 in all eight conditions (`experiment/loop8/`, `loop9/`).
- **Three defects the loop-8 grid found before it could be trusted.** git parses `--since=@0`
  as *now*, not as the epoch, so a fresh store was told its repository had no history and
  captured nothing until its second `maintain` — every new install has been missing its own
  past. Topic inheritance restated the retired value on the `topic:` line whenever the value
  was lowercase, because the filter was `name_tokens`, which knows `PgBouncer` and not
  `sequelize` — an F1 leak worth six held-out cells. And a commit hash is part of an indexed
  record's text, so a grid that commits with wall-clock dates is not deterministic: one cell in
  thirty flipped between runs.
- **An accented query matched nothing.** `fts_term` dropped the accented letter instead of
  folding it, so `móvil` became `mvil` while the index holds `movil`; a non-Latin script was
  emptied outright. Measured on an accented query against an accented record: 0 blocks before,
  1 after.
- **What Muninn tells the assistant about itself was wrong in three places, and one of them
  argued against using the memory at all.** The injected startup note said "Do not … paste
  blocks into files" — it meant do not copy a block's header and evidence line, and an
  assistant asked to write a decision into a document read it as forbidding that. It listed
  six kinds of block, of which three (`no-rebuild`, `stale`, `lineage`) the engine never
  emits, while the four it does emit most (`decision`, `invariant`, `deadend`, `correction`)
  were missing; the fictional `stale` was described as "a fact was retired, assume neither old
  nor new", which is close to what several head-to-head cells came back saying. And the
  shipped skill claimed nine health checks where `muninn doctor` prints ten. All three
  documents now describe the engine that exists, and say the other half out loud: use what a
  block says as freely as anything else you know, in your own words.
- **`why --all` could not find the value it exists to show you.** A retired record leaves the
  full-text index, which is what F1 is for; `--all` is the door back in for a person, and it
  asked that same index which of the question's words were worth searching for. The word you
  would ask about is the one guaranteed to be missing from it, so `muninn why --all PgBouncer`
  returned three commits and no PgBouncer. Under `--all` the words are now taken as typed.
- **The migration that would have emptied every existing index.** Schema 2 rebuilds the
  full-text index once, and the guard that decided whether to rebuild asked
  `SELECT count(*) FROM record_fts` — which, on an external-content FTS5 table, is answered
  from the content table. It was never zero, so the rebuild never ran and every store
  upgrading from a v1 binary would have come up with no index at all. Found by writing the
  upgrade test, fixed, and checked end to end against a store written by the previous build.
- **A value that is a number is read from the diff too.** A number is invisible to a diff read
  for words — the unit is on both lines and the number is not a word — so `10 connections` →
  `25 connections` left nothing that went away. The hunk's two lines are now read as quantity
  slots, which is the rule the conversation already used: on a held-out set of ten measured
  values, retired 10/30 → 19/30 and delivered 9/30 → 18/30, with false retirement still 0/30.
- **A noun is not an announced change.** `\bmigrat` matched `migrations` as readily as
  `migrating`, and `\bcambi` matched `los cambios fueron…`, so any later sentence containing
  one of those nouns could retire a decision it shared two words with. On a held-out set built
  to contain that shape, **3 of 15** true decisions survived; now 12 of 15, with recall
  unchanged in every cell of loops 8 and 9. The gap was found by running on a real store —
  this project's own transcripts — where three of four sampled retirements were wrong.
- **A word the repository uses everywhere is not a value.** The same real store retired a
  record about `delivered` because a commit changed a line containing the word. A word living
  in more than three tracked files is no longer read as a value unless the record spells it
  like a name; every held-out figure is unchanged.
- **The index is stemmed, and a question whose every word is filtered out is no longer met
  with silence.** FTS5 `porter` (schema 2, with a one-off rebuild on migration) plus a
  fall-back that asks the index itself for a word the vocabulary does not hold, and a second
  pass over the words the stop list and the rarity test dropped when they take all of them.
  Ablation: porter and the fall-back are worth 1-4 and 4-5 held-out cells; a prefix back-off
  that scored well on the development set moved exactly nothing on either held-out set and was
  deleted.

- **Gate 5a: the compiled controls were measured end to end, and it took three held-out sets.**
  Gate 1 had scored whether a sentence *can* be enforced (precision 0.905) and stopped there.
  `muninn-bench enforce` builds a throwaway project for each hand-labelled tool call, runs
  `compile` → `apply --yes`, and feeds a harness-shaped payload to the real `hook PreToolUse`;
  every call is labelled from **the rule's own words**, so a control broader than its rule is a
  failure rather than a silence. Run 1 (set A, 84 cases): **FAIL** — the controls fired, and one
  benign call in eight fired with them, every case the same shape (a rule about `main` denying
  every force-push, one about `pkill -f zellij` denying every `kill`, one about `.env` denying
  `.env.example`). Run 2 (set B, 71 cases): **FAIL** the other way — false blocks down to
  **0/36** on a set the fixes were not fitted to, block rate four cases under the floor, one of
  them an engine bug the run found. Run 3 (set C, 53 cases over 28 rules from 22 files none of
  the others touched): **PASS, 0.920 block / 0.000 false block.** All three runs, their
  pre-registrations and their raw data are published (`corpora/claude-md/GATE5A.md`,
  `experiment/results/gate5a-holdout{1,2,3}/`).
- **Nine narrowings, so a control is no wider than the rule it came from.** A backticked
  invocation with its own arguments emits that invocation, not its command head; a rule marked
  as the root or whole-repo form emits an end-anchored control; a rule naming a remote scopes to
  it; a branch-scoped force-push reads the target from the command and the bare form from the
  checkout; `git config` matches the writing forms so `--get` still reads; `.env` no longer
  matches inside `.env.example`; an extensionless protected path covers its subtree; `git stash`
  reaches the control that already detected it; the full-suite control knows the runners the
  corpus names. A rule that carves out named paths now compiles to `interpretive_only`, because
  no control can subtract one path from another — coverage given up on purpose.
- **A `new_file` hook condition judged the wrong file.** It resolved a relative tool path
  against the process's working directory rather than the project root, so `Write README.md`
  was judged by whichever README the caller stood next to. Fixed, with a regression test.
- **The enforcement chain has tests.** `crates/muninn-cli/tests/enforce.rs`, 11 cases: compile
  alone enforces nothing, apply merges with foreign `settings.json` entries and revert removes
  only ours, `ask` degrades to a reminder on Codex, a branch-scoped rule stays on its branch, an
  unscoped one keeps its blanket control, every evaluation leaves a line including silence, a
  corrupt artefact never blocks the agent. From `emit` onwards nothing had been tested.
- **CI guards both F2 gates.** `muninn-bench rules --strict` fails below Gate 1's contract and
  `muninn-bench enforce` fails if the controls stop refusing what they should or start refusing
  what they should not. CI also now triggers on `master`, which it did not.
- **`docs/claims.md` has F2 rows**, which it had none of, and two more for the grids that are
  built and deliberately unrun: Gate 5b (does the control change what the agent does?) and a
  `native` arm that finally puts a number on "better than the harness's own memory". Both are
  pre-registered; neither is claimed.
- **Codex hook paths are quoted, and the threat model says what the code does.** `init --codex`
  interpolated `current_exe()` into a shell-form command unvalidated while
  `docs/threat-model.md` claimed it refused metacharacters. The path is now single-quoted, and
  one carrying a single quote or a control character is refused outright. The document's claim
  about the binary being pinned in `plugin.json` was also false and now describes the release
  bundles that make it true.
- **Install without a Rust toolchain.** Every release publishes a plugin bundle per platform
  with the binary already in `plugin/bin/`, stamps the tag's version into `plugin.json` and
  `marketplace.json`, and ships `install.sh` as an asset. `scripts/install.sh` pointed at a
  repository that does not exist, defaulted to 0.1.0, covered three of four published targets
  and filled one of the two places the binary is needed; all four are fixed, and the README
  leads with the no-Rust path.
- **Two cue defects.** `muninn init --cues` overwrote `.muninn/config.json` instead of merging
  into it. And `glob` and `cooldown` cues, which the schema accepts but no evaluator checks,
  counted as satisfied — so a group containing one fired on its other cues alone, having
  checked a condition nobody evaluated. They are refused on import and the conjunction fails
  closed.

## 0.2.0 — 2026-09-17

Built from the source measured as `muninn-latest` (binary `1356069a`) in the head-to-head; the
release binary differs from it only by the version string.

- **Replaced decisions detected from ordinary conversation.** Capture keeps each thing the user
  says as its own decision (origin `user_said`) and retires an earlier decision or short episode when
  a later message replaces or withdraws it: change and reconsideration cues ("switch to", "on second
  thought", "never mind", Spanish "mejor", "cambia"), a new value named in the same slot, labels
  compared without their shared words, and short follow-ups ("let's go with that instead") tied to the
  most recent statement. The replacing record inherits the topic words it omits. Built in five
  improvement loops, each frozen and then measured on phrasings written after the freeze
  (`experiment/loop1`–`loop5`): on the latest held-out set the earlier statement is retired in 21/30
  and recall serves only the current one in 12/20 English cases (0/30 and 0/20 before loop 1).
- **Sessions ended by a headless run are no longer lost.** Hooks note every transcript they see;
  the write path ingests any that the asynchronous Stop hook did not finish.
- **Validator.** An episode's own `assistant:` label is not treated as a role injection (short
  episodes were dropped before delivery).
- **Head-to-head without labels (v1, three runs, 27 replacement cells per arm).** 0.2.0: 27/27;
  claude-mem 26/27 (tie, p = 1); agentmemory 12/27; no memory 1/27. The seed wording of this grid
  was used during the loops, so the result is not held-out; v2, with new wording, is pre-registered
  and running.

## 0.1.0 — 2026-09-14 (MVP, measured)

Tagged at the commit that registered the head-to-head with this build (`973ef470`). In that
head-to-head it scored 12/27 against claude-mem's 26/27 (p = 4.6 × 10⁻⁵): it did not detect replaced
decisions in ordinary conversation.

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
  (`muninn config expand on`) — withdrawn after the five-run replication measured
  −0.125 [−0.275, −0.025] (`GATE4.md` §1 replications); PM-Bench with the fired items next to the step
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
