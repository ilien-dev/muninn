# Muninn — conclusión de la investigación

Fecha: 2026-09-12. Toda afirmación numérica está en `00-evidence-log.md` con su
fuente; las marcadas `[H*]` las medí en esta máquina y los scripts están en
`research/bench/`.

---

## Veredicto en una página

**Los MCP de memoria que existen hoy son, en su mayoría, ruido — y hay número que
lo demuestra, no opinión.** El servidor de referencia oficial cobra ~3 000 tokens
de peaje en cada petición y su búsqueda es un `includes()` sobre el grafo entero:
con 20 000 entidades devuelve 5,2 millones de tokens si buscas "the" y 11 tokens si
buscas "system design" `[H2]`. Los productos serios (Mem0, Zep) sí tienen ingeniería
real detrás, pero la comparación entre ellos es inservible: tres cifras distintas
(84% / 58,44% / 75,14%) para el mismo sistema sobre el mismo dataset de **diez
conversaciones** `[B1][B2]`. Y en el propio paper de Mem0, un baseline que mete la
conversación entera en el contexto gana a Mem0 (~73% vs ~68%) `[B1]`.

**El fallo no está en guardar ni en buscar. Está en consolidar.** Cuando un LLM
reescribe experiencias en una "memoria consolidada" turno a turno, la utilidad sube,
luego baja, y **acaba por debajo de no tener memoria**. Consolidando desde soluciones
correctas, GPT-5.4 falla el 54% de los problemas ARC-AGI que ya había resuelto sin
memoria. Guardar los episodios crudos, sin consolidar, iguala o duplica el
resultado `[C1]`. Codex implementa exactamente el patrón que ese trabajo
desaconseja (`extract_model` + `consolidation_model`), y por eso probablemente lo
tiene desactivado por defecto y marcado experimental `[F2]`.

**El hueco real no es "recordar cosas del usuario".** Es esto, y es medible:

1. La compactación borra restricciones vigentes. Violación de políticas: 0% con la
   política en contexto → **30% tras compactar, 59% en algunos modelos**. Si la
   restricción sobrevive al resumen, 0% `[C2]`.
2. La similitud vectorial **no distingue una contradicción de una reformulación**:
   AUROC 0,59, casi azar. RAG sirve valores superados entre el **15% y el 40%** de
   las veces `[E1]`.
3. El agente vuelve a proponer lo que ya existe, incluso con la documentación
   delante `[G3]`.
4. Nadie vigila la propia memoria: se degrada en silencio `[G3]`.

**La aproximación que la evidencia sostiene:** no un servidor de memoria, sino un
**harness de continuidad local, determinista y en proceso**, enganchado al ciclo de
vida del agente (hooks), con silencio por defecto y un presupuesto de tokens duro.
Lo construí lo justo para medirlo. El hook real en Rust — arrancar el proceso, abrir
el índice, aplicar la compuerta de intención, consultar BM25 sobre 50 000 registros y
emitir el resultado — cuesta **4,07 ms p50 / 4,31 ms p95**, y **0,49 ms** en los turnos
que no piden nada `[I3]`. Frente a los 79 ms del MCP oficial con 20 000 entidades
`[H2]`, a los 149-241 ms de Zep en producción `[B3]`, y a los 2,5 s de mediana que
reporta el único precedente local publicado con esta misma arquitectura, que se los
gasta en un servidor de embeddings `[G3]`.

---

## 1. Cómo funcionan por dentro los MCP de memoria

Hay cuatro familias, y la diferencia entre ellas está en el **camino de escritura**,
no en el de lectura.

### 1.1 Grafo de conocimiento plano en fichero — `@modelcontextprotocol/server-memory`

Es la implementación de referencia y la que más gente instala. Modelo de datos:
`Entity {name, entityType, observations[]}` y `Relation {from, to, relationType}`,
serializados como JSON Lines.

Lo que hace de verdad, leído del código fuente:

```ts
// loadGraph(): en CADA operación
const data = await fs.readFile(this.memoryFilePath, "utf-8");
const lines = data.split("\n").filter(l => l.trim() !== "");
for (const line of lines) { item = JSON.parse(line); ... }

// searchNodes(): sin ranking, sin límite
graph.entities.filter(e =>
  e.name.toLowerCase().includes(query.toLowerCase()) ||
  e.entityType.toLowerCase().includes(query.toLowerCase()) ||
  e.observations.some(o => o.toLowerCase().includes(query.toLowerCase())))
```

Consecuencias, medidas `[H2]`:

- **O(n) por consulta, con parseo completo.** p50 de 3,9 ms (1k entidades) a 79 ms
  (20k) a 384 ms (144k). Lineal, sin excepción.
- **Sin ranking ni `LIMIT`.** Devuelve el subgrafo entero que coincide. Con 20k
  entidades: 5,17 M de tokens para "the", 87 500 para "project", 14 200 para
  "database".
- **Matching de subcadena.** "system design" devuelve 11 tokens: dos palabras
  seguidas casi nunca aparecen literalmente. Es la peor combinación: explota con
  términos frecuentes y no encuentra nada con lenguaje natural.
- **9 tools = 10 750 caracteres de JSON schema ≈ 3 000 tokens** en cada petición.

