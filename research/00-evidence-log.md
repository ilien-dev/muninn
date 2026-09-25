# Muninn — registro de evidencia (fuentes primarias)

Formato: [ID] Fuente | Dato duro | Por qué importa.

## A. El contexto es un recurso escaso (no una suposición)

[A1] Chroma Research, "Context Rot: How Increasing Input Tokens Impacts LLM Performance"
(Hong, Troynikov, Huber; jul-2025) — 18 modelos frontera (GPT-4.1, Claude 4, Gemini 2.5,
Qwen3), ~194k llamadas. Hallazgos textuales:
  - "Across all experiments, model performance consistently degrades with increasing input length."
  - "Lower similarity needle-question pairs increases the rate of performance degradation."
  - Distractores tienen impacto NO uniforme y crece con la longitud del input.
  - Posición del needle: sin variación notable en su tarea NIAH (11 posiciones).
  - NIAH clásico sobreestima: mide matching léxico, no recuperación semántica.
  URL: https://www.trychroma.com/research/context-rot
  => Consecuencia de diseño: menos tokens de mayor señal > más tokens. Un sistema de
     memoria que inyecta 2k tokens irrelevantes por turno degrada activamente al agente.

[A2] Anthropic Applied AI, "Effective context engineering for AI agents" (29-sep-2025)
  - "attention budget": cada token consume presupuesto; n^2 relaciones por pares.
  - Recomienda "just-in-time context": el agente mantiene identificadores ligeros
    (rutas, queries, links) y carga datos en runtime, en vez de pre-cargar.
  - Claude Code = híbrido: CLAUDE.md precargado + glob/grep just-in-time,
    "effectively bypassing the issues of stale indexing and complex syntax trees".
  - Tres técnicas para tareas largas: compaction, structured note-taking (agentic
    memory), sub-agentes.
  - Subagente: explora con decenas de miles de tokens, devuelve 1 000–2 000 tokens.
  URL: https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents

[A3] Anthropic, "Code execution with MCP" (4-nov-2025)
  - Cargar definiciones de tools por adelantado: caso medido 150 000 → 2 000 tokens
    (-98.7%) al exponer tools como ficheros en un filesystem y cargarlos on-demand.
  - Resultados intermedios pasan dos veces por el contexto en el patrón tool-call directo.
  - Progressive disclosure + `search_tools` con nivel de detalle.
  URL: https://www.anthropic.com/engineering/code-execution-with-mcp
  => Un MCP de memoria con 8-10 tools cobra peaje de tokens en CADA petición,
     antes de aportar nada.

## B. Los benchmarks de los vendors de memoria no son evidencia

[B1] Zep, "Is Mem0 Really SOTA in Agent Memory?" — LoCoMo: 10 conversaciones, 16-26k
tokens. Dato clave: "Mem0's own results show their system being outperformed by a
simple full-context baseline ... ~73% (full context) vs ~68% (Mem0)".
Errores documentados de LoCoMo: categoría 5 sin ground truth, atribución de hablante
incorrecta, preguntas ambiguas, errores multimodales (BLIP).
  URL: https://blog.getzep.com/lies-damn-lies-statistics-is-mem0-really-sota-in-agent-memory/

[B2] Mem0 CTO (Deshraj Yadav), getzep/zep-papers issue #5 — replica de Zep: 58.44%,
no 84%; acusa error aritmético (categoría 5 en numerador pero no en denominador) y
"prompt tampering". Zep corrige después a 75.14%.
  URL: https://github.com/getzep/zep-papers/issues/5
  => Tres cifras (84 / 58.44 / 75.14) para un mismo sistema en un mismo dataset.
     Ningún número de accuracy de vendor es comparable. No construir sobre ellos.

[B3] Zep, "The Retrieval Tradeoff: what 50 experiments taught us" — el único estudio
de vendor con metodología usable (50 corridas, gpt-4o-mini como agente y juez):
  | Config | Accuracy      | Context tokens | Retrieval p50 |
  | 5/2    | 69.62 ± 0.47  |   347          | 149 ms |
  | 10/2   | 73.72 ± 0.41  |   504          | 161 ms |
  | 15/5   | 77.06 ± 0.41  |   756          | 199 ms |
  | 20/20  | 80.06 ± 0.33  | 1 378          | 241 ms |
  | 30/30  | 80.32 ± 0.43  | 1 997          | 189 ms |
  - 5/2 → 20/20: +10.4 pp por 4x tokens. 20/20 → 30/30: +0.26 pp por 1.5x tokens.
  - "Completeness" (¿el contexto recuperado bastaba?) es mejor señal que accuracy:
    en 5/2, 23.8% de preguntas con contexto insuficiente y accuracy (70%) por encima
    de completeness (57%) => el agente estaba adivinando.
  - La latencia NO fue el cuello de botella (p50 < 250 ms siempre); el coste es tokens.
  URL: https://blog.getzep.com/the-retrieval-tradeoff-what-50-experiments-taught-us-about-context-engineering/

## C. La consolidación por LLM es el fallo central (evidencia negativa dura)

[C1] "Useful Memories Become Faulty When Continuously Updated by LLMs"
arXiv:2605.12978 (v2 ago-2026)
  - "memory utility first rises, then degrades, and can fall below the no-memory baseline".
  - Consolidando desde soluciones ground-truth, GPT-5.4 FALLA en 54% de problemas
    ARC-AGI que ya había resuelto sin memoria.
  - La regresión está en el paso de consolidación, no en la experiencia.
  - Control episódico puro (guardar trayectorias crudas) sigue siendo competitivo.
  - En ARC-AGI Stream con acciones Retain/Delete/Consolidate: los agentes conservan
    episodios crudos por defecto y DUPLICAN la accuracy de sus contrapartes forzadas
    a consolidar. Desactivar consolidación = igual que el régimen automático.
  - Recomendación de los autores: "treat raw episodes as first-class evidence and gate
    consolidation explicitly rather than firing it after every interaction".
  => Esto invalida el diseño por defecto de Mem0/OpenMemory/A-MEM (extract→consolidate
     en cada turno).

[C2] "Governance Decay: How Context Compaction Silently Erases Safety Constraints"
arXiv:2606.22528. Benchmark ConstraintRot, 1 323 episodios, 7 familias de modelos:
  - Violación de restricciones: 0% con la política en contexto completo → 30% tras
    compaction; hasta 59% en algunos modelos.
  - Si la restricción sobrevive al resumen: 0% de violación. Si se cae: 38%.
  - Ataque Compaction-Eviction: inyección que sesga al summarizer para omitir la política;
    funciona contra todos los modelos evaluados.
  - Mitigación "Constraint Pinning" (aislar invariantes de la compactación lossy):
    devuelve la violación a 0%.
  => Hueco REAL y medible en Claude Code / Codex hoy. Es una capacidad de memoria,
     no un lujo.

[C3] "Parallel Context Compaction for Long-Horizon LLM Agent Serving" arXiv:2605.23296
  - La compactación por summarization "blocking call stalls agent inference for tens of
    seconds"; el operador no controla el volumen del resumen (las instrucciones del prompt
    se ignoran en gran medida); lo retenido fluctúa mucho entre corridas.
  => La memoria del agente hoy es no determinista. Ese es el problema a resolver.

[C4] "Auditing Self-Evolution in Financial Agents" arXiv:2608.17684 (Qwen 3.7 Flash)
  - SkillOpt: utilidad 0.741 → 0.837, PERO exposición a contenido inyectado 0.820 → 0.943,
    ASR global 0.496 → 0.530, cambios de estado no autorizados 0.685.
  - ReasoningBank: utilidad → 0.859 SIN aumentar ASR agregado.
  => Ganar capacidad con memoria puede comprarse con superficie de ataque. ReasoningBank
     es el único de los tres que sale limpio.

[C5] Envenenamiento de memoria (superficie nueva, no teórica):
  - "From Untrusted Input to Trusted Memory" arXiv:2606.04329 + MPBench:
    "agents designed to write and retrieve memory more aggressively are more exploitable";
    las defensas de prompt injection existentes NO cubren memory poisoning.
  - ShadowMerge arXiv:2605.09033: 93.8% ASR medio contra memoria en grafo (evaluado
    sobre Mem0), +50.3 pp sobre el mejor baseline.
  - MINJA arXiv:2503.03704: inyección solo con queries, sin acceso al banco de memoria.
  - MemPoison arXiv:2605.29960: hasta 0.95 ASR sorteando extracción selectiva.

## D. Qué SÍ funciona (ganancias medidas, no marketing)

[D1] ACE — "Agentic Context Engineering" arXiv:2510.04618 (Stanford/SambaNova)
  - Nombra los dos fallos: "brevity bias" (resumir tira los detalles del dominio) y
    "context collapse" (la reescritura iterativa erosiona el detalle).
  - Solución: el contexto como *playbook* que crece por updates incrementales
    estructurados (delta), no por reescritura.
  - +10.6% en agentes, +8.6% en finanzas; menor latencia de adaptación y coste de rollout.
  - Funciona SIN supervisión etiquetada, con feedback de ejecución natural.

[D2] ReasoningBank arXiv:2509.25140 (Google Research)
  - Destila estrategias de razonamiento de experiencias exitosas Y fallidas
    (auto-juzgadas). Supera a memorias que guardan trayectorias crudas o solo rutinas
    exitosas, en web browsing y software engineering.
  - MaTTS: escalar experiencia en test-time mejora la calidad de la memoria.

[D3] Trace2Skill arXiv:2603.25158
  - Consolida trayectorias en un *directorio de skills* unificado. Supera a la edición
    secuencial de skills y a memorias de recuperación tipo ReasoningBank.
  - Las skills transfieren entre escalas y familias de modelos y a OOD:
    skills derivadas de Qwen3.5-35B mejoran a un agente Qwen3.5-122B hasta +57.65 pp
    en WikiTableQuestions.
  - Clave: "portable skills that can be reused without ... test-time retrieval".

[D4] CODESKILL arXiv:2605.25430 — skills procedurales desde trayectorias de coding agents.
  EnvBench + SWE-bench Verified + Terminal-Bench 2: +9.69 pass rate sobre no-skill y
  +4.01 sobre el mejor baseline de prompt/memoria, con skill bank de tamaño estable.

[D5] "Agentic Harness Engineering" arXiv:2604.25850
  - 10 iteraciones: Terminal-Bench 2 pass@1 69.7% → 77.0%, superando a Codex-CLI (71.9%)
    y a ACE. Transferencia sin re-evolución: SWE-bench Verified con 12% menos tokens.
  - Ablaciones: la ganancia se localiza en **tools, middleware y long-term memory**,
    NO en el system prompt. "factual harness structure transfers while prose-level
    strategy does not".
  => La memoria útil es estructura factual del harness, no prosa motivacional.

[D6] "Code Isn't Memory: A Structural Codebase Index Inside a Coding Agent"
arXiv:2606.22417 — Claude Opus 4.7 fijo, 3 semillas, sandbox con auditoría de fugas,
SWE-PolyBench Verified + SWE-bench Pro. Índice estructural vs mismo harness sin él vs
agentic-grep: ganancia grande en localización y ganancia de resolve estadísticamente
separada, sin penalización de coste por celda y **menor $/solved que agentic grep**.
  => El índice paga cuando hay cambios multi-fichero. Contradice el "grep siempre basta".

[D7] EncouRAGe arXiv:2511.04696 — 25k pares QA, 51k documentos, 4 datasets:
  - "RAG still underperforms compared to the Oracle Context"
  - "**Hybrid BM25 consistently achieves the best results across all four datasets**"
  - Reranking: mejora marginal, mayor latencia.
[D8] RAG biomédico arXiv:2505.07917 — 24M docs PubMed: BM25 top-50 + rerank MedCPT es el
  óptimo; **BM25 se mantiene en 82 ms estable**; el reranker es el coste dominante.

## E. Staleness: la similitud no distingue contradicción (dato estructural)

[E1] MemStrata — "Temporal Validity in Retrieval Memory" arXiv:2606.26511
  - Sobre un dataset calibrado, la similitud cosénica separa un hecho CONTRADICHO
    de uno DUPLICADO con **AUROC 0.59** (casi azar): una contradicción suele ser
    más similar en el espacio de embeddings que una reformulación.
  - RAG sirve valores superados **entre 15% y 40%** de las veces cuando se le
    obliga a responder.
  - Una regla determinista de supersesión (sujeto, relación, objeto) sobre un
    ledger bi-temporal lleva esa tasa a ~0%, **sin umbral de similitud y sin
    llamada a LLM**. 0.95-1.00 de accuracy en conocimiento evolutivo frente a
    0.20-0.47 de RAG. Latencia ~2.1 s frente a 16-18 s de baselines con reranking LLM.
  => Consecuencia: la invalidación NO puede delegarse al embedding. Necesita
     hechos tipados y una regla determinista.

[E2] InKH arXiv:2606.01886 (dominio financiero, benchmark sintético controlado,
  46 080 evaluaciones): inyección pasiva de contexto acotado + invalidación en
  tiempo de escritura reduce el uso de conocimiento obsoleto **96.58%** y la
  latencia 82.95% frente a memoria tipo "wiki-walk" conducida por el agente.
  => Inyectar > dejar que el agente navegue, cuando la latencia importa.

## F. La realidad de los harness hoy (línea base a superar)

[F1] Claude Code ya tiene DOS capas de memoria (docs oficiales):
  - CLAUDE.md / CLAUDE.local.md / .claude/rules/: escritas por el humano, cargadas
    en TODAS las sesiones. Objetivo recomendado: <200 líneas por fichero;
    "longer files consume more context and reduce adherence". Los imports @path
    "no reducen el contexto": se expanden al lanzar.
  - Auto memory (activa por defecto): `~/.claude/projects/<proyecto>/memory/`
    con `MEMORY.md` como índice + un fichero por tema. Se cargan **las primeras
    200 líneas o 25 KB** de MEMORY.md en cada conversación; los ficheros de tema
    NO se cargan al inicio, se leen on-demand. Campo `modified` en el frontmatter.
    Es local a la máquina; no se comparte entre máquinas ni con el equipo.
    No se hereda en subagentes (salvo fork).
  URL: https://docs.claude.com/en/docs/claude-code/memory
  => La recuperación es "Claude lee un índice plano y decide qué fichero abrir".
     No hay ranking, ni presupuesto por turno, ni supersesión, ni procedencia.

[F2] Codex (docs oficiales):
  - `features.memories` = **false, Experimental**. Almacén local en
    `~/.codex/memories/`. Usa `memories.extract_model` y
    `memories.consolidation_model`: es decir, extracción y **consolidación por LLM**
    — exactamente el paso que [C1] demuestra que degrada.
  - Advertencia de los propios docs: "Keep required team guidance in AGENTS.md...
    Treat memories as a helpful recall layer, not as the only source for rules
    that must always apply."
  - Hooks (estables): SessionStart (matcher `startup|resume|clear|compact`),
    UserPromptSubmit, PreToolUse, PostToolUse, PreCompact, PostCompact, Stop,
    SubagentStart/Stop, SessionEnd. Inyección vía `additionalContext` con
    `additionalContextLimit` (por defecto ~2 500 tokens; 0 = sin límite).
    Hooks en background con `async = true`, hasta 8 concurrentes.
  URLs: https://developers.openai.com/codex/customization/memories
        https://developers.openai.com/codex/hooks
  => El punto de integración portátil entre Claude Code y Codex son los HOOKS,
     no las tools MCP. Mismo ciclo de vida, misma forma de inyectar contexto.

[F3] AGENTS.md: formato abierto, >60k repos públicos, ahora bajo la Agentic AI
  Foundation (Linux Foundation). El repo principal de OpenAI tiene 88 AGENTS.md.
  URL: https://agents.md/
  => Cualquier cosa que se construya debe escribir/leer estos ficheros, no
     competir con ellos.

## G. Contraposiciones (lo que podría invalidar la tesis)

[G1] "Written by AI, Managed by AI: Semantic Space Control and Index Sickness
Elimination Across 391 Consecutive Sessions" arXiv:2606.19121
  - Acción-investigación sobre un proyecto real, ~1 mes, 391 sesiones.
  - Hallazgo CONTRARIO a la intuición de "añadir más scaffolding": al superar un
    umbral de complejidad simbólica, el LLM **abandona la comprensión semántica
    del negocio** y se repliega a razonamiento auto-referencial dentro de la capa
    simbólica ("Index Sickness", manifestación canónica "Phantom Legislation").
  - El arreglo redujo el volumen de instrucciones ~**75%** y en las ~150 sesiones
    siguientes no reapareció el fallo.
  => Más memoria persistente puede empeorar el resultado. El presupuesto de
     tokens y el silencio por defecto no son optimizaciones: son requisitos.

[G2] "Always-On Agents: A Survey of Persistent Memory, State, and Governance"
arXiv:2606.30306 — corpus codificado de 435 trabajos: "the literature concentrates
more heavily on accumulating and retrieving state than on governing, recovering,
or relinquishing it". Seis ejes por ítem de estado: autoridad, alcance,
mutabilidad, procedencia, recuperabilidad, accionabilidad.
  => El hueco del campo no es guardar y buscar. Es gobernar, recuperar y soltar.

[G3] "Memory as Infrastructure" arXiv:2609.05510 (Mike Helwig, ago-2026)
Registro operativo de 8 meses de una sola línea de sesión de Claude Code sobre
633 446 líneas de código. SIx Harness (MIT). Arquitectura casi idéntica a la que
esta investigación concluye: **FTS5 (BM25) + sqlite-vec en un solo fichero SQLite,
100% local**, inyección con compuertas de precisión, almacenes anti-recurrencia,
convenciones que sobreviven a la compactación, y capa de fiabilidad (health gate
de 11 checks, heartbeats, economía de alertas).
  Números: 78 933 invocaciones de hook; 85 fallos registrados, ninguno silencioso
  (84 en las 3 primeras semanas, 1 desde entonces, 0 en los últimos 20 días);
  ventana de precisión de 10 días: 68 prompts, 14 con intención, 3 inyecciones
  (todas relevantes), **0 falsos disparos en 54 prompts sin intención**;
  estado acumulado: 19 065 chunks, 389 ficheros de memoria, 154 decisiones y
  186 callejones sin salida capturados, de los que sobrevivieron 59 y 47 al triaje.
  Antes de las compuertas, el bloque de "stale-claim" disparaba en ~4 de cada 5
  prompts con intención: "topically adjacent often enough to be defensible and
  useful rarely enough to be skimmed".
  Latencia de recuperación cuando se ejecutó: **mediana 2.5 s, máximo 10.3 s**
  (embeddings contra un servidor Ollama local).
  Siete principios destilados; el 3 y el 6 son los operativos:
    "Precision beats recall for injected context... Silence must be the default;
     a retrieval layer the reader has learned to skim is worse than none."
    "Continuity must be an artifact, not a habit."
  Limitaciones que el propio autor declara: N=1, sin brazo de control, auto-reportado,
  8 meses que abarcan varias generaciones de modelo, etiquetas adjudicadas por los
  autores, y **no instrumentaron su propio coste** (minutos de triaje, tokens
  inyectados, atención de mantenimiento).
  => Validación arquitectónica fuerte + el hueco exacto que queda: su ruta de
     lectura es 250x más lenta de lo necesario, y nadie ha medido el coste neto.

## H. Mediciones propias (esta máquina, 2026-09-12)

Entorno: Linux 6.x, 32 núcleos, 61 GB RAM, SQLite 3.53.4, Node 22, Python 3.13.
Corpus: 144 487 registros extraídos de un dump de Simple Wikipedia
(60-160 palabras por registro, distribución de términos real). Scripts en
`research/bench/`, reproducibles.

