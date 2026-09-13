"""Mide la latencia de recuperación de tres estrategias sobre el mismo corpus.

  A. FTS5 + bm25()   -> índice invertido en proceso (SQLite embebido)
  B. scan de subcadena sobre JSONL -> lo que hace el servidor MCP oficial de memoria
  C. FTS5 con prefiltro por recencia -> caso realista de memoria de agente

Todo en un solo proceso, sin red. Reporta p50/p95/p99 en milisegundos.
"""
import json, os, re, random, sqlite3, statistics, sys, time

CORPUS = "/tmp/corpus.jsonl"
SIZES = [1_000, 10_000, 50_000, 144_000]
NQ = 300
random.seed(7)


def load(n):
    rows = []
    with open(CORPUS) as fh:
        for i, line in enumerate(fh):
            if i >= n:
                break
            rows.append(json.loads(line))
    return rows


def make_queries(rows, k=NQ):
    """Consultas de 2-4 palabras tomadas del propio corpus: el caso favorable
    para el scan de subcadena y realista para memoria (el agente recuerda términos)."""
    qs = []
    for _ in range(k):
        r = random.choice(rows)
        w = [re.sub(r"[^0-9A-Za-z]", "", x) for x in r["text"].split()]
        w = [x for x in w if len(x) > 4]
        if len(w) < 4:
            continue
        i = random.randrange(0, len(w) - 3)
        qs.append(" ".join(w[i:i + random.choice([2, 3, 4])]))
    return qs


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(len(xs) * p))]


def bench_fts5(rows, queries, path="/tmp/bench_fts5.db"):
    if os.path.exists(path):
        os.remove(path)
    db = sqlite3.connect(path)
    db.execute("PRAGMA journal_mode=WAL")
    db.execute("PRAGMA synchronous=NORMAL")
    db.execute("CREATE VIRTUAL TABLE mem USING fts5(title, text, ts UNINDEXED, tokenize='unicode61')")
    t0 = time.perf_counter()
    db.executemany("INSERT INTO mem(title, text, ts) VALUES (?,?,?)",
                   ((r["title"], r["text"], r["id"]) for r in rows))
    db.commit()
    db.execute("INSERT INTO mem(mem) VALUES('optimize')")
    db.commit()
    build = time.perf_counter() - t0
    size = os.path.getsize(path)

    lat = []
    sql = ("SELECT rowid, bm25(mem, 2.0, 1.0) AS s FROM mem WHERE mem MATCH ? "
           "ORDER BY s LIMIT 10")
    for q in queries:
        m = " ".join(f'"{w}"' for w in q.split())
        t = time.perf_counter()
        db.execute(sql, (m,)).fetchall()
        lat.append((time.perf_counter() - t) * 1000)
    db.close()
    return build, size, lat


def bench_scan(rows, queries):
    """Réplica fiel de searchNodes() del servidor MCP oficial: recarga el fichero
    entero y filtra por includes() en minúsculas."""
    path = "/tmp/bench_scan.jsonl"
    with open(path, "w") as fh:
        for r in rows:
            fh.write(json.dumps(r) + "\n")
    lat = []
    for q in queries[:40]:                      # es O(n) por consulta; 40 basta
        ql = q.lower()
        t = time.perf_counter()
        hits = []
        with open(path) as fh:
            for line in fh:
                item = json.loads(line)
                if ql in item["text"].lower() or ql in item["title"].lower():
                    hits.append(item)
        lat.append((time.perf_counter() - t) * 1000)
    return lat


print(f"{'n':>8} {'motor':<26} {'build s':>8} {'MB':>7} {'p50 ms':>9} {'p95 ms':>9} {'p99 ms':>9}")
for n in SIZES:
    rows = load(n)
    if len(rows) < n:
        n = len(rows)
    qs = make_queries(rows)
    b, sz, lat = bench_fts5(rows, qs)
    print(f"{n:>8} {'SQLite FTS5 + bm25':<26} {b:>8.2f} {sz/1e6:>7.1f} "
          f"{statistics.median(lat):>9.3f} {pct(lat,.95):>9.3f} {pct(lat,.99):>9.3f}")
    lat2 = bench_scan(rows, qs)
    print(f"{n:>8} {'scan JSONL (MCP oficial)':<26} {'-':>8} {'-':>7} "
          f"{statistics.median(lat2):>9.3f} {pct(lat2,.95):>9.3f} {pct(lat2,.99):>9.3f}")
