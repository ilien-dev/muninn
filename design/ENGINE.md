# Muninn — especificación del motor de memoria

Estado: diseño, no implementado. Fecha: 2026-09-12. Toda referencia `[X]` apunta a
`research/00-evidence-log.md`. Las cifras `[H*]` `[I*]` `[M2]` son mediciones propias.

## 0. La decisión, y lo que fija

Muninn lleva **su propio motor de memoria, siempre**. No detecta, no lee y no adapta la
memoria nativa del harness ni la de ningún otro producto. Un solo paquete, un solo
almacén, un solo esquema. Los únicos adaptadores que existen son los de **eventos** del
harness —cinco, delgados— porque no hay forma de evitarlos: es donde el harness deja
entrar código `[Q2]`.

Consecuencia operativa en Claude Code: la instalación escribe `autoMemoryEnabled: false`
en `.claude/settings.json` del proyecto (ámbito de proyecto, documentado `[O5]`), con un
flag para no hacerlo. Codex ya trae `features.memories` apagado. OpenCode, Pi y Cline no
tienen nativa. Lo que se pierde al apagar la nativa —topes duros, carga del índice al
arrancar, compartido entre worktrees— lo cubre el motor, y está en esta especificación.

**La función objetivo del motor no es "la más completa": es la mínima complejidad que
pasa una medición con oráculo independiente `[Y1]`.** Cada característica de este
documento tiene un dato detrás. Las que no lo tienen, no están.

## 1. Principios, cada uno con su dato

| # | Principio | Dato que lo fija |
|---|---|---|
| P1 | **Almacenamiento literal. Ninguna consolidación por LLM.** | Almacenar literal ya cobra la mayor parte del beneficio: 45,56 % vs 11,67 % sin memoria, y ningún diseño bate a otro `[K2]`. Consolidar cae por debajo de no tener memoria: 54 % de regresión `[C1]`. Zero-Mem: operaciones de memoria sin LLM, −57,6 % de tiempo `[S3]`. |
| P2 | **Recuperación determinista. Ningún vector en la ruta caliente.** | Once harness de producción, cuatro millones de líneas: ninguno recupera con embeddings `[X3]`. Denso+HNSW es no determinista en el 80 % de consultas (Jaccard 0,611) frente a 1,0 de BM25 `[V4]`. Cargar un modelo de embeddings cuesta 35–106 ms por proceso `[I2]`; BM25 solo da el 93,3 % del recall del híbrido `[I5]`. |
| P3 | **Invalidación gruesa. Retener, marcar, no servir. Nunca borrar.** | Con control de render, el ledger de grano fino tiene residuo cero y la invalidación gruesa lo bate por 0,084 `[X1]`. Borrar de un índice vectorial no borra semánticamente `[S6]`. La revocación hay que aplicarla en la lectura, no en el almacén `[K7]` `[N4]`. |
| P4 | **Procedencia estampada en la escritura, y confianza acotada por procedencia.** | Sin ella, la consolidación lava la fuente y el ASR llega a 1.000 `[V3]`. La confianza derivada del contenido es manipulable; acotarla por fuente resiste envenenamiento volumétrico `[W3]`. Inyectar memoria es X-CPE por construcción `[W2]`. |
| P5 | **Topes duros en todo lo que crece.** | Los ficheros de instrucciones crecen +226 % y no se borran `[K4]`. Los topes nativos (200 líneas / 25 KB) son la única defensa que existe `[O5]`. |
| P6 | **Escritura fuera del camino crítico. Lectura en milisegundos de un solo dígito.** | "The cost is durability, not governance": 11 ms de escritura gobernada, 1,9 µs la valla `[Y1]`. `SessionEnd` tiene 1,5 s y los plugins no pueden subirlo `[P6]`. Hook completo 4,07 ms, cues 1,034 ms `[I3]` `[M2]`. |
| P7 | **Entrega involuntaria. El agente no pide.** | 0 operaciones voluntarias en 114 turnos con almacén pre-cargado `[K1]`. |
| P8 | **Presupuesto de tokens duro, y silencio por defecto.** | Recuperar de más daña la decisión secuencial `[J6]`; el contexto irrelevante produce inestabilidad por ejemplo `[S1]`; el canal que el lector aprende a saltarse es peor que ninguno `[G3]`. |
| P9 | **Esquema tipado propio, exportable.** | El filtro exacto exige tripleta; sobre prosa el guard captura 4/14 `[N4]`. Formato cerrado es apuesta contra un CG del W3C activo `[Q4]`. |
| P10 | **El motor se vigila a sí mismo.** | 85 fallos en el mes uno, todos en la capa de almacén y captura `[Q3]`. |

