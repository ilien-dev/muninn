import json,os,random,re,sqlite3,statistics,time
import numpy as np
from model2vec import StaticModel
os.environ.setdefault("OMP_NUM_THREADS","1")
random.seed(3); N=50_000
rows=[json.loads(l) for i,l in enumerate(open("/tmp/corpus.jsonl")) if i<N]
db=sqlite3.connect("/tmp/readpath.db")
model=StaticModel.from_pretrained("minishlab/potion-retrieval-32M")
E=model.encode([r["text"] for r in rows],show_progress_bar=False).astype(np.float32)
E/=np.linalg.norm(E,axis=1,keepdims=True)+1e-9
stage={k:[] for k in("bm25_24","bm25_8","encode","dot","fuse")}
ps=[]
for _ in range(150):
    r=random.choice(rows);w=r["text"].split();i=random.randrange(0,max(1,len(w)-14))
    ps.append("why does "+" ".join(w[i:i+14])+" fail?")
def q(toks):
    try:
        return [r[0]-1 for r in db.execute("SELECT rowid FROM mem WHERE mem MATCH ? ORDER BY bm25(mem,2.0,1.0) LIMIT 50",(" OR ".join(f'"{t}"' for t in toks),)).fetchall()]
    except sqlite3.OperationalError: return []
for p in ps:
    toks=re.findall(r"[0-9A-Za-z]{3,}",p)
    t=time.perf_counter(); bm=q(toks[:24]); stage["bm25_24"].append((time.perf_counter()-t)*1000)
    # solo los 8 términos más raros (IDF alto) en lugar de los 24 primeros
    t=time.perf_counter()
    idf=sorted(toks,key=lambda w:len(w),reverse=True)[:8]
    bm2=q(idf); stage["bm25_8"].append((time.perf_counter()-t)*1000)
    t=time.perf_counter(); v=model.encode([p],show_progress_bar=False).astype(np.float32)[0]; stage["encode"].append((time.perf_counter()-t)*1000)
    v/=np.linalg.norm(v)+1e-9
    t=time.perf_counter(); s=E@v; idx=np.argpartition(s,-50)[-50:]; vec=[int(i) for i in idx[np.argsort(-s[idx])]]; stage["dot"].append((time.perf_counter()-t)*1000)
    t=time.perf_counter()
    sc={}
    for L in (bm,vec):
        for rk,d in enumerate(L): sc[d]=sc.get(d,0.)+1/(60+rk+1)
    used=0;out=[]
    for d,_ in sorted(sc.items(),key=lambda x:-x[1]):
        c=len(rows[d]["text"])//4
        if used+c>700: break
        out.append(d);used+=c
    stage["fuse"].append((time.perf_counter()-t)*1000)
def pct(x,p): x=sorted(x); return x[min(len(x)-1,int(len(x)*p))]
for k,v in stage.items():
    print(f"{k:<10} p50 {statistics.median(v):8.3f} ms  p95 {pct(v,.95):8.3f} ms")