Veredicto: **ruido**, y ruido caro. No es utilizable más allá de unas cientos de
entidades.

### 1.2 Pipeline de extracción + consolidación por LLM — Mem0, OpenMemory, Codex memories

Camino de escritura en dos fases: un LLM extrae "hechos, decisiones o preferencias"
de los mensajes; un segundo paso decide si el hecho nuevo es ADD, UPDATE, DELETE o
NOOP contra lo que ya hay. Almacenamiento en un vector store, opcionalmente con un
grafo encima. Lectura: búsqueda vectorial filtrada por `user_id` / `agent_id` /
`run_id`, con `expiration_date` opcional `[F2 y docs de Mem0]`.

Cifras publicadas: ~66,9% en LoCoMo, 0,71 s de latencia mediana, ~1 800 tokens por
conversación; reruns independientes bajan a 58-66% `[B1]`.

Los tres problemas son estructurales, no de implementación:

- **La consolidación degrada.** Es literalmente el mecanismo que `[C1]` mide
  cayendo por debajo del baseline sin memoria.
- **Un LLM en el camino de escritura** significa coste y latencia por turno, y
  no determinismo: los mismos hechos producen memorias distintas según el
  calendario de actualización `[C1]`.
- **La invalidación se apoya en similitud**, que no sabe distinguir contradicción
  de duplicado (AUROC 0,59) `[E1]`.

Veredicto: ingeniería real, tesis equivocada. Útil para personalización blanda
("prefiere respuestas cortas"), peligroso para hechos operativos.

### 1.3 Grafo de conocimiento temporal — Zep / Graphiti

Es el diseño más sofisticado del mercado. Graphiti construye un grafo bi-temporal:
cada arista lleva un hecho con **dos ejes de tiempo** (cuándo ocurrió el evento y
cuándo lo supo el sistema), de modo que un hecho superado se invalida en vez de
borrarse. Recuperación en un solo paso (`one-shot`), sin bucles iterativos,
combinando búsqueda full-text, vectorial y recorrido del grafo, con reranking
opcional `[Zep arXiv:2501.13956, D/B3]`.

Su propio estudio de 50 experimentos es el mejor dato público que existe sobre el
compromiso real `[B3]`:

| Config | Accuracy | Tokens de contexto | p50 recuperación | Contexto insuficiente |
|--------|----------|--------------------|------------------|----------------------|
| 5/2    | 69,62%   |   347              | 149 ms           | 23,8%                |
| 15/5   | 77,06%   |   756              | 199 ms           | 16,9%                |
| 20/20  | 80,06%   | 1 378              | 241 ms           | 13,2%                |
| 30/30  | 80,32%   | 1 997              | 189 ms           | 13,5%                |

De 5/2 a 20/20: **+10,4 puntos por 4x los tokens**. De 20/20 a 30/30: **+0,26 puntos
por 1,5x**. Y el dato que casi nadie cita: la latencia **nunca fue el cuello de
botella** (p50 < 250 ms siempre). Lo que se paga son tokens.

Veredicto: no es ruido. Pero es un servicio de red con un grafo caro de mantener,
resolviendo un problema (memoria conversacional multi-sesión de un asistente) que
no es el problema de un agente de código. Y su propia métrica de "completeness"
demuestra que con poco contexto **el agente acierta adivinando** (70% de accuracy
con 57% de completeness), que es exactamente lo que no queremos.

### 1.4 Memoria como sistema operativo — MemGPT / Letta

Jerarquía de memoria inspirada en la paginación: contexto "core" siempre presente,
memoria de archivo paginada dentro y fuera mediante llamadas a función que el propio
agente emite, con interrupciones para ceder el control `[MemGPT arXiv:2310.08560]`.

Veredicto: la abstracción correcta históricamente, pero hoy la implementan los
propios harness (compactación, subagentes, ficheros de memoria). Montar un MemGPT
encima de Claude Code o Codex es duplicar una capa que ya existe.

### 1.5 Ficheros Markdown + índice — basic-memory, claude-mem, SIx Harness

Notas en Markdown en disco, indexadas en SQLite con FTS5 y vectores, editables por
humano y por agente. Es la familia que más se acerca a lo correcto: el dato es un
artefacto legible y versionable, no una fila opaca en una base de datos.

Veredicto: dirección correcta. El problema de las implementaciones actuales es la
**latencia de la vía semántica** (embeddings contra un servidor local: mediana 2,5 s,
máximo 10,3 s `[G3]`) y la ausencia de un modelo de invalidación.

---

## 2. Por qué la mayoría es ruido: los cinco fallos, con número

| # | Fallo | Evidencia | Magnitud |
|---|-------|-----------|----------|
| 1 | Peaje de contexto sin contrapartida | `[A3][H2]` | 3 000 tokens/petición solo por las definiciones de tools; 150k→2k tokens al exponer tools como ficheros |
| 2 | Contexto irrelevante degrada la salida | `[A1]` | 18 modelos, ~194k llamadas: "performance consistently degrades with increasing input length"; los distractores pegan más cuanto más largo el input |
| 3 | La consolidación por LLM corrompe lo que consolida | `[C1]` | Cae por debajo del baseline sin memoria; 54% de regresión en ARC-AGI desde soluciones correctas |
| 4 | La invalidación por similitud no funciona | `[E1]` | AUROC 0,59; 15-40% de hechos superados servidos como vigentes |
| 5 | Superficie de ataque nueva | `[C5]` | ShadowMerge: 93,8% de éxito contra memoria en grafo (evaluado sobre Mem0); "agents designed to write and retrieve memory more aggressively are more exploitable"; las defensas de prompt injection no cubren esto |

