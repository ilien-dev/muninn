"""Coste de la ruta de escritura incremental: ingestar UNA sesión en un índice ya
poblado (50 000 registros). Es lo que corre en SessionEnd.
"""
import json,os,sqlite3,statistics,time
import numpy as np
from model2vec import StaticModel
os.environ.setdefault("OMP_NUM_THREADS","1")
rows=[json.loads(l) for i,l in enumerate(open("/tmp/corpus.jsonl")) if i<50200]
base,new=rows[:50000],rows[50000:]
db=sqlite3.connect("/tmp/write.db"); 
db.executescript("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
db.execute("DROP TABLE IF EXISTS mem")
db.execute("CREATE VIRTUAL TABLE mem USING fts5(title,text,tokenize='unicode61')")
db.executemany("INSERT INTO mem(rowid,title,text) VALUES (?,?,?)",((r["id"]+1,r["title"],r["text"]) for r in base))
db.execute("INSERT INTO mem(mem) VALUES('optimize')"); db.commit()
m=StaticModel.from_pretrained("minishlab/potion-retrieval-32M")
E=m.encode([r["text"] for r in base],show_progress_bar=False).astype(np.float32)

lat_f,lat_e=[],[]
for r in new:                     # 200 registros nuevos = ~1 sesión larga
    t=time.perf_counter()
    db.execute("INSERT INTO mem(rowid,title,text) VALUES (?,?,?)",(r["id"]+1,r["title"],r["text"])); db.commit()
    lat_f.append((time.perf_counter()-t)*1000)
    t=time.perf_counter()
    v=m.encode([r["text"]],show_progress_bar=False).astype(np.float32)
    lat_e.append((time.perf_counter()-t)*1000)
def pct(x,p): x=sorted(x); return x[min(len(x)-1,int(len(x)*p))]
print(f"insert FTS5 + commit, por registro   p50 {statistics.median(lat_f):.3f} ms  p95 {pct(lat_f,.95):.3f} ms")
print(f"encode del registro                  p50 {statistics.median(lat_e):.3f} ms  p95 {pct(lat_e,.95):.3f} ms")
tot=sum(lat_f)+sum(lat_e)
print(f"total de ingestar 200 registros:     {tot:.0f} ms")
t=time.perf_counter(); db.execute("INSERT INTO mem(mem) VALUES('optimize')"); db.commit()
print(f"optimize del indice tras la ingesta: {(time.perf_counter()-t)*1000:.0f} ms")
