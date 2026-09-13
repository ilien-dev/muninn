# Muninn — conclusión v2

Fecha: 2026-09-12. Supersede a `CONCLUSION-v1-superada.md`, que se conserva solo para
trazabilidad. Toda afirmación numérica está en `00-evidence-log.md`; las marcadas
`[H*]`, `[I*]` y `[M2]` las medí en esta máquina, con scripts en `research/bench/`.

**Qué cambió respecto a la v1.** Un barrido con ventana estricta de 12-jul a 12-sep-2026
encontró nueve trabajos que **falsifican o acotan** la v1 y seis que la confirman con
mejor evidencia, más dos features nativas que yo contaba como hueco y no lo son. La v1
prometía lo que no puede cumplir y proponía construir lo que ya existe. Esta v2 promete
menos, lo promete acotado, y se apoya en arte previo en vez de reinventarlo.

---

## 0. Veredicto en una página

**La v1 asumía que los agentes de código fallan por falta de conocimiento del proyecto.
Eso está falsificado.** 288 corridas evaluadas con tests de oro, sobre Claude Code Y
Codex, 17 tareas reales: la estrategia de contexto **no mueve la corrección de forma
medible** (acotado a ≤10-15 pp por test de equivalencia), y el triage de fallos dice por
qué — los agentes fallan por **habilidad de implementación**, no por conocimiento que un
fichero de contexto pudiera aportar. El AGENTS.md real nunca convierte un casi-acierto en
acierto `[J1]`.

**Y la variante "inyectemos skills" es activamente dañina.** Con control irrelevante de
longitud igualada: 31 skills públicas sobre 1 000 tareas y 4 modelos **bajan el Pass@2
entre 1.3% y 4.2%** y suben el coste en tokens **72%–394%**, con ganancia en solo el
17%–36% de los pares `[J2]`.

**Lo que sí está medido, y es grande, es otra cosa.** En DreamBench-SWE, un benchmark
multi-sesión con oráculos ejecutables ocultos donde las tareas tardías dependen de
evidencia no inferible de las tempranas: sin memoria externa **21/180 (11.67%)**; con
memoria de eventos literal **82/180 (45.56%)**; con Mem0 en configuración de almacenado
literal **97/180 (53.89%)**. Las tres condiciones con memoria baten a la condición sin
memoria tras corrección de Holm. Y el hallazgo incómodo: **ningún diseño de memoria le
gana a otro de forma concluyente** `[K2]`.

De ahí salen las dos frases que definen el proyecto:

> **La memoria multi-sesión vale unas 4x la tasa de acierto. La sofisticación del diseño
> de memoria no es lo que la compra.**

> **Muninn no mejora la corrección de una sesión. Elimina cuatro fallos de continuidad
> entre sesiones, cada uno medido por separado.**

Los cuatro fallos, con su cifra:

| Fallo | Magnitud medida | Fuente |
|---|---|---|
| Los hechos mueren en la compactación | 10 hechos ausentes en **106 de 108** compactaciones forzadas; inyectados desde un almacén del harness llegan **138/138** compact-resumes, y el resumen final no lleva ninguno | `[K1]` |
| El agente actúa sobre hechos superados | Top-k vectorial acierta **6%–27%** en supersesión, completitud y negación, frente a **98%–100%** de registros tipados con enlaces de supersesión. Y **ningún sistema hace cumplir la revocación por defecto**: el hecho revocado gana en ranking a su reemplazo | `[K6]` `[K7]` |
| Se repite lo ya decidido o ya intentado | **39%** de las re-lecturas intra-sesión vuelven a pagar contenido ya pagado antes de un límite de compactación; la síntesis de decisiones enrutada sube la suficiencia de respuesta de **0.07–0.20 a 0.83** | `[K1]` `[K3]` |
| Las instrucciones crecen sin racional | **+226%** sobre la vida del fichero, **+4.9** por commit, y cuanto más vieja menos se borra. Registrar el razonamiento latente elimina el **99.3%** del exceso y mejora el seguimiento de instrucciones **hasta un 23.1%** | `[K4]` |

**Y el agente no va a pedir nada de esto.** La medición que decide la arquitectura
entera: **0 operaciones de memoria en 114 turnos**, con el almacén pre-cargado de
conocimiento relevante `[K1]`. La entrega tiene que ser involuntaria.

**Y una segunda poda, de la propia v2.** Al verificar el arte previo apareció
**rekal-cli 1.0**: captura la sesión en cada commit, la guarda cruda en git, la indexa
localmente, la comparte por `git push` en ramas huérfanas solo con el trabajo mergeado, y
responde con compuerta de silencio a **90.57% en LoCoMo y 86.60% en LongMemEval** con
~7.5–10.5K tokens por consulta y sin servicio `[N1]`. Dos de las cuatro funciones que la
v2 iba a construir —el almacén con captura y el respondedor enrutado— **ya están hechas**.

**Lo que queda, y es defendible precisamente porque es pequeño:** Muninn no almacena y no
responde. **Dispara, filtra y hace cumplir.** Tres funciones, las dos primeras con cero
tokens inyectados:

1. **Filtrar** en la recuperación lo superado y lo revocado, por regla determinista sobre
   registros tipados. Es la función con el número extremo a extremo más fuerte de toda la
   investigación: sobre 360 corridas y 9 modelos, un filtro en la lectura baja la
   invocación de herramienta peligrosa del **17.2% al 2.2%** y la exfiltración confirmada
   del **2.5% al 0%**; el mismo control puesto en la frontera de la herramienta es
   **indistinguible de no tener defensa** (17.2%). *"The containment boundary that holds
   is the read."* `[N4]`
2. **Compilar** las reglas escritas en CLAUDE.md y AGENTS.md a controles que se hacen
   cumplir. Solo el 4.4% (IC 2.6–6.7%) de las reglas de seguridad escritas tiene hoy un
   control detrás `[K5]`, y una evaluación de flota sobre **46 endpoints de 6 fabricantes**
   concluye que *"secure agents require external enforcement, not merely better
   recognition"* `[N2]`. **Corrección de la iteración 9: el mecanismo sí existe** — ver
   §8-quinquies. Lo que no existe es aplicado a CLAUDE.md y AGENTS.md `[U1]`.
3. **Repartir** de forma involuntaria con condiciones de disparo permanentes sobre
   `{symbol, event, temporal}` —los tres tipos que nadie lleva enviados— reusando `path`
   y palabra clave, que ya son nativos `[K1]` `[L2]`.

**Rendimiento, medido en Rust en esta máquina:** evaluación de cues por turno
**1.034 ms p50 / 1.143 ms p99** sobre 5 000 memorias y 15 000 triggers con filtro de
supersesión; hook completo **4.07 ms p50**; F1 y F3 no gastan tokens `[M2]` `[I3]`.

---

## 1. La prueba de la rueda: qué ya es nativo

Esto va primero porque es el criterio de eliminación. Todo lo de esta tabla **no se
construye**.

| Ya existe | Dónde | Consecuencia |
|---|---|---|
| Memoria automática por proyecto, con índice y ficheros por tema | Claude Code `2.1.32` y posteriores; índice `MEMORY.md` de 200 líneas / 25 KB, `modified` ISO en frontmatter (`2.1.214`), error explícito al pasarse (`2.1.210`) | No hay que construir un almacén de notas |
| **Almacenes de memoria de equipo** | `CLAUDE_MEMORY_STORES`, Claude Code `2.1.172` | **La v1 listaba esto como hueco. Era un error.** |
| Triggers por path | `.claude/rules/` con `paths:` como lista YAML de globs (`2.1.84`); condiciones `if:` de hook (`2.1.214`); hook `InstructionsLoaded` (`2.1.69`). Fuera: reglas disparadas por path y por palabra clave en OpenHands; `trigger description` obligatoria en los Knowledge items de Devin; `always_on`/`glob`/`model_decision` en Devin y Windsurf; reglas de Cursor | **Dos de los cinco tipos de cue ya son industria estándar.** No se reinventan |
| Contabilidad de coste de contexto | `/skill-doctor` (`2.1.261`): qué skills cargadas no se usan y cuánto cuestan | El vendor ya mide parte de lo que yo iba a instrumentar |
| Pinning de autorizaciones sobre la compactación | Codex "Guardian thread context" (`rust-v0.151`–`v0.154`): las instrucciones del usuario, sus respuestas y las autorizaciones válidas sobreviven a la compactación | **Acota mi pilar de invariantes**: para aprobaciones, ya está hecho |
| Compactación, subagentes, skills, hooks completos | Ambos harness, mismo ciclo de vida | El punto de integración, no el producto |

Y lo que **no existe en ningún sistema enviado ni publicado**, que es donde queda el
proyecto:

1. **La captura.** El arte previo más cercano lo declara textualmente: *"Capture is the
   unevaluated half... capture-side automation is future work"* `[K1]`. Sus diez notas de
   prueba las sembró el harness a mano. Y todo lo demás depende de que el agente decida
   escribir, que es lo que no hace.
2. **Triggers de símbolo.** Requieren un grafo de símbolos de código *"which no memory
   system carries"* `[K1]`. Tampoco `event` más allá de session-start ni `temporal`.
3. **Supersesión y revocación hechas cumplir en la recuperación.** Nadie `[K7]`.
4. **Compilar la regla interpretada en un control que se hace cumplir.** Solo el
   **4.4% (IC 95%: 2.6–6.7%)** de las reglas de seguridad escritas en 481 CLAUDE.md
   públicos tiene un control nativo que las respalde. *"CLAUDE.md is a write-only
   channel"* `[K5]`.