Y un sexto que no es de la memoria sino del que la usa: **"retrieved-but-unused"**.
En el registro de 8 meses de `[G3]`, tres sesiones consecutivas volvieron a proponer
construir un subsistema que ya existía, **con la documentación de diseño inyectada en
el contexto cada vez**. Recuerdo perfecto no impidió la redundancia. Un canal que el
lector ha aprendido a saltarse es peor que no tenerlo.

---

## 3. Qué sí funciona, con número

| Enfoque | Ganancia medida | Fuente |
|---------|-----------------|--------|
| **Contexto como playbook con updates incrementales** (ACE): nada de reescribir, solo deltas estructurados | +10,6% en agentes, +8,6% en finanzas, menor latencia de adaptación | `[D1]` |
| **Memoria de razonamiento destilada de éxitos Y fallos** (ReasoningBank) | Supera a guardar trayectorias crudas y a guardar solo rutinas exitosas, en web y en software engineering. Único de tres sistemas auditados que sube utilidad **sin** subir la tasa de ataque | `[D2][C4]` |
| **Skills procedurales desde trayectorias** (Trace2Skill, CODESKILL) | +9,69 pass rate sobre no-skill en SWE-bench/Terminal-Bench; transferencia entre escalas y familias de modelo (+57,65 pp en un caso) | `[D3][D4]` |
| **Estructura del harness, no prosa** (AHE) | Terminal-Bench 2: 69,7% → 77,0%, superando a Codex-CLI (71,9%). Ablación: la ganancia está en **tools, middleware y memoria de largo plazo**, no en el system prompt | `[D5]` |
| **Índice estructural del código dentro del harness** | Ganancia de resolve estadísticamente separada y **menor $/solved que agentic grep**, con Claude Opus 4.7 fijo y sandbox auditado | `[D6]` |
| **BM25 híbrido** | Mejor resultado en los 4 datasets; el reranking solo aporta mejora marginal a cambio de latencia | `[D7]` |
| **Supersesión determinista bi-temporal** | Hechos obsoletos servidos: 15-40% → ~0%, sin umbral de similitud y sin llamada a LLM | `[E1]` |
| **Fijar invariantes fuera de la compactación** (Constraint Pinning) | Violaciones: 30-59% → 0% | `[C2]` |
| **Compuertas de precisión en la inyección** | 3 inyecciones en 10 días, todas relevantes, **0 falsos disparos en 54 turnos sin intención**; antes de las compuertas, ~4 de cada 5 disparos eran ruido | `[G3]` |

El patrón que emerge es uno solo: **lo que funciona es determinista, incremental,
verificado por ejecución y silencioso por defecto.** Lo que no funciona es
probabilístico, reescritura completa, auto-juzgado y siempre encendido.

---

## 4. Cómo funcionan los agentes hoy, y qué hueco queda

Independientemente del harness, un agente de código hoy es: *un LLM usando tools en
bucle*, con cuatro mecanismos de continuidad `[A2]`:

1. **Ficheros de instrucciones precargados** — CLAUDE.md, AGENTS.md, `.claude/rules/`.
   Cargados en cada sesión. La recomendación oficial es <200 líneas porque
   "longer files consume more context and reduce adherence" `[F1]`.
2. **Compactación** — resumir y reiniciar. Es lossy, bloqueante (decenas de segundos)
   y no determinista: lo que se retiene varía entre corridas `[C3]`.
3. **Note-taking / ficheros de memoria** — Claude Code ya lo automatiza: auto memory
   con `MEMORY.md` como índice (200 líneas / 25 KB cargados) y ficheros de tema leídos
   on-demand `[F1]`. Codex tiene el equivalente experimental y apagado `[F2]`.
4. **Subagentes** — contexto limpio, devuelven 1 000-2 000 tokens `[A2]`.

**Lo que ya está resuelto y no hay que reconstruir:** guardar notas en Markdown,
cargar un índice al arrancar, compactar, delegar en subagentes. Cualquier proyecto
que empiece por ahí llega tarde.

**Lo que sigue roto, y es exactamente el hueco:**