## 2. Modelo de datos

Un fichero SQLite por proyecto en `.muninn/muninn.db`, WAL, `synchronous=NORMAL`.
Todo texto legible se **proyecta** además a Markdown con frontmatter en `.muninn/`
(sección 9). SQLite es la verdad operativa; el Markdown es la verdad portable.

### 2.1 `record` — el ledger tipado

| Columna | Tipo | Notas |
|---|---|---|
| `id` | INTEGER PK | |
| `kind` | TEXT | `invariant` · `decision` · `deadend` · `correction` · `claim` · `episode` |
| `subject` | TEXT | Clave de supersesión (con `relation`). Ej.: `retry.policy`, `src/auth/jwt.rs` |
| `relation` | TEXT | Ej.: `is`, `uses`, `must_not`, `tried_and_failed` |
| `object` | TEXT | El valor. Para `episode`, el resumen de una línea |
| `body` | TEXT | Texto completo, literal. Nunca reescrito |
| `origin` | TEXT | `user_said` · `review_accepted` · `commit_linked` · `tool_observed` · `agent_inferred` · `imported` |
| `trust` | INTEGER 0–3 | **Derivado de `origin`, no del texto** `[W3]`. `user_said`/`review_accepted`=3, `commit_linked`=2, `tool_observed`=1, `agent_inferred`/`imported`=0 |
| `anchor_path` | TEXT NULL | Fichero al que la afirmación está anclada |
| `anchor_hash` | TEXT NULL | Hash del contenido que estableció la afirmación `[K11]` |
| `session_id` | TEXT | |
| `transcript_ref` | TEXT NULL | `ruta:offset` a la transcripción cruda. Evidencia, no copia |
| `created_at` | INTEGER | epoch ms |
| `invalid` | INTEGER 0/1 | **Grueso.** 1 = retenido, nunca servido |
| `invalidated_by` | INTEGER NULL | `record.id` del reemplazo, si lo hay |
| `invalid_reason` | TEXT NULL | `superseded` · `revoked` · `anchor_changed` · `reverted` · `user` |

Sin `t_evento`/`t_registro`/`t_invalidación` separados, sin "fuerza de evidencia", sin
estado "no demostrable". Todo eso era grano fino y `[X1]` demuestra que no paga. Si una
medición futura lo pide, se añade entonces.

### 2.2 `cue` — condiciones de disparo permanentes (F3)

| Columna | Tipo | Notas |
|---|---|---|
| `record_id` | INTEGER FK | |
| `kind` | TEXT | `dir` · `symbol` · `event` · `after` · `cooldown` · `keyword` |
| `key` | TEXT | Para `dir`: prefijo de directorio **normalizado** (no glob) `[M2]`. Para `symbol`: nombre exacto. Para `event`: `session_start` · `prompt` · `pre_edit` · `pre_run` · `pre_compact` · `post_compact`. Para `after`: epoch ms. Para `cooldown`: ms |
| `group` | INTEGER | Conjunción dentro del grupo, disyunción entre grupos `[K1]` |

Índices: `(kind, key)`. Los globs `dir/**` se normalizan a prefijo en la escritura; los
que no se pueden normalizar se guardan como `glob` y se evalúan aparte, porque un `GLOB`
por fichero tocado cuesta 5,4x más `[M2]`.

### 2.3 `rule` — reglas del proyecto y su estado de cumplimiento (F2)

| Columna | Tipo | Notas |
|---|---|---|
| `id` | INTEGER PK | |
| `source_file` | TEXT | `CLAUDE.md`, `AGENTS.md`, `.claude/rules/x.md` |
| `source_hash` | TEXT | Para re-compilar solo lo que cambió |
| `text` | TEXT | La regla, literal |
| `class` | TEXT | `enforceable_permission` · `enforceable_hook` · `enforceable_sandbox` · `interpretive_only` |
| `emitted` | TEXT NULL | El artefacto generado (regla de permisos, hook `deny`, entrada de sandbox) |
| `rationale_ref` | INTEGER NULL | `record.id` de la decisión que justifica la regla — la **licencia de borrado** `[K4]` |