5. **El racional como licencia de borrado de una instrucción** `[K4]`. Nota: Claude Code
   **elimina** los comentarios HTML de CLAUDE.md antes de inyectarlo, así que el canal
   nativo de comentarios es solo para humanos.
6. **Evaluación con control irrelevante de longitud igualada** `[J2]` y con auditoría
   de contrafactual de restauración `[2609.08279]`. Es el estándar mínimo desde agosto y
   casi nadie lo cumple.

### 1.1 Rekal ya construyó la mitad del plan v2

Descubierto al verificar el arte previo de `[K3]`: **rekal-cli, versión 1.0, con paper
y benchmarks** `[N1]`.

Lo que ya hace, y por tanto **no se construye**:
- Captura la sesión de IA **en cada commit** por hook `post-commit`, la guarda **cruda**
  y append-only en git, y la indexa localmente en background (BM25 FTS + embeddings LSA
  + embeddings profundos + co-ocurrencia + facetas + chunks de prosa del HEAD).
- **Memoria de equipo sin servidor**: ramas huérfanas `rekal/<email>`, y **solo el
  trabajo mergeado** sale al remoto — un spike no mergeado nunca deja la máquina.
- Redacción de secretos (patrón + entropía de Shannon) y anonimización de rutas de
  home **antes** de escribir en la base.
- **Compuerta de silencio con umbrales**: veredictos `INJECT` / `KNOWLEDGE` / `SILENCE`,
  con confianza absoluta por semilla y masa BM25 cruda, para poder rechazar una consulta
  que solo es la mejor de un conjunto débil.
- **Router de pregunta**: tree (grep, ahora) / knowledge (prosa en HEAD) / ledger
  (razonamiento pasado) / map (estructura).
- Procedencia completa: turno → sesión → commit.
- Un solo binario, embeddings on-device, sin servicio. Funciona con Claude Code, Cursor,
  Copilot, Codex, Gemini, Kiro y OpenCode.
- Benchmarks con GPT-5 Sol respondiendo: **LoCoMo 90.57% de accuracy, recall@20 98.61%,
  ~7.5K tokens de contexto por consulta, 5.9 turnos de agente; LongMemEval 86.60%,
  recall@20 99%, ~10.5K tokens, 6.6 turnos.**

Lo que **no** hace, comprobado contra su README y su paper:
1. **Entrega involuntaria.** Rekal se invoca: el agente ejecuta `rekal "<query>"` a
   través de una skill que tiene que elegir usar. Es exactamente el fallo que `[K1]`
   mide: **0 operaciones de memoria en 114 turnos**. Su compuerta de silencio actúa
   sobre la *respuesta*, no hay condiciones de disparo permanentes evaluadas por el
   harness, ni re-inyección en `PostCompact`.
2. **Supersesión y revocación hechas cumplir.** Conserva sesiones crudas y razona a
   tiempo de consulta; no hay regla bi-temporal, ni estado "no demostrable", ni filtro
   de revocación en la recuperación.
3. **Triggers de símbolo** ni grafo de símbolos por memoria.
4. **Compilar reglas en controles.** Escribe una línea marcada en CLAUDE.md apuntando a
   su skill; no clasifica reglas ni genera permisos.

Y su coste dice qué función es: **~170 MB de descarga, ~200 MB en disco** (lleva dentro
el motor de inferencia, el modelo de embeddings, la base y la extensión de texto
completo), y una recuperación de **5.9–6.6 turnos de agente y "unos segundos"**. Es
responder, no repartir. Tres órdenes de magnitud por encima de mi 1.03 ms `[M2]`, porque
son funciones distintas.

=> **Responder "por qué" está resuelto. Repartir sin que el agente lo pida, no.**

### 1.2 No ser un servidor MCP más

Censo de 24 135 servidores del registro, muestra probabilística de 400 sondeados por
cable: **solo el 48.8% completa un handshake `initialize`**, y el fallo dominante no son
credenciales (13.3%) sino **servidores que no arrancan nunca (37.5%)** `[L6]`. Y sobre
3 171 repos públicos con configuración de agente, **el 16.0% lleva un defecto de
seguridad**: 9.8% instalan un MCP sin versión fijada, 3.8% llevan una skill que
pre-aprueba la shell a quien la instale `[L6]`.

---

## 2. Arte previo: lo que hay que adoptar en vez de derivar

La v1 iba a inventar el mecanismo de entrega. Ya está especificado y medido.

**El modelo cue-anchored** `[K1]`, textual: cada memoria lleva condiciones de disparo de
primera clase sobre un vocabulario componible `{path, symbol, semantic, event, temporal}`,
evaluadas **deterministamente por el harness**. Conjunción dentro de un trigger,
disyunción entre triggers. `path` es glob sobre ficheros tocados; `symbol` es que se
referencie una entidad de código nombrada; `semantic` es similitud de la actividad actual
sobre un suelo; `event` es {session start, prompt submit, pre-edit, pre-run,
pre-compaction, post-compaction}; `temporal` es {not-before, cooldown}. El tipo define la
entrega por defecto cuando no hay trigger explícito: las directivas disparan en session
start y post-compaction; las notas de tarea prioritarias en session start incluyendo cada
compact-resume; los hallazgos entran en recall semántico a tiempo de prompt.

**La disciplina de entrega**, también textual `[K1]`: inyección presupuestada en dos
niveles (índice compacto por defecto, contenido completo on-demand o en disparo
anclado); un ledger de disparos por sesión que deduplica y **se reinicia en los límites
de compactación para que los hechos anclados se re-armen en la ventana nueva**; el
contenido inyectado lleva marco de procedencia — *"recorded by an AI session, not
human-endorsed — verify"* — para que la memoria siga siendo la superficie epistémica y
las instrucciones la imperativa; la obsolescencia se comprueba contra ground truth y la
inyección lleva aviso explícito si el fichero anclado cambió después de escribirse la
nota.

**Git como sustrato** `[K3]`: ground truth de los commits, frescura del rebuild,
verificación del merge, contención de la review. Y el ground truth de evaluación se mina
de enlaces commit↔sesión, **replicable sobre el historial de cualquiera a coste cero de
etiquetado**.

**El enrutado de respuesta** `[K3]`: amplitud → mapa estructural anclado en git;
consultas puntuales → episodios con compuerta de confianza; racional → síntesis de
decisiones. 0.83 de suficiencia frente a 0.07–0.20 de un solo disparo, a 382–980 tokens
por pregunta.

**La compuerta de consolidación** `[K10]`: conservar cada experiencia primero como parche
específico de instancia; abstraer solo si pasan replay cruzado y comprobación de
mecanismo; comitear solo si se preserva el comportamiento validado de todos los
constituyentes. El aviso que la acompaña: consolidar por similitud semántica o por juicio
de un LLM **mezcla estrategias superficialmente parecidas y conductualmente
incompatibles, y degrada el rendimiento**.

**El estado de un hecho caduco** `[K11]`: no "posiblemente obsoleto" sino **"no
demostrable"**, con fuerza de evidencia separada de frescura. Cuando el contenido de
reemplazo no está disponible, el claim pasa a no demostrable en vez de adivinado. En su
testbed, ninguna sesión fabricó contenido retenido.

---

## 3. Qué funciona y qué no, con las revisiones de la ventana

### 3.1 Degradado o contradicho respecto a la v1

| Lo que decía la v1 | Lo que dice la ventana |
|---|---|
| Skills procedurales: pilar fuerte, +9.69 pass rate, transferibles `[D3][D4]` | **Contradicho** para skills importadas: −1.3 a −4.2 pp de Pass@2 con control de longitud igualada, +72–394% de tokens, ganancia en 17–36% de pares `[J2]`. Sigue en pie solo para memoria destilada de **tus propias** trayectorias contrastadas: 26.2% → 55.3% `[K8]` |
| ACE y ReasoningBank: evidencia fuerte `[D1][D2]` | **No replicado bajo varianza.** La evaluación de agentes es ruidosa, el bucle de auto-mejora amplifica el ruido, y la mejora **depende del orden de las tareas**, que impone un curriculum implícito `[J4]` |
| El índice vectorial es prescindible; BM25 basta | **Acotado.** Para memoria, BM25 sigue compitiendo `[H4]` `[2608.12888]`. Para **código**, dos resultados independientes dicen que el índice paga: 65.2% vs 46.2% y a menos de la mitad de coste `[J3]`, y menor $/resuelto que agentic grep `[D6]` |
| El peaje de 3 000 tokens del MCP oficial | **Cierto con condición.** El diferimiento automático de tools se activa cuando las descripciones MCP pasan del 10% de la ventana; 3 000 tokens son ~1.5%, así que se cargan por adelantado. Pero el argumento fuerte contra las tools no es el peaje: es **0 operaciones en 114 turnos** `[M1]` `[K1]` |
| La recuperación de un disparo con compuertas resuelve la lectura | **Insuficiente para responder.** Suficiencia de respuesta de un solo disparo: **0.07–0.20**. Enrutada: 0.83 `[K3]`. Un disparo sirve para **entregar cues**, no para contestar preguntas |
| El equipo no comparte memoria: hueco | **Error.** `CLAUDE_MEMORY_STORES` es nativo `[L1]` |
| Los invariantes sobre la compactación: hueco entero | **Acotado.** Codex ya preserva autorizaciones y respuestas verificadas del usuario a través de la compactación `[L5]`. Queda el caso de invariantes de proyecto arbitrarios |

### 3.2 Reforzado