| Hueco | Por qué la línea base no lo cubre | Evidencia del daño |
|-------|-----------------------------------|--------------------|
| **Recuperación con ranking y presupuesto** | La auto memory es un índice plano; "el modelo lee MEMORY.md y decide qué fichero abrir". Sin ranking, sin tope por turno, y el índice se corta a 200 líneas | Con un proyecto grande el índice se satura y lo que queda fuera simplemente no existe `[F1]` |
| **Supervivencia de invariantes a la compactación** | La compactación es un resumen por LLM; nada garantiza que una restricción sobreviva | 0% → 30-59% de violaciones `[C2]` |
| **Invalidación de hechos obsoletos** | Solo hay un timestamp `modified`. Nada retira un hecho superado | 15-40% de hechos superados servidos `[E1]` |
| **Anti-recurrencia** (decisiones tomadas, callejones sin salida) | No existe como categoría en ningún harness | Tres sesiones seguidas reconstruyendo lo mismo `[G3]` |
| **Procedencia y verificación** | Una nota de auto memory no distingue "lo dijo el usuario" de "lo dedujo el modelo" de "lo confirmó un test" | Regla de arbitraje necesaria entre redescubrimiento y decadencia `[G3, principio 4]` |
| **Salud de la propia memoria** | No hay instrumentación | El fallo que hace invisibles a los otros tres `[G3]` |
| **Portabilidad entre harness** | CLAUDE.md ≠ AGENTS.md ≠ memories de Codex | 60k+ repos ya usan AGENTS.md `[F3]` |

---

## 5. La aproximación: Muninn como harness de continuidad, no como servidor de memoria

### 5.1 Las cinco decisiones de arquitectura

**1. En proceso y local. Sin servidor, sin red, sin LLM en la ruta de lectura.**
Justificación medida: el hook completo cuesta 4,07 ms p50 `[I3]`; el mismo trabajo
con un servidor de embeddings local cuesta 2,5 s de mediana `[G3]`; con un servicio
remoto, 149-241 ms `[B3]`.

**Corrección que salió de medir en vez de suponer.** Mi primer diseño metía la rama
vectorial en el hook síncrono. Al portarlo a Rust apareció el coste real: cargar la
tabla de embeddings cuesta **106 ms** (potion-retrieval-32M) o 35 ms (potion-base-8M)
*por proceso*, mientras que codificar la consulta ya cargada cuesta 0,008 ms `[I2]`.
Un hook es un proceso nuevo en cada invocación: la carga del modelo es entre 25x y
100x más cara que toda la búsqueda. La rama vectorial no puede vivir en la ruta
caliente. Y el precio de sacarla es pequeño: BM25 solo alcanza el **93,3%** del
recall del híbrido `[I5]`.

**2. Hooks primero, tools después.** El peaje de 3 000 tokens por definiciones de
tool `[H2]` se paga en cada petición, se use o no la memoria. Los hooks cuestan cero
tokens cuando no disparan. Claude Code y Codex exponen el **mismo** ciclo de vida —
`SessionStart` (con matcher `compact`), `UserPromptSubmit`, `PreCompact`,
`PostCompact`, `PreToolUse`, `PostToolUse`, `SessionEnd` — y ambos inyectan por
`additionalContext` con un tope configurable `[F2]`. Un solo binario sirve a los dos.
La superficie MCP se reduce a **dos tools opcionales** para cuando el agente quiera
tirar del hilo deliberadamente: `muninn_recall(query, budget)` y
`muninn_why(symbol|decision_id)`.

**3. Episodios crudos como evidencia de primera clase; consolidación explícita y
con compuerta.** Es la recomendación literal de `[C1]`: *"treat raw episodes as
first-class evidence and gate consolidation explicitly rather than firing it after
every interaction"*. Muninn nunca reescribe un episodio. Añade capas por encima
(decisión, callejón sin salida, invariante, skill) que **apuntan** al episodio y
conservan el enlace. Nada se consolida sin que un humano o una verificación por
ejecución lo confirme. El ratio de rechazo del triaje en `[G3]` (59 de 154
decisiones, 47 de 186 callejones) no es desperdicio: es la curaduría funcionando.

**4. Silencio por defecto, con presupuesto duro.** Cascada de rechazos, la barata
primero: compuerta de intención (regex; corta el 80-85% de los turnos en 3 µs `[H5]`)
→ candidatos → compuerta de relevancia por IDF acumulado sobre términos raros →
amortiguación por sesión → **tope de tokens duro**. `[G3]` muestra que sin compuerta
el bloque disparaba en 4 de cada 5 turnos con intención y el lector aprendió a
saltárselo; con compuerta, 3 disparos en 10 días, 0 falsos. Y `[G1]` es el aviso
opuesto: pasado un umbral de andamiaje simbólico, el modelo se repliega a razonar
sobre los símbolos y se desconecta de la realidad. El presupuesto no es una
optimización, es una restricción de corrección.

**5. Determinismo en toda decisión consecuente.** Qué se inyecta, qué se invalida,
qué se marca en rojo: aritmética sobre evidencia tipada, reconstruible a posteriori.
El modelo no juzga la salud ni la relevancia de su propia memoria. Esto no es
purismo: es lo que hace que `[E1]` funcione (supersesión por tripleta, no por
similitud) y lo que hace auditable el sistema frente a `[C5]` (envenenamiento).

### 5.1-bis Dónde vive cada etapa (consecuencia directa de `[I2]`)

