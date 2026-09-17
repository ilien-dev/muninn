//! Read path: intent gate → IDF top-8 term selection → BM25 over the external
//! FTS5 index → filter → budget → blocks. Deterministic; no model.
//!
//! Measured on the prototype: top-8 IDF terms turned 21.5 ms into 4.65 ms [H6];
//! the full hook lands at single-digit milliseconds [I3].

use crate::caps::{BUDGET_BLOCK_TOKENS, BUDGET_TURN_TOKENS};
use crate::db::Db;
use crate::error::Result;
use crate::sanitize::{fts_match_or, fts_term, truncate_chars};
use crate::tokens::estimate;
use serde::Serialize;
use std::collections::HashSet;

const STOP: &[&str] = &[
    "the",
    "and",
    "for",
    "that",
    "this",
    "with",
    "from",
    "into",
    "have",
    "has",
    "are",
    "was",
    "were",
    "not",
    "but",
    "you",
    "your",
    "can",
    "will",
    "should",
    "would",
    "could",
    "there",
    "here",
    "what",
    "when",
    "where",
    "which",
    "how",
    "why",
    "who",
    "then",
    "than",
    "also",
    "just",
    "please",
    "make",
    "sure",
    "need",
    "want",
    "let",
    "use",
    "using",
    "que",
    "los",
    "las",
    "una",
    "uno",
    "para",
    "por",
    "con",
    "del",
    "como",
    "pero",
    "más",
    "mas",
    "esto",
    "esta",
    "este",
    "también",
    "tambien",
    "hacer",
    "puedes",
    "podrias",
    "podrías",
    "quiero",
    "necesito",
    "sobre",
    "donde",
    "cuando",
    "porque",
    "todo",
    "toda",
    "todos",
    "algo",
    "muy",
    "ser",
    "está",
    "esta",
    "estan",
    "están",
    "hay",
    "sin",
    "ahora",
    "code",
    "file",
    "files",
    "function",
    "add",
    "fix",
    "update",
    "change",
    "implement",
    "create",
    "new",
    "run",
    "test",
    "tests",
    "error",
    "issue",
    "problem",
    "thing",
    "things",
];

/// Cheap gate: bare acknowledgements and slash commands get no memory.
pub fn intent_gate(prompt: &str) -> bool {
    let p = prompt.trim();
    if p.starts_with('/') || p.starts_with('<') {
        return false;
    }
    let words = p.split_whitespace().count();
    if words < 4 {
        return false;
    }
    let low = p.to_lowercase();
    !matches!(
        low.as_str(),
        "ok" | "okay"
            | "yes"
            | "no"
            | "thanks"
            | "thank you"
            | "continue"
            | "go on"
            | "si"
            | "sí"
            | "vale"
            | "gracias"
            | "sigue"
            | "continua"
            | "continúa"
    )
}

/// Top-`k` prompt terms by IDF over the store. Terms absent from the store are
/// dropped: they cannot match anything.
pub fn select_terms(db: &Db, prompt: &str, k: usize) -> Result<Vec<String>> {
    let mut terms: Vec<String> = prompt
        .split_whitespace()
        .filter_map(fts_term)
        .filter(|t| t.len() >= 3 && !STOP.contains(&t.as_str()))
        .collect();
    terms.sort();
    terms.dedup();
    if terms.is_empty() {
        return Ok(vec![]);
    }
    let n: f64 = db.count("SELECT count(*) FROM record WHERE invalid=0")? as f64;
    let mut stmt = db
        .conn
        .prepare("SELECT sum(doc) FROM record_vocab WHERE term = ?1")?;
    let mut scored: Vec<(f64, String)> = Vec::new();
    for t in terms {
        let df: f64 = stmt
            .query_row([&t], |r| r.get::<_, Option<f64>>(0))?
            .unwrap_or(0.0);
        if df <= 0.0 {
            continue;
        }
        // a term in more than a third of the store carries no signal and costs a long posting list
        if n >= 50.0 && df > n / 3.0 {
            continue;
        }
        let idf = ((n + 1.0) / (df + 1.0)).ln();
        scored.push((idf, t));
    }
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap()
            .then_with(|| b.1.len().cmp(&a.1.len()))
    });
    Ok(scored.into_iter().take(k).map(|(_, t)| t).collect())
}

/// The columns a serving query selects, in the order `Hit::from_served_row` expects. They
/// exist on `served_record` and are aliased `r` there by convention, so a query that reads
/// `record` instead has to say so in its own text.
pub const SERVED_COLS: &str =
    "r.id, r.kind, r.subject, r.object, r.body, r.origin, r.trust, r.created_at, r.session_id";