[H1] Recuperación léxica: SQLite FTS5 + bm25() en proceso, consultas de 2-4 términos
  |       n | build  | tamaño   | p50       | p95       | p99       |
  |  1 000  | 0.02 s |   0.0 MB | 0.015 ms  | 0.029 ms  | 0.044 ms  |
  | 10 000  | 0.24 s |  21.8 MB | 0.027 ms  | 0.101 ms  | 0.238 ms  |
  | 50 000  | 1.46 s |  91.9 MB | 0.071 ms  | 0.366 ms  | 0.726 ms  |
  |144 000  | 4.17 s | 251.2 MB | 0.091 ms  | 0.594 ms  | 0.949 ms  |
  Escala sublinealmente: 144x más datos, 6x más latencia.

[H2] El servidor MCP oficial de memoria (@modelcontextprotocol/server-memory),
medido extremo a extremo por stdio con el cliente JSON-RPC real:
  - `search_nodes` es un **scan de subcadena sobre todo el grafo**, recargado del
    fichero JSONL en CADA llamada (`loadGraph()` → `readFile` + `JSON.parse` por
    línea → `filter(e => ...includes(query.toLowerCase()))`). Sin ranking, sin LIMIT.
  - Latencia p50: 3.9 ms (1k entidades) / 20.1 ms (5k) / 79.1 ms (20k). Lineal.
    Reimplementado en Node aparte: 2.1 / 12.0 / 49.5 / 127.1 / 383.9 ms
    (1k / 5k / 20k / 50k / 144k).
  - `tools/list`: 9 tools, 10 750 caracteres de JSON ≈ **~3 000 tokens de peaje
    en cada petición al modelo**, antes de recuperar nada.
  - Tamaño de la RESPUESTA con 20 000 entidades:
      query "the"        → 18 608 556 caracteres ≈ 5 169 000 tokens
      query "was"        →  6 211 579 caracteres ≈ 1 725 000 tokens
      query "project"    →    315 044 caracteres ≈    87 500 tokens
      query "database"   →     50 974 caracteres ≈    14 200 tokens
      query "system design" →      39 caracteres ≈        11 tokens
    Es decir: **salida no acotada con términos frecuentes y salida vacía con
    lenguaje natural**. La peor combinación posible.
  => No es una crítica de diseño: es la implementación de referencia que mucha
     gente instala como "memoria para Claude".

[H3] Embeddings estáticos (model2vec / potion, sin red neuronal en inferencia):
  - Codificar UNA consulta: p50 0.088 ms (10k) / 0.159 ms (50k) / 0.234 ms (144k).
  - Indexar 144 000 registros: 2.3 s = **62 683 registros/s** en CPU, dim 256.
  - Producto punto exhaustivo (numpy f32, 1 hilo): 0.176 ms (10k) / 2.787 ms (50k)
    / 8.109 ms (144k). Por debajo de ~100k registros NO hace falta índice ANN.
  - RAM: 147 MB en f32 para 144k x 256. int8 ocupa 37 MB pero en numpy resultó
    MÁS LENTO (20.3 ms vs 8.1 ms a 144k) porque el upcast a int16 no pasa por BLAS:
    la cuantización solo paga con un kernel SIMD propio.
  - Calidad publicada (MTEB): potion-base-32M = 52.13 avg = 93.2% de
    all-MiniLM-L6-v2; en retrieval potion-retrieval-32M = 35.06 = 81.7% de MiniLM
    (42.92). Existe `potion-code-16M-v2` destilado de CodeRankEmbed.

[H4] Calidad léxico vs vectorial vs híbrido — protocolo propio de known-item con
frase-fuera: 50 000 registros, 500 consultas; a cada registro objetivo se le quita
una frase del texto indexado y esa frase es la consulta (hueco léxico real).
  | motor                  | recall@10 | MRR@10 |
  | BM25 (FTS5)            |   0.392   | 0.240  |
  | vector estático        |   0.406   | 0.224  |
  | híbrido RRF (k=60)     | **0.420** | **0.251** |
  El híbrido gana por poco pero gana en las dos métricas. Coincide con EncouRAGe
  [D7]. Aviso honesto: los valores absolutos son bajos porque el objetivo es UN
  chunk concreto entre 50 000 con muchos vecinos temáticos; lo que importa aquí
  es el orden relativo, no el nivel.

[H5] Ruta de lectura completa propuesta, medida en un proceso Python:
compuerta de intención → BM25 (k=50) → embedding estático → producto punto (k=50)
→ fusión RRF → presupuesto duro de 700 tokens. 50 000 registros.
  | turno                                   | p50      | p95      | p99      |
  | con intención (ruta completa)           |  9.73 ms | 14.67 ms | 18.59 ms |
  | sin intención (corta en la compuerta)   |  0.003 ms|  0.004 ms|  0.008 ms|
  | mezcla realista (15% con intención)     |  0.003 ms| 10.73 ms | 12.45 ms |
  RAM del índice vectorial 102 MB, fichero SQLite 92 MB.

[H6] Desglose por etapa (mismo corpus) — dónde está realmente el coste:
  | etapa                                  | p50      | p95      |
  | BM25 con los 24 primeros términos      | 21.53 ms | 35.53 ms |
  | BM25 con los 8 términos de mayor IDF   |  4.65 ms |  8.75 ms |
  | encode de la consulta (estático)       |  0.156 ms|  0.216 ms|
  | producto punto 50k x 256               |  4.38 ms |  4.66 ms |
  | fusión RRF + presupuesto               |  0.044 ms|  0.053 ms|
  => La **selección de términos por IDF es la mayor palanca de la ruta de lectura**
     (4.6x), no el índice vectorial. El embedding es gratis. El LLM en la ruta de
     lectura es lo único que no cabe en el presupuesto.

Comparativa de la misma función (recuperar contexto relevante para un turno):
  | sistema                                   | p50 de lectura |
  | hook propuesto en Rust, proceso entero [I3] |   4.1 ms     |
  | ruta propuesta en Python, en proceso [H5] |    9.7 ms      |
  | MCP oficial de memoria, 20k entidades [H2]|   79 ms        |
  | Zep, servicio en producción [B3]          |  149-241 ms    |
  | SIx Harness (embeddings vía Ollama) [G3]  | 2 500 ms (max 10 300) |

## I. Mediciones en Rust (prototipo de viabilidad, misma máquina y corpus)

Construido con `model2vec-rs 0.2.1` (implementación oficial en Rust de Model2Vec,
declarada ~1.7x más rápida que la de Python) y `rusqlite 0.32` con la feature
`bundled` (SQLite compilado dentro, FTS5 incluido). Binario único de 9.1 MB, sin
dependencias en la máquina del usuario. Código en `research/bench/rust/`.

[I1] Comparación Python vs Rust, mismas etapas, 50 000 registros:
  | etapa                        | Python   | Rust      | factor |
  | indexar 50k en FTS5          | 1.46 s   | 1.30 s    | 1.1x   |
  | encode de la consulta        | 0.156 ms | 0.023 ms  | 6.8x   |
  | BM25 con top-8 términos      | 4.65 ms  | 1.433 ms  | 3.2x   |

[I2] **El coste dominante NO es la búsqueda: es cargar el modelo de embeddings.**
  | modelo                 | tamaño en disco | carga    | encode en caliente |
  | potion-retrieval-32M   | 249 MB          | 106 ms   | 0.008 ms |
  | potion-base-8M         |  59 MB          |  35 ms   | 0.006 ms |
  | potion-code-16M-v2     |  33 MB          |  94 ms   | 0.006 ms |
  Wall clock del proceso entero con potion-retrieval-32M: 116-118 ms.
  => Un hook es un proceso nuevo en cada invocación. Cargar el modelo en la ruta
     caliente cuesta 35-110 ms, entre 25x y 100x más que toda la búsqueda.
     **La rama vectorial no puede vivir en el hook síncrono.** O va en un demonio
     residente, o va en los hooks asíncronos y de arranque de sesión.
     El encode en caliente (0.006-0.008 ms) confirma que el modelo en sí es gratis;
     lo caro es materializar la tabla de embeddings.

[I3] Hook real medido de extremo a extremo (fork + exec + abrir SQLite + compuerta
de intención + BM25 top-8 + salida), índice de 50 000 registros, **sin modelo**:
  | turno              | p50     | p95     |
  | con intención      | 4.07 ms | 4.31 ms |
  | sin intención      | 0.49 ms | 0.56 ms |
  Trabajo interno reportado por el propio binario: 1.06 ms. El resto es el coste
  de arrancar el proceso.
  => Este es el número real que paga un agente por turno con la arquitectura
     correcta: **4 ms**, frente a los 79 ms del MCP oficial con 20k entidades [H2],
     los 149-241 ms de Zep [B3] y los 2 500 ms de SIx Harness [G3].

[I4] Ruta de escritura incremental (lo que corre en SessionEnd), índice ya poblado
con 50 000 registros, ingestando 200 registros nuevos (≈ una sesión larga):
  insert FTS5 + commit por registro: p50 0.089 ms · encode por registro: p50 0.276 ms
  total de los 200 registros: 80 ms · `optimize` del índice tras la ingesta: 195 ms
  => 275 ms para consolidar una sesión entera. Irrelevante, y además es asíncrono.

[I5] Calidad si se elimina la rama vectorial de la ruta caliente (de [H4]):
  BM25 solo alcanza recall@10 0.392 frente a 0.420 del híbrido = **93.3% del
  híbrido**. La rama vectorial aporta 6.7% de recall relativo a cambio de 35-110 ms
  de carga de modelo en cada turno. En la ruta caliente el intercambio no compensa;
  en la ruta asíncrona sí, porque allí la latencia no la paga nadie.

---

# VENTANA DE CONTRASTE: 12-jul-2026 → 12-sep-2026

Barrido con filtro de fecha estricto (`submittedDate:[202607120000 TO 202609122359]`)
sobre arXiv, más el changelog de Claude Code (versiones 2.1.207–2.1.269, fechadas por
la API de GitHub: 2.1.200 = 3-jul, 2.1.240 = 22-ago, 2.1.269 = 11-sep) y las releases
de openai/codex del mismo periodo. Ordenado por lo que **cambia** el plan primero.

## J. Lo que FALSIFICA o acota la conclusión anterior

[J1] **"Do Context Files Help Coding Agents? A Two-Agent Ablation Study on Real
Repositories"** arXiv:2607.27250 (28-jul-2026). **El resultado más importante de todo
el barrido.**
  - Ablación controlada de la estrategia de inyección de contexto en **Claude Code Y
    Codex**, 17 tareas reales de 3 repositorios, **288 corridas evaluadas** con tests
    de oro.
  - *"Context strategy does not measurably move correctness on either agent (bounded
    to <=10-15pp via equivalence testing)."*
  - Triage de modos de fallo: los agentes fallan por **habilidad de implementación**
    —diseño de la feature, elección del patrón, cableado exacto— **no por
    conocimiento del repositorio que un fichero de contexto pudiera aportar**.
  - Sonda de manipulación: el AGENTS.md real **nunca** convierte un casi-acierto en
    acierto, en ninguno de los dos agentes.
  - La dificultad marginal de las tareas es específica del agente (Spearman ρ=0.75),
    lo que explica por qué los estudios previos se contradicen.
  => **Invalida la premisa implícita de la v1**: "si le damos el conocimiento del
     proyecto, responderá mejor". Para corrección en una sola sesión, NO. Cualquier
     promesa de Muninn sobre calidad debe acotarse a fallos multi-sesión.

[J2] **"Signal or Noise? A Benchmark Study of Agent Skills in Web Development"**
arXiv:2608.23067 (24-ago-2026). 31 skills públicas, 50 proyectos de Web-Bench,
1 000 tareas ordenadas, 4 modelos, con **control irrelevante de longitud igualada** y
ablaciones leave-one-out.
  - La inyección de la skill objetivo **BAJA** el Pass@2 medio entre **1.3% y 4.2%**,
    reduce la profundidad de tarea completada y **sube el coste en tokens 72%–394%**.
  - Ganancia en solo el **17%–36%** de los pares skill-proyecto.
  - Dos modos de fallo: **length-distracted** (una skill irrelevante igual de larga
    reproduce casi toda la pérdida) y **content-misled** (la longitud es neutra pero
    el contenido aun así baja el Pass@2 1.1–1.4%).
  - Las pérdidas se concentran en las tareas fáciles tempranas. El ranking de skills
    transfiere mal entre modelos. **Las reglas anti-patrón superan al contenido lleno
    de ejemplos** dentro de las skills que sí ayudan.
  - *"reframing injection as a per-deployment routing decision and making
    length-matched controls and per-model audits a minimum standard"*.
  => Degrada el pilar de "skills procedurales" de [D3][D4] de fuerte a **contradicho**.
     Y fija el estándar mínimo de evaluación: control de longitud igualada. Mi plan
     de evaluación v1 no lo tenía.

[J3] **"Deep Agentic Search for Repository-Level Code QA: An Empirical Study"**
arXiv:2608.01507 (2-ago-2026). SWE-QA.
  - **Búsqueda semántica: 65.2% de respuestas correctas. Búsqueda agéntica profunda
    (grep por subagente): 46.2%**, y la semántica produjo cada acierto a menos de la
    mitad de coste.
  - Taxonomía de fallos: el mayor grupo de fallos de la búsqueda agéntica, **41.8%**,
    ocurre en el **traspaso entre el planificador y su subagente**, y son
    **silenciosos**: terminan en una respuesta fluida, confiada y equivocada.
  - *"for read-only questions over a repository that can be indexed, retrieval was
    the stronger and cheaper option."*
  => Contradice el "glob y grep evitan el índice obsoleto" de [A2] y corrige mi
     desprecio de la rama indexada. Junto con [D6], son dos resultados independientes:
     **para código, un índice paga.**

[J4] **"On the Fragility of Self-Improving Agents: Variance, Task Order, and
Underspecification"** arXiv:2608.18066 (18-ago-2026).
  - Re-evaluación de dos métodos de auto-mejora basados en memoria con múltiples
    corridas y orden de tareas aleatorizado.
  - La evaluación de agentes es intrínsecamente ruidosa y **el bucle de auto-mejora
    amplifica ese ruido**.
  - **La mejora depende fuertemente del orden de las tareas**: los órdenes por defecto
    imponen un curriculum implícito que actúa como prerrequisito oculto del éxito.
  - Añadir rúbricas y feedback del entorno cierra parte de la degradación, pero
    quedan huecos significativos.
  => Degrada [D1] (ACE) y [D2] (ReasoningBank) de "fuerte" a "**no replicado bajo
     varianza**". Cualquier cifra de auto-mejora sin múltiples corridas y sin
     aleatorizar el orden es un falso reporte de funcionamiento.

[J5] **"MemoryLake on MemoryArena: A Matched Study of Agent Memory Backends"**
arXiv:2608.13883 (14-ago-2026). Comparación emparejada (mismo framework, mismo modelo,
mismas tareas, mismo código de scoring) de MemoryLake vs Mem0 vs RAG vectorial vs
control de contexto largo, en las cinco áreas de MemoryArena.
  - Tasas de éxito absolutas: matemáticas 9/40, física 12/20, recuperación progresiva
    4/20. **Planificación de viajes: 0 para TODOS los sistemas.** Compra web: 1/150.
  - Media de la suite: 20.5% frente a 13.6% del mejor comparador. Los propios autores:
    *"point estimates: sample sizes are modest, confidence intervals overlap, and we
    do not report paired significance tests."*
  => Ducha de agua fría: en tareas multi-sesión interdependientes, **todos los
     sistemas de memoria están cerca de ser inútiles**. Ninguna promesa de Muninn
     puede apoyarse en "completar tareas interdependientes".

[J6] **"Harness the Memory: A Holistic Evaluation of Memory Substrates"**
arXiv:2608.15008 (15-ago-2026). 8 familias de sustrato, 3 modelos base, 4 suites,
26 métricas bajo un harness unificado.
  - *"no single substrate consistently dominates: broad retrieval benefits
    long-context factual QA, while **excessive retrieval can harm sequential
    decision-making** by shifting attention away from action-critical context."*
  - Los sustratos buenos con historiales moderados se vuelven costosos o frágiles en
    horizontes largos → el enrutado de sustrato es necesario, no opcional.
  => Programar es toma de decisiones secuencial. Esto dice que recuperar de más daña
     precisamente nuestro caso de uso. El silencio por defecto pasa de heurística a
     requisito.

[J7] **"Engineering Reliable Coding Agents"** arXiv:2608.13867 (14-ago-2026). Revisión
multivocal de 164 trabajos académicos, 100 registros de practicantes, 29 de benchmarks.
  - *"many apparent model failures originate elsewhere in the system, while
    **improvements at one layer often fail to propagate to end-to-end outcomes**."*
  => La advertencia central contra el optimismo de capa: mejorar la memoria puede no
     propagarse al resultado. Exige medir el resultado, no la capa.

## K. Lo que CONFIRMA, con mejor evidencia que la que yo tenía

[K1] **"Delivery, Not Storage: Cue-Anchored Working Memory as a Harness Property for
Coding Agents"** arXiv:2607.20972 (23-jul-2026). **El arte previo más cercano a
Muninn, y el más importante de leer entero.**
  - Medición central: **la memoria voluntaria es ~cero incluso con el almacén
    pre-cargado con conocimiento relevante: 0 operaciones de memoria en 114 turnos.**
  - Inyección determinista: entregó en todas las corridas equipadas (n=3), con
    **cero falsos disparos** en las evaluaciones de trigger auditadas.
  - **39% de las re-lecturas intra-sesión vuelven a pagar contenido que la sesión ya
    había pagado antes de un límite de compactación.**
  - Sonda de decaimiento: 10 hechos solo en conversación desaparecen en el primer
    resumen y siguen ausentes en **106 de 108** compactaciones forzadas; al final el
    agente privado está **haciendo grep de los ficheros de sesión del propio harness**
    para reconstruirlos, contra instrucción expresa. Los mismos 10 hechos inyectados
    desde un almacén del harness llegan íntegros **139 veces (lanzamiento + 138/138
    compact-resumes)**, mientras el resumen final no lleva ninguno.
  - **Vocabulario de triggers, textual:** `path` (glob sobre ficheros tocados),
    `symbol` (se referencia una entidad de código nombrada; **requiere un grafo de
    símbolos**), `semantic` (similitud de la actividad actual sobre un suelo),
    `event` (session start, prompt submit, pre-edit, pre-run, pre-compaction,
    post-compaction), `temporal` (not-before, cooldown). Conjunción dentro de un
    trigger, disyunción entre triggers.
  - **Disciplina de entrega:** dos niveles (índice compacto por defecto, contenido
    completo on-demand o en disparo anclado); ledger de disparos por sesión que
    deduplica y **se reinicia en los límites de compactación para que los hechos
    anclados se re-armen**; el contenido inyectado lleva marco de procedencia
    (*"recorded by an AI session, not human-endorsed — verify"*); la obsolescencia se
    comprueba contra ground truth y la inyección lleva aviso explícito si el fichero
    anclado cambió después de escribirse la nota.
  - **Lo que declaran como no resuelto:** *"Capture is the unevaluated half... the
    probe's ten fact notes were seeded by the harness, and §5's voluntary-write counts
    measure initiative, not capture quality; capture-side automation is future work."*
  - Otros límites declarados: n=3 brazos; supervivencia medida por coincidencia
    literal de tokens; hechos sintéticos con obligación explícita al cierre, así que
    mide entrega y supervivencia, **no uso espontáneo**; coste en tokens por proxy
    chars/4; **los triggers `symbol` y `temporal` están implementados pero no
    dispararon en ninguna corrida evaluada**, así que la reivindicación de composición
    se apoya en 3 de sus 5 elementos; autor compartido entre implementación y
    benchmark.
  => Confirma tres decisiones de Muninn con medición directa (hooks y no tools;
     invariantes que sobreviven a la compactación; determinismo) y **reasigna la
     contribución defendible: la captura, el grafo de símbolos, y la evaluación con
     brazos de control.**