| Momento | Qué corre | Coste medido | Modelo cargado |
|---------|-----------|--------------|----------------|
| `UserPromptSubmit` (ruta caliente, cada turno) | Compuerta de intención → BM25 top-8 por IDF → compuerta de relevancia → presupuesto | **4,07 ms p50**, 0,49 ms si la compuerta corta `[I3]` | **No** |
| `SessionStart` / `PostCompact` (una vez) | Reinyección de invariantes + render curado + health gate | ~110 ms una sola vez, aceptable fuera del bucle | Sí |
| `SessionEnd` (asíncrono) | Ingesta de la traza, embeddings, propuesta de decisiones y callejones | 275 ms por sesión de 200 registros `[I4]` | Sí |
| Background (`async = true` en Codex; hook en background en Claude Code) | Validadores de obsolescencia, re-embedding, `optimize` del índice | fuera del camino crítico | Sí |
| Opcional: demonio residente `muninnd` + socket unix | Híbrido completo también en la ruta caliente | el hook pasa a ser un cliente de ~1 ms | Sí, una vez |

El demonio es una optimización, no un requisito: **4 ms sin él ya cumple el
contrato**. Se añade solo si la evaluación demuestra que ese 6,7% de recall extra
del híbrido `[I5]` cambia resultados de tarea.

### 5.2 Las cinco cosas que Muninn guarda (y que ningún harness guarda hoy)

Ordenadas por relación valor/coste, no por elegancia:

| Tipo | Qué es | Cuándo se escribe | Cuándo se lee | Respaldo |
|------|--------|-------------------|---------------|----------|
| **Invariante** | Regla que debe sobrevivir a todo: "nunca tocar `schema/v1`", "los tests de integración necesitan Redis" | Explícito, o al detectar una corrección del usuario repetida | Reinyectado en `PostCompact` y `SessionStart(compact)`, siempre, sin compuerta | `[C2]`: es la diferencia entre 59% y 0% de violaciones |
| **Decisión** | Qué se decidió, por qué, qué se descartó, y el commit/PR que lo materializa | `SessionEnd`, propuesto y triado; o explícito | Cuando el prompt toca los símbolos o ficheros que la decisión nombra | `[G3]`, `[D1]` |
| **Callejón sin salida** | Qué se intentó, por qué no funcionó, con el error real | Al detectar revert, test que sigue rojo, o el usuario diciendo "no" | Al detectar intención de construir algo que ya se intentó | `[G3]`: el fallo de redundancia es el más caro que documentan |
| **Skill procedural** | Procedimiento reutilizable extraído de una trayectoria que **pasó verificación** | Solo tras señal de ejecución verificable (tests verdes, build, PR mergeado) | Al coincidir la tarea | `[D2][D3][D4]`: +9,69 pass rate; transferible entre modelos |
| **Episodio** | La traza cruda, comprimida, con su hash de commit | Siempre, en `SessionEnd`, en background | Solo cuando algo de arriba apunta a él | `[C1]`: es lo que impide que la consolidación destruya la evidencia |

Todo se escribe también como **Markdown en el repo** (`.muninn/`), versionable y
legible. SQLite es el índice, no la fuente de verdad. Si Muninn desaparece, quedan
los ficheros.

### 5.2-bis Cómo se inyecta (el fallo de "recuperado pero no usado")

Recuperar bien no basta. En el registro de `[G3]`, tres sesiones consecutivas
volvieron a proponer construir algo que ya existía **con la documentación de diseño
en el contexto cada vez**. Y `[D5]` localiza la ganancia de su harness en "tools,
middleware y memoria de largo plazo", no en el system prompt, y concluye que
"factual harness structure transfers while prose-level strategy does not".

De ahí tres reglas de formato, no de recuperación:

1. **Bloques tipados y accionables, no prosa.** Tres tipos y solo tres:
   `NO-RECONSTRUIR: <subsistema> existe en <ruta> (decisión <id>, commit <sha>)`,
   `DATO POSIBLEMENTE OBSOLETO: <hecho> registrado el <fecha>, <fichero> cambió desde
   entonces`, y `LINAJE: <decisión> → <decisión> → <commit>`. Cada bloque nombra un
   artefacto verificable. Un bloque sin ruta, id o sha no se emite.
2. **Referencia, no contenido.** El bloque da el identificador y una línea; el agente
   abre el fichero si lo necesita. Es el patrón *just-in-time* de `[A2]` y lo que
   convierte 150 000 tokens en 2 000 en `[A3]`.
3. **El invariante se re-lee, no se inyecta.** `[G3, principio 6]`: lo que debe
   sobrevivir a la compactación necesita un artefacto que el flujo de trabajo obligue
   a revisitar, porque el contexto inyectado pasivamente se hojea. Muninn escribe los
   invariantes en un fichero y hace que el `PostCompact` obligue a leerlo, en vez de
   confiar en que el bloque inyectado se lea.

### 5.3 Modelo de invalidación

Cada hecho tipado se guarda como `(sujeto, relación, objeto, t_evento, t_registro,
t_invalidación, procedencia)`. Cuando entra un hecho con el mismo `(sujeto, relación)`
y distinto `objeto`, el anterior se retira **por regla, no por umbral** `[E1]`.
Además, tres validadores baratos corren en background y marcan hechos como sospechosos
sin borrarlos:

- un símbolo citado que ya no existe en el repo (`rg` sobre el árbol),
- un fichero referenciado que cambió desde `t_registro` (mtime + hash),
- una decisión cuyo commit fue revertido (`git log`).

