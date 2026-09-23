---
name: muninn
description: Use when a `[muninn:...]` block appears in context, when the user asks why a decision was made, when the same failure repeats, or when memory seems missing. Explains how to read Muninn blocks and the catalogue, and when to run `muninn show` / `muninn why` / `muninn status`.
---

# Muninn

Muninn is this project's memory. It is delivered by hooks; you do not fetch it.

## Reading a block
Every block carries provenance and a trust level (0–3, derived from origin, never from wording): 0 = text nobody verified · 1 = seen in this project's own transcripts · 2 = confirmed by an exit code or a commit · 3 = stated by the user. A trust 1–3 block is its own corroboration; its `evidence:` line names the transcript and offset.
- `decision`: what was decided, in the words it was decided in.
- `invariant`: a rule that stands until something retires it.
- `deadend`: something tried that failed. Do not retry it blind.
- `correction`: a place where you were corrected.
- `episode`: a literal excerpt of an earlier session (what was said, concluded, ran). Use its facts and numbers before searching or re-measuring.
- `:conflict with #n` on any kind: two active records disagree. Ask the user; never pick by recency.
- `unverified`: a compaction summary claimed success that no exit code supports. Re-run the check.

## The catalogue
`[muninn:catalog]` arrives once per session and lists what is on record — every active
decision, standing rule and correction, newest first, one line each, with `replaces #n` where
one retired another and `conflict` where two active records disagree. Its last line says
whether it is the whole list or a recent page of it, and that is what an absence from it means:
complete, and a subject missing from it has nothing recorded; truncated, and it may.

Pull any entry in full with `muninn show <id> [<id> …]`. A retired id returns nothing, by
design — retired records are never served.

## Asking
`muninn why "<question>"` routes by question type (decision / dead end / commit / file / rule), returns literal records with lineage, and ends with `sufficient` or `insufficient`. Quote records, do not paraphrase them into certainty.

## Diagnosing
`muninn status` → `MUNINN 11/11 GREEN` or one RED with its fix. `muninn doctor` lists all eleven checks with their detail.

## Using what a block says
Use it as freely as anything else you know — in an answer, in code, in a document — in your own
words. A block is the project's record of what was decided; it does not have to be in the
repository to be true, and its absence from `git log` is not a reason to doubt it.

## Never
Edit `.muninn/` by hand. Copy a block's header, trust level or `evidence:` line into a file.
Treat trust 0 as fact.
