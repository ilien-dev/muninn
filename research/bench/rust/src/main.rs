// Prueba de viabilidad en Rust de la ruta de lectura: FTS5 embebido (rusqlite bundled)
// + embeddings estáticos (model2vec-rs). Mide encode y búsqueda BM25 por separado.
use model2vec_rs::model::StaticModel;
use rusqlite::Connection;
use std::time::Instant;

fn pct(v: &mut Vec<f64>, p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 * p) as usize).min(v.len() - 1)]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::open("/tmp/rust_fts.db")?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
                        DROP TABLE IF EXISTS mem;
                        CREATE VIRTUAL TABLE mem USING fts5(title,text,tokenize='unicode61');")?;
    let data = std::fs::read_to_string("/tmp/corpus.jsonl")?;
    let rows: Vec<(String, String)> = data
        .lines().take(50_000)
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .map(|v| (v["title"].as_str().unwrap_or("").to_string(),
                  v["text"].as_str().unwrap_or("").to_string()))
        .collect();
    let t = Instant::now();
    {
        let tx = conn.unchecked_transaction()?;
        let mut st = tx.prepare("INSERT INTO mem(title,text) VALUES (?1,?2)")?;
        for (a, b) in &rows { st.execute((a, b))?; }
        drop(st);
        tx.commit()?;
    }
    conn.execute_batch("INSERT INTO mem(mem) VALUES('optimize');")?;
    println!("indexado FTS5 de {} registros: {:.2} s", rows.len(), t.elapsed().as_secs_f64());

    let t = Instant::now();
    let model = StaticModel::from_pretrained("minishlab/potion-retrieval-32M", None, None, None)?;
    println!("carga del modelo: {:.2} s", t.elapsed().as_secs_f64());

    // consultas: 8 términos largos de un registro, como en el benchmark de Python
    let mut queries = Vec::new();
    for r in rows.iter().step_by(rows.len() / 150).take(150) {
        let mut w: Vec<&str> = r.1.split_whitespace()
            .filter(|x| x.len() > 4 && x.chars().all(|c| c.is_ascii_alphanumeric()))
            .collect();
        w.sort_by_key(|x| std::cmp::Reverse(x.len()));
        if w.len() >= 8 { queries.push(w[..8].join(" ")); }
    }

    let mut le = Vec::new();
    let mut lb = Vec::new();
    for q in &queries {
        let t = Instant::now();
        let _ = model.encode(&[q.clone()]);
        le.push(t.elapsed().as_secs_f64() * 1000.0);

        let m: String = q.split_whitespace().map(|w| format!("\"{}\"", w))
            .collect::<Vec<_>>().join(" OR ");
        let t = Instant::now();
        let mut st = conn.prepare(
            "SELECT rowid FROM mem WHERE mem MATCH ?1 ORDER BY bm25(mem,2.0,1.0) LIMIT 50")?;
        let n = st.query_map([&m], |r| r.get::<_, i64>(0))?.count();
        lb.push(t.elapsed().as_secs_f64() * 1000.0);
        std::hint::black_box(n);
    }
    println!("encode de la consulta   p50 {:.3} ms  p95 {:.3} ms",
             pct(&mut le.clone(), 0.5), pct(&mut le, 0.95));
    println!("BM25 top-8 terminos     p50 {:.3} ms  p95 {:.3} ms",
             pct(&mut lb.clone(), 0.5), pct(&mut lb, 0.95));
    Ok(())
}