### 2.4 `fire_ledger` — qué se entregó, cuándo, y si cabía

| Columna | Tipo | Notas |
|---|---|---|
| `session_id` | TEXT | |
| `compaction_epoch` | INTEGER | Se incrementa en cada `PostCompact`; el ledger **se reinicia** por época `[K1]` |
| `record_id` | INTEGER | |
| `fired_at` | INTEGER | |
| `tokens` | INTEGER | Tokens **entregados**, no solo inyectados `[2608.31057]` |
| `reason` | TEXT | Qué cue disparó, o `gated` con motivo |

Es el denominador. Toda decisión de disparar o callar deja fila `[G3]`.

### 2.5 `heartbeat` y `health`

Una fila por invocación de hook **antes** de trabajar: `(hook, session_id, started_at,
ok, error, ms)`. Un hook que empieza a fallar aparece como filas fallidas; uno que deja de
disparar aparece como ausencia `[G3]`.

### 2.6 Índices

- `record_fts`: FTS5 **de contenido externo** sobre `record(subject, object, body)`, para
  no almacenar el texto dos veces. Solo entran filas con `invalid=0`; al invalidar se
  ejecuta el `delete` del FTS5 externo y la fila queda en `record`.
- `record(subject, relation) WHERE invalid=0`: índice parcial para la supersesión en O(1).
- `record(anchor_path)`: para el validador de anclas.
- `cue(kind, key)`, `heartbeat(session_id, started_at)`.

Medido: 50 000 registros → 92 MB, construido en 1,30 s; consulta BM25 top-8 1,433 ms en
Rust `[I1]`. Escala sublineal: 144x datos, 6x latencia `[H1]`.

## 3. Ruta de escritura (asíncrona, fuera del camino crítico)

Disparada en `SessionEnd`, `Stop` y `PostToolUse` (este último solo para capturar
señales de ejecución, no para escribir registros).

```
transcript_path ──► 1. extraer candidatos (reglas deterministas + patrones)
                    2. redactar secretos (patrón + entropía) y anonimizar rutas de home  [N1] [P2]
                    3. deduplicar por hash de (kind, subject, relation, object)
                    4. supersesión: mismo (subject, relation), distinto object
                       → el anterior pasa a invalid=1, invalidated_by=nuevo
                    5. INSERT record + cues derivadas + proyección Markdown
                    6. INSERT en FTS5 externo
                    7. heartbeat
```

**Qué se captura sin pedirle nada al agente**, en orden de fiabilidad:

| Fuente | Kind | Origin / trust | Dato |
|---|---|---|---|
| Comentarios de review aceptados en el PR | `correction` → `invariant` | `review_accepted` / 3 | 0 % de recurrencia medida para clases con regla `[K14]` |
| Enlaces commit ↔ sesión (post-commit) | `decision`, `episode` | `commit_linked` / 2 | Ground truth a coste cero de etiquetado `[K3]` |
| Reverts y tests que siguen rojos tras un intento | `deadend` | `tool_observed` / 1 | Las reglas anti-patrón superan a los ejemplos `[J2]` |
| El usuario diciendo "no, así no" | `correction` | `user_said` / 3 | `[G3]` |
| Salida de herramientas (códigos de salida, ficheros tocados) | señal, no registro | — | Alimenta cues y validadores |
| Lo que el agente infiera por su cuenta | `claim` | `agent_inferred` / 0 | Solo se sirve con marco de procedencia explícito |

**Lo que NO hace la ruta de escritura:** resumir con un LLM, fusionar registros
parecidos, "reflexionar", decaer con curvas de olvido. Todo eso es `[C1]` `[K10]`.

**Presupuesto:** ingesta de 200 registros en 80 ms más 195 ms de `optimize` `[I4]`.
Objetivo: < 500 ms por sesión, siempre asíncrono, y si el hook expira, la ingesta se
retoma en el siguiente `SessionStart` desde una marca de agua. Un hook que expira no
pierde datos: pierde tiempo.

**Topes duros `[P5]`:**
- `invariant` activos: ≤ 60 por proyecto. Al pasarse, el más antiguo sin `rationale_ref`
  pasa a `invalid=1, reason=user` y se avisa.
