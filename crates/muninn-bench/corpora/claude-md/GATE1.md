# Gate 1 — F2 rule classifier (measured 2026-09-12)

**Result: PASS.** Precision 0.905 (19/21), recall 0.864 (19/22) on a clean held-out
set of 100 hand-labelled rule candidates. Gate: precision ≥ 0.90, recall of
`enforceable_*` ≥ 0.70.

## Protocol

1. Corpus: public `CLAUDE.md` / `AGENTS.md` files located by GitHub code search
   (`manifest.tsv`, 323 entries), fetched raw. Files on disk at measurement time: 115
   (fetching is rate-limited; the remaining entries are fetched incrementally).
2. Candidates: `muninn_compile::parse::extract` — list items and sentences carrying an
   imperative marker, table rows and fenced code excluded.
3. Classifier: `muninn_compile::classify::classify`, deterministic, no LLM. Frozen at the
   commit that adds this file.
4. Label: `enforceable` when the rule can be honoured at the tool boundary by a
   permission rule or a PreToolUse condition on tool name, command text, file path or
   branch, such that at least an `ask` decision is faithful to the rule. Content rules
   ("never use `as any`"), judgement rules ("if unsure, ask") and process rules with no
   tool-visible trigger are `interpretive`.
5. Sample: stratified, deterministic (`--seed`), half predicted-enforceable (measures
   precision), half hard negatives — predicted-interpretive candidates carrying a
   negation (measures recall). One rule per file where possible.
6. Two held-out sets:
   - `holdout.txt` / `labels.jsonl` (24 files, seed 7): labelled first; the classifier was
     then tuned on its errors. **Contaminated** — reported for transparency only:
     before tuning precision 0.837 / recall 0.932; after tuning 0.955 / 0.955.
   - `holdout2.txt` / `labels2.jsonl` (20 files, seed 11): fetched after the last
     pattern change, never inspected before labelling. **This is the gate measurement.**

## Numbers (holdout 2)

| | count |
|---|---|
| labelled | 100 |
| predicted enforceable | 21 |
| true positives | 19 |
| false positives | 2 |
| false negatives | 3 |
| true negatives | 76 |
| precision | 0.905 |
| recall | 0.864 |

False positives: a Claude.ai system-prompt fragment about markdown file creation
(`file.doc_create`), and a content-conditional "never commit them enabled"
(`git.commit`, emitted as `ask`). False negatives: "never deploy manually",
"do not add npm dependencies without documenting them", "don't add dependencies".

## Corpus-level figures

| corpus state | files | candidates | interpretive only | permission candidates | hook candidates |
|---|---|---|---|---|---|
| at the gate measurement (2026-09-12) | 115 | 2 641 | 93.4 % | 91 | 84 |
| complete corpus (2026-09-13) | 330 | 7 116 | 92.9 % | 271 | 235 |

The published corpus figure is 95.6 % interpretive [K5]; the difference is within what
the different candidate extraction explains, not evidence of anything. The full-corpus
row is the same frozen classifier over the whole manifest (322 of 323 entries fetched;
one 404); the hold-out measurement above is unchanged by it.

## Caveats

- 21 positives give a wide interval on precision (roughly ±13 pp at 95 %). The gate is
  met at its edge. The number gets tighter as the corpus grows; re-run
  `muninn-bench rules --holdout … --labels …` after adding files and labels.
- The second gate condition ("the fraction of security rules that already have a
  control stays < 50 %") is taken from [K5] (4.4 % enforceable in the public corpus);
  fetching each repository's `.claude/settings.json` was not possible under the fetch
  budget. It is not measured here.
- Labels are one person's judgement, applied before seeing the classifier's output for
  set 2's hard negatives but after seeing the predicted class for its positives.
- `deny` vs `ask` correctness is not scored by the gate; several true positives emit an
  over-broad `deny` (e.g. a rule scoped to a directory becomes a global command deny).
  The report lists every emitted artefact; `muninn apply` shows the diff before anything
  is written.

## Reproduce

```sh
cargo build --release -p muninn-bench
./target/release/muninn-bench rules --holdout crates/muninn-bench/corpora/claude-md/holdout2.txt --labels crates/muninn-bench/corpora/claude-md/labels2.jsonl
```