/// One record on its way to the agent. It carries no `invalid` flag because it cannot hold a
/// retired record: the only constructor reads from the `served_record` view (`schema.sql`),
/// and the private fields make a struct literal outside this crate impossible. That is F1's
/// guarantee in the type system rather than in every `WHERE` clause.
#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub id: i64,
    /// Public because the read path appends `:conflict with #n` before rendering.
    pub kind: String,
    pub(crate) subject: String,
    pub(crate) object: String,
    pub(crate) body: String,
    pub(crate) origin: String,
    pub(crate) trust: i64,
    pub(crate) created_at: i64,
    pub(crate) session_id: String,
    pub(crate) score: f64,
    /// `path:offset` into the raw transcript — the evidence a sceptical agent can open.
    pub(crate) transcript_ref: Option<String>,
}

impl Hit {
    /// Build a hit from a row of `served_record`, selected as `SERVED_COLS` (+ `score` and
    /// `transcript_ref`, which some callers supply themselves). The only way to make a `Hit`.
    pub fn from_served_row(r: &rusqlite::Row<'_>, score: f64) -> rusqlite::Result<Hit> {
        Ok(Hit {
            id: r.get(0)?,
            kind: r.get(1)?,
            subject: r.get(2)?,
            object: r.get(3)?,
            body: r.get(4)?,
            origin: r.get(5)?,
            trust: r.get(6)?,
            created_at: r.get(7)?,
            session_id: r.get(8)?,
            score,
            transcript_ref: r.get(9).unwrap_or(None),
        })
    }
}