[K2] **DreamBench-SWE** arXiv:2608.20664 (21-ago-2026). Benchmark multi-sesión de
higiene de memoria para agentes de software, con oráculos ejecutables ocultos y
auditoría pre-registrada. **Es el benchmark de nuestro caso de uso exacto.**
  - Corrida sucesora, 360/360 unidades completadas, cuatro condiciones:
      sin memoria externa             **21/180  (11.67%)**
      memoria de eventos literal      **82/180  (45.56%)**
      sonda de referencia typed+raw   **83/180  (46.11%)**
      Mem0 en configuración literal   **97/180  (53.89%)**
    Las tres condiciones con memoria superan a la condición sin memoria tras
    corrección de Holm.
  - En el fold original, el contraste primario fue **nulo** (95/180 vs 89/180,
    p agrupada = .518, Holm p = 1).
  - Los autores: *"does not establish an external-system mechanism, superiority among
    memory-bearing conditions, equivalence, or broad product generality."*
  => El dato más importante para decidir si el proyecto existe: **la memoria
     multi-sesión vale ~4x la tasa de acierto**, y **ningún diseño de memoria le gana
     a otro de forma concluyente**. Almacenar literal funciona. Consolidar no hace
     falta para cobrar la mayor parte del beneficio.

[K3] **"Why Git Is the Memory Solution for the Agentic Development Lifecycle"**
arXiv:2607.14390 (15-jul-2026).
  - Tesis: la memoria debe estar **ligada a git** — ground truth de los commits,
    frescura del rebuild, verificación del merge, contención de la review.
  - Estudio de recuperación sobre ocho corpus con disciplina de publicación
    pre-registrada: **cinco mecanismos de ranking importados rechazados, dos
    conservados**, mejor configuración ~**0.31 de MRR agrupado** — ~60x el suelo de
    grep sobre transcripciones crudas, ~15x un suelo honesto de turnos parseados.
  - *"Answer assembly is where ranking stops helping: **single-shot retrieval scores
    only 0.07-0.20 answer-sufficiency** on real developer questions, and **ungated
    episode injection measurably degrades good answers**."*
  - Un router despacha: amplitud → mapa estructural anclado en git; consultas
    puntuales → episodios con compuerta de confianza; racional → **síntesis de
    decisiones**, que reconstruye arcos de "por qué" que ninguna sesión contiene
    (**0.83 de suficiencia** en un sistema en producción de ~50k líneas).
  - Enrutado, responde a **382–980 tokens por pregunta**, tres órdenes de magnitud por
    debajo del historial registrado.
  - Ground truth minado de enlaces commit↔sesión, replicable sobre el historial de
    cualquiera **a coste cero de etiquetado**. *"The remaining constraint is capture."*
  => Corrige dos cosas de mi v1: (a) la recuperación de un solo disparo **no basta**
     para responder preguntas (0.07–0.20), aunque sí sirva para entregar cues;
     (b) git da gratis la frescura y la verificación que yo pensaba construir a mano.

[K4] **"Why Does CLAUDE.md Keep Growing? Catastrophic Remembering in Agentic Coding"**
arXiv:2608.11095 (11-ago-2026). 247 694 vidas de instrucción en 1 867 repositorios.
  - Los prompts agénticos crecen sin límite: **+226% sobre su vida**, **+4.9
    instrucciones netas por commit**.
  - **Cuanto más vieja es una instrucción, menos probable es que se borre**
    (log-hazard −0.032/commit).
  - Causa: añadir es siempre barato; borrar sin conocer el racional cuesta O(2^|D|)
    en riesgo de regresión.
  - **Los comentarios de prompt detienen el crecimiento**: comentarios que codifican
    el razonamiento latente eliminan el **99.3%** del exceso de instrucciones
    (+211.3% → +1.4%) en mundos verificables, y mejoran el seguimiento de
    instrucciones del mundo real **hasta un 23.1%** en WildIFEval.
  - *"If English is the new code, why don't we have comments yet?"*
  => Regalo de diseño: el racional registrado es la **licencia de borrado** de una
     instrucción. Y es un mecanismo medido de **mejora de calidad** (hasta +23.1%),
     no solo de higiene. Nota: Claude Code **elimina** los comentarios HTML de
     CLAUDE.md antes de inyectarlo, así que el canal nativo es solo para humanos.

[K5] **"When 'Do Not' Is Not Deny: Security Rules in CLAUDE.md vs Built-In Controls"**
arXiv:2608.23550 (24-ago-2026). 481 ficheros CLAUDE.md públicos.
  - Solo entre el **4% y el 16%** de las reglas de seguridad recuperadas tienen un
    control nativo que las haga cumplir. Bajo el criterio más estricto,
    **4.4% (IC 95%: 2.6–6.7%)**.
  - *"**CLAUDE.md is a write-only channel.** A developer writes a security rule but
    gets no feedback on whether a control will enforce it."*
  - La misma forma de texto plano esconde dos clases de regla: las que una regla de
    permisos, un modo o un sandbox pueden hacer cumplir, y las que quedan a
    interpretación del modelo.
  => La capacidad de mayor valor y **coste cero en tokens** que ha salido de todo el
     barrido: compilar la regla interpretada en un control que se hace cumplir, y
     decirle al desarrollador cuáles de sus reglas son solo persuasión.

[K6] **MOOSEDev — "Ontology-Grounded Project Memory for Coding Agents"**
arXiv:2608.13662 (13-ago-2026). Registros tipados con estado de ciclo de vida,
procedencia y **enlaces de supersesión**, contra una herramienta de memoria vectorial
en producción, sobre 835 registros tipados de un corpus público neutral.
  - En preguntas de **supersesión, completitud de conjunto y negación**:
    **0.98–1.00** frente a **6%–27%** del top-k vectorial.
  - Recall de relevancia y coste en tokens **equivalentes** entre los dos sistemas.
  => Cuantifica exactamente en qué falla el vector: no en relevancia, sino en
     "¿esto sigue vigente?", "¿están TODOS?" y "¿qué decidimos NO hacer?".
     Los callejones sin salida son preguntas de negación: por eso importan.

[K7] **"Revoked but Still Authoritative"** arXiv:2609.08258 (8-sep-2026). Cinco
sistemas de memoria cargados con una política revocada y su reemplazo, 9 escenarios,
9 modelos, 6 condiciones de defensa.
  - **Ningún sistema hace cumplir la revocación por defecto**: el hecho revocado se
    devuelve siempre que la etiqueta de revocación es visible a la capa de
    recuperación, **gana en ranking a su reemplazo**, y lleva al agente a la acción
    insegura.
  => La revocación suave (marcar y conservar) es lo que hace todo el mundo, y no
     funciona. Hay que filtrarla **en la recuperación**, no en el almacén.

[K8] **CONTRAMEM** arXiv:2608.22533 (23-ago-2026). El resultado positivo más grande
de la ventana. Memoria procedural auto-evolutiva, sin entrenamiento, que usa la
**variación de resultado en la misma tarea como supervisión**.
  - GAIA2/ARE held-out: **26.2% → 55.3%** de media. GPT-5.5 27.5→61.0;
    Claude Sonnet 4.6 28.0→52.5; DeepSeek V4 Pro 23.0→52.5.
  - El mismo banco transfiere sin cambios a Qwen3.7 Plus no visto (18.5→35.5).
  - *"localized curation rather than append-only accumulation or whole-bank
    rewriting."*
  - Con presupuesto de trayectorias igualado, las trayectorias **heterogéneas de
    varios modelos** dan memoria más fuerte que las propias multi-rollout: *"the
    margin comes from contrastive behavioral diversity."*
  => Resuelve la contradicción con [J2]: **la memoria destilada de tus propias
     trayectorias contrastadas funciona; las skills genéricas importadas dañan.**

[K9] **SkillGate** arXiv:2608.18852 (19-ago-2026). "Selector credit starvation":
qué skill leer es la decisión clave y la RL por recompensa de resultado **no puede
enseñarla** por una razón estructural (la ventaja a nivel de secuencia reparte crédito
de signo cada vez más equivocado sobre los pocos tokens que nombran la skill elegida).
  - Separando los canales de crédito: política de 9B de **40.8% → 53.2%**, con
    **dos tercios menos de exposición a candidatos engañosos y leyendo menos skills**.
  => Con un modelo alojado y congelado (Claude, Codex) no podemos entrenar el
     selector. Luego el selector tiene que ser **externo y determinista**. Es
     exactamente el argumento de Muninn, por una vía independiente.

[K10] **SkillCommit** arXiv:2608.15165 (15-ago-2026). *"existing methods consolidate
experience based on semantic similarity or LLM judgments, which may merge superficially
related but behaviorally incompatible strategies and thereby degrade performance."*
Cada experiencia se conserva primero como **parche específico de instancia**; se
abstrae solo si pasan replay cruzado y comprobación de mecanismo; se comitea solo si
preserva el comportamiento validado de todos sus constituyentes.
  => Es la compuerta de consolidación concreta que mi v1 pedía sin especificar.

[K11] **EA-Graph** arXiv:2608.04278 (4-ago-2026). Memoria de claims de verificación
anclada a artefactos, bajo deriva upstream. 42 sesiones, 7 mundos, 3 condiciones de
memoria, 2 niveles de modelo.
  - Mantiene **fuerza de evidencia separada de frescura**; cuando el contenido de
    reemplazo no está disponible, el claim pasa a **"no demostrable" en vez de
    adivinado**.
  - Ronda Haiku: la memoria anclada superó a las notas en prosa y a no tener memoria
    en los 7 mundos, p=0.0156 en cada comparación Wilcoxon emparejada exacta. Ronda
    Sonnet: la condición anclada fue perfecta, pero los techos del control dejaron los
    contrastes pre-registrados no significativos.
  - **Ninguna sesión fabricó contenido retenido.**
  => Dos correcciones de diseño: el estado correcto de un hecho caduco es **"no
     demostrable"**, no "posiblemente obsoleto"; y el beneficio es mayor para los
     modelos más pequeños y baratos.

[K12] **"@skills: Attention is all you have"** arXiv:2608.12610 (12-ago-2026).
Hay **56 804 skills públicas**. El modelo de entrega dominante es la instalación, y
una skill instalada deja su descripción en el system prompt *"competing for fewer than
100 reliable trigger slots"*. Separa contenido, persistencia y disparo automático:
solo el último necesita residencia en el prompt.
  => Cuantifica el recurso escaso: ~100 slots de disparo fiables. Nada que Muninn
     haga puede gastar uno sin justificarlo.

[K13] **"From Agent Behaviour to Agent-Friendly Documentation"** arXiv:2608.20195
(20-ago-2026). 557 sesiones agénticas, 94 813 eventos, 3 033 interacciones con
documentación; 33 097 PRs agénticos, 690 260 registros de cambio.
  - **Los ficheros de instrucciones y las notas de trabajo son el 60.5% de todas las
    interacciones con documentación**; la documentación técnica clásica, 10.6%; las
    referencias de API, 1.3%.
  - El vínculo consulta→edición es débil (probabilidad de transición adyacente 0.002;
    OR ajustada 1.33 [1.09, 1.62]).
  - **No se observó ninguna secuencia de validación basada en documentación**, y la
    consulta se asocia con **menos** testing inmediato (OR ajustada 0.39 [0.25, 0.60]).
  - La consulta es **auto-iniciada el 70.2%** de las veces, y solo el 7.5% viene de un
    fallo. El código se toca antes que los documentos 4.7x más a menudo.
  - Las dos propiedades que se asumen de la "documentación amigable para agentes",
    accionabilidad y verificabilidad, *"lack consistent behavioural support"*.
  => El canal a mejorar ya existe y ya concentra el 60% de la atención documental del
     agente: ficheros de instrucciones y notas de trabajo. No hay que crear un canal
     nuevo.

[K14] **Reglas de comportamiento en bucle cerrado** arXiv:2607.13091 (13-jul-2026).
Cada comentario de review aceptado se codifica como regla de comportamiento persistente
en un fichero de instrucciones versionado, con checklist de auto-review antes de
enviar y validación automática de la integridad del conjunto.
  - Despliegue en una plataforma de 35+ microservicios: el conjunto creció de 5 a 18
    reglas; **0% de recurrencia medida para las clases de error con regla**; transfiere
    entre interfaces de agente heterogéneas.
  - Evidencia débil: 11 sesiones registradas, sin brazo de control.
  => La mejor fuente de captura disponible: los comentarios de review aceptados ya
     existen, ya están verificados y ya los aprobó un humano. Coste de captura cero.

## L. Lo que ya es NATIVO (la prueba de "no reinventar la rueda")

Changelog de Claude Code, con versión. La ventana de 2 meses es ≈ 2.1.207 → 2.1.269.

[L1] Memoria automática, **anterior a la ventana y ya madura**:
  - `2.1.32` *"Claude now automatically records and recalls memories as it works"*.
  - `2.1.63` configs de proyecto y auto memory compartidas entre worktrees del repo.
  - `2.1.74` `autoMemoryDirectory`. `2.1.83` índice truncado a 25 KB además de 200
    líneas. `2.1.86` nombres de fichero de memoria clicables.
  - `2.1.172` **`CLAUDE_MEMORY_STORES`: almacenes de memoria de equipo montables.**
    => Mi v1 listaba "no se comparte con el equipo" como hueco. **Es nativo.** Error
       corregido.
  - `2.1.186` el agente recibe recordatorio para compactar su `MEMORY.md`.
  - Dentro de la ventana: `2.1.210` error explícito en vez de truncado silencioso;
    `2.1.211` el aviso mide solo contenido cargado; `2.1.214` **timestamp ISO
    `modified` en el frontmatter**; `2.1.268` el aviso dice cuántas líneas se cortaron
    y dónde empieza el corte.

[L2] **Triggers de path ya son nativos e industria estándar**:
  - `2.0.64` soporte de `.claude/rules/`. `2.1.69` **hook `InstructionsLoaded`** que
    dispara cuando CLAUDE.md o `.claude/rules/*.md` entran en contexto.
  - `2.1.84` `paths:` en frontmatter de rules y skills acepta lista YAML de globs.
  - `2.1.214` condiciones `if:` de hook por path (`dir/**` vs `**/dir/**`).
  - Fuera de Anthropic: **OpenHands** lleva skills con trigger por palabra clave y
    **reglas disparadas por path**; **Devin** exige una *trigger description* en cada
    Knowledge item (emparejada semánticamente y falible según su propia
    documentación), más macros `!nombre`; las reglas de **Devin/Windsurf** tienen
    `always_on`, `manual`, `model_decision`, `agent`, `glob`; **Cursor** tiene Project,
    Team y User Rules con globs.
  => De los cinco tipos de cue de [K1], **`path` y la parte de palabra clave de
     `semantic` ya están enviados en toda la industria**. Lo que no existe en ningún
     sistema enviado: `symbol`, `event` más allá de session-start, y `temporal`,
     y ninguno los lleva sobre **memoria escrita por el agente** en vez de reglas
     escritas por humanos.

[L3] Superficie de hooks disponible, dentro de la ventana:
  - `2.1.219` hook **`DirectoryAdded`**. `2.1.251` **`PreModelSwitch`/`PostModelSwitch`**,
    y los hooks `SessionStart` de resume ahora reciben **la antigüedad de la sesión y
    el coste estimado de re-cachear**.
  - `2.1.121` (previo) **`PostToolUse` puede reemplazar la salida de cualquier
    herramienta** vía `hookSpecificOutput.updatedToolOutput`. Es un canal de
    inyección que no gasta un slot de trigger.
  - `2.1.218` skills con `context: fork` corren en background por defecto.
  - `2.1.246` `/cd` recarga settings, hooks, MCP, skills y agents del nuevo directorio.
  - `2.1.261` **`/skill-doctor`: muestra qué skills cargadas no se usan y cuánto
    cuestan en contexto.** El vendor ya está enviando contabilidad de coste de contexto.

[L4] **No existe ningún índice ni recuperación con ranking en lo nativo.** Barrido del
changelog completo por `index|retriev|embedding|vector|semantic search|BM25`: el único
índice es el de nombres de fichero para las menciones `@` (`2.1.47`). La memoria
automática es un índice plano de texto que el modelo lee y decide qué abrir.
  => **El hueco central de Muninn sobrevive intacto.**

[L5] Codex, releases en la ventana (openai/codex):
  - `rust-v0.153.0` (3-sep) añade `features.context_management.experimental_mode`,
    desactivado por defecto, para sesiones elegibles de Plus/Pro/Pro Lite.
  - Serie de PRs de **"Guardian thread context"**: *"Automatic approval reviews
    preserve user instructions, answers, and valid authorizations across history
    compaction"*, *"Guardian review history survives compaction, restarts, and
    user-created forks"*, *"Require Guardian review for incompatible compaction
    checkpoints"*, *"Retain user instructions in guardian thread context"*.
  => **Codex ya implementa constraint-pinning nativo para autorizaciones y respuestas
     verificadas del usuario.** Mi pilar de "invariantes que sobreviven a la
     compactación" queda acotado: para aprobaciones y autorizaciones, ya está hecho.
     Para invariantes de proyecto arbitrarios, no.
  - `features.memories` sigue **experimental y desactivado por defecto**, con
    `extract_model` y `consolidation_model`: sigue siendo el patrón que [C1]
    desaconseja.

[L6] Salud del ecosistema MCP, por si la tentación fuera enviar un servidor más:
  - arXiv:2609.10962 (10-sep): censo de 24 135 servidores del registro, muestra
    probabilística de 400 npm/stdio sondeados por cable. **Solo el 48.8% completa un
    handshake `initialize`** (66.7% en un marco curado con el mismo instrumento), y el
    fallo dominante no son credenciales (13.3%) sino **servidores que no arrancan
    nunca (37.5%)**. Omisión de anotaciones de seguridad a nivel de herramienta:
    58.8% en muestra aleatoria.
  - arXiv:2609.07360 (7-sep): 3 171 repos públicos; **16.0% de los setups llevan un
    defecto de seguridad**; 9.8% instalan un servidor MCP sin versión fijada; 3.8%
    llevan una skill que pre-aprueba la shell a quien la instale.

## M. Corrección a mis propias cifras de la v1

[M1] **El peaje de 3 000 tokens del MCP oficial necesita una condición.**
`2.1.7` del changelog: *"Enabled MCP tool search auto mode by default for all users.
**When MCP tool descriptions exceed 10% of the context window**, they are automatically
deferred and discovered via the MCPSearch tool instead of being loaded upfront."*
  - 3 000 tokens son ~1.5% de una ventana de 200k: **por debajo del umbral**, luego un
    servidor de memoria con 9 herramientas **sí se carga por adelantado** y el peaje
    medido es real.
  - Pero si el usuario ya tiene muchos servidores MCP y el total pasa del 10%, todo se
    difiere y el peaje se convierte en un viaje de ida y vuelta de `ToolSearch`.
  - `2.1.121` `alwaysLoad: true` fuerza la carga por adelantado.
  => La cifra se mantiene con su condición explícita. Pero **el argumento fuerte
     contra las tools no es el peaje: es [K1], 0 operaciones en 114 turnos.**