| Lo que decía la v1 | Evidencia nueva, mejor |
|---|---|
| Hooks, no herramientas MCP | **0 operaciones de memoria en 114 turnos** con almacén pre-cargado `[K1]`. Y el selector de skills no se puede entrenar por recompensa de resultado — "selector credit starvation" — así que con un modelo congelado el selector **tiene que ser externo y determinista** `[K9]` |
| Silencio por defecto | **Recuperar de más daña específicamente la toma de decisiones secuencial** `[J6]`, que es lo que es programar. Y las skills instaladas compiten por *"fewer than 100 reliable trigger slots"* `[K12]` |
| No consolidar automáticamente | Almacenar literal ya cobra la mayor parte del beneficio: 45.56% de memoria de eventos literal frente a 11.67% sin memoria `[K2]`. Y consolidar por similitud o juicio de LLM degrada `[K10]` |
| Supersesión determinista | Cuantificado: 6–27% vs 98–100% `[K6]`, y ningún sistema hace cumplir la revocación `[K7]` |
| Determinismo en toda decisión consecuente | Cuatro capas de control, **solo dos se hacen cumplir**, y confundirlas es la causa más común de pérdida de control `[2608.26742]` |

### 3.3 Nuevo, y es lo mejor que salió del barrido

**Compilar instrucciones en controles.** Solo el 4.4% de las reglas de seguridad escritas
en CLAUDE.md tiene un control nativo que las respalde; el resto queda a interpretación del
modelo y el desarrollador **no recibe ningún feedback de cuál es cuál** `[K5]`. Convertir
la regla interpretable en una regla de permisos, un hook `deny` o una configuración de
sandbox **cuesta cero tokens inyectados** y cambia persuasión por cumplimiento. Es la
única capacidad del barrido que mejora la calidad sin gastar ni un token de contexto.

**El racional como licencia de borrado.** El motivo por el que CLAUDE.md crece sin límite
es que borrar una instrucción cuyo racional se perdió cuesta O(2^|D|) en riesgo de
regresión. Registrar el razonamiento latente elimina el 99.3% del exceso y mejora el
seguimiento de instrucciones hasta un 23.1% `[K4]`. Muninn ya iba a guardar decisiones
con su "por qué": el hallazgo es que ese "por qué" **no es documentación, es el permiso
para borrar** y además un mecanismo medido de mejora.

**La captura ya existe y es gratis.** Dos fuentes verificadas que no piden nada al agente:
los **enlaces commit↔sesión** `[K3]` y los **comentarios de review aceptados** — 0% de
recurrencia medida para las clases de error con regla, en una plataforma de 35+
microservicios `[K14]`. Y el canal ya tiene la atención del agente: ficheros de
instrucciones y notas de trabajo son el **60.5%** de todas sus interacciones con
documentación `[K13]`.

---

## 4. Muninn v3: qué es exactamente, después de descontar lo que ya existe

**Muninn no almacena y no responde. Dispara, filtra y hace cumplir.** Es la capa del
harness que hace que *cualquier* almacén que ya tengas —la memoria automática nativa, el
ledger en git de Rekal, o Markdown a pelo— entregue sin que el agente lo pida y se niegue
a servir lo que ya está muerto.

Tres funciones. Las dos primeras **no inyectan ni un token**.

### F1 — Compilador de reglas a controles (0 tokens inyectados)

Lee CLAUDE.md, AGENTS.md y `.claude/rules/`; clasifica cada regla en *ejecutable por un
control* o *solo interpretable*; emite las reglas de permiso, los hooks `deny` y la
configuración de sandbox que la hacen cumplir; y devuelve un informe de lo que quedó
siendo solo persuasión.

Respaldo, y es el más fuerte de todo el barrido:
- **El hueco**: solo el **4.4% (IC 95%: 2.6–6.7%)** de las reglas de seguridad escritas
  en 481 CLAUDE.md públicos tiene un control nativo que las respalde; el desarrollador no
  recibe ningún feedback de cuál es cuál. *"CLAUDE.md is a write-only channel"* `[K5]`.
- **El mecanismo**: evaluación de flota sobre **46 endpoints de modelo de 6 fabricantes**
  (spoofing de autoridad) y **48 modelos** (conflicto de memoria), 14 294 ensayos.
  Existe una **brecha reconocimiento-cumplimiento**: los modelos identifican
  explícitamente la autoridad falsificada cuando se les pregunta, **y aun así algunas
  configuraciones emiten la llamada a herramienta conflictiva**. La ejecución media bajo
  ataques nuevos es 1.21% [0.5–2.1%], pero **la vulnerabilidad se concentra en celdas
  reproducibles y varía hasta 47 pp dentro de una misma ventana de despliegue**.
  *"Restrictive policies and diverse prompts can eliminate execution on the same models,
  while permissive configurations and particular prompt-model pairs yield deterministic
  failures."* Y *"prompt-layer defenses likewise fail to generalize across models and
  adaptive formulations"*. Conclusión de los autores: *"Secure agents require external
  enforcement, not merely better recognition."* `[N2]`
- **La confirmación estructural**: cuatro capas de control —instrucción, permisos,
  sandbox, aislamiento del SO— y **solo dos se hacen cumplir**; confundirlas es la causa
  más común de pérdida de control `[2608.26742]`.

**Límite medido, y hay que declararlo:** un guardrail en la **frontera de la
herramienta** es **estadísticamente indistinguible de no tener defensa** cuando lo que
autoriza la acción es una memoria caduca: 62/360 de invocaciones de herramienta peligrosa
con guardrail frente a 62/360 sin defensa, y 7 exfiltraciones frente a 9. Rechazó 9
comandos de 360 y el agente no evadió ninguno: *"the rule fails upstream of evasion,
because the returned revoked policy licenses actions the rule does not classify as
dangerous"* `[N4]`. Luego **F1 no cubre el fallo de memoria obsoleta** — eso es F3, y son
dos fronteras distintas. F1 cubre su propio hueco: que el 95.6% de las reglas escritas no
tenga nada detrás.

=> Escribir la regla en el prompt es la capa equivocada, con medición de flota. Compilarla
a un control cuesta cero tokens y convierte persuasión en cumplimiento. **Nadie lo envía,
verificado en arXiv y en código** `[N5]`.

### F2 — Repartidor cue-anchored (presupuesto duro, silencio por defecto)

Implementa el modelo de `[K1]` con los tres tipos de cue que **nadie lleva enviados**
—`symbol`, `event` completo (pre-edit, pre-run, pre/post-compaction) y `temporal`
(not-before, cooldown)— y **reusa los dos que sí existen**: `path` vía `.claude/rules`
con `paths:`, y palabra clave. Evaluación determinista en el harness, ledger de disparos
que se reinicia en cada límite de compactación, marco de procedencia en todo lo
inyectado, tope de tokens duro.

Respaldo: **0 operaciones de memoria en 114 turnos** con el almacén pre-cargado; 10
hechos ausentes en 106/108 compactaciones frente a 138/138 entregas desde un almacén del
harness `[K1]`. Violaciones de restricciones 30–59% → 0% cuando el invariante sobrevive
`[C2]`. Y el selector no se puede entrenar con un modelo congelado, así que tiene que ser
externo y determinista `[K9]`.

Coste medido: **1.034 ms p50, 1.143 ms p99** `[M2]`.

Aviso que obliga el propio respaldo: la memoria es una de las fuentes de instrucción que
el modelo arbitra, y el conflicto de memoria se midió en 48 modelos `[N2]`. Luego lo que
Muninn inyecta **es en sí un evento de frontera de confianza**: lleva siempre marco de
procedencia y nunca puede escalar privilegios.

### F3 — Filtro de supersesión y revocación en la recuperación (≈0 tokens)

> **Simplificado en la iteración 12.** El diseño original —tripleta bi-temporal completa,
> fuerza de evidencia separada de frescura, estado "no demostrable"— **no sobrevive al
> control de render** de `[X1]`: el residuo del mecanismo de grano fino es indistinguible
> de cero, y la invalidación gruesa lo bate por 0,084. Lo que queda: **retener el
> registro invalidado y no servirlo.**

Regla determinista sobre `(sujeto, relación)`: cuando entra un hecho con el mismo sujeto
y relación y distinto objeto, el anterior se retiene marcado como inválido y deja de
servirse. El filtro se aplica **en la recuperación**, no en el almacén.

**Es la función con el número extremo a extremo más fuerte de toda la investigación.**
Sobre 360 corridas y 9 modelos, con una política permisiva revocada presente en el
contexto recuperado:

| Defensa | Herramienta peligrosa invocada | Exfiltración confirmada |
|---|---|---|
| Sin defensa | **17.2%** | 2.5% |
| **Filtro en la recuperación** | **2.2%** | **0%** |
| Guardrail en la herramienta | 17.2% | 1.9% |

*"The containment boundary that holds is the read."* `[N4]`

Respaldo adicional: top-k vectorial acierta 6–27% en supersesión, completitud y negación
frente a 98–100% de registros tipados con enlaces de supersesión, **con recall de
relevancia y coste en tokens equivalentes** `[K6]`. Ningún sistema hace cumplir la
revocación por defecto, y donde el registro revocado se expone **rankea primero** y el
42–44% de los ensayos elige la acción insegura `[K7]`. Regla determinista: 15–40% → ~0%,
sin umbral de similitud y sin llamada a LLM `[E1]`.

**Y el arte previo aquí también existe, pero es débil por una razón que se puede
arreglar.** `guard/stale_guard.py` está publicado: módulo agnóstico del backend, dos
etapas —campo de estado donde el backend lo expone, y donde no, un test de contención +
oposición sobre palabras de contenido (α=0.45, β=0.6)—. Sus propios autores: *"shows that
the guard is deployable, not that its design is sound"*. En Zep captura **4/14**; en
cognee y Graphiti bajo inserción indirecta es **marginalmente peor que no tener defensa**;
y una versión anterior que rompía empates por orden de resultado **convirtió una tasa
insegura del 0% en el 100%** `[N4]`.

