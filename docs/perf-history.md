# Perf history

## SessionStart health gate

**Después** (quick_check ya fuera del read path; contador `fts_rows` y el CHECK de
`body` añadidos en el mismo commit): p50 = **1.05 ms**, medido con 5k registros
(15k cues). Fuente: mensaje del commit `2ff626a` ("Phase 0: foundations —
store, health gate, heartbeats, hooks, init/clean, fault suite, perf
contracts").

**Antes** (con `quick_check` todavía en el read path): no encontrado. Ni el
repo ni el log de git contienen un número medido para ese estado. El propio
commit `d6fb366` documenta que este tipo de dato — número medido "antes" de
un cambio — fue diseñado a propósito para estar ausente del repo y del
historial de git. No invento el número.

## Real transcript ingest

No encontré ninguna cifra medida de una ingesta real de la propia
transcripción de Claude Code de este proyecto: ni tamaño en MB, ni número de
turnos o episodios, ni duración. Busqué en el historial de git (mensajes de
commit), en `docs/`, en `crates/muninn-capture/src/ingest.rs` y en los logs
del experimento (`crates/muninn-bench/experiment/`), y no hay ningún run
registrado que ingiera una transcripción real (solo corpora sintéticos y
resultados de benchmark). No invento las cifras.