- Registros activos totales: ≤ 20 000. Por encima, los `episode` más antiguos sin
  referencias entrantes se archivan (siguen en disco, salen del índice).
- Un `record.body`: ≤ 2 000 caracteres. Lo que no cabe se queda en `transcript_ref`.

## 4. Ruta de lectura (síncrona, cada turno)

```
UserPromptSubmit ──► 0. heartbeat
                     1. compuerta de intención (regex; ~80 % de turnos salen aquí)     0,49 ms wall [I3]
                     2. contexto del turno: ficheros tocados (PostToolUse anterior),
                        símbolos referenciados, evento, hora
                     3. evaluación de cues por índice: dir por ancestros, symbol exacto,
                        event exacto, after/cooldown por comparación                    1,03 ms [M2]
                     4. léxico opcional: 8 términos de mayor IDF del prompt → BM25 top-8  1,43 ms [I1]
                     5. FILTRO: invalid=1 nunca; trust<1 solo con marco explícito;
                        conflicto no resuelto → se conservan ambos y se reporta [N4]
                     6. fusión RRF k=60 entre cues y léxico                              0,04 ms [H6]
                     7. fire_ledger: descartar lo ya entregado en esta época de compactación
                     8. presupuesto: ≤ 700 tokens entregados, tope duro; por bloque ≤ 200
                     9. formato: bloques tipados con procedencia (sección 6)
                    10. fire_ledger: una fila por cada disparo Y por cada silencio
```

**Contratos:**

| Etapa | p50 objetivo | Medido |
|---|---|---|
| Compuerta cortando | < 1 ms | 0,49 ms `[I3]` |
| Cues + filtro | < 2 ms | 1,034 ms, p99 1,143 `[M2]` |
| Hook completo con léxico | < 6 ms | 4,07 ms, p95 4,31 `[I3]` |
| Modelo de embeddings cargado | **nunca en esta ruta** | 106 ms si se hiciera `[I2]` |

Límites externos que respeta: `additionalContext` derrama a fichero por encima de
10 000 caracteres en Claude Code y de 2 500 tokens en Codex por defecto `[P6]`; si el
hook expira, la salida se descarta entera. 700 tokens cabe con margen en ambos.

## 5. Invalidación (F1), en su forma gruesa

Tres disparadores, todos deterministas, ninguno por similitud:

1. **Supersesión en escritura:** nuevo `(subject, relation)` con distinto `object` →
   el anterior `invalid=1, reason=superseded, invalidated_by=nuevo`. 15–40 % de hechos
   superados servidos → ~0 % `[E1]`; 6–27 % → 98–100 % en supersesión, completitud y
   negación `[K6]`.
2. **Ancla cambiada:** validador en background compara `anchor_hash` con el contenido
   actual; si difiere, `invalid=1, reason=anchor_changed`. **No se adivina un valor
   nuevo**: se retira `[K11]`.
3. **Revert detectado:** el commit enlazado fue revertido → `deadend` se mantiene,
   `decision` pasa a `invalid=1, reason=reverted`.

Regla de la lectura: `invalid=1` **nunca** se sirve por defecto; existe `muninn why --all`
para verlo. Si dos registros activos entran en conflicto y no hay orden establecible, se
sirven ambos marcados como conflicto — nunca se elige por orden de ranking, que fue lo
que convirtió un 0 % de acciones inseguras en 100 % en el guard publicado `[N4]`.

Coste medido de la alternativa: memorias inseguras suben el código vulnerable 2,7–50,3 pp;
el filtrado a nivel de memoria detecta el 100 % y restaura el baseline sin coste de
corrección `[W1]`.

## 6. Formato de entrega (lo único que el modelo ve)

Tres tipos de bloque y solo tres `[G3]` `[D5]`:

```
[muninn:no-rebuild] src/webhooks/retry.rs ya implementa backoff exponencial.
  decisión #142 · commit a1b2c3d · origen: review_accepted · verificar antes de actuar
[muninn:stale] "el endpoint /v1/sync es idempotente" — src/api/sync.rs cambió desde que se
  registró (2026-08-30). Retirado, no reemplazado.
[muninn:lineage] #142 ← #98 ← #61 · `muninn why 142`
```