=> **La contribución exacta:** el guard es débil *porque* es agnóstico del backend y tiene
que inferir el conflicto de texto sin estructura. Con esquema propio —tripleta
(sujeto, relación, objeto), timestamps reales, enlace de supersesión explícito— la etapa
heurística **desaparece** y el filtro pasa a exacto. No hay que escribir un guard mejor:
hay que quitarle la necesidad de adivinar.

### 4.1 Delegado explícitamente — no construir

| Función | A quién se delega | Por qué |
|---|---|---|
| Almacén, captura, transporte de equipo, responder "por qué" | **Rekal 1.0** `[N1]`, o la memoria automática nativa para el caso simple | Ya está, con paper y benchmarks: LoCoMo 90.57%, LongMemEval 86.60%, ~7.5–10.5K tokens/consulta, embeddings locales, sin servicio |
| Triggers de `path` y palabra clave | `.claude/rules` con `paths:`; OpenHands; Devin; Cursor; Windsurf | Industria estándar `[L2]` |
| Pinning de autorizaciones sobre la compactación | Codex Guardian thread context | Ya nativo `[L5]` |
| Compresión y desalojo de memoria | MemForest y similares | 50% de compresión retiene 97.1% del rendimiento con 1.89x de speedup `[2609.08273]`; no es nuestro problema |

Todo lo que Muninn escribe va como Markdown en `.muninn/` dentro del repo, versionado y
legible. SQLite es índice derivado y reconstruible.

### 4.2 Lo que Muninn NO promete

- No mejora la corrección de una tarea de implementación en una sola sesión. Falsificado
  con brazo de control: 288 corridas, dos agentes, ≤10–15 pp de equivalencia `[J1]`.
- No hace que el agente complete tareas multi-sesión interdependientes. En MemoryArena
  **todos** los sistemas están cerca del suelo `[J5]`.
- No importa skills públicas. Con control de longitud igualada hacen daño `[J2]`.
- No consolida automáticamente. Almacenar literal ya cobra la mayor parte `[K2]`.
- No almacena ni responde. Eso ya está resuelto `[N1]`.

## 5. Stack revisado

Cambios respecto a la v1 en **negrita**.

| Capa | Elección | Dato |
|---|---|---|
| Lenguaje | Rust | Hook completo 4.07 ms p50; encode 6.8x y BM25 3.2x más rápido que Python `[I1]` `[I3]` |
| Almacén | SQLite, un fichero por proyecto | 50 000 registros = 92 MB en 1.30 s `[I1]` |
| **Evaluación de cues** | **Tablas de trigger indexadas: `trig_dir(dir)`, `trig_sym(name)`, `trig_ev(ev)`** | **1.034 ms p50 con 5 000 memorias y 15 000 triggers. Normalizar los globs `dir/**` a prefijo de directorio y resolverlos por ancestros indexados es 5.4x más rápido que un `GLOB` por fichero tocado (5.614 ms)** `[M2]` |
| Léxico | FTS5 + `bm25()`, top-8 términos por IDF | 1.433 ms p50 en Rust; la selección de términos es la mayor palanca, 21.53 → 4.65 ms en Python `[H6]` `[I1]` |
| Semántico | Embeddings estáticos `model2vec-rs`, **fuera de la ruta caliente** | Encode en caliente 0.008 ms, pero **cargar la tabla cuesta 35–106 ms por proceso** `[I2]`. Y BM25 solo alcanza el 93.3% del recall del híbrido `[I5]` |
| **Índice de código** | **Grafo de símbolos propio (símbolo → fichero → memoria)** | **Requisito de los triggers `symbol`, que ningún sistema de memoria lleva `[K1]`. Y dos resultados independientes dicen que para código el índice paga: 65.2% vs 46.2% `[J3]`, menor $/resuelto que agentic grep `[D6]`** |
| Supersesión | Tabla bi-temporal + regla determinista sobre (sujeto, relación) | 15–40% → ~0% de hechos superados servidos `[E1]`; 6–27% → 98–100% en supersesión, completitud y negación `[K6]` |
| Fusión | RRF k=60 | +2.8 pp de recall@10 sobre BM25, 0.044 ms `[H4]` `[H6]` |
| Reranking | Ninguno por defecto | Mejora marginal, latencia mayor `[D7]` |
| Integración | Hooks de Claude Code y Codex; **una sola** herramienta MCP (`muninn_why`) | 0 operaciones voluntarias en 114 turnos `[K1]`; y `PostToolUse` puede reemplazar la salida de cualquier herramienta sin gastar un slot de trigger `[L3]` |
| Consolidación | Ninguna automática; compuerta de replay + mecanismo | `[K10]`, `[C1]` |

**Excluido, con su motivo:** base de datos vectorial dedicada (4 ms de numpy la
reemplazan `[H6]`); base de datos de grafos (el registro de 8 meses la descartó y dijo por
qué `[G3]`); servidor de embeddings (causa raíz de la mediana de 2.5 s y de dos de sus
tres incidentes `[G3]`); grafo temporal completo (una tabla y una regla dan la parte que
importa `[E1]`).

---

## 6. Contratos

| Contrato | Objetivo | Medido |
|---|---|---|
| Evaluación de cues por turno, p99 | < 3 ms | **1.143 ms** `[M2]` |
| Hook completo, p95 | < 10 ms | **4.31 ms** `[I3]` |
| Hook cuando la compuerta corta, p95 | < 1 ms | **0.56 ms** `[I3]` |
| Ingesta de una sesión, asíncrona | < 1 s | **275 ms** `[I4]` |
| Tokens inyectados por turno | ≤ 700, tope duro | por construcción |
| **Slots de trigger residentes consumidos** | **0** | **por construcción: F1 y F2 no inyectan; F3 va por hook; F4 es una sola herramienta** `[K12]` |
| Falsos disparos en turnos sin intención | 0 | falta instrumentar con denominador completo |
| Hechos revocados servidos | 0 | filtro en la recuperación, no en el almacén `[K7]` |
| Fallos silenciosos del subsistema | 0 | heartbeat antes de trabajar `[G3]` |
| **Modelo cargado en la ruta caliente** | **nunca** | **106 ms por proceso** `[I2]` |

Contrato de degradación: si la rama vectorial falla, la búsqueda degrada a BM25 y registra
la degradación; **nunca bloquea el hook** `[G3]`.

---

## 7. Orden de construcción y criterios de muerte

Reordenado por fuerza de evidencia **dentro de la ventana** y por tokens gastados.
Cada nivel lleva su condición de abandono.

| # | Qué | Evidencia | Tokens inyectados | Criterio de muerte |
|---|---|---|---|---|
| **1** | **F3 — filtro de supersesión y revocación en la recuperación** | **17.2% → 2.2% de invocación de herramienta peligrosa y 2.5% → 0% de exfiltración, 360 corridas × 9 modelos** `[N4]`; 6–27% → 98–100% con coste en tokens equivalente `[K6]`; 15–40% → ~0% `[E1]`; estado "no demostrable" `[K11]` | **≈0** | Si con esquema propio el filtro no bate al `stale_guard.py` publicado en su propia rejilla de escenarios, no aporta nada `[N4]` |
| **2** | **F1 — compilador de reglas a controles** | 4.4% (IC 2.6–6.7) de reglas con control, canal de solo escritura `[K5]`; *"external enforcement, not merely better recognition"* sobre 46 endpoints de 6 fabricantes, y las defensas a nivel de prompt no generalizan `[N2]`; cuatro capas de control, dos se hacen cumplir `[2608.26742]`. **El mecanismo existe** `[U1]` `[U3]` `[U4]`; lo que falta es aplicarlo al harness | **0** | Si al compilar un corpus real de CLAUDE.md la fracción ejecutable sale ≥50%, el problema no existe. Y no puede venderse como defensa contra memoria caduca: ahí el control en la herramienta no hace nada `[N4]` |
| **3** | **F2 — entrega cue-anchored: `event` y `temporal` primero** | 0 ops en 114 turnos `[K1]`; 106/108 vs 138/138 `[K1]`; violaciones 30–59% → 0% `[C2]`; el selector no se entrena con modelo congelado `[K9]` | ≤ 700, tope duro | **Si un control irrelevante de longitud igualada reproduce la ganancia, es longitud y no contenido. Se mata** `[J2]` |
| **4** | **Racional ligado a la instrucción, como licencia de borrado** | 99.3% del exceso eliminado; hasta +23.1% de seguimiento `[K4]` | bajo | Si el racional no cambia la tasa de borrado de instrucciones en repos reales, es solo documentación |
| **5** | **Triggers `symbol` + grafo de símbolos** | Lo que nadie lleva `[K1]`; el índice paga para código `[J3]` `[D6]` | ≤ 700 | Si `path` + `event` ya capturan los disparos útiles, `symbol` es complejidad sin ganancia. Los propios autores de `[K1]` no lo dispararon nunca en una corrida evaluada |
| — | ~~Almacén, captura, transporte de equipo, respondedor enrutado~~ | **Delegado a Rekal 1.0** `[N1]` | — | Se construye solo si Rekal deja de mantenerse o si su coste (170 MB, 5.9–6.6 turnos por consulta) resulta inaceptable para el caso |
| **último** | Skills procedurales, solo desde trayectorias propias contrastadas | +29 pp `[K8]`, pero −1.3 a −4.2 pp para skills importadas `[J2]`, y auto-mejora no replicada bajo varianza `[J4]` | alto | Sin control de longitud igualada y sin múltiples corridas con orden aleatorizado, **no se construye** |

