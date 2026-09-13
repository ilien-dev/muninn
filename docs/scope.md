# What Muninn does not do

Each row is a decision with its evidence id (`research/00-evidence-log.md`), not an
omission. The table mirrors `design/ENGINE.md` §12.

| Not in Muninn | Why |
|---|---|
| Consolidation, reflection, LLM summaries of memory | Consolidated memory scores below no memory [C1] [K10]; the products that do it name it as their weakest part [P3] [X4] |
| Vectors in the hook read path | 35–106 ms model load per process [I2]; dense+HNSW answers differ between runs on 80 % of queries [V4]. The sidecar exists, on the write path, for `muninn why` only |
| A knowledge graph | Eight months of an operational log never failed on expressiveness [G3] |
| Fine-grained bi-temporal ledger, "unprovable" states, evidence strength | With a render-matched control the fine mechanism is indistinguishable from zero; coarse invalidation is what pays [X1] |
| Ebbinghaus decay, learned importance | No measurement asks for it [P2] |
| Imported skills | −1.3 to −4.2 pp [J2] |
| More than two MCP tools | Zero voluntary memory operations in 114 turns [K1]; every tool is a toll [H2] |
| A proactive "why" | `muninn why` is invoked by the agent or the user, never injected [K3] |
| Deleting records | Retired records stay on disk, leave the index, and are never served; `muninn why --all` shows them |
| Reading the harness's native memory | Muninn is the only memory; `muninn init` turns the native one off for the project (`--keep-native` keeps it) |
| Team transport | Needs signed provenance and admission control [C5]; deferred |
| Review-comment capture | Needs an API; deferred [K14] |
| Other harnesses (OpenCode, Pi, Cline) | The MVP covers Claude Code and Codex; an in-process addon is a v1.1 decision |
| Tree-sitter grammars beyond Rust, TypeScript/TSX, JavaScript/JSX, Python, Go | Each grammar adds to the binary; others are added on measured demand — a crate, two `.scm` files, a fixture and a test |

What it does, with numbers, is in `README.md` and the gate reports under
`crates/muninn-bench/experiment/`.