[M2] Mediciones propias nuevas, con el diseño corregido (misma máquina, Rust):
  - **Evaluación de cues por turno: p50 1.034 ms, p95 1.064 ms, p99 1.143 ms.**
    Escala: 5 000 memorias, 15 000 triggers, índice de símbolos de escala 200 000,
    20 ficheros tocados con sus directorios ancestros, 50 símbolos referenciados,
    un evento, más el filtro de supersesión sobre 64 candidatos. Construcción del
    índice: 0.01 s.
  - La forma ingenua (un `GLOB` por fichero tocado contra la tabla de triggers) cuesta
    **5.614 ms p50**: normalizar los triggers de path a prefijo de directorio y
    resolverlos por índice es **5.4x más rápido** y misma semántica para el patrón
    dominante `dir/**`.
  - Ruta caliente completa revisada: proceso (~3 ms) + cues (1.03 ms) + vía léxica
    BM25 opcional (1.43 ms) ≈ **5–6 ms p50**.

## N. Arte previo enviado y respaldo del cumplimiento externo (iteración 3)

[N1] **rekal-cli 1.0** — github.com/rekal-dev/rekal-cli, paper arXiv:2607.14390 `[K3]`.
Verificado contra su README. **Cubre dos de las cuatro funciones que el plan v2 iba a
construir.**
  - Captura la sesión de IA **en cada commit** vía hook `post-commit`; la guarda **cruda**
    y append-only en `data.db`; sin pre-resumen por LLM, sin destilación lossy.
  - Índice derivado y desechable (`index.db`): BM25 FTS + embeddings LSA + embeddings
    profundos + co-ocurrencia + facetas + chunks de prosa del HEAD. *"a pure function of
    that ledger, so it can never drift"*; las pasadas pesadas corren en background con
    timebox y el commit no espera.
  - **Memoria de equipo sin servidor**: ramas huérfanas `rekal/<email>`, y **solo el
    trabajo mergeado llega al cable** (ancestro de `main`, o squash detectado por
    equivalencia de parche). Un spike no mergeado nunca sale de la máquina.
  - Redacción de secretos (patrón + entropía de Shannon) y anonimización de rutas de home
    **antes** de escribir en la base.
  - **Compuerta de silencio**: veredictos `INJECT` / `KNOWLEDGE` / `SILENCE`, con
    confianza absoluta por semilla, score normalizado y masa BM25 cruda, *"so a silence
    gate can reject a query that is merely the best of a weak set"*.
  - **Router de pregunta** en la skill: tree (grep, ahora) / knowledge (prosa en HEAD) /
    ledger (razonamiento pasado) / map (estructura); y un segundo paso que clasifica el
    *tipo de respuesta*.
  - Grafo de recall personal y local (nunca sincronizado): `[reached N× drilled N×]` con
    boost de ranking.
  - Procedencia completa: turno → sesión → commit.
  - Un solo binario; embeddings on-device; funciona con Claude Code, Cursor, Copilot,
    Codex, Gemini, Kiro, OpenCode; escribe una línea marcada en CLAUDE.md / AGENTS.md /
    GEMINI.md / copilot-instructions.md apuntando a su skill.
  - **Benchmarks** (GPT-5 Sol responde, Rekal aporta la memoria):
      LoCoMo:       accuracy **90.57%**, recall@20 98.61%, ~7.5K tokens/consulta, 5.9 turnos
      LongMemEval:  accuracy **86.60%**, recall@20 99%,    ~10.5K tokens/consulta, 6.6 turnos
  - Coste: **~170 MB de descarga, ~200 MB en disco** (lleva dentro el motor de inferencia,
    el modelo, la base y la extensión FTS); recuperación en *"unos segundos"* y 5.9–6.6
    **turnos de agente** por consulta.
  - **Lo que NO hace**: (1) entrega involuntaria — se invoca vía skill que el agente debe
    elegir usar, que es exactamente el fallo de `[K1]`, 0 ops en 114 turnos; no hay
    condiciones de disparo permanentes ni re-inyección en `PostCompact`; (2) supersesión ni
    revocación hechas cumplir; (3) triggers de símbolo por memoria; (4) compilar reglas en
    controles.
  => **Responder "por qué" está resuelto y bien. Repartir sin que el agente lo pida, no.**
     La función de Muninn se separa limpiamente: Rekal responde (segundos, multi-turno),
     Muninn reparte (1 ms, involuntario).

[N2] **"Recognition Without Enforcement: Configuration-Dependent Failures in LLM Agent
Instruction Arbitration and External Control"** arXiv:2608.28502 (28-ago-2026), 81 páginas.
**El respaldo más fuerte de todo el barrido para compilar reglas a controles.**
  - Evaluación de flota: spoofing de autoridad sobre **46 endpoints de modelo de 6
    fabricantes** (incluyendo open-weight); conflicto de memoria sobre **48 modelos**;
    14 294 ensayos falsificados de 29 modelos.
  - **Brecha reconocimiento-cumplimiento**: los rasgos de formato de la fuente son
    linealmente decodificables de las activaciones y los modelos **identifican
    explícitamente la autoridad falsificada cuando se les pregunta**, y aun así algunas
    configuraciones emiten la llamada a herramienta conflictiva.
  - Ejecución media bajo ataques nuevos y diversos: **1.21% [0.5–2.1%]**, pero la
    vulnerabilidad **se concentra en celdas reproducibles** y se desplaza entre ventanas
    de despliegue: hasta **47 pp de rango intra-ventana por fingerprint**.
  - *"Restrictive policies and diverse prompts can eliminate execution on the same models,
    while permissive configurations and particular prompt-model pairs yield deterministic
    failures."* → es dependiente de la **configuración**, no de los pesos.
  - *"Prompt-layer defenses likewise fail to generalize across models and adaptive
    formulations."*
  - Su solución: un **monitor de referencia externo** con enrutado de fuente autenticado y
    ejecución de herramientas con capacidades. Rechaza deterministamente todo lo
    falsificado, manipulado, reproducido y sin firmar, preservando las operaciones
    legítimas. Un red-team adaptativo separado encontró un fallo de implementación
    (admisión por desfase de reloj, ya parcheado), no un bypass criptográfico.
  - Conclusión textual: *"Secure agents require external enforcement, not merely better
    recognition."*
  => Dos consecuencias para Muninn: (1) escribir la regla en el prompt es la capa
     equivocada, con medición de flota; F1 queda respaldado por el hueco `[K5]` y por el
     mecanismo `[N2]`. (2) **La memoria es una de las fuentes de instrucción que el modelo
     arbitra**, y el conflicto de memoria se midió en 48 modelos: lo que Muninn inyecta es
     en sí un evento de frontera de confianza, así que debe llevar procedencia siempre y
     no poder escalar nunca.

[N3] **MemForest** arXiv:2609.08273 (8-sep-2026). Compresión de memoria por particionado
en EventTrees y fusión progresiva. Bajo Mem0: retiene **97.1%** del rendimiento
comprimiendo el **50%** del historial en tres benchmarks, con **1.89x** de speedup de
recuperación. Bajo M3-Agent: 99.7% con 50% de compresión y 2.24x.
  => La compresión y el desalojo son un problema resuelto por otros y medido. Se delega.

[N4] **Texto completo de `[K7]`** — arXiv:2609.08258, *"Revoked but Still Authoritative"*.
Leído entero porque su guard es mi F3. Cinco sistemas medidos: Graphiti, Zep CE (mismo
motor), mem0, langmem, cognee. 9 escenarios × 9 modelos de 6 fabricantes × 10 ensayos,
6 condiciones de defensa, 49 121 llamadas de decisión.

**Tres formas de fallo, todas reales:**
  1. La revocación **nunca se registra** (cognee, langmem, y mem0 en inserción indirecta).
  2. Se registra y se devuelve, pero **el estado se oculta al llamante**, así que ninguna
     aplicación puede hacerla cumplir (Zep).
  3. Se registra, se devuelve con su estado expuesto, **y aun así no se hace cumplir en
     la recuperación** (Graphiti por defecto; mem0 con el override de expiry activado).
  - Con el registro revocado expuesto, **rankea primero** y el 42.1–44.2% de los ensayos
    elige la acción insegura. Ningún sistema hace cumplir la revocación bajo inserción
    indirecta.

**El resultado que reordena mis prioridades** (Apéndice C, cadena de herramientas,
360 corridas, 9 modelos, 4 escenarios con política permisiva revocada más prohibición
vigente):
  | defensa | herramienta peligrosa invocada | secreto leído | exfiltración confirmada |
  | sin defensa                | **62/360 = 17.2%** | 18/360 = 5.0% | 9/360 = 2.5% |
  | filtro en la recuperación  | **8/360 = 2.2%**   | 1/360 = 0.3%  | **0/360** |
  | guard (§5)                 | **8/360 = 2.2%**   | 3/360 = 0.8%  | **0/360** |
  | guardrail en la herramienta| **62/360 = 17.2%** | 18/360 = 5.0% | 7/360 = 1.9% |
  - El guardrail a nivel de herramienta es **estadísticamente indistinguible de no tener
    defensa**. Rechazó un comando en 9 de 360 y el agente no evadió ninguno de esos 9:
    *"the rule fails upstream of evasion, because the returned revoked policy licenses
    actions the rule does not classify as dangerous."*
  - *"The containment boundary that holds is the read."*
  => **Consecuencia para F1**: compilar reglas a controles en la frontera de la
     herramienta **no** protege de lo que una memoria caduca autoriza. Son dos fallos
     distintos con dos fronteras distintas. F1 sigue justificado por su propio hueco
     `[K5]` y por `[N2]`, pero **no puedo reclamar que F1 cubra el fallo de memoria
     obsoleta**. Solo F3 lo cubre.
  => **Consecuencia para el orden**: F3 tiene el número de seguridad extremo a extremo
     más fuerte de toda la investigación (17.2% → 2.2%, y 2.5% → 0% de exfiltración) y
     cuesta ~0 tokens. Pasa a ser prioridad 1.

**El guard ya está publicado** — `guard/stale_guard.py` en
github.com/VulcanLab/Memory-Rebirth-Attack. Módulo agnóstico del backend que lee solo
los registros que devuelve una llamada de recuperación. Dos etapas: campos de estado
explícitos donde el backend los expone; y donde no, un test de **contención + oposición**
sobre conjuntos de palabras de contenido stemmed (α=0.45, β=0.6) que detecta pares con
valores incompatibles del mismo sujeto y retira el más antiguo. Lo retirado se devuelve
aparte con un motivo; nada se borra del almacén. Los registros con timestamp conocido se
consideran primero en orden descendente de τ, lo que garantiza que el más antiguo del par
es siempre el entrante. Si los timestamps no permiten ordenar, **conserva ambos y reporta
el conflicto**.

**Y es débil, según sus propios autores:**
  - *"shows that the guard is deployable, not that its design is sound: the first stage
    reads the same field the filter reads, so the agreement is not independent evidence."*
  - En Zep capturó solo **4/14**, porque Zep parte una política en varias aristas y revoca
    solo algunas, y nada en el texto devuelto dice cuáles.
  - En cognee y Graphiti bajo inserción indirecta el guard es **marginalmente PEOR que no
    tener defensa** (53/810 vs 44/810 y 83/810 vs 81/810), porque el test de conflicto
    también empareja pares en conflicto que no son una política revocada y su reemplazo.
  - Una versión anterior rompía empates por orden de resultado, *"which is unsound because
    result order is a relevance ranking rather than a chronology"*: contra un backend sin
    metadatos retiró la política vigente y conservó la revocada, **convirtiendo una tasa
    insegura del 0% en el 100%**.
  => **Aquí está la contribución exacta y defendible**: el guard es débil *porque* es
     agnóstico del backend y tiene que inferir el conflicto de texto que un almacén le
     devuelve sin estructura. Si el esquema es propio —tripleta (sujeto, relación, objeto)
     con timestamps reales y enlace de supersesión explícito— la etapa 2 heurística
     **desaparece** y el filtro pasa a ser exacto. Eso es precisamente lo que miden
     `[K6]` (98–100% vs 6–27%) y `[E1]` (15–40% → ~0% sin umbral y sin LLM).
     No hay que escribir un guard mejor: hay que quitarle la necesidad de adivinar.

[N5] Comprobación de arte previo para F1: búsqueda en arXiv con ventana
(`permission`+`instruction`+`enforce`+`agent`; `compile`+`policy`+`guardrail`) y búsqueda
de código en GitHub (`CLAUDE.md rules to permissions deny compile settings.json generate`)
→ **cero resultados**. El hueco de `[K5]` sigue sin herramienta. Es el único de los tres
que nadie ha tocado ni en paper ni en código.

## O. Multi-harness: ¿un todo en uno que reemplace lo nativo? (iteración 5)

[O1] **La premisa "esos harness no tienen memoria y dependen de plugins" es medio falsa.**
Verificado en documentación oficial:
  - **OpenCode** tiene sistema de plugins nativo con lista de eventos publicada:
    `session.created`, `session.compacted`, `session.idle`, `session.updated`,
    `tool.execute.before`, `tool.execute.after`, `permission.asked`,
    `permission.replied`, `message.updated`, más eventos de comando, fichero, LSP,
    shell y TUI. Y uno que **no tiene ni Claude Code ni Codex**:
    **`experimental.session.compacting`**, que dispara *antes* de que el LLM genere el
    resumen de continuación y permite **inyectar contexto en el prompt de compactación
    o reemplazarlo entero**. Plugins auto-cargados de `.opencode/plugins/` y
    `~/.config/opencode/plugins/`. Reglas en AGENTS.md.
    => Para el pilar de supervivencia a la compactación `[C2]` `[K1]`, **OpenCode es
       el mejor objetivo del mercado, no el peor.**
  - **Cline** tiene SDK de plugins con **hooks de runtime** (`beforeRun`, `beforeModel`,
    `afterTool`) y **file hooks** externos con payload JSON (`prompt_submit`,
    `tool_call`, `agent_end`). Limitación declarada: los plugins aplican a SDK, CLI y
    Kanban, **no a las extensiones de VSCode ni JetBrains** por ahora. Y los hooks de
    Cline corren **solo en macOS y Linux**.
  - **Pi** (earendil-works/pi, Mario Zechner): harness minimalista, cuatro tools por
    defecto, todo lo demás por extensiones, skills, plantillas de prompt y paquetes npm
    o git. Sin memoria nativa. Aquí la premisa **sí se cumple**.

[O2] **Y la idea de "memoria por hooks en vez de MCP" ya está enviada como producto.**
Hindsight (Vectorize), *"Cline Persistent Memory: Lifecycle Hooks Instead of MCP"*,
junio-2026. Verificado en su propia documentación:
  - Cuatro hooks: `TaskStart`, `UserPromptSubmit`, `TaskComplete`, `TaskCancel`.
  - *"Recall is deterministic. Because it runs on hooks, memory is injected
    automatically. There's no MCP tool the model can forget to use."* Es exactamente la
  tesis de `[K1]`, en producción, tres meses antes de esta investigación.
  - Inyecta un bloque `<hindsight_memories>`; presupuesto configurable
    (`recallBudget` low/mid/high, `recallMaxTokens`); granularidad agent / project /
    session / user; banco compartido de equipo.
  - **Latencia declarada: "typically under 300ms"** con Hindsight Cloud — es una llamada
    de red en cada prompt.
  - Extracción de hechos **server-side por LLM** (`retainMission` orienta la extracción):
    es el patrón de consolidación que `[C1]` mide cayendo por debajo del baseline sin
    memoria.
  - Sin filtro de supersesión ni de revocación. Recall por búsqueda semántica sobre la
    descripción de la tarea y el prompt: **no hay condiciones de disparo permanentes**
    de tipo path, symbol, event o temporal.
  - Retain solo al final de la tarea; los transcripts salen de la máquina por defecto.
  => **Tercera poda**: "entregar por hook y no por tool" ya no es novedad, es producto.
     Lo que sigue sin estar en el mercado es el **filtro determinista** (F1), el
     **compilador de reglas a controles** (F2) y los **cues no semánticos** (F3).
     Y la diferencia de latencia es de ~300x: 300 ms de red frente a 1.03 ms local `[M2]`.

[O3] **"One Recipe, Many Harnesses: What Self-Evolution Encodes Across Languages and
Models"** arXiv:2608.10178 (10-ago-2026). Receta de evolución fija sobre una rejilla de
**ocho lenguajes** (Multi-SWE-Bench) y **tres modelos base**.
  - *"Gains compensate recoverable execution defects, where **defect mass is near zero,
    and gain is near zero**; which defect dominates is cell-specific. A harness closes
    the gap between what a policy can do and what it does."*
  - *"Evolved harnesses share an abstract playbook across languages but instantiate it
    with almost disjoint language ecosystem machinery."*
  - *"**The shared core transfers and can be distilled into one universal harness, while
    an ecosystem margin resists both and requires native re-evolution.**"*
  => Dos consecuencias directas: (1) la forma correcta de un multi-harness es **núcleo
     universal + margen que hay que reimplementar por harness**, y está medido; (2) **la
     ganancia es proporcional a la masa de defecto recuperable**, así que sustituir una
     memoria nativa competente rinde cerca de cero, y dotar de memoria a un harness que
     no tiene ninguna rinde mucho. Aviso de precisión: el paper mide defectos de
     ejecución, no de memoria; el número específico de memoria es `[K2]`.

[O4] **"What Does Multi-Harness RL Learn? Credit Assignment and Portability in Coding
Agents"** arXiv:2609.04518 (3-sep-2026). Qwen3-8B, harnesses Aider, OpenHands, Qwen Code
y SWE-agent, más un harness mínimo retenido fuera del entrenamiento, **24 000
evaluaciones selladas** con oráculo SWE-bench Verified.
  - *"**The evaluation harness is the dominant variable**: across 24,000 sealed
    evaluations it moves the mean solve rate from **2.14% to 9.27%, a factor of 4.3**,
    where the training recipe moves it by 1.16."*
  - *"Cross-harness credit yields **configuration adaptation and no more portable
    capability** than within-harness credit."*
  => El harness es un factor 4.3x sobre la tasa de resolución, más que cualquier receta
     de entrenamiento. Pero entrenar cruzando harnesses da **adaptación de
     configuración, no capacidad portable**. Traducido a Muninn: el núcleo compartido
     se puede distribuir, pero **no hay que prometer que "lo aprendido en un harness
     mejora otro"**.

[O5] Desactivar lo nativo es trivial, y eso es parte del problema.
  - Claude Code: `autoMemoryEnabled: false` en settings, o
    `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`, o el toggle de `/memory`.
  - Codex: `features.memories` ya viene **desactivado** por defecto.
  => La pregunta no es si se *puede* reemplazar. Es si al reemplazar se pierde algo que
     no se puede reconstruir barato: la carga de `MEMORY.md` al arrancar la hace el
     propio harness, el compartido entre worktrees del mismo repo lo hace el harness,
     los almacenes de equipo montados (`CLAUDE_MEMORY_STORES`) los hace el harness, y
     **los topes duros de 200 líneas / 25 KB con error explícito al pasarse** son la
     única defensa nativa contra el crecimiento catastrófico que mide `[K4]`
     (+226% de vida, −0.032 de log-hazard de borrado por commit).

## P. ¿Un paquete universal con almacén propio? (iteración 6)