**Regla de parada:** no pasar de un nivel al siguiente sin correr su medición con brazo
de control. Los niveles 1 y 2 **no inyectan tokens**, así que no pueden degradar la
respuesta por ruido — son los que se construyen primero incluso si la evaluación del
nivel 3 sale plana. El nivel 3 es el primero que puede hacer daño, y por eso es el
primero con criterio de muerte por control de longitud.

### 7.1 Protocolo de evaluación, con el estándar de agosto

1. **Control irrelevante de longitud igualada** en cada inyección. Estándar mínimo desde
   `[J2]`, que encontró modelos *length-distracted* donde una skill irrelevante de igual
   longitud reproduce casi toda la pérdida.
2. **Múltiples corridas y orden de tareas aleatorizado.** Sin eso, cualquier cifra de
   auto-mejora es un curriculum implícito `[J4]`.
3. **Benchmark multi-sesión con oráculos ejecutables**, no recall post-hoc. DreamBench-SWE
   es el único que mide nuestro caso `[K2]`; la línea a batir es **memoria de eventos
   literal, 45.56%**, no "sin memoria".
4. **Contrafactual de restauración** para separar pérdida irreversible de fallo de
   recuperación recuperable `[2609.08279]`.
5. **Cuatro niveles de medida**, no solo tokens: estado almacenado, contexto entregado,
   trabajo de gestión, resultado de tarea. Presupuestos de tokens iguales **no implican
   contexto entregado igual ni coste de gestión igual** `[2608.31057]`.
6. **Suficiencia de respuesta** sobre preguntas reales de desarrollador, no relevancia de
   recuperación: es donde el ranking deja de ayudar `[K3]`.

---

## 8. Autocrítica

**1. El riesgo principal ya no es el rendimiento: es que la capa no se propague.** La
revisión de 164 trabajos académicos y 100 registros de practicantes lo dice directamente:
*"improvements at one layer often fail to propagate to end-to-end outcomes"* `[J7]`.
Muninn puede cumplir todos sus contratos de latencia y precisión y no mover el resultado.
Mitigación: los niveles 1–3 no inyectan nada, así que su valor no depende de que el modelo
haga algo distinto — cambian lo que el sistema *permite*, no lo que el modelo *decide*.

**1-bis. El proyecto se ha reducido dos veces en una tarde, y puede reducirse otra.**
La v1 prometía mejorar la calidad de respuesta; `[J1]` lo falsificó para una sesión. La
v2 proponía cuatro funciones; Rekal ya tenía dos `[N1]`. Si mañana aparece un proyecto que
compile reglas a controles, o si Anthropic lo mete en `/doctor`, queda F3 y medio F2.
**Eso es lo correcto, no un fracaso**: el criterio del encargo era no reinventar la rueda,
y cada poda es el criterio funcionando. Pero hay que asumir que el resultado final puede
ser un plugin de unos miles de líneas, no una herramienta.

**2. El arte previo se me adelantó en el mecanismo.** `[K1]` publicó el modelo
cue-anchored, la disciplina de entrega y tres de las cuatro mediciones que yo iba a hacer,
en julio. Lo honesto es adoptarlo y citarlo. Lo que queda es real pero más estrecho de lo
que la v1 creía: la captura, el grafo de símbolos, la supersesión hecha cumplir, el
compilador de reglas, y la evaluación con brazos de control.

**3. Mi mejor evidencia sigue teniendo agujeros.** `[K1]` es n=3 brazos con hechos
sintéticos y autor compartido entre implementación y benchmark; nunca disparó los triggers
`symbol` ni `temporal`. `[K2]` declara explícitamente que no establece superioridad entre
condiciones con memoria. `[K14]` son 11 sesiones sin control. `[G3]` es N=1 sin brazo de
control y sin medir su propio coste. Lo que sí es fuerte y no depende de ninguno de ellos:
`[J1]` (288 corridas, dos agentes), `[J2]` (1 000 tareas, 4 modelos, control de longitud),
`[K4]` (247 694 vidas de instrucción), `[K5]` (481 ficheros, dos anotadores
independientes), `[K6]` (835 registros tipados), `[K7]` (5 sistemas × 9 escenarios × 9
modelos), `[C2]` (1 323 episodios, 7 familias), y mis mediciones.

**4. Si `path` basta, el proyecto se encoge a la mitad.** Los triggers de path ya son
nativos e industria estándar `[L2]`. Si en repos reales `path` + `event` capturan casi
todos los disparos útiles, F3 se reduce a configuración de `.claude/rules` y lo único que
queda es F1, F2 y la supersesión. **Eso seguiría valiendo la pena** —son las tres piezas
de coste cero en tokens— pero no es una herramienta, es un plugin pequeño. Hay que estar
dispuesto a aceptar ese resultado.

**5. El vendor se mueve más rápido que el proyecto.** En la ventana: `/skill-doctor`
(contabilidad de coste de contexto), antigüedad de sesión y coste de re-cacheo en los
hooks de resume, Guardian thread context en Codex. Dos de mis huecos de la v1 se cerraron
solos. Mitigación: construir sobre hooks y Markdown, nunca sobre formato propio, y
priorizar lo que el vendor tiene menos incentivo a hacer — procedencia verificable,
auditabilidad y compilación a controles.

**6. La superficie de envenenamiento no se ha reducido.** En la ventana: MemGauge
demuestra que el riesgo varía por etapa, con una transición de umbral en la escritura
`[2608.30177]`; e InjecMEM añade otro ataque `[2608.23471]`. Muninn nace mono-usuario y
local; la memoria compartida por equipo requiere procedencia firmada y control de
admisión, y es v2.

---

## 8-bis. Multi-harness: ¿reemplazar lo nativo o complementarlo?

Pregunta planteada después de la v3: dado que OpenCode, Cline o Pi no tienen memoria
nativa, ¿conviene hacer un todo-en-uno que también sirva para Claude Code y Codex
desactivando sus nativas? Respuesta corta: **el objetivo prioritario sí cambia, el
reemplazo no.**

### Lo que la pregunta acierta, y corrige la v3

**El mayor delta medible está en los harness sin memoria, no en Claude Code.** La v3
estaba implícitamente centrada en Claude Code. Eso era un error de priorización:

- En DreamBench-SWE, la condición **sin memoria externa rinde 11.67%** y la de
  **memoria de eventos literal 45.56%** `[K2]`. Ese ~4x es el delta que se cobra en un
  harness que no tiene nada. En Claude Code la línea base **ya incluye** la memoria
  automática, así que ahí el delta disponible es mucho menor y no está cuantificado.
- El principio general está medido: *"Gains compensate recoverable execution defects,
  where defect mass is near zero, and gain is near zero"* `[O3]`. Reemplazar una memoria
  nativa competente es precisamente el caso de masa de defecto pequeña.
- Y el harness importa más que casi nada: sobre **24 000 evaluaciones selladas**, el
  harness de evaluación mueve la tasa de resolución de **2.14% a 9.27%, un factor 4.3**,
  mientras la receta de entrenamiento la mueve 1.16 `[O4]`.

**La forma de "un todo en uno" está medida, y es núcleo + adaptadores.** *"The shared
core transfers and can be distilled into one universal harness, while an ecosystem
margin resists both and requires native re-evolution"* `[O3]`. No es un binario
universal; es un núcleo portable más un margen que hay que reimplementar por harness.

**Y la superficie existe.** OpenCode tiene eventos publicados —`session.compacted`,
`tool.execute.before/after`, `permission.asked`, `message.updated`— y uno que **no
tienen ni Claude Code ni Codex**: `experimental.session.compacting`, que dispara antes
de que el LLM genere el resumen y permite **inyectar en el prompt de compactación o
reemplazarlo**. Para el pilar de supervivencia a la compactación, **OpenCode es el mejor
objetivo del mercado** `[O1]`.

### Lo que la pregunta se equivoca

**1. "Mejor que la nativa" no es demostrable con la evidencia que existe.** DreamBench
compara tres condiciones con memoria y declara explícitamente que **no establece
superioridad entre ellas**: literal 45.56%, typed+raw 46.11%, Mem0 literal 53.89%, con
la diferencia marcada como no confirmatoria `[K2]`. No hay base para prometer que un
diseño propio bate a una memoria nativa competente. Prometerlo sería exactamente el
falso reporte de funcionamiento que este proyecto se propuso evitar.

**2. Sería retomar lo que acabo de delegar con datos.** Almacén, captura y respuesta ya
son Rekal 1.0 `[N1]`. Y **la entrega involuntaria por hooks ya es producto**: Hindsight
la envía para Cline desde junio-2026, con la misma tesis textual —*"no MCP tool the model
can forget to use"*— presupuesto configurable y banco de equipo `[O2]`. Un todo-en-uno
competiría de frente con dos productos enviados en su eje más fuerte, sin mediciones que
lo respalden.

**3. Desactivar lo nativo tira las únicas defensas contra el ruido que hoy existen.** Lo
nativo aporta topes duros —200 líneas / 25 KB con error explícito al pasarse— y la carga
del índice al arrancar, el compartido entre worktrees y los almacenes de equipo montados,
todo hecho por el harness `[O5]`. Sin esos topes se reproduce el crecimiento catastrófico
medido: **+226% sobre la vida del fichero y log-hazard de borrado −0.032 por commit**
`[K4]`. Reemplazar es asumir ese riesgo a cambio de una ganancia no cuantificada.

**4. No se puede prometer transferencia entre harness.** *"Cross-harness credit yields
configuration adaptation and no more portable capability"* `[O4]`. El núcleo se
distribuye; lo aprendido en un harness no mejora otro.

