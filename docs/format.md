# Portable format

SQLite (`.muninn/muninn.db`) is the operational truth. Everything a human or
another tool needs is projected to text so the store can be read, versioned,
diffed and imported without Muninn.

## Records as Markdown with frontmatter

`.muninn/records/<kind>/<id>.md`

```markdown
---
id: 142
kind: decision
subject: retry.policy
relation: is
object: exponential backoff, base 200ms, max 5 tries
origin: review_accepted
trust: 3
anchor_path: src/webhooks/retry.rs
anchor_hash: 4f3a…
session_id: 7c1e…
transcript_ref: ~/.claude/projects/…/7c1e….jsonl:48213
created_at: 2026-09-12T14:03:11Z
invalid: false
invalidated_by: ~
invalid_reason: ~
---
Review comment accepted on PR #88: "use exponential backoff here, the linear
one hammers the upstream during incidents". Applied in a1b2c3d.
```

The body is literal and is never rewritten, summarised or merged. An invalidated
record keeps its file, and only the frontmatter changes.

`kind` ∈ `invariant`, `decision`, `deadend`, `correction`, `claim`, `episode`.
`origin` ∈ `user_said`, `review_accepted`, `commit_linked`, `tool_observed`,
`agent_inferred`, `imported`. `trust` is derived: `user_said`/`review_accepted`
= 3, `commit_linked` = 2, `tool_observed` = 1, `agent_inferred`/`imported` = 0.

## `index.md`

`.muninn/index.md` lists active invariants and the most recent decisions, one
line each, ≤ 200 lines and ≤ 25 KB. It is regenerated on every ingest and is
what a human opens first. It is *not* injected into the agent.

## JSONL export / import

`muninn export` writes one JSON object per record, same fields as the
frontmatter plus `body`, to `.muninn/export-<timestamp>.jsonl`.
`muninn import <file>` accepts that JSONL, or a directory of Markdown files
with the frontmatter above (including the harness's native topic files, whose
frontmatter is a subset). Imported records get `origin: imported`, trust 0,
unless the file carries its own provenance fields.

The rest is derived or operational and is not projected: cues are rebuilt from
records, rules are recompiled from their source files, and the fire ledger is
telemetry. `muninn export --all` includes them for reproducibility kits.

Fields follow the vocabulary of the W3C Community Group draft on agent memory
interchange where one exists (`subject`/`relation`/`object`, provenance,
validity) [Q4]. When that draft stabilises, `muninn export --w3c` will emit it,
and the internal schema does not change.