[P1] **La categoría "memoria universal multi-harness" está llena.** Encontrados en una
sola búsqueda, todos vivos en 2026: **agentmemory** (rohitg00, open source),
**AgentVault Memory** (sauravkalia), **Universal Memory** (universal-memory.com,
comercial), **ai-memory-layer** (TonyBlanco), **memory.audaxic.com** (comercial,
"hasta 50% más barato que mem0"), **Cognee**, y un artículo en Towards Data Science,
*"Unified Agentic Memory Across Harnesses Using Hooks"*, que hace hooks + Neo4j sobre
Claude Code, Codex y Cursor.

[P2] **agentmemory ES la idea propuesta, ya enviada.** Verificado contra su README:
  - **12 hooks de auto-captura**, "zero manual effort". Para Claude Code registra
    `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PreCompact`,
    `Stop`. Y reutiliza los mismos scripts en Codex, porque *"Codex's hook engine
    injects `CLAUDE_PLUGIN_ROOT` into hook subprocesses... so the same hook scripts work
    across both hosts without duplication"*.
  - **BM25 + vector + grafo con fusión RRF.** Exactamente mi stack más grafo.
  - **SQLite, sin dependencias externas, self-hosted por defecto**, modo keyless que
    desactiva vectores y usa solo BM25, y embeddings locales opcionales
    (`Xenova/all-MiniLM-L6-v2`, 384 dim, on-device).
  - **"Memory evolution: Versioning, supersession, relationship graphs"** y
    **"Recall hygiene: Superseded memory versions leave the search indexes; the version
    chain in KV keeps full history"**. Es F1 —el filtro de supersesión en la
    recuperación— ya construido.
  - **"Write-time provenance: every observation and memory carries an immutable origin
    channel (user, agent, tool, import, or shared) stamped at capture, save, and
    import"**. Es mi requisito de procedencia, ya construido.
  - Secretos eliminados antes de almacenar; **puente bidireccional con `MEMORY.md`**
    (complementa la nativa en vez de reemplazarla); memoria de equipo con namespaces;
    snapshots en git; visor en tiempo real; trazas OTEL; colas con dead-letter.
  - Presupuesto de tokens por defecto **2 000**, ~1 900 tokens por sesión declarados.
  - Distribución: plugin de Claude Code, `agentmemory connect codex --with-hooks`,
    plugin de Copilot, y **17 skills instalables vía el CLI `skills` de vercel-labs en
    50+ agentes** (Cursor, Cline, Continue, Droid, Warp, Codex, Antigravity, Kiro,
    OpenCode, Goose, Roo, Trae, Windsurf...).
  - Benchmarks propios: LongMemEval-S **R@5 95.2%, R@10 98.6%, MRR 88.2%**; ruta híbrida
    **p50 14 ms**; y una nota honesta de que solo su cifra es medida por ellos y el
    resto son números publicados por terceros sobre datasets distintos.
  => **Cuarta poda, y la más grande.** Almacén propio, multi-harness, captura por hooks,
     BM25+vector+grafo, supersesión y procedencia ya existen en un solo paquete open
     source. Construir "la memoria universal" es entrar por detrás en una categoría
     ocupada.

[P3] **Y agentmemory está del lado equivocado de mi evidencia más fuerte.**
  - *"Consolidation (graph nodes, lessons, crystals) is on by default whenever an LLM
    provider is configured"*, más "4-tier consolidation + decay + auto-forget" y decaimiento
    tipo Ebbinghaus. Es exactamente el patrón que `[C1]` mide cayendo **por debajo del
    baseline sin memoria** (GPT-5.4 falla el 54% de problemas ARC-AGI que ya había
    resuelto). Se puede desactivar con `CONSOLIDATION_ENABLED=false`, pero el defecto
    es el contrario al que dice la evidencia.
  - **54 herramientas MCP y 17 skills.** El peaje de definiciones `[A3]` `[H2]` a escala
    industrial, y `[J2]` mide que las skills inyectadas **restan** Pass@2 con control de
    longitud igualada.
  - Sus benchmarks son de **calidad de recuperación** (R@5, MRR), no de **resultado de
    tarea**. Es precisamente la métrica que `[J1]`, `[J2]` y `[K2]` demuestran que no
    predice el resultado.
  - Latencia: 14 ms p50 de búsqueda **más** un servidor en `localhost:3111`, el motor
    iii y el runtime de Node. Frente a 1.034 ms en proceso `[M2]`.
  - No tiene condiciones de disparo permanentes por memoria: recupera por búsqueda
    híbrida sobre el prompt, igual que Hindsight `[O2]`. Sin `path`, `symbol` ni
    `temporal`.
  - No tiene compilador de reglas a controles.

[P4] **Superficie real por harness, verificada en documentación oficial.** Es lo que
decide si "cualquier harness" es una promesa cumplible.

| Harness | Inyección por turno | Compactación | Transcripción a hooks | Superficie de permisos (F2) | Formato de plugin |
|---|---|---|---|---|---|
| **Claude Code** | `UserPromptSubmit` → `additionalContext`, **tope 10 000 caracteres**, timeout 30 s y **si expira se descarta la salida** | `PreCompact` + `PostCompact` con `compact_summary` | Sí, `transcript_path` en todos los hooks | Sí: `PreToolUse` → `permissionDecision` allow/deny/ask/defer, reglas de permisos, sandbox | Subproceso (command/http/mcp_tool) |
| **Codex** | `UserPromptSubmit` → `additionalContext` con `additionalContextLimit` (2 500 por defecto, 0 = sin límite) | `PreCompact` + `PostCompact` | Sí, `transcript_path` | Sí: `PreToolUse`, `PermissionRequest`, sandbox | Subproceso; comparte scripts con Claude Code |
| **OpenCode** | `chat.message` / `chat.params` | **`experimental.session.compacting`: inyecta en el prompt de compactación o lo reemplaza** — nadie más lo tiene | Almacenamiento en `~/.local/share/opencode/storage/`, por proyecto | Sí: `permission.ask`, `permission.asked/replied` | **Plugin TypeScript en proceso** |
| **Cline** | `prompt_submit` (file hook) o `beforeModel` (runtime) | No documentada | **No. Cline no entrega transcripción a los hooks** — Hindsight tuvo que acumular estado en `~/.hindsight/cline/state/` `[O2]` | Parcial | **Plugin TS o file hooks. Solo macOS y Linux. No cubre la extensión de VSCode ni JetBrains** |
| **Pi** | Extensiones | No documentada | Sesiones persistidas (las publican a HuggingFace) | **Ninguna.** *"Pi does not include a built-in permission system for restricting filesystem, process, network, or credential access"* | **Extensión TypeScript / paquete npm** |

  => Tres consecuencias duras:
     1. **Dos objetivos de compilación, no uno.** Claude Code y Codex toman subprocesos:
        un binario Rust sirve. OpenCode, Cline y Pi quieren un módulo **en proceso de
        Node/TypeScript**. La salida limpia es un núcleo Rust expuesto como binario CLI
        *y* como addon nativo napi-rs, no dos implementaciones.
     2. **F2 no tiene diana en Pi.** Sin sistema de permisos no hay control al que
        compilar. Ahí el compilador degrada a informe, o Muninn tendría que *ser* la
        capa de permisos, que es un componente crítico de seguridad y otro proyecto.
     3. **La captura no es uniforme.** Claude Code y Codex la regalan; Cline no la da
        en absoluto. "Un paquete que captura en cualquier harness" es falso de partida.

[P5] **El coste real no es el núcleo: es la cinta de correr de cinco adaptadores.**
  - Claude Code publicó ~62 versiones en la ventana de dos meses (2.1.207 → 2.1.269).
  - La actividad de commits sobre plugins creció **8.8x en seis meses** tras el
    lanzamiento del marketplace, y los ficheros de instrucciones y los scripts
    **co-evolucionan por encima del azar, con el 78% de los co-cambios funcionalmente
    acoplados** — una clase de dependencia de mantenimiento que no existía en software
    tradicional `[2608.28497]`.
  - El 16.0% de 3 171 setups públicos de agentes lleva un defecto de seguridad
    `[2609.07360]`.
  - Y el hook de compactación de OpenCode se llama, literalmente,
    `experimental.session.compacting`.

[P6] **Restricciones de ingeniería que fija la documentación y que hay que respetar:**
  - `additionalContext` de Claude Code está **capado a 10 000 caracteres** (~2 500
    tokens). El presupuesto de ≤700 tokens cabe con holgura.
  - `UserPromptSubmit` **bloquea el procesamiento del modelo hasta que termina**; si
    expira, la salida se descarta y el prompt llega sin contexto. Los 4.07 ms medidos
    `[I3]` son el margen que hace esto seguro.
  - `SessionEnd` tiene timeout por defecto de **1.5 s**, y **los timeouts de hooks
    provistos por un plugin no elevan el presupuesto**. La ingesta medida de 275 ms
    `[I4]` cabe, pero la captura tiene que ser asíncrona o estrictamente sub-segundo.
  - `PostCompact` entrega `compact_summary`: se puede **medir** qué sobrevivió al resumen
    en vez de suponerlo, y re-inyectar solo lo que se cayó. Nadie lo está haciendo.

## Q. Propiedad del almacén: corrección de la sección 8-ter (iteración 7)

[Q1] **Fallo en mi propia recomendación anterior.** En §8-ter propuse "Muninn es el
gobernador que se pone delante de la memoria que ya tengas". Contado en integraciones,
esa arquitectura es **peor**:
  - Un gobernador sobre N almacenes necesita **N drivers**: parsear el `MEMORY.md`
    nativo (markdown más la semántica del índice de 200 líneas / 25 KB), hablar REST con
    agentmemory en `localhost:3111`, leer el DuckDB o el CLI de Rekal, llamar a la API
    cloud de Hindsight. Cuatro drivers y cuatro fuentes de churn.
  - Si Muninn posee el ledger, los adaptadores solo necesitan **eventos + un canal de
    inyección + la ruta de la transcripción**. Cero acoplamiento de formato.
  - Y la intersección de lo que esos almacenes exponen es *"texto más timestamp"*, que
    es exactamente la entrada degenerada que hace débil a `stale_guard`: 4/14 en Zep y
    peor que no tener defensa en dos sistemas `[N4]`. **Un gobernador nunca puede ser
    exacto sobre almacenes que no controla.** Mi propia evidencia lo dice.
  => La decisión de poseer el ledger tipado y el motor **es correcta**, y la había
     infravalorado. Poseerlo adelgaza los adaptadores y es lo único que hace exacto al
     filtro.

[Q2] **Pero la independencia es de formato, no de eventos.** Poseer el almacén elimina
el acoplamiento de *formato*; no elimina el de *eventos*. Si Claude Code renombra
`UserPromptSubmit` o OpenCode retira `experimental.session.compacting`, Muninn se rompe
igual. Datos: ~62 versiones de Claude Code en la ventana de dos meses `[P5]`, el hook de
compactación de OpenCode se llama literalmente `experimental`, y el formato de memoria
nativo **se movió tres veces dentro de la ventana** (frontmatter `modified` en 2.1.214,
semántica de truncado en 2.1.210 y 2.1.211, aviso detallado en 2.1.268) `[L1]`.
  => Reclamación honesta: **5 adaptadores sobre eventos, 0 drivers sobre formatos.**
     Aproximadamente la mitad del acoplamiento, no inmunidad.

[Q3] **Poseer un almacén tiene un coste medido, y es el mes uno.** El único registro
operativo publicado de un subsistema de memoria propio `[G3]` es también el inventario
de lo que cuesta: **85 fallos de hook registrados, 84 en las tres primeras semanas**, y
todas las familias son de la capa de almacén y captura:
  - 20 fallos de codificación de texto en Windows, el primer día, en cuatro hooks;
  - 34 fallos por un fallo de tabla virtual FTS5 más contención de bloqueo de la base
    bajo hooks concurrentes, con causa raíz en un patrón de consulta FTS5 no acotado;
  - 17 fallos de la tubería de captura: 13 abortos por escritura a un stdout muerto
    (el proceso que lanza el hook sale antes de que termine el cuerpo de `SessionEnd`)
    y 4 colisiones de chunk duplicado bajo captura concurrente;
  - 14 fallos por caracteres de control en el texto ingerido que rompen el parseo de
    cadenas de FTS5.
  => Es pagable y converge —un fallo en las últimas cinco semanas, ninguno en los
     últimos 20 días— pero hay que presupuestarlo, y es el argumento para que el almacén
     sea **mínimo**.

[Q4] **Riesgo nuevo: hay una carrera de estandarización de formato en marcha.**
  - **W3C AI Agent Memory Interoperability Community Group**: *"develop, evolve, and
    document an open protocol-level specification for AI agent memory that is portable
    across vendors, models, agent frameworks, and tool ecosystems."*
  - **arXiv:2605.11032, "Portable Agent Memory"**: protocolo abierto para transferencia
    de memoria verificada criptográficamente entre agentes heterogéneos, cubriendo
    eventos episódicos, conocimiento semántico, skills procedurales, estado de trabajo y
    preferencias de identidad. Su encuadre: *"while MCP standardizes tool access and A2A
    standardizes task delegation, no prior protocol"* estandariza la memoria.
  - Y al menos cuatro propuestas más compitiendo: PAM Specification v1.0
    (portable-ai-memory.org), Universal Memory Protocol, `memory-md-spec` de cromus-ai,
    y *"Portable Memory"* de MacPaw Research.
  => Corta en los dos sentidos. **A favor de poseer el motor:** ningún estándar ha
     ganado, así que adaptarse a formatos de vendor es perseguir una diana móvil.
     **En contra de un formato cerrado:** con un CG del W3C activo y cinco contendientes,
     un esquema propietario es una apuesta contra la estandarización.
  => Resolución: **poseer el motor, mantener el ledger exportable.** Los diferenciadores
     de Muninn viven en *campos* (condiciones de disparo, tripleta de supersesión,
     ancla, procedencia), no en un contenedor cerrado. Y los estándares de transferencia
     casi nunca especifican **semántica de cumplimiento**, que es justo donde está el
     valor de Muninn.

---

# AUDITORÍA DE VERACIDAD (iteración 8)
Ventana exigida: **12-jun-2026 → 12-sep-2026**. Objetivo: que ninguna afirmación del
dossier se apoye en algo que los datos no digan, ni en fuentes fuera de ventana sin
declararlo.

## R. Fuentes fuera de la ventana de 3 meses que el dossier usaba

| ID | Fuente | Fecha | Antigüedad | Qué se hizo |
|---|---|---|---|---|
| A1 | Chroma, *Context Rot* | jul-2025 | 14 meses | **Sustituida** por `[S1]`, que además la matiza |
| A2 | Anthropic, *Effective context engineering* | sep-2025 | 11,5 meses | Degradada a contexto histórico |
| A3 | Anthropic, *Code execution with MCP* | nov-2025 | 10 meses | El número que importa (peaje de tokens) es medición propia `[H2]`, en ventana |
| B3 | Zep, *The Retrieval Tradeoff* | dic-2025 | 9 meses | **Marcada como fuera de ventana** y acompañada de `[S2]`, en ventana y más dura |
| D7 | EncouRAGe | oct-2025 | 11 meses | Sustituida por `[S2]` para la afirmación del híbrido |
| D1, D2 | ACE, ReasoningBank | oct/sep-2025 | 11-12 meses | Ya estaban degradadas por `[J4]`; ahora además fuera de ventana |
| C5 | Envenenamiento (2606.04329, 2605.09033, 2503.03704) | jun-2026 y antes | 3+ meses | Fuera del cuerpo; solo en fuentes |
| Q4 | Portable Agent Memory 2605.11032 | may-2026 | 4 meses | Se mantiene solo como señal de la carrera de estándares, declarado |
| O2 | Hindsight, blog de Cline | 09-jun-2026 | 3 meses y 3 días | Declarado como marginalmente fuera |

## S. Fuentes nuevas encontradas en la auditoría, todas en ventana

[S1] **"The Illusion of Robustness: Aggregate Accuracy Hides Prediction Flips under
Task-Irrelevant Context"** arXiv:2607.12963 (14-jul-2026). **Corrige mi afirmación más
repetida.**
  - *"state-of-the-art models often appear robust to task-irrelevant context at the
    aggregate level: prepending it to benchmark questions causes **little change in
    overall accuracy**."*
  - *"This aggregate stability, however, masks significant **per-example instability**.
    Even semantically meaningless pseudo-words... can markedly shift model predictions on
    a small fraction of examples, **degrading performance on some while improving it on
    others**."*
  - El efecto de dos caras es consistente entre modelos y datasets, pero **los ejemplos
    afectados son específicos de cada modelo**. Modulado por tipo de contexto, longitud,
    cómputo en test-time y etapa de desarrollo del modelo.
  => **Corrección obligatoria.** "El contexto irrelevante degrada la salida" es
     demasiado fuerte para 2026: en agregado a menudo **no** baja la accuracy. El daño es
     **riesgo de cola e inestabilidad por ejemplo**, en dos direcciones. Eso no debilita
     el argumento del silencio por defecto: lo cambia de "pierdes precisión en promedio"
     a "introduces varianza impredecible y específica de tu modelo", que para una
     herramienta que promete no degradar es un argumento igual de fuerte y más honesto.
  => Y refuerza indirectamente `[J2]`: que las skills bajaran el Pass@2 **en agregado**,
     con control de longitud, es un resultado más fuerte de lo que yo lo estaba
     presentando, porque sobrevivió a la agregación que aquí borra los efectos.

[S2] **"Beyond Memory Leaderboards: Evaluating Scientific Memory as Budgeted Context
Restoration"** arXiv:2607.16848 (18-jul-2026). Ocho sistemas de memoria y recuperación
más un baseline sin recuperación, sobre dos benchmarks de texto completo.
  - *"memory leaderboards are not interpretable without the full protocol: ingestion
    granularity, raw-text preservation, retrieval budget, retrieval modality, rubric
    audit, and judge choice all affect the outcome."*
  - **"on PAIM Graphiti wins convincingly but uses 2.6M characters of retrieved context
    per query, and after controlling for retrieval budget the lead disappears."**
  - **"the sparse-dense hybrid is the single most significant intervention: hybrid
    variants of Simple RAG, Mem0, and Theoria tie for the lead within 0.03 points."**
  - Calibración multi-juez y comparación humana: los rankings de LLM-as-judge son
    consistentes entre jueces frontera y concuerdan con la evaluación humana, con una
    resolución efectiva de ~1 punto sobre 10.
  => Reemplaza a `[B3]` y a `[D7]` con datos en ventana, y es más contundente: el
     híbrido disperso-denso es la intervención que más mueve la aguja, y **el liderazgo
     de un grafo desaparece al igualar el presupuesto de recuperación**.

[S3] **Zero-Mem: Zero-Token Memory Operations for LLM Agents** arXiv:2607.29377
(31-jul-2026). **El trabajo publicado más cercano a la tesis de "sin LLM en la ruta de
lectura".**
  - *"no step outside final question answering invokes an LLM or consumes LLM input or
    output tokens"*. Solo el lector final invoca un LLM.
  - **Preserva las trazas de interacción originales como fuente de registro** — es el
    almacenamiento literal de `[K2]`.
  - Dos vistas: grafo entidad-contexto y jerarquía temporal, ponderadas por consulta.
  - **"Deterministic calibration first discards conflicting evidence"** — es una forma
    de mi F1.
  - Rendimiento competitivo, y **−57,6% de coste de tiempo en operaciones de memoria**
    frente al baseline más rápido comparado.
  => Poda parcial de F1 y confirmación de la arquitectura. Hay que citarlo y no
     presentar "sin LLM en la ruta de lectura" como una idea propia.