**5. El orquestador es otro proyecto, y ya lo está automatizando un modelo entrenado.**
JIT-Agent sintetiza harnesses a medida y sale competitivo con Claude Code y OpenCode,
con +20.2 puntos en un caso `[2608.25593]`. Competir ahí no es una extensión de Muninn:
es cambiar de producto.

### La decisión

**Un núcleo portable, dos modos de despliegue, y el modo lo decide la masa de defecto
del harness — no la ambición del producto.**

| | Modo complemento | Modo completo |
|---|---|---|
| **Harness** | Claude Code, Codex | OpenCode, Pi, Cline, y cualquiera sin memoria |
| **Nativas** | **Encendidas.** No se toca el almacén | No existen |
| **Muninn aporta** | F1 filtro de supersesión y revocación · F2 compilador de reglas a controles · F3 cues `symbol`/`event`/`temporal` | Lo anterior **más** el almacén — pero **literal, sin consolidar** |
| **Tokens inyectados** | 0 en F1 y F2; ≤700 en F3 | Igual, con los mismos topes duros que la nativa |
| **Delta esperado** | No cuantificado. Se justifica por los huecos medidos, no por batir a la nativa | ~4x sobre no tener memoria, por `[K2]` |
| **Prioridad** | Segunda | **Primera** |

Y una regla de no-competencia, que es lo que convierte esto en un todo-en-uno sin ser
otro almacén más: **si en el repo hay Rekal, Muninn no almacena.** Usa su ledger como
backend y se limita a filtrar, compilar y repartir. Lo mismo con la memoria automática
nativa y con Hindsight: Muninn se pone **delante** de cualquier almacén, no en su lugar.
Eso es precisamente lo que `[N4]` demuestra que hace falta —*"the containment boundary
that holds is the read"*— y lo que ningún almacén hace por sí mismo.

### El experimento que decide esto

En un harness sin memoria (OpenCode o Pi, por tener hooks limpios y ninguna nativa que
confunda la medición), tres brazos sobre un benchmark multi-sesión con oráculos
ejecutables:

1. sin memoria,
2. entrega involuntaria de episodios literales por hook,
3. lo mismo más el filtro de supersesión y revocación.

Brazo 1 vs 2 replica el 11.67% → 45.56% de `[K2]` en un harness real y con hooks, que
nadie ha publicado. Brazo 2 vs 3 es el valor de F1, que es la función con el número más
fuerte de toda la investigación. Y hace falta el cuarto brazo obligatorio desde `[J2]`:
**un control irrelevante de longitud igualada**, para saber si la ganancia es del
contenido o de la longitud.

Si el brazo 2 no bate al 1 en un harness real, el proyecto no existe. Si el 3 no bate al
2, Muninn se reduce a F2, el compilador de reglas — que sigue siendo lo único que nadie
ha construido `[N5]`.

## 8-ter. ¿Un paquete universal con almacén propio? Análisis crítico

Pregunta planteada después de §8-bis: hacer un solo paquete con los dos modos, con
almacén propio adaptado al filtro, los cues y el compilador, sin depender de terceros,
optimizado al punto técnico más alto. Respuesta: **una parte de la intuición es
correcta y la otra ya está construida por otros. Y "al punto técnico más alto" es el
objetivo equivocado, con evidencia.**

### Lo que la intuición acierta, y es importante

**"Una memoria adaptada para el filtro" es exactamente el diagnóstico correcto.** El
guard de revocación publicado es débil precisamente porque es agnóstico del backend y
tiene que inferir el conflicto de texto sin estructura: captura **4/14** en Zep, y en
cognee y Graphiti bajo inserción indirecta es **peor que no tener defensa** `[N4]`. No
se puede hacer un filtro exacto sobre un almacén que solo te devuelve prosa.

Pero de ahí no se sigue "hay que ser dueño de la memoria". Se sigue algo mucho más
pequeño: **hay que ser dueño del ledger tipado**, no del archivo.

| Muninn posee | Muninn no posee |
|---|---|
| El **ledger de claims tipados**: `(sujeto, relación, objeto, t_evento, t_registro, t_invalidación, ancla, procedencia)` para ~5 tipos de registro | Las transcripciones crudas y el archivo de sesiones |
| La **tabla de cues** con condiciones de disparo permanentes | Los embeddings y el índice semántico |
| El **compilador** de reglas a controles | El transporte de equipo |
| El presupuesto y el ledger de disparos | La ruta de respuesta a preguntas de "por qué" |

Eso es una tabla y un motor de reglas, no un producto de memoria. Es lo que hace el
filtro exacto sin competir con nadie.

### Lo que ya está construido, y hay que aceptarlo

**agentmemory** (open source) es literalmente la propuesta, enviada: 12 hooks de
auto-captura, **BM25 + vector + grafo con RRF**, SQLite sin dependencias externas,
self-hosted por defecto, modo keyless solo-BM25, embeddings locales opcionales,
**versionado y supersesión con "recall hygiene" que saca las versiones superadas de los
índices**, **procedencia inmutable estampada en la escritura**, secretos filtrados,
puente bidireccional con `MEMORY.md`, memoria de equipo con namespaces, visor en tiempo
real, trazas OTEL, y distribución a **50+ agentes** vía el CLI `skills`. Benchmarks
propios: LongMemEval-S R@5 95.2%, MRR 88.2%, búsqueda híbrida p50 14 ms `[P2]`.

Más **Hindsight** con entrega por hooks para Cline `[O2]`, **Rekal** para el ledger en
git `[N1]`, y al menos cuatro proyectos más en la misma categoría `[P1]`.

De las tres funciones de §4, **una ya la tiene agentmemory** (la supersesión, aunque
por versionado y detección de contradicciones, no por regla determinista medida contra
la rejilla de `[K7]`), y **la tercera la tienen agentmemory y Hindsight en su forma
semántica** (recall híbrido sobre el prompt en `SessionStart` / `UserPromptSubmit`).

**Lo que ninguno de los siete tiene, verificado uno a uno:**
1. ~~**El compilador de reglas a controles.** Cero en arXiv, cero en búsqueda de código.~~
   **Falso, corregido en §8-quinquies.** Existe como autoformalización a policy-as-code
   `[U1]`, más FAVA `[U4]`, VIGIL `[U3]` y ActPlane `[U2]`. Sigue ausente de agentmemory,
   Hindsight y Rekal, y sigue sin existir aplicado a CLAUDE.md y AGENTS.md.
2. **Condiciones de disparo permanentes por memoria** sobre `path`, `symbol` y
   `temporal`. Todos recuperan por similitud sobre el prompt.
3. ~~**Supersesión determinista exacta.**~~ **Parcialmente falso.** Está formalizada en
   *Stored Is Not Supported* `[U5]`, con firewall de procedencia en PPMF `[V3]` y con
   implementación bitemporal en `[S5]`. Lo que no existe es dentro de un harness de
   agente de código, en la ruta caliente y en milisegundos.
4. **Evaluación con control irrelevante de longitud igualada y resultado de tarea.**
   Los benchmarks de agentmemory son de recuperación (R@5, MRR), que es exactamente la
   métrica que `[J1]`, `[J2]` y `[K2]` demuestran que no predice el resultado.

### Lo que la intuición se equivoca

**1. "Al punto técnico más alto" es el objetivo equivocado, y agentmemory es la prueba.**
Tiene todo lo técnicamente maximal —grafo, consolidación de 4 niveles, decaimiento de
Ebbinghaus, embeddings, visor, OTEL, 54 herramientas MCP, 17 skills— y
**la consolidación está encendida por defecto en cuanto hay un proveedor LLM
configurado** `[P3]`. Es el único mecanismo que mi evidencia más fuerte mide cayendo
**por debajo del baseline sin memoria**: 54% de regresión en problemas ya resueltos
`[C1]`. El maximalismo técnico es lo que produce eso.

Y la configuración ganadora en el único benchmark multi-sesión con oráculos ejecutables
fue **almacenamiento literal de eventos, sin consolidar: 45.56% frente al 11.67% sin
memoria** — y el estudio declara que **ningún diseño con memoria bate a otro** `[K2]`.

La función objetivo correcta no es "el punto técnico más alto". Es **la mínima
complejidad que pasa una medición con brazo de control**.

**2. "No depender de terceros" cuesta cinco adaptadores contra cinco dianas móviles.**
Claude Code publicó ~62 versiones en dos meses. La actividad de commits sobre plugins
creció 8.8x en seis meses, con el 78% de los co-cambios entre instrucciones y scripts
funcionalmente acoplados. El 16% de los setups públicos lleva un defecto de seguridad.
Y el hook de compactación de OpenCode se llama `experimental.session.compacting` `[P5]`.
El núcleo no es el coste; los adaptadores lo son.

**3. "Cualquier harness" es falso de partida, y la documentación lo dice.** Cline **no
entrega la transcripción a los hooks**, solo corre en macOS y Linux, y sus plugins **no
cubren la extensión de VSCode**, que es como casi todo el mundo usa Cline. Y **Pi no
tiene sistema de permisos**, así que el compilador no tiene diana: ahí degrada a informe,
o Muninn tendría que *ser* la capa de permisos, que es otro proyecto y crítico de
seguridad `[P4]`.

**4. Hay dos objetivos de compilación, no uno.** Claude Code y Codex toman subprocesos:
el binario Rust sirve. OpenCode, Cline y Pi quieren un módulo **en proceso de
Node/TypeScript**. La salida limpia es un núcleo Rust expuesto como binario CLI y como
addon nativo `napi-rs` — un objetivo de build más, no una reimplementación `[P4]`.

### Viabilidad, con números

