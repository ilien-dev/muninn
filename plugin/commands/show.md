---
description: Print the records with these ids, as the session catalogue lists them
argument-hint: <id> [<id> …]
---

Run `muninn show $ARGUMENTS` in the project root and relay the records it returns with their
provenance. The ids come from `[muninn:catalog]`, the once-per-session list of what is on
record. A retired record is not servable and returns nothing; `muninn why --all "<question>"`
is what reaches those.