Cada bloque lleva **referencia, no contenido** `[A2]`; **procedencia y nivel de
confianza** siempre `[K1]` `[W2]`; y va enmarcado como evidencia, nunca como instrucción:
la memoria es la superficie epistémica y las instrucciones la imperativa. Ningún bloque
puede contener texto que se parezca a un rol de sistema o de usuario — es la defensa
contra M-CPE `[W2]`.

## 7. Compactación

- `PreCompact`: snapshot de los `invariant` activos y de los `correction` recientes.
- `PostCompact` y `SessionStart(source=compact)`: **reinyección incondicional** de los
  invariantes (sin compuerta), reinicio del `fire_ledger` para la nueva época, y
  **verificación del resumen**: `compact_summary` se contrasta contra los códigos de
  salida reales de la transcripción; toda afirmación de "confirmado" cuyo comando terminó
  en 143 o no terminó se inyecta como `[muninn:unverified]` `[W4]`.
- En OpenCode, además, `experimental.session.compacting` permite inyectar los invariantes
  **dentro del prompt de compactación** `[O1]`.

Dato: los hechos solo en conversación mueren en 106/108 compactaciones; desde un almacén
del harness llegan 138/138 `[K1]`. Violaciones de restricciones 30–59 % → 0 % si el
invariante sobrevive `[C2]`.

## 8. Compilador de reglas (F2)

Independiente del almacén salvo por la tabla `rule`. En `SessionStart` e
`InstructionsLoaded`:

1. Lee `CLAUDE.md`, `AGENTS.md`, `.claude/rules/*.md`; solo re-compila si cambió el hash.
2. Clasifica cada regla: `enforceable_permission` (mapea a una regla `deny`/`ask` de
   permisos), `enforceable_hook` (mapea a `PreToolUse` → `permissionDecision: deny`),
   `enforceable_sandbox`, o `interpretive_only`.
3. Emite los artefactos a `.muninn/compiled/` y los registra en la configuración del
   harness con marcador propio; nunca toca contenido ajeno.
4. Escribe el **informe de cobertura**: qué fracción quedó siendo solo persuasión. Hoy
   es el 95,6 % `[K5]`.
5. Cada regla enlaza a su `rationale_ref`; una regla sin racional se marca, porque sin
   racional no se puede borrar con seguridad `[K4]`.

Límite declarado: el cumplimiento a nivel de herramienta **no cubre caminos indirectos**
`[U2]` ni protege de lo que una memoria caduca autoriza `[N4]`. F2 mejora la cobertura
frente a nada; no da garantías. El mecanismo de clasificación se adapta de la
autoformalización publicada `[U1]`, no se inventa.

## 9. Formato portable

- Cada `record` se proyecta a `.muninn/records/<kind>/<id>.md` con frontmatter YAML:
  `kind, subject, relation, object, origin, trust, anchor_path, anchor_hash, created_at,
  invalid, invalid_reason, invalidated_by`. El cuerpo es `body` literal. Es compatible por
  forma con los ficheros de tema de la memoria nativa, así que un usuario puede irse.
- `.muninn/index.md`: una línea por registro activo, ≤ 200 líneas, ≤ 25 KB — el mismo
  tope que la nativa `[O5]`, porque es el único tope que se sabe que funciona.
- Exportación JSONL completa con `muninn export`. Importación desde JSONL y desde
  Markdown con frontmatter.
- **Ningún campo propietario en el contenedor.** Cuando el CG del W3C o PAM fijen un
  formato `[Q4]`, la proyección cambia; el esquema interno no. Los diferenciadores
  —cues, tripleta, procedencia— viven en campos, no en el contenedor.

## 10. Empaquetado y canal

- **Núcleo en Rust**, un crate. Dos objetivos de build: binario CLI para los harness de
  subproceso (Claude Code, Codex) y addon `napi-rs` para los harness de Node en proceso
  (OpenCode, Cline, Pi) `[P4]`. Mismo código, dos envolturas.
- Binario ≤ 15 MB, sin modelo dentro. Comparación: Rekal 170 MB `[N1]`.
- **Hooks en forma exec, nunca shell.** Versión fijada. Suma de comprobación publicada.
  Sin `${...}` interpolado en comandos. Es lo que HookPry explota: 92,5 % de éxito, 0 %
  de recall de Defender `[X2]`.