[S4] **"Does a Language Server Save Tokens for Coding Agents?"** arXiv:2608.13568
(29-jun-2026). **Advertencia directa contra el nivel 6 del plan (triggers de símbolo).**
  - *"The claim that semantic retrieval is more token-efficient is, we find, **asserted
    almost everywhere and measured almost nowhere**."*
  - *"The answer is conditional and **usually negative**. On symbol-named localization
    the LSP **costs tokens (+6% to +118%)** and **the agent ignores it when free**."*
  - grep resuelve los renombrados multi-fichero perfectamente; un LSP solo-de-localización
    falla tres cuartas partes.
  - Los modelos usan grep por defecto en localización (0-6% de uso semántico) pero
    recurren al LSP la mitad de las veces en tareas de referencias, sin que se lo pidan.
  - *"The implication is not LSP-always but an **adaptive router** keyed on task class,
    model capability, and lexical noise."*
  => Obliga a matizar mi frase "para código, un índice paga". `[J3]` y `[D6]` miden un
     **índice estructural o semántico para QA de repositorio y cambios multi-fichero**;
     esto mide **retrieval semántico por símbolo** y sale usualmente negativo en tokens.
     No son lo mismo, y el dossier los estaba juntando.
  => Y baja la prioridad de los triggers de `symbol`: la precisión a nivel de símbolo es
     lo que el agente ignora cuando la tiene gratis.

[S5] **"A Graph-Native Bitemporal Memory Store for Conversational AI Agents"**
arXiv:2607.26520 (29-jul-2026). Neo4j local al agente + HNSW + modelo **bitemporal
completo**: nodo de identidad inmutable enlazado a nodos de contenido versionados con dos
intervalos cerrado-abierto, tiempo válido y tiempo de transacción. Sin sobrescribir
historia.
  - LongMemEval, 60 preguntas muestreadas: la ruta de búsqueda semántica de estado actual
    da **46,7% R@10 global**, subiendo a **80% en preguntas de actualización de
    conocimiento**. La ruta de viaje en el tiempo da 80% en actualización pero **baja el
    recall de razonamiento temporal de 50% a 37,5%** por dilución de post-filtro.
  => Alguien más está construyendo lo bitemporal. Los números honestos son mediocres en
     global y **fuertes justo en el caso de supersesión**, que es el de F1. Se cita como
     arte previo y como aviso: el post-filtro puede diluir otras preguntas.

[S6] **"Ghost Echoes: Semantic Erasure Failure in Retrieval-Backed Applications"**
arXiv:2608.20352 (16-jun-2026).
  - Aunque las bases vectoriales implementan correctamente el borrado visible por API,
    **eso no garantiza borrado semántico**: el registro borrado deja influencia residual
    medible en el contexto recuperado. Deriva mediana del centroide de recuperación
    **0,1522**, superando el control del mismo clúster en **53 de 54** comparaciones
    emparejadas (p < 0,001), detectable con 61,1% de accuracy con presupuesto de 5
    consultas. **Reconstruir el índice entero no elimina la deriva.**
  => Apoya la decisión de F1 de **retirar sin borrar**, y añade un aviso para la rama
     vectorial: borrar no es borrar.

[S7] **"Measuring Stability and Failure Behavior in Language Models Under Structured
Perturbations"** arXiv:2608.22138 (22-ago-2026). 4 473 tests con compuerta de validez
sobre 100 problemas semilla, cuatro modelos, siete familias de perturbación.
  - *"two stressors expose **consistent weaknesses across all models**: **conflicting
    instructions** and questions built on an impossible premise."*
  => Apoyo nuevo y en ventana para F1 por otra vía: **una memoria caduca no retirada es,
     literalmente, una instrucción en conflicto**, y las instrucciones en conflicto son
     uno de los dos únicos estresores que rompen a todos los modelos por igual.

## T. Falacias y sobre-afirmaciones corregidas en el propio dossier

[T1] **Deslizamiento de categoría en la cifra principal.** El dossier abría con
"17,2 → 2,2 %" como número estrella de una herramienta que promete mejorar la calidad de
respuesta. Ese número es la **tasa de invocación de herramienta peligrosa bajo una
política revocada**: es seguridad y corrección bajo memoria caduca, **no calidad general**.
Corregido: se etiqueta como lo que es.

[T2] **Ninguna de las tres funciones tiene un número de resultado de tarea.** F1 se apoya
en un benchmark de seguridad, F2 en una medición de hueco más un estudio de flota de
seguridad, y F3 en mediciones de entrega y supervivencia, que son mecanismo. El único
número de resultado del corpus entero que sostiene el proyecto es DreamBench, y mide
**tener memoria**, no ninguna de las tres funciones. Corregido: se dice en el veredicto,
no solo en la autocrítica.

[T3] **"0 de 7 competidores tienen el compilador"** era una sobre-afirmación del
denominador. Verificado **en profundidad** solo para agentmemory, Rekal y Hindsight;
para Mem0, Zep, Letta y Cognee la verificación de `[K7]` cubre la revocación, no el
compilador. Corregido a "3 verificados en profundidad, 4 sin evidencia de ello, y cero
resultados en arXiv y en búsqueda de código".

[T4] **"Consolidación encendida por defecto"** en agentmemory omitía la condición. El
texto exacto es *"on by default **whenever an LLM provider is configured**"*, y la
instalación keyless es un camino soportado. Corregido en todas las menciones.

[T5] **El 4,4 %** es de **reglas de seguridad**, no de reglas en general, y el propio
estudio declara que su extracción capturó el **66,3 %** de las reglas elegibles.
Corregido.

[T6] **Cifras de vendor presentadas junto a las mías.** LoCoMo 90,57 % de Rekal y R@5
95,2 % de agentmemory son **auto-reportadas, sobre datasets distintos y sin comparación
cabeza a cabeza** — el propio README de agentmemory lo advierte. Corregido con etiqueta.

[T7] **"~62 versiones en dos meses"** es una inferencia de la numeración. Corregido a
"62 entradas de versión en el changelog".

[T8] **Mi 1,034 ms es sobre un esquema sintético**, no sobre un repositorio real: 5 000
memorias y 15 000 triggers generados, con ficheros y símbolos simulados. Corregido con
etiqueta.

[T9] **"Nadie ha publicado esa comparación, verificado con tres búsquedas"** es una
negativa universal apoyada en una búsqueda acotada. Corregido a "no aparece en este
barrido".

[T10] **"0 operaciones en 114 turnos"** se presentaba sin su tamaño de muestra. Es una
evaluación con **n=3 brazos**, hechos sintéticos sembrados por el harness, y sus propios
autores dicen que los conteos de escritura voluntaria *"measure initiative, not capture
quality"*. Corregido allí donde aparece el número.

[T11] **"Cline solo macOS y Linux"** procede de la documentación de integración de
Hindsight, no de la de Cline: la página de hooks de Cline redirige a SDK Plugins y no
declara plataformas. Corregido con atribución.

[T12] **"Para código, un índice paga"** juntaba dos cosas distintas. Corregido: índice
**estructural o semántico** para QA de repositorio y cambios multi-fichero, sí `[J3]`
`[D6]`; **retrieval semántico por símbolo vía LSP**, usualmente negativo en tokens
`[S4]`.

## U. Quinta poda: F1 y F2 tienen arte previo publicado, en ventana (iteración 9)

Encontrado al repetir la búsqueda con la terminología del campo —**autoformalización**,
**policy-as-code**, **runtime enforcement**— en vez de con la mía. **Mi afirmación de que
el compilador no existía era falsa por error de vocabulario, no por falta de búsqueda.**

[U1] **"Autoformalization of Agent Instructions into Policy-as-Code"** arXiv:2606.26649
(25-jun-2026). **Es F2.**
  - Traduce *"agent prompts, MCP tool descriptions, and natural language policy
    documents"* a **políticas formalmente verificadas**, con un bucle generador-crítico
    basado en LLM. Salida en **Cedar Policy Language**.
  - Sobre MedAgentBench, las políticas autoformalizadas *"cover substantially more of the
    source natural-language specification than the hand-coded symbolic enforcement in
    prior work"*.
  - Su encuadre coincide con el mío: los enfoques existentes son *"probabilistic
    guardrails... that offer no formal guarantees"* o *"hand-coded symbolic enforcement
    that does not scale"*.
  => F2 **no es una idea nueva**. Lo que no está hecho es aplicarlo a CLAUDE.md y
     AGENTS.md, emitir a la superficie de permisos y hooks del propio harness, y
     devolver al desarrollador el informe de cobertura. Eso es integración, no invención.

[U2] **ActPlane: Programmable OS-Level Policy Enforcement for Agent Harnesses**
arXiv:2606.25189 (23-jun-2026). **Y además cuestiona la diana de F2.**
  - Encuadre idéntico al mío: los harness llevan *"an engine that enforces safety and
    effectiveness policies, e.g., 'run tests before committing'"*, y hay que salvar un
    hueco semántico entre intención en lenguaje natural y acciones concretas del sistema.
  - **"Tool-call guardrails miss system actions that bypass the tool layer, while OS
    sandboxes control resource access instead of actions, returning opaque errors that
    confuse the agent."**
  - Su tesis: el contexto de la política vive en el agente, **pero el cumplimiento tiene
    que ocurrir en el SO para cubrir todos los caminos de ejecución**. DSL de control de
    flujo de información, implementado con eBPF. Sobrecarga 1,9 %–8,4 %. Open source.
  => **Tercera confirmación independiente** de que el cumplimiento a nivel de herramienta
     es insuficiente, y va más lejos que `[N4]`: la interceptación de tool-calls **ni
     siquiera puede observar** los caminos indirectos.
  => Consecuencia para F2: compilar a reglas de permiso de Claude Code es cumplimiento a
     nivel de herramienta, y **hereda esa limitación**. Hay que decirlo.

[U3] **VIGIL: Runtime Enforcement of Behavioral Specifications in AI Agent Skills**
arXiv:2606.26524 (25-jun-2026). Lenguaje de políticas sobre eventos agente-herramienta
con dependencias temporales, restricciones de argumentos y condiciones de flujo de valor;
evaluación simbólica que traduce políticas a restricciones SMT sobre trazas finitas.
Sobre corridas reales de agentes LLM: **>95 % de recall y <10 % de falsos positivos**.

[U4] **FAVA: Formal Authorization for Verified Agents** arXiv:2607.27267 (29-jul-2026).
Tarea en lenguaje natural → **IR de permisos** guiada por LLM → paso de lowering
determinista → grafo de permisos respaldado por evidencia con flujos de datos y etiquetas
contextuales → autorizador SMT que verifica antes de cualquier acción con efecto →
gateway que aplica el resultado o intercepta con un contraejemplo preciso.
**90,5 % de Decision Compliance Rate.**

[U5] **"Stored Is Not Supported: Typed Provenance and Assertion Guardrails for Persistent
AI Agents"** arXiv:2609.02127 (2-sep-2026, hace diez días). **Es F1, formalizado.**
  - Tesis textual: *"Persistence changes availability, not epistemic standing: **stored or
    retrieved material is not thereby supported**."*
  - **Grafo de procedencia tipado** que separa origen, linaje de dependencias, rol
    epistémico, validez y alcance de divulgación.
  - Un **resolver** devuelve un estado evidencial, banderas ortogonales de **conflicto,
    obsolescencia y retención**, y un testigo de decisión protegido. Un mediador
    generar-verificar-revisar comprueba las unidades semánticas candidatas **antes de
    liberarlas**.
  - Suite ejecutable de 24 casos de conformidad: la mediación tipada **no dejó pasar sin
    cualificar ninguna de las 19 oportunidades inseguras**, conservando los 5 controles
    soportados. Las reglas de comparación plana y por etiqueta de fuente liberaron
    **19/19 y 18/19**.
  - Su propio límite declarado: *"these results validate the encoded resolver and mediator
    obligations; they do not constitute an end-to-end evaluation of language models or
    retrieval systems."*
  => F1 tampoco es una idea nueva. Lo que no está hecho es ejecutarla **dentro de un
     harness de agente de código, en la ruta caliente, en milisegundos, sobre la memoria
     del propio harness**.

[U6] **TRUSS** arXiv:2608.17588 (18-ago-2026). Generación de skills con compuerta
estática de nueve propiedades de seguridad más un agente sombra en un entorno de ejecución
controlable con herramientas intermediadas y trazas con procedencia. 100 % de precisión y
recall en detección de vulnerabilidades; éxito de ataque 38,71 % → 19,35 %; efectividad de
tarea 17,11 % → 52,94 % con tasa de seguridad 50,80 % → 100 %.

### Consecuencia: la reclamación de novedad hay que reescribirla entera

| Función | Lo que yo decía | Lo que dicen los datos |
|---|---|---|
| F1 filtro | «Nadie hace cumplir la revocación» | Cierto **para los cinco productos medidos** en `[K7]`. Falso como afirmación general: `[U5]` lo formaliza con suite de conformidad, `[S3]` descarta evidencia en conflicto de forma determinista, `[S5]` implementa bitemporal |
| F2 compilador | «Cero en arXiv y en búsqueda de código» | **Falso.** `[U1]` autoformaliza instrucciones a Cedar; `[U4]` hace NL → IR de permisos → SMT; `[U3]` hace specs de skills → política ejecutable. Mi búsqueda usó el vocabulario equivocado |
| F3 cues | «Los tres tipos que nadie envía» | Se mantiene: `[K1]` hizo el barrido y no encontró la composición; ninguno de `[U1]`–`[U5]` lleva condiciones de disparo permanentes por memoria |

**La novedad defendible ya no es ninguna función suelta. Es la composición bajo
restricción:** ledger tipado + filtro determinista + entrega por cues + compilación de
reglas, **dentro de un harness de agente de código, en la ruta caliente, en milisegundos
de un solo dígito, con presupuesto de tokens y disciplina de evaluación de resultado**.
Cada pieza tiene arte previo publicado, casi todo de los últimos tres meses. Lo que nadie
ha hecho es juntarlas ahí y **medir resultado de tarea**.

Es una reclamación mucho más débil y mucho más honesta. Sigue siendo un proyecto real
—integrar bajo restricciones es trabajo real— pero **el argumento de venta "esto no lo
tiene nadie" está muerto** y no puede aparecer en el dossier.

## V. Sexta poda: F3 también tiene arte previo medido, y aparece un benchmark (iteración 10)

Encontrado al repetir el barrido con seis vocabularios más: **prospective memory**,
**typed intention store**, **memory middleware**, **context governance**,
**proactive retrieval**, **event-driven retrieval**.

[V1] **PM-Bench: Evaluating Prospective Memory in LLM Agents** arXiv:2607.12385
(14-jul-2026). **Un benchmark para exactamente lo que hace F3.**
  - Memoria prospectiva: *"the ability to execute an intention at a specific future cue
    or state while other activities are ongoing"*. Paradigma Virtual Week de la ciencia
    cognitiva, semana simulada de siete días, ocho LLMs bajo ocho configuraciones.
  - **El mejor método, un agente GPT-5.4, llega solo al 65,1 % de F1.**
  - *"no single strategy for improving prospective memory dominates across models."*
  => **Regalo de evaluación.** Yo declaraba que F3 no tenía forma de medirse por
     resultado. La tiene, es pública, y está en ventana.

[V2] **"Making Prospective Memory SLM-Shaped: Typed Intention Stores for Small-Model
Agents"** arXiv:2609.01272 (1-sep-2026, hace once días). **Es F3, construido y medido.**
  - Tesis: *"this loop is **schema-constrained state tracking rather than open-ended
    reasoning**, and small models can execute it when the **action space is typed**."*
  - El **Prospective Intention Store (PIS)** *"puts lifecycle logic in code and scoped
    language work on the model"*. Agéntico y **sin entrenamiento**: sin fine-tuning del
    selector, sin destilación de trayectorias.
  - Resultados sobre PM-Bench: DeepSeek-Chat con PIS llega a **82,9 % Set-F1** frente al
    65,1 % del mejor scaffold publicado. En Gemma-E2B: **4,2 % sin almacén, como mucho
    6,6 % con siete memorias retrospectivas, y 66,2 % con PIS**. Los métodos de memoria
    retrospectiva se quedan como mucho en 54,4 %.
  => Lógica de ciclo de vida en código + espacio de acción tipado + disparo por cue: es
     el mecanismo de F3. Y trae lo que a F3 le faltaba, **un número de resultado**.
  => Matiz que conservo: PIS es un scaffold **dentro del bucle del agente** para
     intenciones diferidas, no una capa de entrega del harness para agentes de código. Y
     su propio dato de que la memoria retrospectiva se queda en ≤54,4 % confirma que
     recuperar por similitud **no** resuelve la memoria prospectiva. La distinción se
     mantiene; la novedad del mecanismo, no.

[V3] **PPMF — Provenance-Preserving Memory Firewall** arXiv:2607.29167 (31-jul-2026).
Se autodenomina **"lightweight memory middleware"**, que es exactamente la posición que
yo reclamaba para Muninn.
  - Identifica el **blanqueo de procedencia en memoria**: durante la consolidación por
    LLM, una observación externa puede reescribirse como aparente historial del usuario,
    *"preserving an action trigger while erasing the low-trust source that should limit
    its authority"*.
  - *"Existing prompt filters, content sanitizers, and tool guards **do not enforce
    source-authority non-amplification** after lossy memory consolidation."*
  - Autoriza llamadas a herramienta emparejando el riesgo de la acción con la autoridad
    de las memorias relevantes. Memorias consolidadas vulnerables alcanzan **hasta 1.000
    de ASR**; con procedencia mantenida por la plataforma, confirmación y etiquetas de
    riesgo, **ninguna acción de alto riesgo no autorizada pasa la compuerta**.

[V4] **ContextNest: Verifiable Context Governance for Autonomous AI Agent**
arXiv:2607.02116 (2-jul-2026). Especificación abierta **e implementación de referencia
publicada con licencia abierta, incluido un servidor MCP.**
  - Markdown tipado con metadatos, selectores deterministas de álgebra de conjuntos, URIs
    `contextnest://`, historiales de versión encadenados por SHA-256, checkpoints de
    grafo, nodos fuente vía MCP y trazas de auditoría del consumo de contexto.
  - *"ContextNest does not replace RAG; it supplies the **governance layer beneath
    retrieval**"* — determina qué artefactos están aprobados, vigentes, atribuibles y
    verificados **antes** de que la recuperación opere.
  - **Ataque de versión obsoleta:** la selección gobernada domina a BM25 en el sentido de
    Pareto, con **97 % de tasa de aprobación frente a 93–90 %, a un tercio del coste en
    tokens de entrada**.
  - **Determinismo, corpus de 1 060 documentos:** los selectores deterministas y BM25
    devuelven conjuntos estables ante consultas idénticas repetidas (**Jaccard 1,0**),
    mientras que un baseline denso con HNSW es **no determinista en el 80 % de las
    consultas** (Jaccard medio 0,611, peor caso 0,210).
  => El número de determinismo es mejor argumento para dejar la rama vectorial fuera de
     la ruta caliente que mi propia medición de recall@10. Y es, otra vez, arte previo de
     la posición de Muninn.

### Estado real de la novedad, después de seis podas

