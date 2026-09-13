"""Calidad de recuperación con protocolo known-item de frase-fuera.

Para 500 registros se extrae una frase, se indexa el registro SIN esa frase y se usa
la frase como consulta. Así hay hueco léxico real y se mide si el vector aporta algo
sobre BM25. Métricas: recall@10 y MRR@10. Fusión: RRF (k=60).
"""
import json, os, random, re, sqlite3, time
import numpy as np
from model2vec import StaticModel

random.seed(11)
N = 50_000
NQ = 500
rows = [json.loads(l) for i, l in enumerate(open("/tmp/corpus.jsonl")) if i < N]

sent_re = re.compile(r"(?<=[.!?]) +")
probes = []
tries = 0
while len(probes) < NQ and tries < NQ * 20:
    tries += 1
    r = random.choice(rows)
    sents = [s for s in sent_re.split(r["text"]) if len(s.split()) >= 12]
    if not sents:
        continue
    s = random.choice(sents)
    probes.append((r["id"], s))
hold = {i: s for i, s in probes}

texts = []
for r in rows:
    t = r["text"]
    if r["id"] in hold:
        t = t.replace(hold[r["id"]], " ")
    texts.append(t)

# --- BM25 ---
db = sqlite3.connect(":memory:")
db.execute("CREATE VIRTUAL TABLE mem USING fts5(title, text, tokenize='unicode61')")
db.executemany("INSERT INTO mem(rowid, title, text) VALUES (?,?,?)",
               ((r["id"] + 1, r["title"], t) for r, t in zip(rows, texts)))
db.execute("INSERT INTO mem(mem) VALUES('optimize')")

def bm25(q, k=50):
    toks = [w for w in re.findall(r"[0-9A-Za-z]{3,}", q)][:24]
    if not toks:
        return []
    m = " OR ".join(f'"{w}"' for w in toks)
    try:
        return [r[0] - 1 for r in db.execute(
            "SELECT rowid FROM mem WHERE mem MATCH ? ORDER BY bm25(mem,2.0,1.0) LIMIT ?",
            (m, k)).fetchall()]
    except sqlite3.OperationalError:
        return []

# --- vectores estáticos ---
model = StaticModel.from_pretrained("minishlab/potion-retrieval-32M")
E = model.encode(texts, show_progress_bar=False).astype(np.float32)
E /= np.linalg.norm(E, axis=1, keepdims=True) + 1e-9

def vec(q, k=50):
    v = model.encode([q], show_progress_bar=False).astype(np.float32)[0]
    v /= np.linalg.norm(v) + 1e-9
    s = E @ v
    idx = np.argpartition(s, -k)[-k:]
    return [int(i) for i in idx[np.argsort(-s[idx])]]

def rrf(*lists, k=60, top=10):
    sc = {}
    for L in lists:
        for rank, d in enumerate(L):
            sc[d] = sc.get(d, 0.0) + 1.0 / (k + rank + 1)
    return [d for d, _ in sorted(sc.items(), key=lambda x: -x[1])[:top]]

def score(runs):
    rec = mrr = 0
    for gold, res in runs:
        r10 = res[:10]
        if gold in r10:
            rec += 1
            mrr += 1.0 / (r10.index(gold) + 1)
    return rec / len(runs), mrr / len(runs)

rb, rv, rh = [], [], []
t0 = time.perf_counter()
for gold, q in probes:
    b, v = bm25(q), vec(q)
    rb.append((gold, b)); rv.append((gold, v)); rh.append((gold, rrf(b, v)))
dt = time.perf_counter() - t0

print(f"corpus={len(rows)}  consultas={len(probes)}  ({dt/len(probes)*1000:.1f} ms/consulta, 3 motores)")
for name, runs in [("BM25 (FTS5)", rb), ("vector estático", rv), ("híbrido RRF", rh)]:
    r, m = score(runs)
    print(f"  {name:<18} recall@10 {r:.3f}   MRR@10 {m:.3f}")