| Pregunta | Respuesta |
|---|---|
| ¿Es técnicamente posible? | Sí, y está hecho. No es riesgo de investigación: es riesgo de ejecución y de diferenciación. |
| ¿Es posible ser "la memoria por defecto de cualquier harness"? | No como se enunció. Sí para **cuatro** harness con adaptadores no triviales. Cline queda cojo por falta de transcripción y de Windows; Pi queda sin diana para el compilador. |
| ¿Conviene tener almacén propio? | **Solo el ledger tipado.** El archivo, los embeddings y el transporte no: ahí se compite de frente con agentmemory y Rekal sin mediciones que lo respalden. |
| ¿Conviene desactivar las nativas? | No. agentmemory, que es el incumbente, hace lo contrario: **puente bidireccional con `MEMORY.md`**. Y las nativas aportan los únicos topes duros contra el crecimiento catastrófico `[K4]` `[O5]`. |
| ¿Qué queda como diferencia defendible? | El compilador de reglas a controles, los cues no semánticos, la supersesión exacta medida, y la disciplina de evaluación. Tres features y un método, no una plataforma. |

### El encuadre que sí funciona

> **Corregido en §8-quater.** El encuadre de "gobernador sobre almacenes ajenos" que
> sigue es **peor en número de integraciones** que poseer el almacén, y se corrige en la
> sección siguiente. Se conserva el texto original para trazabilidad del cambio.

**Muninn no es la memoria. Es el gobernador que se pone delante de la memoria que ya
tengas** — nativa, agentmemory, Rekal, o una mínima literal propia en los harness que no
tienen ninguna. Posee el ledger tipado porque sin esquema el filtro no puede ser exacto.
No posee el archivo porque eso ya está resuelto mejor de lo que lo haríamos.

Y trae una cosa que ninguno de los siete competidores trae, que además no es código sino
método: **medir resultado de tarea con brazo de control y control de longitud igualada,
en vez de R@5.** Sobre la evidencia de esta investigación, ese es el hueco más grande de
la categoría entera: siete productos optimizando una métrica que tres estudios
independientes demuestran que no predice el resultado.

## 8-quater. Corrección: poseer el almacén sí es la decisión correcta

Planteado después de §8-ter: controlar la memoria de inicio a fin para poder añadir las
características que explotan el filtro, los cues y el compilador, y para no quedar
expuesto a que cada proveedor cambie su formato. **Es una decisión de diseño correcta, y
mi recomendación anterior era peor. Con tres correcciones a la reclamación.**

### Por qué "gobernador sobre almacenes ajenos" era peor

Contado en integraciones, la arquitectura de §8-ter se equivoca de dirección:

| | Gobernador sobre N almacenes | Ledger y motor propios |
|---|---|---|
| Drivers de formato | **4** — `MEMORY.md` nativo, REST de agentmemory, DuckDB/CLI de Rekal, API cloud de Hindsight | **0** |
| Adaptadores por harness | 5 (eventos) | 5 (eventos) |
| Entrada al filtro | La **intersección** de lo que exponen: texto y timestamp | Tripleta tipada con anclas y procedencia |
| Exactitud del filtro | Heurística. Es literalmente por qué `stale_guard` captura 4/14 en Zep y es peor que no tener defensa en dos sistemas `[N4]` | Exacta por construcción |

Poseer el ledger **adelgaza** los adaptadores y es lo único que hace exacto al filtro.
Concedido: la intuición era correcta y yo la infravaloré `[Q1]`.

### Tres correcciones a la reclamación

**1. La independencia es de formato, no de eventos.** Poseer el almacén elimina el
acoplamiento de formato. No elimina el de eventos: si Claude Code renombra
`UserPromptSubmit` o OpenCode retira `experimental.session.compacting`, Muninn se rompe
igual. La reclamación honesta es **5 adaptadores sobre eventos y 0 drivers sobre
formatos** — la mitad del acoplamiento, no inmunidad `[Q2]`.

**2. Poseer un almacén cuesta un mes de fallos, y está inventariado.** El único registro
operativo publicado de un subsistema de memoria propio lo documenta: **85 fallos de hook,
84 en las tres primeras semanas**, todos de la capa de almacén y captura — codificación
de texto en Windows, fallo de tabla virtual FTS5 con contención de bloqueo bajo hooks
concurrentes, captura abortando contra un stdout muerto, caracteres de control rompiendo
el parseo de FTS5 `[Q3]`. Converge —ninguno en los últimos 20 días— pero es el argumento
decisivo para que **el almacén sea mínimo**: cada capa que añades es una familia de
fallos que te toca descubrir en producción.

**3. "Mejor que agentmemory" no en su eje.** Ellos tienen vector, grafo, RRF y una cifra
de LongMemEval. Competir en R@5 es una pelea perdida y, peor, **irrelevante**: tres
estudios independientes dicen que la calidad de recuperación no predice el resultado
`[J1]` `[J2]` `[K2]`. La reclamación defendible es **otra función objetivo**: no degradar
al modelo, ser determinista, ser exacto sobre lo que está muerto, y costar ~1 ms. No es
"mejor memoria"; es una memoria construida para otra cosa.

### El riesgo nuevo: la carrera de estandarización

Hay un **W3C AI Agent Memory Interoperability Community Group** activo, más al menos
cinco propuestas compitiendo: Portable Agent Memory (arXiv:2605.11032, transferencia
verificada criptográficamente), PAM Specification v1.0, Universal Memory Protocol,
`memory-md-spec`, y la propuesta de MacPaw Research `[Q4]`.

Corta en los dos sentidos. Ningún estándar ha ganado, así que adaptarse a formatos de
vendor es perseguir una diana móvil — eso **apoya** poseer el motor. Pero con un CG del
W3C en marcha, un formato **cerrado** es una apuesta contra la estandarización.

**Resolución: poseer el motor, mantener el ledger exportable.** Los diferenciadores de
Muninn viven en *campos* —condiciones de disparo, tripleta de supersesión, ancla,
procedencia—, no en un contenedor cerrado. Y los estándares de transferencia casi nunca
especifican **semántica de cumplimiento**, que es exactamente donde está el valor.

### La decisión es correcta; el orden es lo que decide si funciona

Poseer el almacén, sí. **Primero, no.**

| Orden | Qué | Por qué en ese sitio |
|---|---|---|
| **1** | **F2, el compilador de reglas a controles** | No necesita almacén **en absoluto** y cuesta cero tokens. El mecanismo ya existe `[U1]` `[U3]` `[U4]`: aquí se **adapta** a CLAUDE.md, AGENTS.md y la superficie de permisos del harness, con el informe de cobertura que ninguno devuelve. Se envía solo y sirve solo. |
| **2** | **Almacén literal desechable + el experimento de cuatro brazos** | Es el go/no-go del proyecto. No se construye un almacén de producto para correr un experimento. Si el brazo de entrega involuntaria no bate a los episodios literales, te has ahorrado toda la superficie de agentmemory. |
| **3** | **El almacén de verdad, con el ledger tipado y los cues** | Y solo con las características que el experimento pidió. |

La regla que mantiene esto honesto, y que es exactamente lo que agentmemory no tiene:
**cada característica del almacén tiene que ser trazable a un punto de dolor medido, y la
configuración por defecto tiene que ser la que la evidencia soporta** — literal, sin
consolidar, con la sofisticación detrás de flags apagados. agentmemory trae la
consolidación **encendida por defecto** `[P3]`, que es el mecanismo con la evidencia
negativa más fuerte de toda esta investigación `[C1]`. Ese es el fallo que se hereda por
construir hacia el máximo técnico en vez de hacia la medición.

## 8-quinquies. Auditoría de veracidad y novedad: las tres funciones tienen arte previo

Resultado de dos iteraciones de auditoría con ventana estricta de tres meses
(12-jun → 12-sep-2026) y siete barridos con vocabularios distintos. Detalle completo en
las secciones R a W del log de evidencia.

### El fallo de método

Busqué el compilador con mi vocabulario —`compile`, `permission`, `instruction`— y
concluí que no existía. El campo lo llama **autoformalización**, **policy-as-code** y
**runtime enforcement**. Con esos términos aparecen cuatro trabajos en ventana. **Una
negativa por ausencia vale lo que valga el vocabulario de la búsqueda**, y esa es la
lección más cara de toda la investigación.

### Arte previo de las tres funciones, todo de los últimos tres meses

| Función | Arte previo | Qué queda sin hacer |
|---|---|---|
| **F1** filtro | `[U5]` *Stored Is Not Supported*: procedencia tipada, resolver con banderas de conflicto, obsolescencia y retención; en 24 casos de conformidad **no dejó pasar ninguna de las 19 oportunidades inseguras** frente a 19/19 de las reglas planas · `[V3]` PPMF, "lightweight memory middleware", ASR de 1.000 a cero · `[S3]` Zero-Mem descarta evidencia en conflicto de forma determinista · `[S5]` almacén bitemporal · `[E1]` MemStrata | Dentro del harness, en la ruta caliente, en milisegundos, sobre la memoria del propio harness |
| **F2** compilador | `[U1]` autoformalización de prompts, descripciones de tools MCP y documentos de política a Cedar · `[U4]` FAVA: NL → IR de permisos → SMT, 90,5 % de cumplimiento · `[U3]` VIGIL: specs de skills → política ejecutable, >95 % recall · `[U2]` ActPlane: cumplimiento en el SO con eBPF | Aplicado a CLAUDE.md y AGENTS.md, sobre la superficie de permisos del harness, con informe de cobertura |
| **F3** cues | `[V2]` Prospective Intention Store: lógica de ciclo de vida en código, espacio de acción tipado, sin entrenamiento, **82,9 % Set-F1 en PM-Bench** frente al 65,1 % del mejor scaffold publicado · `[K1]` modelo cue-anchored | En el harness de código en vez del bucle del agente, con presupuesto de tokens |