| Función | Arte previo publicado, en ventana | Qué queda |
|---|---|---|
| **F1** filtro | `[U5]` Stored Is Not Supported (formalizado, suite de conformidad) · `[V3]` PPMF (memory middleware, ASR a cero) · `[E1]` MemStrata · `[S5]` almacén bitemporal · `[S3]` Zero-Mem (descarte determinista de conflicto) | Ejecutarlo **dentro del harness, en la ruta caliente, en milisegundos**, sobre la memoria del propio harness |
| **F2** compilador | `[U1]` autoformalización a Cedar · `[U4]` FAVA · `[U3]` VIGIL · `[U2]` ActPlane (y este dice que la capa de herramienta no basta) | Aplicarlo a **CLAUDE.md y AGENTS.md** y a la superficie de permisos del harness, con informe de cobertura |
| **F3** cues | `[V2]` Prospective Intention Store (82,9 % Set-F1, sin entrenamiento) · `[K1]` modelo cue-anchored | Llevarlo al **harness de código** en vez del bucle del agente, con presupuesto de tokens |

**Las tres funciones tienen arte previo medido, todo de los últimos tres meses.** El
proyecto es, sin ambigüedad, **un proyecto de integración**. Su única novedad defendible
es la composición dentro de un harness de agente de código, en la ruta caliente, con
presupuesto de tokens, y medida por resultado de tarea.

Y hay dos regalos que compensan: **PM-Bench** da a F3 una evaluación pública con línea
base a batir (65,1 % publicado, 82,9 % con PIS), y el número de determinismo de
ContextNest **(no determinista en el 80 % de las consultas con denso+HNSW)** es mejor
argumento para el diseño que cualquiera de mis mediciones.

## W. Séptimo barrido: la mejor evidencia de F1, y un aviso que afecta a todas las cifras

[W1] **"Insecure Coding Preferences in Long-Term Memory: Security Risks for LLM-based
Code Generation"** arXiv:2607.17619 (20-jul-2026). **La mejor evidencia de resultado para
F1 que he encontrado, y es en dominio de código.** Cuatro LLMs, cinco lenguajes.
  - Las memorias inseguras **aumentan el riesgo de generar código vulnerable entre 2,7 y
    50,3 puntos porcentuales**.
  - Crean una **brecha de aviso de 5,4 a 14,0 pp**: la tasa de advertencias va por detrás
    de la tasa de vulnerabilidad. El agente no avisa de lo que empeora.
  - Las memorias inseguras **son difíciles de sobrescribir por interacción normal** e
    influyen aunque el prompt esté formulado de otra manera.
  - Mitigaciones comparadas: añadir requisitos de seguridad y reducir el almacenamiento
    bajan la vulnerabilidad 19,7–33,6 pp **pero degradan la corrección funcional hasta
    15,9 pp**; el **filtrado a nivel de memoria alcanza 100 % de detección y restaura el
    comportamiento al baseline sin memoria**, sin ese coste.
  => Es exactamente F1, en dominio de código, con resultado medido, y muestra que las
     dos alternativas obvias cuestan corrección funcional mientras el filtro no.
     **Sustituye al proxy de ejecución de herramientas como evidencia principal de F1.**

[W2] **"What's in Your Agent's Context? Context Privilege Escalation Attacks against AI
Agent Harness"** arXiv:2609.01222 (1-sep-2026). Primer análisis sistemático del ensamblado
de contexto en **12 harness reales, incluidos Claude Code y Codex**.
  - **M-CPE**: contenido controlado por el atacante que viene de un contexto de bajo
    privilegio se incorpora a un rol de mensaje de mayor privilegio.
  - **X-CPE**: contenido controlado por el atacante **persiste más allá del contexto en
    el que se introdujo**.
  - Consecuencias documentadas: compromiso total del agente, ejecución remota de código,
    denegación de servicio e invocaciones manipuladas de herramientas o skills.
  => **Muninn es, por construcción, una máquina de X-CPE**: su función es hacer que
     contenido persista más allá del contexto donde apareció, y lo inyecta en
     `additionalContext`, que es ensamblado de contexto. Sin etiquetas de privilegio y
     procedencia en cada inyección, Muninn *es* el vector. Esto convierte el marco de
     procedencia de recomendación de diseño en **requisito de seguridad con nombre**.

[W3] **"When Does Belief-Based Agent Memory Help? Reliability-Conditional Updating and
Provenance-Capped Poisoning Defense"** arXiv:2606.22030 (20-jun-2026).
  - Ablación controlada sobre LoCoMo: la actualización bayesiana de creencias **por sí
    sola aporta poco frente a un simple last-write-wins**, porque *"existing
    conversational memory benchmarks rarely contain contradictory or differently reliable
    evidence"*.
  - Con **actualización condicionada a fiabilidad**, sobre un benchmark de contradicción
    controlada, la actualización de creencias **supera sustancialmente** a last-write-wins
    y a la recuperación de memoria cruda.
  - Y como la fiabilidad derivada del contenido es manipulable, proponen **acotar la
    confianza por procedencia de la fuente** en vez de por confianza textual: resiste
    ataques de envenenamiento volumétrico.
  - **Aviso que afecta a todo mi corpus:** cuantifican una **discrepancia de 27,5 puntos
    entre token-F1 estricto y LLM-as-judge sobre las mismas salidas**, y lo señalan como
    problema de reproducibilidad de los benchmarks de memoria a largo plazo.
  => Dos consecuencias. Una: la maquinaria sofisticada solo paga **donde la evidencia se
     contradice y difiere en fiabilidad**, que es justo el caso de F1 y no el caso
     conversacional — apoya F1 y explica por qué los benchmarks conversacionales no lo
     detectan. Dos: **toda cifra de LoCoMo o LongMemEval de este dossier, incluidas las
     de Rekal y agentmemory, hereda una incertidumbre de método de ese orden.**

[W4] **"Compaction as Epistemic Failure: How Agentic LLM Tools Fabricate Confirmed
Results from Killed Processes"** arXiv:2607.13071 (11-jul-2026). Documentado **en Claude
Code**: la salida parcial de comandos que expiran (código de salida 143) queda registrada
en los resúmenes de compactación **como resultados confirmados**, y se propaga como falso
positivo entre sesiones y versiones de modelo **sin re-verificación**. Mecanismo: *"a
conflation of observation and persistence, where information that appeared in the terminal
is treated as equivalent to information written to durable storage."*
  => Modo de fallo nuevo, concreto y del harness que nos ocupa. Y es **accionable con lo
     que ya encontré**: `PostCompact` entrega `compact_summary`, así que se puede
     contrastar lo que el resumen afirma contra los códigos de salida reales de la
     sesión. Nadie lo está haciendo.

## X. Octavo barrido: el diseño de F1 estaba sobre-especificado, y el canal de entrega es un riesgo

[X1] **"Presentation, Not Mechanism: A Render Confound in Deprecation-Aware Memory
Evaluation"** arXiv:2607.16019 (17-jul-2026). **Cambia el diseño de F1.**
  - Compara recuperación plana, **invalidación gruesa de aristas** y un
    **RevisionLedger de grano fino** sobre 2 907 preguntas de alta concordancia de
    GitHub, historiales de issues multi-repo, Wikipedia y flujos temporales tipo DyKnow.
  - **El confound central:** con un control de render igualado —mismo layout, deprecación
    desactivada— cuando un valor cambia y luego se restaura, el RevisionLedger *parece*
    batir al baseline plano por **+0,182**, pero **casi toda la ganancia viene de una
    presentación más fácil**; el residuo del mecanismo de grano fino es
    **indistinguible de cero (+0,021 a +0,025 en dos familias de juez)**.
  - **Controlada la presentación, la invalidación gruesa es el único mecanismo que paga**
    para consultas de estado actual, y **bate al ledger fino por 0,084**.
  - *"provenance mainly needs **retained invalidated evidence, not richer typing**"*.
  - Recomendación textual: *"Memory evaluations should hold render fixed, and
    deprecation-aware systems should deploy the **coarsest retained state that covers
    their queries**."*
  => **Mi diseño de F1 estaba sobre-especificado.** Yo proponía tripleta bi-temporal
     completa con `t_evento`, `t_registro`, `t_invalidación`, fuerza de evidencia separada
     de frescura y estado "no demostrable". Esto dice que el grano fino **no paga** y que
     lo que paga es **retener el registro invalidado y no servirlo**. Es exactamente el
     principio de "mínima complejidad que pasa la medición" que llevo predicando y que
     estaba a punto de violar.
  => Y añade un **tercer control obligatorio** al protocolo de evaluación, junto al de
     longitud igualada: **control de render igualado**.

[X2] **HookPry — "A Blind Trust, the Bloody Thrust: When Attacker-Controlled Hook Updates
Steer AI Agent Harnesses towards Malicious Behaviors"** arXiv:2609.03884 (3-sep-2026).
**Riesgo de primer orden sobre el canal de entrega de Muninn.**
  - Los hooks de ciclo de vida atan comandos de shell a eventos de runtime, **corren con
    privilegios del host**, y *"may fire at times the LLM never observes"*.
  - **La ruta de actualización de la configuración de hooks se confía ciegamente**: un
    plugin versionado benigno puede troyanizarse con una actualización que ate comandos
    del atacante a eventos benignos.
  - **1 000 corridas extremo a extremo sobre 25 combinaciones de harness y backend:
    compromete los siete harness evaluados, con tasas de éxito por harness de hasta
    92,5 %.**
  - **Microsoft Defender: 0 % de recall.** La unión de tres defensas estáticas se pierde
    el **47,5 %** de los artefactos maliciosos.
  => Muninn se distribuye como hooks. El canal está comprometido por diseño y las
     defensas no funcionan. Consecuencias obligatorias: versiones fijadas, artefactos
     firmados, **forma exec y nunca forma shell** en los comandos de hook, privilegio
     mínimo, y auditabilidad de cada disparo.

[X3] **"Harness Engineering: Anatomy, Architecture, and Evolution of Coding Agents — A
Source-Code Study of Eleven Systems"** arXiv:2609.00006 (15-jul-2026). **La mejor
referencia de integración que existe, y contiene el dato que más apoya el stack.**
  - Anatomía de código fuente de once harness de producción: **Claude Code, Codex CLI,
    Gemini CLI, Mistral Vibe, OpenHands, Aider, Mini-SWE-Agent, Hermes, Pi, OpenCode,
    OpenClaw**, más Omnigent como primer meta-harness. Siete subsistemas canónicos,
    13 observaciones transversales, 29 patrones de diseño recurrentes.
  - **El dato:** *"Two absences survive a threefold corpus expansion: across roughly four
    million lines of Python, TypeScript, and Rust, no agent runtime imports a
    general-purpose agentic framework, and **none retrieves code with vector
    embeddings**; the field runs on hand-rolled async loops and **deterministic
    retrieval**."*
  - **SKILL.md supera a MCP en adopción: 9/11 frente a 8/11.** ACP en seis sistemas.
  - Longitudinal, mismo trimestre: *"convergence becoming imitation and **behavioral
    policy migrating from prompt prose to configuration**"*.
  - Tesis: en la primera mitad de 2026 el harness de código *"completed a turn from tool
    to platform"*. Cierra con 18 recomendaciones y un scaffold mínimo de 90 líneas.
  => Tres consecuencias. Una: **ningún harness de producción recupera código con
     embeddings vectoriales** — confirmación máxima del stack determinista y matiz
     necesario a `[J3]`, que mide un índice semántico que nadie envía. Dos: la migración
     de política de prosa a configuración es **la tesis de F2 observada en el código
     fuente de once sistemas**. Tres: sustituye mi tabla de cinco harness por un
     inventario de once, y SKILL.md por delante de MCP confirma dónde poner la superficie.

[X4] Productos que faltaban en el mapa competitivo:
  - **claude-mem** — el plugin de memoria más popular para Claude Code (decenas de miles
    de estrellas según las fuentes secundarias, v13.x). Cinco hooks de ciclo de vida
    (`SessionStart`, `UserPromptSubmit`, `PostToolUse`, resumen, `SessionEnd`),
    **compresión semántica vía el Agent SDK de Claude**, SQLite con búsqueda de texto
    completo y Chroma. Es decir: consolidación por LLM encendida por diseño.
  - **`agent-memory`, plugin del marketplace comunitario de Anthropic**
    (`anthropics/claude-plugins-community`): hooks de auto-compresión en `PostToolUse`,
    **inyección de contexto con presupuesto de tokens en `SessionStart`**, y bus de
    memoria para compartir contexto entre agentes. Es F3 en su forma básica, **en el
    marketplace del propio vendor**.
  - **«Claude Code Dreaming»**, función en beta de Anthropic que *"curates agent memory
    between sessions"*, con formulario de alta y cabeceras beta.
  => El vendor ya tiene inyección con presupuesto por hooks en su marketplace comunitario
     y está probando curación de memoria entre sesiones. La ventana para F3 es más
     estrecha de lo que el dossier decía.

## Y. Noveno barrido: la advertencia metodológica que más importa

[Y1] **SuperLocalMemory 4.0: The Governed Memory Operating System for AI Agents**
arXiv:2608.08253 v2 (8-ago-2026). **Es a la vez un competidor directo y el paper más
honesto de todo el corpus.**

  Lo que es: sistema operativo de memoria **gobernado y local-first**, que unifica
  recuperación multicanal bajo **fusión de rango recíproco**, **recall bi-temporal**,
  aislamiento multi-alcance, control de acceso por rol, **borrado verificado** y un
  **registro de auditoría encadenado por hash**. Espina de fiabilidad en la ruta de
  escritura: admisión con valla de generación, transacciones de memoria verificables con
  responsables de aplicar, verificar, compensar y borrar por proyección, y manifiestos de
  finalización comprobables por hash. **Once escenarios de inyección de fallos, cada uno
  repetido 200 veces: 2 199 de 2 200 propiedades de componente se sostuvieron.**

  **Y lidera con un resultado negativo, en sus propias palabras:**
  > *"This version leads with a negative result. **Ten mechanisms here were implemented,
  > reachable on a live call path, and ineffective at their final connection.
  > Implemented, reachable and effective are three different questions, and the third
  > requires an oracle independent of the mechanism under test.**"*

  - Ablación de tres brazos variando solo el espacio de nombres del identificador de
    sesión del recall: **no mueve ninguna posterior con el defecto presente**, y mueve
    todos los brazos instanciados con el defecto ausente. Un control negativo que escribe
    todos los tickets pero no aporta engagement **no resuelve nada**.
  - **Retiran una cifra de sobrecarga publicada en su versión anterior** porque *"the two
    paths it differenced are not comparable"*.
  - Medición en su sitio: **escritura gobernada de 11,0 ms, de la que el envelope es el
    70,6 %**, pero la valla de generación cuesta **1,9 µs** y el ledger de obligaciones
    **42 µs**. Conclusión textual: ***"The cost is durability, not governance."***

  => **Tres consecuencias, y la primera vale más que cualquier otro hallazgo de esta
     investigación.**

  **1. «Implementado, alcanzable y efectivo son tres preguntas distintas.»** Muninn puede
  construir las tres funciones, verlas dispararse en la telemetría, y que no hagan nada.
  Diez mecanismos de un sistema con 2 199 de 2 200 propiedades verificadas estaban
  **implementados, en la ruta de llamada viva, y eran inefectivos en su conexión final**.
  La única defensa es un **oráculo independiente del mecanismo bajo prueba**, que es
  exactamente lo que el experimento de cuatro brazos tiene que ser — y explica por qué la
  telemetría de disparos, que era mi instrumento previsto, **no basta**.

  **2. El coste es la durabilidad, no la gobernanza.** Sus primitivas de gobernanza
  cuestan microsegundos; los milisegundos se los lleva escribir con garantías. Coincide
  con mis mediciones —evaluación de cues 1,034 ms, ingesta 275 ms asíncrona— y confirma
  dónde poner el presupuesto: la ruta de lectura puede ser barata, la de escritura hay que
  sacarla del camino crítico.

  **3. Otro competidor en la posición exacta de Muninn**, con RRF, bi-temporal, auditoría
  encadenada y borrado verificado, local-first. Súmese a agentmemory, Rekal, Hindsight,
  claude-mem, `agent-memory` de Anthropic, PPMF y ContextNest.

[Y2] Ataques sobre skills, en ventana, que refuerzan el argumento del ruido:
  **SkillBloat** arXiv:2608.21929, ataques de amplificación de tokens por inyección de
  skills; **ColluSkill** arXiv:2608.09732, composición adversarial entre skills para
  evadir los escáneres; **GitSkills** arXiv:2608.10906, dataset de skills en GitHub;
  **"Who Maintains Agent Skills?"** arXiv:2609.05677, estudio longitudinal de
  mantenimiento; **"Towards a Systems Foundation for Agentic Skills"** arXiv:2608.29596.

### Señal de convergencia

Nueve barridos con vocabularios distintos. Los cinco primeros produjeron cambios de plan
—las seis podas, la corrección de arquitectura, la simplificación de F1—. Los dos últimos
produjeron **una advertencia metodológica y competidores adicionales, pero ningún cambio
de diseño**. El rendimiento marginal de seguir barriendo con este método ha caído.
Lo que queda no se resuelve leyendo más papers: se resuelve corriendo el experimento.

---

# MEDICIÓN EN REPOSITORIO: la rama densa, cerrada por los dos lados (20-sep-2026)

Sondas de mecanismo corridas en este repositorio, no barridos de literatura. Cierran la
pregunta "¿un embedding de la consulta en la ruta de lectura mejora lo que se entrega?",
que `[I5]` había dejado abierta con una cifra de auto-recuperación. Las tres sondas se
borraron después de responder; los números quedan aquí.

[Z1] **El coste de carga se puede esquivar, pero no basta.** `[I2]` mide 35 ms de carga
de `potion-base-8M` y 0,006 ms de encode, y concluye que el modelo no cabe en el hook.
El modelo es una tabla estática de 29 528 × 256 f32 en `model.safetensors`, contigua: una
consulta de 13 tokens necesita 13 filas de 1 KB, no las 29 528. Medido, 20 repeticiones,
p50, misma máquina que `[I3]`:
  | ruta                                        | p50       |
  | carga completa del modelo (lo de hoy)       | 34,93 ms  |
  | solo el tokenizer (`tokenizer.json`, 684 KB)|  6,71 ms  |
  | header safetensors + 13 filas por `seek`    |  0,003 ms |
  => La carga se puede bajar 5x, y **todo el residuo es parsear el tokenizer**. Pero
     6,71 ms sigue siendo ~2x el hook completo de `[I3]` (3,5 ms) y ~8x el compuerteado
     (0,86 ms). Esquivar la tabla no alcanza; habría que reimplementar WordPiece contra
     un vocabulario en SQLite, y eso solo se justifica si la calidad paga. No paga: `[Z2]`.

[Z2] **En la entrega real no hay margen que ganar.** `[I5]`'s +6,7 % es auto-recuperación
(la consulta es la cola del propio cuerpo del registro), que no es la tarea. Sonda con
consultas no degeneradas: los 10 prompts de `revocation/tasks-revocation-public.json`
—escritos para otra rejilla, en palabras distintas a las del registro— contra un pajar de
440 registros activos (430 exportados del almacén vivo de este repositorio más los 20
sembrados). Objetivo por tarea: el registro activo del par sembrado. k = 8, el que pide
`hook.rs`. Fusión RRF k=60.
  | rama             | objetivo en el top-8 |
  | léxica (BM25)    | **10/10**            |
  | densa sola       | 4/10                 |
  | híbrida RRF      | 10/10                |
  => El léxico ya está en el techo: no hay nada que el denso pueda añadir, y solo saca
     4/10 por su cuenta. 10 consultas no estiman una población; lo que la sonda descarta
     es que exista margen medible aquí, que es la pregunta que se hizo.