Un hecho sospechoso **se inyecta con su marca de sospecha**, no se oculta. La regla
de arbitraje es la jerarquía de `[G3, principio 4]`: código ejecutándose y salida de
terminal > hechos derivados en esta sesión > documentos y memoria > conocimiento del
modelo. Los niveles cargan obligaciones **opuestas**: lo derivado esta sesión se usa
sin re-derivar (anti-redescubrimiento); lo que viene de memoria antigua se verifica
antes de actuar (anti-decadencia).

---

## 6. Stack tecnológico, con la justificación medida de cada pieza

| Capa | Elección | Por qué, con dato |
|------|----------|-------------------|
| **Lenguaje** | **Rust** — verificado con prototipo, no supuesto | Binario único de 9,1 MB, sin dependencias en la máquina del usuario. Frente a Python, misma etapa: encode 6,8x más rápido, BM25 3,2x `[I1]`. El proceso completo del hook: 4,07 ms `[I3]`. Node queda descartado por arranque en frío (3,75 s de handshake medidos, aunque incluya `npx`) `[H2]`; Python, por arranque del intérprete e imports. Go es la alternativa válida |
| **Almacén** | **SQLite** (un fichero por proyecto) con WAL | Índice de 50k registros = 92 MB, construido en 1,46 s `[H1][H5]`. Cero administración. Es lo que usa el único precedente operativo publicado `[G3]` |
| **Léxico** | **FTS5 + `bm25()`** integrado en SQLite | p50 0,071 ms a 50k, 0,091 ms a 144k, escalando sublinealmente `[H1]`. Hybrid BM25 gana en los 4 datasets de `[D7]`. Sin dependencia externa |
| **Selección de términos** | **Top-8 por IDF** antes de tocar FTS5 | La mayor palanca de toda la ruta: 21,53 ms → 4,65 ms `[H6]`. Mayor que cualquier optimización del índice vectorial |
| **Semántico** | **Embeddings estáticos** (`model2vec-rs`, crate oficial; potion-retrieval-32M general, `potion-code-16M-v2` para código) — **fuera de la ruta caliente** | Encode en caliente: **0,008 ms** `[I2]`. Indexar 144k registros: 2,3 s a 62 683 reg/s `[H3]`. Sin red neuronal en inferencia, sin servidor, sin GPU. Pero cargar la tabla cuesta 35-106 ms por proceso `[I2]`: por eso corre en `SessionEnd`, en background o en el demonio, nunca en `UserPromptSubmit` |
| **Búsqueda vectorial** | **Producto punto exhaustivo** sobre una matriz en RAM (f32), sin índice ANN | 4,38 ms a 50k `[H6]`. Por debajo de ~100k registros un índice ANN añade complejidad y riesgo de recall sin ganar nada. Cuantizar a int8 **no ayuda** salvo con un kernel SIMD propio: en numpy resultó 2,5x más lento `[H3]` |
| **Fusión** | **RRF (k=60)** | +2,8 pp de recall@10 sobre BM25 y mejor MRR que ambas ramas por separado, medido `[H4]`; coincide con `[D7]`. Coste: 0,044 ms `[H6]` |
| **Reranking** | **Ninguno por defecto**, opcional detrás de un flag | `[D7]`: mejora marginal, latencia mayor. `[D8]`: el reranker es el coste dominante mientras BM25 se mantiene en 82 ms sobre 24M de documentos. No cabe en el presupuesto |
| **Integración** | **Hooks** de Claude Code y Codex (mismo ciclo de vida), + 2 tools MCP opcionales | Los hooks cuestan 0 tokens cuando no disparan; 9 tools cuestan 3 000 tokens siempre `[H2]`. Codex inyecta por `additionalContext` con `additionalContextLimit` `[F2]` |
| **Formato en disco** | **Markdown + frontmatter YAML** en `.muninn/`, SQLite como índice derivado y reconstruible | `[F3]`: 60k+ repos ya viven en AGENTS.md. `[G3, principio 6]`: "continuity must be an artifact, not a habit" |
| **Consolidación** | **Ninguna automática.** Un LLM local pequeño solo propone; nada entra sin confirmación humana o verificación por ejecución | `[C1]`: la consolidación automática es el mecanismo que hunde la utilidad por debajo del baseline |

**Lo que explícitamente NO va en el stack, y por qué:**

- **Base de datos vectorial dedicada** (Qdrant, Weaviate, Pinecone, Chroma):
  un proceso más, un puerto más, un fallo silencioso más, para reemplazar 4 ms de
  numpy `[H6]`.
- **Base de datos de grafos** (Neo4j, FalkorDB): `[G3]` la descartó tras 8 meses
  con la razón correcta: *"our failures were never 'the data model was insufficiently
  expressive' and always 'some part of the pipeline was quietly wrong'. Complexity
  spent on representation is complexity unavailable for reliability."*
- **Servidor de embeddings** (Ollama, TEI): es la causa raíz de la mediana de 2,5 s
  y el máximo de 10,3 s de `[G3]`, y de dos de sus tres incidentes de producción
  (resolución IPv6 a un servidor IPv4, y recarga doble del modelo por desajuste de
  parámetros).
- **Grafo temporal completo tipo Graphiti**: la parte que aporta (invalidación
  bi-temporal) se consigue con una tabla y una regla de supersesión `[E1]`; el resto
  es coste de construcción por LLM en cada escritura.

---

## 7. Contratos de servicio (lo que hay que poder afirmar y medir)