### Lo que eso cambia

**La reclamación pasa de "capacidad nueva" a "integración bajo restricción".** Lo único
defendible es la composición —ledger tipado, filtro determinista, entrega por cues,
compilación de reglas— **dentro de un harness de agente de código, en la ruta caliente,
en milisegundos de un solo dígito, con presupuesto de tokens y medida por resultado de
tarea**. Integrar bajo restricción es trabajo real. «Esto no lo tiene nadie» no lo es.

### Y tres cosas que la auditoría dio a cambio

**1. La mejor evidencia de F1, y es en código.** Memorias inseguras aumentan el código
vulnerable **entre 2,7 y 50,3 puntos** en cuatro LLMs y cinco lenguajes, con una brecha
de aviso de 5,4–14,0 pp. Añadir requisitos de seguridad o reducir el almacenamiento bajan
la vulnerabilidad 19,7–33,6 pp **pero cuestan hasta 15,9 pp de corrección funcional**; el
**filtrado a nivel de memoria detecta el 100 % y restaura el baseline sin ese coste**
`[W1]`. Esto sustituye al proxy de ejecución de herramientas como evidencia principal.

**2. Una evaluación pública para F3.** **PM-Bench** mide memoria prospectiva; el mejor
método publicado llega al 65,1 % de F1 y el almacén de intenciones tipado al 82,9 %
`[V1]` `[V2]`. F3 deja de no tener forma de medirse.

**3. Un requisito de seguridad con nombre.** El análisis de ensamblado de contexto en 12
harness reales, Claude Code y Codex incluidos, nombra **X-CPE**: contenido que persiste
más allá del contexto en el que se introdujo. **Hacer eso es literalmente la función de
F3.** Sin etiquetas de privilegio y procedencia en cada inyección, Muninn *es* el vector
`[W2]`.

### Y dos avisos que degradan cifras de este documento

**El 27,5 %.** Una ablación en ventana cuantifica una discrepancia de **27,5 puntos entre
token-F1 estricto y LLM-as-judge sobre las mismas salidas** `[W3]`. Toda cifra de LoCoMo o
LongMemEval de este documento —incluidas las de Rekal y agentmemory— hereda esa
incertidumbre de método.

**El contexto irrelevante.** «Degrada la salida» es demasiado fuerte para 2026: en
agregado a menudo **no** baja la accuracy; lo que produce es **inestabilidad por ejemplo
en dos direcciones**, específica de cada modelo `[S1]`. El argumento del silencio por
defecto pasa de «pierdes precisión» a «introduces varianza impredecible», que es igual de
fuerte y más honesto.

## 9. Frase de trabajo, corregida dos veces

> Muninn no le da conocimiento al agente: eso ya lo tiene y no es lo que le falla.
> No almacena ni responde: eso ya está resuelto y funciona mejor de lo que yo lo haría.
> **Filtra** por regla lo superado y lo revocado antes de que llegue al modelo —la
> frontera que aguanta es la lectura—, **compila** las reglas que el proyecto escribe en
> controles que se hacen cumplir, y **reparte** lo que queda de forma involuntaria, en un
> milisegundo, dentro de un presupuesto duro y con su procedencia puesta.
> Las dos primeras no gastan un solo token de contexto. La tercera tiene tope y criterio
> de muerte.

Los tres criterios del encargo, contestados con lo que el barrido de dos meses permite
afirmar:

- **No reinventar la rueda.** Tres cosas delegadas con nombre y versión: el almacén, la
  captura y el respondedor a Rekal 1.0; los triggers de path a `.claude/rules`; el pinning
  de autorizaciones a Codex Guardian. Y una corrección de la v1: los almacenes de memoria
  de equipo ya eran nativos.
- **No añadir ruido.** Dos de las tres funciones no inyectan ni un token. La tercera
  hereda la disciplina medida del arte previo —silencio por defecto, tope duro, ledger que
  se reinicia en la compactación, procedencia— y lleva como criterio de muerte el control
  irrelevante de longitud igualada, que es el estándar mínimo desde `[J2]`.
- **Rendimiento.** 1.034 ms p50 y 1.143 ms p99 para la evaluación de cues completa con
  filtro de supersesión; 4.07 ms para el hook entero; cero para lo que no inyecta. Medido
  en esta máquina, no estimado.

**Lo que sigue sin estar demostrado, y hay que demostrar antes de construir el nivel 3:**
que entregar involuntariamente el recuerdo correcto mejora el resultado de una tarea
multi-sesión **por encima de guardar los episodios literales**, que es la línea real a
batir (45.56% frente a 11.67% sin memoria `[K2]`). Nadie ha publicado esa comparación.
Es el experimento que justifica o mata el proyecto.


---

## 8-sexies. Cierre de la auditoría: qué queda en pie

Nueve barridos de literatura con vocabularios distintos sobre la ventana de tres meses,
más la verificación producto a producto de nueve sistemas. Detalle en las secciones R a Y
del log de evidencia.

### Lo que la auditoría destruyó

1. **La reclamación de novedad, entera.** Las tres funciones tienen arte previo publicado
   y medido, todo de los últimos tres meses `[U1]`–`[U5]`, `[V2]`, `[V3]`, `[Y1]`.
2. **El diseño de grano fino de F1.** Con control de render, su residuo es
   indistinguible de cero; la invalidación gruesa lo bate `[X1]`.
3. **"El contexto irrelevante degrada la salida."** En agregado a menudo no; lo que
   produce es inestabilidad por ejemplo, en dos direcciones `[S1]`.
4. **Dos errores de hecho**: Letta no estaba entre los cinco sistemas medidos por `[K7]`,
   y MemoryArena comparó cuatro sistemas, no "todos".
5. **Doce sobre-afirmaciones**, listadas en la sección T del log.

### Lo que la auditoría dio a cambio

1. **La mejor evidencia de F1, y es en código:** memorias inseguras suben el código
   vulnerable **2,7–50,3 pp**; el filtrado a nivel de memoria detecta el 100 % y
   **restaura el baseline sin coste de corrección funcional**, mientras las dos
   alternativas obvias cuestan hasta 15,9 pp `[W1]`.
2. **Una evaluación pública para F3:** PM-Bench, con 65,1 % del mejor scaffold publicado
   y 82,9 % del almacén de intenciones tipado como líneas a batir `[V1]` `[V2]`.
3. **Un requisito de seguridad con nombre:** X-CPE. Hacer que algo persista más allá de
   su contexto es la función de F3, y sin etiquetas de privilegio Muninn *es* el vector
   `[W2]`.
4. **Confirmación máxima del stack:** sobre cuatro millones de líneas de once harness de
   producción, **ninguno recupera código con embeddings vectoriales** `[X3]`.
5. **Un tercer control obligatorio** en el protocolo: render igualado `[X1]`.
6. **La advertencia que gobierna el proyecto:** *"implementado, alcanzable y efectivo son
   tres preguntas distintas, y la tercera exige un oráculo independiente del mecanismo
   bajo prueba"* — de un sistema que tenía diez mecanismos implementados, alcanzables e
   **inefectivos** `[Y1]`.

### Lo que queda en pie

El **diagnóstico**: los cuatro fallos multi-sesión siguen medidos y sin resolver dentro
de un harness. La **arquitectura**: motor y ledger propios, filtro en la lectura,
compilación a controles, entrega involuntaria con presupuesto. El **rendimiento**:
4,07 ms de hook y 1,034 ms de evaluación de cues, medidos. Y el **orden**: lo que no
inyecta tokens primero, el experimento antes que el almacén de producto.

Lo que ya no queda en pie es la frase "esto no lo tiene nadie". Y el siguiente paso no es
leer más papers — el rendimiento marginal de los dos últimos barridos fue cero cambios de
diseño. **Es correr el experimento de cuatro brazos con un oráculo independiente.**

---

## 10. Veredicto competitivo (técnico, sin contar comunidad ni exposición)

**En un eje sí. En el conjunto no.** Y "mejor" depende de qué se mida.

| Eje | Veredicto |
|---|---|
| Latencia en la ruta caliente | **Mejor, estructural.** 4,07 ms hook / 1,03 ms cues frente a 14 ms + servidor (agentmemory), ~300 ms (Hindsight), 149–241 ms (Zep). Medido sobre esquema sintético |
| No degradar al modelo | **Mejor por construcción, no medido** |
| Invalidación exacta | Entre los pocos, no único `[K7]` `[Y1]` `[V3]` `[U5]`; la versión gruesa que paga la hace cualquiera `[X1]` |
| Compilador de reglas | Único en la categoría; el mecanismo está publicado `[U1]` |
| Cues no semánticos | Plausible, no demostrado `[K1]` `[V2]` |
| Calidad de recall | **Peor, a propósito**; y toda cifra ahí carga 27,5 pp de incertidumbre `[W3]` |
| Responder "por qué", transporte de equipo, amplitud de harness, madurez | **Peor** |
| Resultado de tarea | Nadie lo tiene. Ventaja de método, no de producto |

La función objetivo donde Muninn gana la eligió esta investigación. Es defendible
`[J1]` `[J2]` `[K2]` `[S2]`, pero un competidor puede elegir recall y amplitud con la
misma legitimidad. En el único benchmark multi-sesión ejecutable, ningún diseño bate a
otro `[K2]`: nadie puede reclamar "mejor" por resultado, y Muninn tampoco.

**Puede ser la mejor en lo que elige hacer. No puede ser "la mejor memoria."** Lo que
cambiaría eso es un número que nadie tiene: resultado de tarea con brazo de control,
control de longitud y control de render, en un harness real.
