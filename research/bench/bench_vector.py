"""Mide el coste real de la vía semántica en la ruta caliente:
   1. codificar la consulta con un embedding estático (model2vec / potion-base-8M)
   2. producto punto exhaustivo contra N vectores en memoria (numpy, float32)
   3. lo mismo con vectores int8 cuantizados
Un solo hilo salvo lo que haga BLAS. Sin red, sin GPU.
"""
import json, os, random, statistics, time
import numpy as np
from model2vec import StaticModel

random.seed(7)
os.environ.setdefault("OMP_NUM_THREADS", "1")

rows = [json.loads(l) for i, l in enumerate(open("/tmp/corpus.jsonl")) if i < 144000]
print(f"corpus: {len(rows)} registros")

t = time.perf_counter()
model = StaticModel.from_pretrained("minishlab/potion-base-8M")
print(f"carga del modelo: {time.perf_counter()-t:.2f} s")

t = time.perf_counter()
emb = model.encode([r["text"] for r in rows], show_progress_bar=False).astype(np.float32)
enc_all = time.perf_counter() - t
emb /= np.linalg.norm(emb, axis=1, keepdims=True) + 1e-9
print(f"indexado (encode de {len(rows)} registros): {enc_all:.1f} s "
      f"= {len(rows)/enc_all:,.0f} registros/s, dim={emb.shape[1]}, "
      f"RAM float32 = {emb.nbytes/1e6:.0f} MB")

q8 = np.clip(np.round(emb * 127), -127, 127).astype(np.int8)
print(f"RAM int8 = {q8.nbytes/1e6:.0f} MB")

queries = []
for _ in range(200):
    r = random.choice(rows)
    w = r["text"].split()
    i = random.randrange(0, max(1, len(w) - 12))
    queries.append(" ".join(w[i:i + 12]))

def pct(xs, p):
    xs = sorted(xs); return xs[min(len(xs) - 1, int(len(xs) * p))]

for label, n in [("10k", 10_000), ("50k", 50_000), ("144k", len(rows))]:
    M, M8 = emb[:n], q8[:n]
    lat_e, lat_s, lat_s8 = [], [], []
    for q in queries:
        t = time.perf_counter()
        v = model.encode([q], show_progress_bar=False).astype(np.float32)[0]
        lat_e.append((time.perf_counter() - t) * 1000)
        v /= np.linalg.norm(v) + 1e-9

        t = time.perf_counter()
        np.argpartition(M @ v, -10)[-10:]
        lat_s.append((time.perf_counter() - t) * 1000)

        v8 = np.clip(np.round(v * 127), -127, 127).astype(np.int8)
        t = time.perf_counter()
        np.argpartition(M8.astype(np.int16) @ v8.astype(np.int16), -10)[-10:]
        lat_s8.append((time.perf_counter() - t) * 1000)
    print(f"\nN={label}")
    print(f"  encode consulta   p50 {statistics.median(lat_e):7.3f} ms  p95 {pct(lat_e,.95):7.3f} ms")
    print(f"  búsqueda f32      p50 {statistics.median(lat_s):7.3f} ms  p95 {pct(lat_s,.95):7.3f} ms")
    print(f"  búsqueda int8     p50 {statistics.median(lat_s8):7.3f} ms  p95 {pct(lat_s8,.95):7.3f} ms")