- Privilegio mínimo: el hook de lectura abre la base en modo solo lectura; solo la ruta
  de escritura escribe.

### Gramáticas

Compiladas estáticamente en el binario (crate `muninn-symbols`), no como plugin externo:

| Lenguaje | Crate | Extensiones |
|---|---|---|
| Rust | `tree-sitter-rust` | `.rs` |
| TypeScript / TSX | `tree-sitter-typescript` | `.ts`, `.tsx`, `.mts`, `.cts` |
| JavaScript / JSX | `tree-sitter-javascript` | `.js`, `.jsx`, `.mjs`, `.cjs` |
| Python | `tree-sitter-python` | `.py`, `.pyi` |
| Go | `tree-sitter-go` | `.go` |

## 11. Salud del motor (P10)

Health gate en `SessionStart`, una línea: `MUNINN 9/9 GREEN` o el nombre del fallo y su
arreglo. Nueve comprobaciones, todas aritmética sobre evidencia tipada, ninguna con
modelo `[G3]`:

1. marca de agua de ingesta fresca; 2. heartbeats de todos los hooks esperados desde el
último límite; 3. integridad del `.db` (`PRAGMA quick_check`); 4. FTS5 coherente con
`record` (conteo); 5. cola de captura pendiente; 6. render de invariantes no idéntico al
de la sesión anterior (un render congelado es un fallo); 7. `rule` en sincronía con los
hashes de los ficheros fuente; 8. topes no excedidos; 9. artefactos compilados presentes
y con el marcador propio.

Un RED es accionable por definición. "Frío" no es "muerto": un timeout de ingesta
diferida no es alarma, es "verificar en el siguiente límite".

## 12. Lo que el motor NO lleva, con el dato

| No lleva | Por qué |
|---|---|
| Consolidación, reflexión, resumen por LLM | `[C1]` `[K10]`; claude-mem y agentmemory lo llevan y es su mayor debilidad `[P3]` `[X4]` |
| Vectores en la ruta caliente | `[I2]` `[V4]` `[X3]` |
| Grafo de conocimiento | El registro de 8 meses lo descartó: los fallos nunca fueron de expresividad `[G3]` |
| Ledger bi-temporal fino, estado "no demostrable", fuerza de evidencia | `[X1]` |
| Decaimiento tipo Ebbinghaus, importancia aprendida | Ninguna medición lo pide; agentmemory lo lleva `[P2]` |
| Skills importadas | −1,3 a −4,2 pp `[J2]` |
| Más de dos herramientas MCP | 0 operaciones voluntarias `[K1]`; peaje `[H2]` |
| Respondedor de "por qué" enrutado | Fuera de la v1; si se hace, se hace como herramienta invocada y midiendo suficiencia `[K3]` |

Vector como **sidecar opcional**, apagado por defecto: embeddings estáticos calculados en
la ruta de escritura y consultados solo por la herramienta `muninn why`, nunca por el hook.
Se enciende únicamente si una medición con oráculo independiente lo justifica; el
híbrido da +2,8 pp de recall@10 `[H4]` a cambio del determinismo `[V4]`.

## 13. Qué mide que el motor sirve

La telemetría de disparos **no es un oráculo** `[Y1]`. Lo que cuenta:

1. **Cuatro brazos en un harness sin nativa** (OpenCode o Pi): sin memoria · episodios
   literales por hook · lo mismo más el filtro · control irrelevante de longitud
   igualada. Benchmark multi-sesión con oráculos ejecutables. Línea a batir: 45,56 % de
   memoria literal, no 11,67 % `[K2]`.
2. **Control de render** en cualquier comparación de invalidación `[X1]`.
3. **PM-Bench** para F3: 65,1 % publicado, 82,9 % con almacén tipado `[V1]` `[V2]`.
4. **Rejilla de revocación** de `[K7]` (nueve escenarios, nueve modelos) para F1, contra
   el `stale_guard.py` publicado.
5. **Corpus real de CLAUDE.md** para F2: fracción ejecutable, y si sale ≥ 50 % el problema
   no existe `[K5]`.
6. Cuatro niveles de medida siempre: almacenado, entregado, gestión, resultado
   `[2608.31057]`.

Si el brazo 2 no bate al 1, el motor no existe. Si el 3 no bate al 2, el motor se
reduce a captura literal y F2.
