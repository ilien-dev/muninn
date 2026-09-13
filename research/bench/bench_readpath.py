"""Ruta de lectura completa propuesta, medida extremo a extremo en un solo proceso.

Etapas, en el orden en que se ejecutan y cortan:
  1. compuerta de intención  (regex sobre el prompt; la mayoría de turnos salen aquí)
  2. BM25 sobre FTS5         (candidatos léxicos, k=50)
  3. embedding estático      (potion-retrieval-32M, sin red neuronal en inferencia)
  4. producto punto exhaustivo sobre la matriz en RAM (k=50)
  5. fusión RRF + presupuesto de tokens duro
Sin red, sin servidor, sin llamada a LLM. Un proceso, un hilo BLAS.
"""
import json, os, random, re, sqlite3, statistics, time
import numpy as np
from model2vec import StaticModel

os.environ.setdefault("OMP_NUM_THREADS", "1")
random.seed(3)
N = 50_000
BUDGET_TOKENS = 700

rows = [json.loads(l) for i, l in enumerate(open("/tmp/corpus.jsonl")) if i < N]

db = sqlite3.connect("/tmp/readpath.db")
db.executescript("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
db.execute("DROP TABLE IF EXISTS mem")
db.execute("CREATE VIRTUAL TABLE mem USING fts5(title, text, tokenize='unicode61')")
db.executemany("INSERT INTO mem(rowid,title,text) VALUES (?,?,?)",
               ((r["id"] + 1, r["title"], r["text"]) for r in rows))
db.execute("INSERT INTO mem(mem) VALUES('optimize')")
db.commit()

model = StaticModel.from_pretrained("minishlab/potion-retrieval-32M")
E = model.encode([r["text"] for r in rows], show_progress_bar=False).astype(np.float32)
E /= np.linalg.norm(E, axis=1, keepdims=True) + 1e-9

INTENT = re.compile(r"\b(implement|build|add|fix|refactor|why|investigate|debug|"
                    r"decide|design|should we|migrate|remove|replace)\b", re.I)

def read_path(prompt):
    if not INTENT.search(prompt):            # 1. compuerta
        return []
    toks = re.findall(r"[0-9A-Za-z]{3,}", prompt)
    toks = sorted(toks, key=len, reverse=True)[:8]   # proxy de IDF alto
    if not toks:
        return []
    try:                                      # 2. BM25
        bm = [r[0] - 1 for r in db.execute(
            "SELECT rowid FROM mem WHERE mem MATCH ? ORDER BY bm25(mem,2.0,1.0) LIMIT 50",
            (" OR ".join(f'"{t}"' for t in toks),)).fetchall()]
    except sqlite3.OperationalError:
        bm = []
    v = model.encode([prompt], show_progress_bar=False).astype(np.float32)[0]  # 3.
    v /= np.linalg.norm(v) + 1e-9
    s = E @ v                                                                   # 4.
    idx = np.argpartition(s, -50)[-50:]
    vec = [int(i) for i in idx[np.argsort(-s[idx])]]
    sc = {}                                                                     # 5.
    for L in (bm, vec):
        for rank, d in enumerate(L):
            sc[d] = sc.get(d, 0.0) + 1.0 / (60 + rank + 1)
    out, used = [], 0
    for d, _ in sorted(sc.items(), key=lambda x: -x[1]):
        cost = len(rows[d]["text"]) // 4
        if used + cost > BUDGET_TOKENS:
            break
        out.append(d); used += cost
    return out

prompts_intent, prompts_plain = [], []
for _ in range(200):
    r = random.choice(rows); w = r["text"].split()
    i = random.randrange(0, max(1, len(w) - 14))
    frag = " ".join(w[i:i + 14])
    prompts_intent.append("why does " + frag + " fail?")
    prompts_plain.append("thanks, " + frag)

def run(ps):
    lat = []
    for p in ps:
        t = time.perf_counter(); read_path(p); lat.append((time.perf_counter() - t) * 1000)
    return lat

for w in range(20):
    read_path(prompts_intent[w])

def pct(xs, q):
    xs = sorted(xs); return xs[min(len(xs) - 1, int(len(xs) * q))]

li, lp = run(prompts_intent), run(prompts_plain)
print(f"corpus = {len(rows)} registros, presupuesto = {BUDGET_TOKENS} tokens\n")
for name, lat in [("turno CON intención (ruta completa)", li),
                  ("turno SIN intención (corte en la compuerta)", lp)]:
    print(f"{name:<45} p50 {statistics.median(lat):7.3f} ms   "
          f"p95 {pct(lat,.95):7.3f} ms   p99 {pct(lat,.99):7.3f} ms")
mix = li[:30] + lp[:170]      # ~15% de turnos con intención, como en el registro de SIx Harness
print(f"{'mezcla realista 15% intención':<45} p50 {statistics.median(mix):7.3f} ms   "
      f"p95 {pct(mix,.95):7.3f} ms   p99 {pct(mix,.99):7.3f} ms")
print(f"\nRAM del índice vectorial: {E.nbytes/1e6:.0f} MB   "
      f"tamaño SQLite: {os.path.getsize('/tmp/readpath.db')/1e6:.0f} MB")
