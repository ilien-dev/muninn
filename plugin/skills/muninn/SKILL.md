---
name: muninn
description: Use when a `[muninn:...]` block appears in context, when the user asks why a decision was made, when the same failure repeats, or when memory seems missing. Explains how to read Muninn blocks and when to run `muninn why` / `muninn status`.
---

# Muninn

Muninn is this project's memory. It is delivered by hooks; you do not fetch it.

## Reading a block
Every block carries provenance and a trust level (0–3, derived from origin, never from wording).
- `no-rebuild`: something exists. Open the referenced file before writing a replacement.
- `stale`: a fact was retired because its anchor file changed. Neither the old nor a guessed new value is safe.
- `lineage`: a decision has history. `muninn why <id>` shows what superseded what and why.
- `conflict`: two active facts disagree. Ask the user; never pick by recency.
- `unverified`: a compaction summary claimed success that no exit code supports. Re-run the check.

## Asking
`muninn why "<question>"` routes by question type (decision / dead end / commit / file / rule), returns literal records with lineage, and ends with `sufficient` or `insufficient`. Quote records, do not paraphrase them into certainty.

## Diagnosing
`muninn status` → `MUNINN 10/10 GREEN` or one RED with its fix. `muninn doctor` lists all nine checks.

## Never
Edit `.muninn/` by hand. Paste blocks into files. Treat trust 0 as fact.