Sin estos números el proyecto no se distingue de lo que ya existe:

| Contrato | Objetivo | Medido hoy en el prototipo |
|----------|----------|----------------------------|
| p95 del hook completo, turno con intención | < 10 ms | **4,31 ms** (Rust, proceso entero, 50k) `[I3]` |
| p95 del hook, turno sin intención | < 1 ms | **0,56 ms** `[I3]` |
| Ingesta de una sesión completa (asíncrona) | < 1 s | 275 ms por 200 registros `[I4]` |
| Tokens inyectados por turno | ≤ 700, tope duro | por construcción `[H5]` |
| Falsos disparos sobre turnos sin intención | 0 | por construcción; hay que instrumentarlo con denominador completo `[G3, principio 2]` |
| Peaje de tokens en reposo | ≤ 400 (2 tools) frente a los 3 000 del MCP oficial | `[H2]` |
| Supervivencia de invariantes a la compactación | 100% | reinyección incondicional en `PostCompact` `[C2]` |
| Hechos superados servidos | ~0% | supersesión determinista `[E1]` |
| Fallos silenciosos del subsistema | 0 | heartbeat por invocación antes de trabajar `[G3, principio 2]` |

Y el contrato de degradación: **si la rama vectorial falla, la búsqueda degrada a
BM25 y registra la degradación; nunca bloquea el hook** `[G3, principio 7]`.

---

## 8. Cómo se prueba que sirve (y no solo que es rápido)

Rápido es fácil de demostrar y ya está demostrado. Útil no. El protocolo mínimo,
tomado prestado de `[G3, §7]` porque es el único pre-registrado que existe:

- **A1 — inyección apagada, 14 días.** Detección de intención, compuertas y
  telemetría siguen activas: cada inyección suprimida se registra como
  *would-have-fired*, así el denominador sobrevive. Métrica primaria: **incidentes
  de re-propuesta** (un turno con intención de construir cuyo registro suprimido
  nombraba un subsistema existente **y** cuya sesión arrancó una implementación
  solapada). La entrega no cuenta como prevención.
- **A2 — health gate apagado, 14 días.** Métrica: tiempo desde la primera evidencia
  de una degradación hasta su remediación.
- **A3 — consolidación encendida vs apagada** sobre el mismo conjunto de tareas.
  Es la réplica directa de `[C1]` en un dominio de código, y no la ha hecho nadie.

Y una evaluación de recuperación propia, sobre trazas reales del proyecto, no sobre
LoCoMo. `[B2]` es la demostración de por qué: tres cifras para un sistema sobre diez
conversaciones. La regla de `[B3]` es la correcta: *"Benchmarks tell you about
benchmark performance. Your application is where the tradeoff actually matters."*

---

## 8-bis. Orden de construcción, por fuerza de evidencia y coste

No todos los cinco tipos de memoria de §5.2 están respaldados igual de bien. Este es
el orden honesto: primero lo que tiene evidencia causal y cuesta poco.

| Orden | Qué | Fuerza de la evidencia | Coste | Por qué va aquí |
|-------|-----|------------------------|-------|-----------------|
| **1** | **Invariantes + pinning en `PostCompact`** | **Fuerte.** 1 323 episodios, 7 familias de modelo, con mitigación validada: 30-59% → 0% `[C2]` | Bajo: un fichero, un hook, sin índice | Es la única pieza donde el efecto está medido con brazo de control y la mitigación es exactamente la que se implementa. Si Muninn solo hiciera esto, ya valdría |
| **2** | **Recuperación BM25 con compuertas y presupuesto** | **Fuerte** en la parte de coste (`[A1]`, `[B3]`, `[H*]`, `[I*]`), **N=1** en la parte de utilidad `[G3]` | Bajo: 4 ms medidos, ya prototipado | La infraestructura de la que cuelga todo lo demás. El riesgo no es que sea lenta, es que sea ruido: por eso las compuertas entran en la v1, no después |
| **3** | **Supersesión determinista bi-temporal** | **Fuerte.** AUROC 0,59 para la alternativa por similitud; 15-40% → ~0% con la regla `[E1]` | Medio: una tabla, una regla, tres validadores baratos | Sin esto la memoria empeora con el tiempo por construcción. Es la diferencia entre un activo y un pasivo |
| **4** | **Skills procedurales con verificación por ejecución** | **Fuerte.** +9,69 pass rate `[D4]`, transferencia entre modelos `[D3]`, único diseño auditado que no aumenta la superficie de ataque `[D2][C4]` | Alto: exige señal de verificación fiable (tests, build, PR) | Es la mayor ganancia publicada, pero solo si la compuerta de verificación es real. Sin ella se convierte en el SkillOpt de `[C4]`: más capacidad y más ASR |
| **5** | **Anti-recurrencia (decisiones y callejones sin salida)** | **Débil: N=1, sin control, auto-reportado** `[G3]` | Medio: exige triaje humano continuo | El fallo que describe es caro y creíble, pero la evidencia es una anécdota bien instrumentada. Va al final y detrás del experimento A1 |
| **6** | Episodios crudos indexados | Fuerte como *sustrato* `[C1]`, sin utilidad directa medida | Bajo | Es lo que hace que los cuatro anteriores puedan apuntar a evidencia en vez de a un resumen |