[Z3] **Y en la ruta de escritura, donde el modelo es gratis, el coseno no separa.** El
cuello real no es la consulta: es la detección de supersession. Réplica determinista, sin
modelo, de la conversación de siembra del brazo `muninn-latest` de h2h v2 (20 turnos, log
en `results/h2h-v2/seeding/r0-muninn-latest.jsonl`) a través de `muninn ingest`: **2/10
pares quedan retirados del todo**, 4 eventos de supersession. La réplica predice todos los
fallos del grid: async-runtime, version-scheme, compression, password-hashing e
internal-http fallan aquí y fallaron allí; cache-eviction y license se detectan aquí y
pasaron allí. `extract::replaces` exige dos palabras de contenido compartidas, y
"semver is cleaner" no comparte ninguna con "for versioning, we're using calver".
  Un coseno entre la frase nueva y la vieja sería el arreglo natural —el modelo ya está
  cargado en la ruta asíncrona, `[I2]` dice que allí no cuesta—. Medido sobre los 10 pares
  contra sus 90 cruces:
  | el par verdadero rankea primero | 4/10           |
  | media verdadera / media cruzada | 0,247 / 0,125  |
  | mínima verdadera / máxima cruzada | **−0,036 / 0,408** |
  => Separado en media, inservible en la cola: no existe umbral. "Wire format: msgpack"
     contra "Switching to cbor - it's more compact" da **−0,036**. Un modelo estático de
     promediado de tokens no sabe que cbor sustituye a msgpack; los tokens técnicos raros
     se parten en subpalabras sin relación aprendida.
  => La rama densa queda cerrada por los dos lados. El cuello de F1 —enlazar una frase de
     cambio con el registro que sustituye cuando no comparten palabras— sigue abierto y
     **no se resuelve con embeddings**. Lo que le falta para atacarse es un corpus de
     negativos: sin medir el falso retiro, cualquier regla más laxa que la actual esconde
     decisiones vigentes, que es el fallo más caro que puede tener F1.

[Z4] **El coseno entre los nombres es peor que entre las frases: la rama densa queda
cerrada también por ahí.** `[Z3]` cerró la ruta de embeddings midiendo frases enteras. Quedaba
una objeción razonable: una frase de cambio es sobre todo prosa del *porqué* ("cuts down on
boilerplate", "less infrastructure to babysit"), y lo que identifica la decisión es el nombre
del producto. Si `gRPC` y `ConnectRPC` están cerca en el espacio del modelo aunque sus frases
no lo estén, bastaría con embeber `extract::name_tokens` en vez de la oración.
  Medido con el modelo model2vec que ya ships, sobre el set de desarrollo del loop 5
  (`crates/muninn-bench/examples/name_cosine.rs`, commitado), 26 pares con nombre en ambos
  lados contra todos sus cruces:
  | el par verdadero rankea primero | **1/26** (frente a 4/10 con frases) |
  | mínima verdadera / máxima cruzada | **−0,061 / 1,000** |
  => Peor que la medida que pretendía mejorar. El cruce máximo llega a 1,000 porque dos
     escenarios distintos comparten un token de nombre tras la normalización, y la verdadera
     mínima es negativa. No hay umbral, ni siquiera separación en media utilizable.
  => Consecuencia: la ruta de embeddings para supersession está cerrada **por los dos
     extremos** —frase `[Z3]` y nombre `[Z4]`— con medida propia en el repositorio. El
     sidecar se queda donde estaba: ruta de escritura, para `muninn why`.

[Z5] **El techo léxico: 23 de 30 pares no comparten ninguna palabra de contenido.** Antes de
tocar umbrales convenía saber cuánto queda al alcance de cualquier regla léxica. Sonda
`muninn-capture/examples/supersede_probe.rs` (commitada) sobre el set de retención del loop 6,
imprimiendo lo que `replaces_text` ve en cada par: **23 pares con 0 palabras compartidas, 6 con
1, 1 con 2**. Bajar el suelo de 2 a 1 se implementó y se midió: loop 6 en orden adyacente se
quedó en 15/30, sin mover una sola celda, y se revirtió.
  => Ninguna regla sobre solapamiento de palabras alcanza una intersección vacía. Junto con
     `[Z3]` y `[Z4]`, esto acota dónde *no* está la mitad que falta de la detección: ni en las
     palabras ni en los embeddings. Lo que queda es un modelo en la ruta de escritura, excluido
     por `docs/scope.md` con razones medidas `[C1]` `[K10]`.
  => Corolario metodológico, medido en el mismo loop: relajar la puerta de anáfora daba +8 en
     el set de desarrollo (loop 6, 15/30 → 23/30) y **+0** en el de retención (loop 7, 19/30 en
     ambos casos). Se retiró. Lo único que replicó fue una guarda de precisión —nunca retirar
     un registro que era él mismo un cambio—, que en orden de bloques sube `kept_b` de 15/30 a
     27/30 en retención, y que corrige una pérdida silenciosa, no un fallo de recall.

[Z6] **Lo que parecía la única ventaja frente a la competencia era un artefacto de
concurrencia, y una rejilla controlada lo desmontó.** La detección empata con claude-mem
`[h2h v2]`. Buscando dónde no empata, `h2h/cost_analysis.py` (commitado) midió el coste por
celda sobre las dos rejillas existentes —post-hoc, sobre rejillas registradas para otra
pregunta— emparejando por (run, tarea), razón de medianas con bootstrap de 10 000 sobre pares:
  | rejilla | reloj muninn-latest / claude-mem | coste | turnos |
  |---|---|---|---|
  | v2 (celdas concurrentes) | 0,599 [0,516, 0,943] | 0,776 [0,613, 1,066] | 0,667 [0,571, 1,000] |
  | v1 (celdas concurrentes) | 0,583 [0,435, 0,832] | 0,820 [0,663, 1,074] | 0,800 [0,571, 1,000] |
  | **v3 (celdas en serie, reloj pre-registrado)** | **0,972 [0,681, 1,368]** | 1,094 [0,801, 1,532] | 1,000 [0,833, 1,500] |
  => Dos rejillas coincidían en ~0,59 y la tercera, que es la única en la que el reloj se
     registró como desenlace **antes** de correr, da 0,97 con el intervalo cruzando el 1. La
     afirmación no se hace.
  => Lo único que v3 cambió a propósito fue `--jobs 1`. Con celdas concurrentes, una celda
     lenta retiene su hueco más tiempo y deja al otro brazo corriendo con menos contención; las
     celdas de claude-mem son más lentas (su siembra tardó unas cuatro veces la de Muninn en
     esta máquina). El efecto vivía en el planificador, no en las herramientas.
  => Lección, y es la que importa: **dos rejillas de acuerdo no son una réplica si comparten el
     defecto.** El control del desenlace —v1 tenía ambos brazos en techo de acierto— descartaba
     la explicación por resultado y no decía nada sobre la contención, que era la verdadera.
  => Queda en pie, sin tocar: el coste del hook en sí, `perf --strict`, sin modelo y medido
     aparte. Eso nunca dependió de esta comparación.

[Z7] **El "empate" con claude-mem no es evidencia de que no haya diferencia: es una rejilla
cuatro veces demasiado pequeña para saberlo.** La cifra publicada es v2: 17/27 contra 14/27,
Holm p = 0,58. La rejilla v3 —corrida para otra cosa, misma redacción de retención, mismo
oráculo, mismo análisis— repitió la dirección y la distancia: **23/27 contra 19/27**, los mismos
cuatro aciertos de diferencia. Agrupadas (agrupación post-hoc, no un contraste registrado):
  | | aciertos | tasa |
  |---|---|---|
  | muninn-latest | 40/54 | 0,741 |
  | claude-mem | 33/54 | 0,611 |
  | diferencia | +0,130 | Fisher bilateral p = 0,217 |
  => Potencia necesaria para ese tamaño de efecto, al 80 % y α = 0,05 bilateral:
     **204 celdas por brazo** — 408 celdas de tarea más unas 920 sesiones de siembra
     (23 runs por brazo, 20 mensajes cada uno). Las rejillas que se han corrido tienen 27 y 54.
  => Consecuencia para la redacción pública: decir "empate" es correcto y decir "no hay
     diferencia" no lo es. Lo honesto es "no distinguible con 27 celdas por brazo, y harían
     falta unas 204 para distinguirlo si el efecto es el que insinúan las dos rejillas".
  => La diferencia apunta consistentemente a favor de Muninn en las dos. Eso **no** es una
     afirmación: dos rejillas apuntando igual es exactamente lo que `[Z6]` demostró que puede
     ser un artefacto compartido. La única forma de convertirlo en número es la rejilla grande,
     registrada con su N fijo y su regla de parada antes de correr.

[Z8] **Lo que la competencia no hace es F2, no F1 — y esto es evidencia débil, por su forma.**
Revisión de documentación pública (2026-09-21): claude-mem
(`docs.claude-mem.ai/hooks-architecture`) captura, comprime e inyecta contexto por hooks de
ciclo de vida; Mem0 (`github.com/mem0ai/mem0`) y Zep/Graphiti (`getzep.com/platform/graphiti`)
son capas de memoria y recuperación; Letta y LangGraph Store, lo mismo con otro reparto.
  => **Ninguna documenta compilar reglas escritas en controles que el harness aplique en la
     frontera de herramienta.** Eso es F2, y es una diferencia de categoría, no una victoria de
     benchmark: son herramientas de memoria y el control es otra cosa. Con el resultado de
     comportamiento de Gate 5b (8/24 contra 0/24 con el control puesto), es lo único de esta
     sesión que se sostiene frente a la línea competitiva, y se sostiene como "hacemos algo que
     ellos no", no como "lo hacemos mejor".
  => **Corrección a una suposición propia:** Graphiti sí invalida hechos superados ("as facts
     change, Graphiti invalidates the old ones"). O sea que "tirar lo que reemplazaste" **no**
     es exclusivo de Muninn; ya lo decían `[U5]` `[S3]` `[S5]` y por eso la afirmación de
     novedad se reescribió en su día. Lo que sigue siendo distinto en F1 no es la idea sino su
     forma: en Muninn la garantía es de construcción —el tipo que lleva una tarjeta al contexto
     tiene un solo constructor y lee de la vista filtrada— y no un reordenamiento de
     resultados. Eso está medido aparte (`fault.rs` s16/s17).
  => **Peso de esta entrada: bajo.** Ausencia en la documentación no es medición. Basta con que
     una de esas herramientas tenga la función sin documentarla, o la añada mañana, para que
     caiga. No se cita en el README ni en `docs/claims.md` como ventaja; vive aquí, fechada y
     con sus fuentes, para que se pueda revisar.

[Z9] **El techo léxico se rompe por fuera del lenguaje: el repositorio.** `[Z5]` acotó dónde
*no* está la mitad que falta de la detección —ni en las palabras ni en los embeddings— y
concluyó que lo que quedaba era un modelo en la ruta de escritura, excluido por `docs/scope.md`
`[C1]` `[K10]`. Quedaba una tercera opción que no se había probado: no mirar las frases, mirar
el código. "HashiCorp Vault" y "AWS Secrets Manager" no comparten nada *como texto*; comparten
algo *como hechos sobre un repositorio*: uno está en él, y después no.
  Implementado en `maintain` (ruta de escritura, sin modelo, sin red): se leen los diffs de los
  commits capturados; un valor que un commit sacó del código y que ningún archivo versionado
  contiene ya retira el registro que lo nombraba. Medido con `experiment/loop8/eval_all.py` en
  dos conjuntos retenidos generados después de congelar el binario, 30 celdas por brazo, con el
  asunto del commit mudo (`update dependencies`) para que el commit solo aporte su diff:
  | conjunto | orden | `talk` | `both` | retirada `talk` | retirada `code` |
  |---|---|---|---|---|---|
  | loop 8 | adyacente | 9/30 | 15/30 | 17/30 | 23/30 |
  | loop 8 | bloques | 0/30 | 11/30 | 6/30 | 23/30 |
  | loop 9 | adyacente | 8/30 | 16/30 | 17/30 | 29/30 |
  | loop 9 | bloques | 1/30 | 15/30 | 5/30 | 29/30 |
  => **La señal no depende del orden.** `code` da el mismo número tanto si la revisión sigue a
     la decisión como si llegan diez decisiones en medio; `talk` cae de 17/30 a 5-6/30. Todas
     las reglas léxicas del motor dependen de que las dos frases estén cerca, porque es lo
     único que las relaciona cuando no comparten palabras. Un commit las relaciona por valor.
  => **Precisión: 0/30 retiradas falsas** en ocho condiciones (dos conjuntos × dos órdenes ×
     dos estilos de asunto). Un commit que cambia algo que ninguna decisión menciona no retira
     nada.
  => **Lo que el asunto del commit vale por separado.** Con un asunto que nombra el valor nuevo
     (`use nats`), loop 9 da 30/30: el asunto contesta la pregunta él solo. La cifra pública es
     la del asunto mudo. El control está corrido y publicado precisamente porque la diferencia
     es grande.
  => **Límite, y es el mismo de siempre por el otro lado:** una decisión que no llega a un
     archivo no deja rastro que leer, y ahí sigue mandando `talk`. El techo de la conversación
     no se ha movido; se ha añadido un segundo camino para los casos que sí tocan el código.
  => Tres defectos que esta rejilla encontró y que no son del mecanismo: git interpreta
     `--since=@0` como *ahora* (una tienda nueva nunca capturaba su propio historial); la
     herencia de tema reimprimía el valor retirado cuando era minúscula, porque el filtro era
     `name_tokens` (fuga de F1, 6 celdas); y el hash de un commit es parte del texto indexado,
     así que una rejilla con commits de fecha real no es determinista (una celda de treinta
     cambiaba entre corridas).

[Z10] **Tres ejes que no son el acierto, medidos sobre la misma rejilla: uno en contra y dos a
favor.** La comparación con claude-mem siempre se ha hecho sobre "¿nota que cambiaste de
idea?" `[Z7]`. La siembra de la rejilla v4 permite leer otras tres cosas del mismo material,
con el mismo modelo, las mismas veinte frases y sin instrumentar a nadie.
  => **Contexto ocupado: perdemos.** Sobre `hook_additional_context` —la carga que con certeza
     es contexto del modelo— tres rejillas coinciden: Muninn ocupa **1,6 a 2,1 veces** lo de
     claude-mem (2,090 [2,061, 2,122] en v1; 1,612 [1,558, 1,751] en v2; 1,700 [1,555, 1,896]
     en v3; 30 celdas emparejadas cada una). Por celda en v2: Muninn 1 410 caracteres al
     arrancar la sesión y 860 más en el prompt; claude-mem 1 380 al arrancar y nada en el
     prompt. Es una decisión de diseño —entregamos en cada prompt, no solo una vez— y es un
     coste. **La primera versión de esta medición daba 0,452 a nuestro favor y era errónea:**
     sumaba el registro del hook, cuyo `stdout` repite el contexto para una herramienta que lo
     devuelve por stdout (Muninn) y no para una que no (claude-mem). Contaba nuestra propia
     inyección dos veces. Una celda es un solo prompt, así que la razón describe una sesión de
     un turno y no se extrapola.
  => **Reproducibilidad: ganamos, y por construcción.** Mismos veinte mensajes, tres veces:
     Muninn guarda 23, 23 y 23 registros con Jaccard 0,917 / 0,917 / 1,000 —y el único ítem que
     difiere era **un defecto nuestro**, el id de sesión ordenado entre las palabras del tema,
     encontrado por esta comprobación y corregido con una prueba que falla sin el arreglo.
     claude-mem no comparte **ningún** texto entre dos corridas (Jaccard 0,000). Como el
     Jaccard exacto es duro con una herramienta que escribe títulos en prosa, se mide también
     la cobertura, que no depende de la redacción: de los 19 valores sembrados, Muninn tiene
     17, 17 y 17 (los 2 que faltan son los que retiró bien) y claude-mem 7 y 10, **y no los
     mismos**: una corrida se quedó con `async-std` y perdió `gzip`, `zstd`, `LRU` y `bcrypt`;
     la otra al revés.
  => **Latencia de la ruta de escritura: ganamos, con reserva.** El paso `settle` de la rejilla
     espera a que la memoria termine de procesar el turno. claude-mem: mediana **24,5 s** por
     mensaje, máximo 177 s, 53 minutos para 40 mensajes. Muninn: `maintain` completo, 1-2 s
     sobre este repositorio, y el ingest en sí 6 ms `[perf --strict]`. **Reserva:** las celdas
     corrieron con `--jobs 2`, y `[Z6]` demostró que una medida de reloj con concurrencia puede
     ser un artefacto del planificador; además ambas rutas son asíncronas, así que lo que esto
     describe es **cuánto tarda la memoria en estar lista**, no cuánto espera el usuario. Con
     esa reserva, el orden de magnitud no lo explica el planificador: el propio registro de
     claude-mem dice a qué espera ("Pool limit reached (2/2)"), que son sus llamadas al modelo.
  => Peso: la primera fila es una medición nuestra en nuestra contra y está en `docs/claims.md`
     como tal. La segunda está pre-registrada y leída después de escribir el lector. La tercera
     es una observación con su reserva y no se cita como afirmación.

[Z11] **Un modelo en la ruta de escritura, medido de principio a fin: entiende la relación, no
paga en la rejilla.** `[Z5]` dejó como única vía restante un modelo en la ruta de escritura.
Esa vía se midió en cinco pasos, todos pre-registrados en `experiment/PREREGISTRATION.md`
(judge-v1 a v42), solo en CPU y sin modelos solo-inglés. Kev-4B quedó fuera por eso: su tarjeta
declara `language: en`.
  => **judge-v1 (sí/no por par, 9 810 pares, cinco idiomas).**
     - Qwen3.5-4B y Gemma 4 E4B separan reemplazos de trampas con AUC 0,993 y 1,000 en 60
       pares escritos para atrapar errores: preguntas, hipótesis, negaciones, otro equipo.
     - mDeBERTa-NLI, laya-multilingual y Qwen3.5-2B quedan en 0,60–0,80.
     - Ninguno rescata nada con cero retiros falsos: "let's use Unleash instead" puntúa ~1
       contra *cualquier* registro. El par solo no dice a qué se refiere "instead"; es la
       pregunta de `[Z3]` otra vez.
  => **judge-v2 (elegir entre todos los candidatos).**
     - Resuelve el "cuál": acierta el registro en 155/189 (en) y 88/105 (es).
     - El fallo se mueve a otro sitio: lee "trata del mismo tema" como "lo reemplaza".
  => **judge-v3 (elegir y confirmar, con la respuesta del asistente, set nuevo).**
     - Qwen3.5-9B en modo *preguntar* (marca `conflict`, no retira) rescata todo lo que las
       reglas perdían en en, es, fr y de, con precisión 17/20 y 8 s p95 por mensaje.
     - En modo retirar deja un falso.
  => **v42 (el h2h, siembra separada, inglés y español).**
     - Con respuestas reales el juez marca más pares falsos que verdaderos en inglés (42/24).
     - En español, donde las reglas solo retiran 4–7 registros por store, la rejilla lee **46/54
       con juez contra 48/54 sin él** (p = 0,78).
     - La build de v41, sin modelo, ya lista lo dicho de más nuevo a más viejo y el agente
       toma el valor vigente aunque no se retire nada.
  => **Cerrado para este benchmark.** El techo de retiro de `[Z5]` es real, pero la entrega de
     v41 lo vuelve irrelevante en celdas, y el modelo añade marcas falsas y un runtime de 6 GB.
     El código se retiró; la evidencia y el arnés quedan en `experiment/judge/`. Reabrirlo
     exige una medición nueva donde la entrega sola no baste.
  => Queda como herramienta: `run_h2h.py --seed-order separated` y el fixture `v8es`, que
     miden el caso que las reglas no alcanzan.
