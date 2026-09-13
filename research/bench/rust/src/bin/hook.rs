// El hook real tal y como lo invocaría Claude Code o Codex: un proceso que arranca,
// abre el índice, decide si hay intención, consulta BM25 y sale. Sin modelo cargado.
use rusqlite::Connection;
use std::io::Read;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let t0 = Instant::now();
    let mut prompt = String::new();
    std::io::stdin().read_to_string(&mut prompt).ok();
    const INTENT: [&str; 12] = ["implement","build","add ","fix","refactor","why","investigate",
                                "debug","decide","design","migrate","replace"];
    let low = prompt.to_lowercase();
    if !INTENT.iter().any(|k| low.contains(k)) {
        println!("{{\"gated\":true,\"ms\":{:.3}}}", t0.elapsed().as_secs_f64() * 1000.0);
        return Ok(());
    }
    let conn = Connection::open("/tmp/rust_fts.db")?;
    conn.execute_batch("PRAGMA query_only=1;")?;
    let mut w: Vec<&str> = low.split_whitespace()
        .filter(|x| x.len() > 4 && x.chars().all(|c| c.is_ascii_alphanumeric())).collect();
    w.sort_by_key(|x| std::cmp::Reverse(x.len()));
    w.truncate(8);
    if w.is_empty() { println!("{{\"gated\":true}}"); return Ok(()); }
    let m = w.iter().map(|x| format!("\"{x}\"")).collect::<Vec<_>>().join(" OR ");
    let mut st = conn.prepare(
        "SELECT rowid, snippet(mem,1,'','',' ',12) FROM mem WHERE mem MATCH ?1 \
         ORDER BY bm25(mem,2.0,1.0) LIMIT 8")?;
    let n = st.query_map([&m], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?.count();
    println!("{{\"hits\":{n},\"ms\":{:.3}}}", t0.elapsed().as_secs_f64() * 1000.0);
    Ok(())
}