Regla de parada: **no pasar de un nivel al siguiente sin correr su medición.** El
proyecto no necesita más arquitectura; necesita brazos de control.

## 9. Autocrítica: qué invalidaría esta conclusión

Cuatro cosas, en orden de probabilidad:

**1. Que el harness absorba el hueco.** Claude Code ya pasó de "escribe tu CLAUDE.md"
a auto memory con índice y ficheros de tema, en menos de un año `[F1]`. Si añade
ranking, presupuesto por turno y pinning de invariantes, Muninn queda reducido a la
invalidación determinista y la anti-recurrencia. **Mitigación:** construir sobre
hooks y ficheros Markdown, no sobre un formato propietario, de modo que lo que sí
sobreviva se pueda desacoplar. Y priorizar lo que el vendor tiene menos incentivo a
hacer: procedencia verificable y auditabilidad.

**2. Que el andamiaje haga daño neto.** `[G1]` es el contraejemplo serio: 391
sesiones, y más estructura simbólica **empeoró** el resultado hasta que redujeron el
volumen de instrucciones un 75%. Mi diseño responde con silencio por defecto y tope
duro, pero eso es una hipótesis, no un resultado. **Es la razón de ser del
experimento A1, y hay que correrlo antes de escalar el sistema.**

**3. Que la evidencia más alineada sea débil.** `[G3]` es el precedente que más se
parece a lo propuesto y es **N=1, sin brazo de control, auto-reportado, con etiquetas
adjudicadas por los propios autores, y sin medir su propio coste** (minutos de triaje,
tokens inyectados, mantenimiento). Sus autores lo dicen ellos mismos. No es evidencia
causal; es una existencia de prueba. Lo que sí es fuerte y no depende de él:
`[A1]` (18 modelos), `[C1]` (regresión reproducible), `[C2]` (1 323 episodios,
7 familias), `[E1]` (AUROC calibrado), `[B3]` (50 corridas), y mis mediciones `[H*]`.

**4. Que la calidad de los embeddings estáticos no baste.** Medido: potion-retrieval-32M
llega al 81,7% de all-MiniLM-L6-v2 en retrieval `[H3]`, y en mi propio protocolo el
híbrido solo saca +2,8 pp de recall@10 a BM25 `[H4]`. Si en trazas reales de código el
vector no aporta, la respuesta correcta es **quitarlo**, no cambiarlo por uno más caro:
BM25 solo cuesta 4,65 ms y la ruta baja a ~5 ms. El diseño debe permitir apagar la
rama vectorial sin tocar nada más.

Un quinto riesgo, más lejano pero peor: **compartir memoria entre personas abre la
superficie de envenenamiento** `[C5]`. ShadowMerge saca 93,8% de éxito contra memoria
en grafo. El propio `[G3]` reconoce que su despliegue de un solo operador esquiva ese
problema. **Decisión: Muninn nace mono-usuario y local. La memoria compartida por
equipo requiere procedencia firmada y control de admisión, y es una v2, no una feature.**

---

## 10. Lo que NO hay que construir

- Otro grafo de conocimiento genérico de entidades y relaciones. `[H2]` y `[G3]` ya
  cerraron esa vía.
- Un pipeline de extracción y consolidación por LLM en cada turno. `[C1]`.
- Un servicio de red o un contenedor. `[H5]` frente a `[B3]`.
- Una base de datos vectorial dedicada por debajo de ~100k registros. `[H6]`.
- Un MCP con muchas tools. `[A3][H2]`.
- Un sustituto de CLAUDE.md o AGENTS.md. Hay que escribir en ellos, no competir.
- Persecución de puntuación en LoCoMo. `[B1][B2]`.

---

## 11. Frase de trabajo

> Muninn no es memoria. Es **continuidad**: un harness local y determinista que
> guarda decisiones, callejones sin salida, invariantes y procedimientos verificados
> junto al código que los produjo; los devuelve en menos de 5 ms **solo cuando el
> turno lo justifica y dentro de un presupuesto de tokens duro**; retira lo obsoleto
> por regla y no por parecido; y se vigila a sí misma para no morir en silencio.

Los tres criterios del encargo, contestados:

- **Rendimiento:** 4,07 ms p50 / 4,31 ms p95 en el hook completo (proceso incluido),
  0,49 ms cuando la compuerta corta. Medido en Rust sobre 50 000 registros, no
  estimado `[I3]`. El límite no lo pone la búsqueda: lo pone arrancar un proceso.
- **Herramienta de verdad, no ruido:** silencio por defecto con denominador
  instrumentado, presupuesto duro, invariantes que sobreviven a la compactación, y
  supersesión determinista. Cada una de esas cuatro decisiones responde a un fallo
  medido, no a una intuición.
- **Tecnologías adecuadas:** Rust + SQLite/FTS5 + embeddings estáticos (fuera de la
  ruta caliente) + RRF, sin servidores, sin LLM en la ruta de lectura, sin base de
  datos vectorial, sin grafo. Cada exclusión tiene su número al lado, y la única
  decisión que cambié durante la investigación —sacar los vectores del hook
  síncrono— la cambió una medición, no un argumento `[I2][I5]`.