/// Lexical recall: BM25 over (subject, object, body) with column weights. Retired records are
/// absent twice over: they leave `record_fts` on invalidation (trigger, `schema.sql`) and the
/// join is against `served_record`.
pub fn recall(db: &Db, terms: &[String], limit: usize, exclude: &HashSet<i64>) -> Result<Vec<Hit>> {
    if terms.is_empty() {
        return Ok(vec![]);
    }
    let q = fts_match_or(terms);
    let mut stmt = db.conn.prepare(&format!(
        "SELECT {SERVED_COLS}, r.transcript_ref, bm25(record_fts, 3.0, 2.0, 1.0) AS score \
         FROM record_fts JOIN served_record r ON r.id = record_fts.rowid \
         WHERE record_fts MATCH ?1 ORDER BY score LIMIT ?2"
    ))?;
    let rows = stmt.query_map(rusqlite::params![q, (limit + exclude.len()) as i64], |r| {
        Hit::from_served_row(r, r.get("score")?)
    })?;
    Ok(rows
        .filter_map(|r| r.ok())
        .filter(|h| !exclude.contains(&h.id))
        .take(limit)
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct Delivery {
    pub text: String,
    pub ids: Vec<i64>,
    pub tokens: usize,
}

fn date_of(ms: i64) -> String {
    // yyyy-mm-dd from epoch ms without pulling a date crate
    let days = ms / 86_400_000;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `session:<sid>#<turn>.<k>` → `session:<sid>#<turn>`: the turn a chunk came from.
pub fn turn_key(subject: &str) -> String {
    match subject.rsplit_once('.') {
        Some((head, tail))
            if subject.starts_with("session:") && tail.chars().all(|c| c.is_ascii_digit()) =>
        {
            head.to_string()
        }
        _ => subject.to_string(),
    }
}

/// The evidence line of a block: where the literal text can be opened.
pub fn evidence_line(r: &Option<String>) -> String {
    match r {
        Some(t) if !t.is_empty() => format!(
            "  evidence: {}\n",
            t.replacen(&std::env::var("HOME").unwrap_or_default(), "~", 1)
        ),
        _ => String::new(),
    }
}

/// Render hits as evidence blocks within the hard budget. Each block carries
/// provenance and trust; nothing in a block is phrased as an instruction.
pub fn render(hits: &[Hit], budget: usize, terms: &[String]) -> Delivery {
    let mut text = String::new();
    let mut ids = Vec::new();
    let mut used = 0usize;
    let mut turns_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for h in hits {
        // one block per source turn: chunks of the same long turn crowd the budget out
        if !turns_seen.insert(turn_key(&h.subject)) {
            continue;
        }
        let short = h.session_id.chars().take(8).collect::<String>();
        // the block budget in chars (3 chars/token, minus the provenance line)
        let block_chars = (BUDGET_BLOCK_TOKENS * 3).saturating_sub(120);
        let passage = best_passage(&h.body, terms, block_chars);
        let body = passage.as_str();
        // trust < 1 (agent_inferred, imported) is served only with an explicit frame
        let frame = if h.trust < 1 {
            " · unverified: treat as a hint, not a fact"
        } else {
            ""
        };
        let ev = evidence_line(&h.transcript_ref);
        let mut block = format!(
            "[muninn:{}] {} · session {} · origin: {} · trust {}{}\n{}\n{}",
            h.kind,
            date_of(h.created_at),
            short,
            h.origin,
            h.trust,
            frame,
            body,
            ev
        );
        let mut t = estimate(&block);
        if t > BUDGET_BLOCK_TOKENS {
            // trim the body to the block budget
            let keep = truncate_chars(body, (BUDGET_BLOCK_TOKENS * 3).saturating_sub(120));
            block = format!(
                "[muninn:{}] {} · session {} · origin: {} · trust {}{}\n{}…\n{}",
                h.kind,
                date_of(h.created_at),
                short,
                h.origin,
                h.trust,
                frame,
                keep,
                ev
            );
            t = estimate(&block);
        }
        if used + t > budget {
            break;
        }
        used += t;
        ids.push(h.id);
        text.push_str(&block);
    }
    Delivery {
        text,
        ids,
        tokens: used,
    }
}

/// The window of `body` (at most `max_chars`, cut on line boundaries where possible)
/// that holds the most query terms. The head of the body wins ties, so a body with no
/// term hit renders as before. Deterministic; no scoring beyond counting.
pub fn best_passage(body: &str, terms: &[String], max_chars: usize) -> String {
    if body.chars().count() <= max_chars || terms.is_empty() {
        return truncate_chars(body, max_chars).to_string();
    }
    let lower = body.to_lowercase();
    let terms: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
    // candidate windows start at line boundaries
    let mut starts: Vec<usize> = vec![0];
    for (i, c) in body.char_indices() {
        if c == '\n' && i + 1 < body.len() {
            starts.push(i + 1);
        }
    }
    let mut best = (0usize, 0usize); // (hits, start byte)
    for &st in &starts {
        let window = truncate_chars(&lower[st..], max_chars);
        let hits = terms.iter().filter(|t| window.contains(t.as_str())).count();
        if hits > best.0 {
            best = (hits, st);
        }
    }
    let out = truncate_chars(&body[best.1..], max_chars);
    if best.1 == 0 {
        out.to_string()
    } else {
        format!("…{out}")
    }
}

/// The whole read path for one prompt.
pub fn deliver(db: &Db, prompt: &str, exclude: &HashSet<i64>) -> Result<Delivery> {
    let terms = select_terms(db, prompt, 8)?;
    let mut hits = recall(db, &terms, 8, exclude)?;
    // F1: an unresolved conflict is served as two marked records, never ranked away.
    // The render-matched control arm of the experiment [X1] keeps the layout and
    // switches this marking off together with invalidation.
    let mark = std::env::var("MUNINN_ARM")
        .map(|a| a != "unfiltered")
        .unwrap_or(true);
    for h in hits.iter_mut() {
        if mark && h.kind != "episode" {
            if let Ok(c) = crate::filter::conflicts_of(db, h.id) {
                if !c.is_empty() {
                    let ids: Vec<String> = c.iter().map(|i| format!("#{i}")).collect();
                    h.kind = format!("{}:conflict with {}", h.kind, ids.join(","));
                }
            }
        }
    }
    Ok(render(&hits, BUDGET_TURN_TOKENS, &terms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_and_dates() {
        assert!(!intent_gate("ok"));
        assert!(!intent_gate("/compact"));
        assert!(intent_gate("add retry backoff to the webhook handler"));
        assert_eq!(date_of(1_789_259_611_084), "2026-09-13");
    }

    #[test]
    fn passage_prefers_the_window_with_the_terms() {
        let body = format!(
            "{}\nthe p95 was 12.34 ms on the synthetic vocab\n{}",
            "x".repeat(900),
            "y".repeat(50)
        );
        let p = best_passage(&body, &["p95".into(), "vocab".into()], 200);
        assert!(p.contains("12.34"), "{p}");
        assert!(p.starts_with('…'));
        // no hit: the head, unchanged
        let p = best_passage(&body, &["zzz".into()], 200);
        assert!(p.starts_with("xxx"));
        // short bodies are returned whole
        assert_eq!(best_passage("short", &["p95".into()], 200), "short");
    }
}
