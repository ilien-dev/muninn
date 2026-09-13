//! `muninn why` (plan, Phase 4 §5-bis). Invoked, never injected. A deterministic
//! router picks the question's shape, lexical BM25 and the exact-kNN sidecar are fused
//! by RRF, the F1 filter applies (invalid rows never, unless `--all`), and the answer is
//! literal records with origin, trust, transcript reference, lineage and conflicts —
//! no synthesis. A sufficiency marker says whether a trust ≥ 2 record answers directly.

use muninn_core::filter::{conflicts_of, lineage};
use muninn_core::project::{load, RecordRow};
use muninn_core::recall::select_terms;
use muninn_core::sanitize::fts_match_or;
use muninn_core::tokens::estimate;
use muninn_core::{Db, Result};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    Decision,
    Deadend,
    Commit,
    File,
    Rule,
    Id,
    All,
}

/// Question shape → route. Fixed patterns, nothing learned. A question that fits none
/// consults everything and says so.
pub fn route(q: &str) -> Route {
    let l = q.trim().to_lowercase();
    if l.chars().all(|c| c.is_ascii_digit()) && !l.is_empty() {
        return Route::Id;
    }
    let has = |ws: &[&str]| ws.iter().any(|w| l.contains(w));
    if l.split_whitespace()
        .any(|w| w.len() >= 7 && w.len() <= 40 && w.chars().all(|c| c.is_ascii_hexdigit()))
        || has(&["commit", "qué cambió", "que cambio", "what changed"])
    {
        return Route::Commit;
    }
    if has(&[
        "prohibid",
        "forbidden",
        "not allowed",
        "why can't",
        "por qué no puedo",
        "regla",
        "rule",
        "nunca",
        "never",
        "siempre",
        "always",
    ]) {
        return Route::Rule;
    }
    if has(&[
        "no funciona",
        "doesn't work",
        "does not work",
        "falló",
        "failed",
        "descart",
        "abandon",
        "dead end",
        "callejón",
        "por qué no",
        "why not",
        "why didn't",
        "why doesn't",
    ]) {
        return Route::Deadend;
    }
    if l.contains('/')
        || l.split_whitespace()
            .any(|w| w.contains('.') && !w.ends_with('.') && w.len() > 3)
        || has(&[
            "quién toca",
            "who touches",
            "historia de",
            "history of",
            "fichero",
            "archivo",
            "file ",
        ])
    {
        return Route::File;
    }
    if has(&[
        "por qué", "porque", "why", "decid", "eleg", "chose", "choose", "razón", "reason",
    ]) {
        return Route::Decision;
    }
    Route::All
}

fn kinds_for(r: Route) -> &'static str {
    match r {
        Route::Decision => "('decision','invariant','correction','claim')",
        Route::Deadend => "('deadend','correction','episode')",
        Route::Commit => "('decision','deadend','episode')",
        Route::File => "('decision','deadend','invariant','claim','episode')",
        Route::Rule => "('invariant','correction','decision')",
        Route::Id | Route::All => {
            "('invariant','decision','deadend','correction','claim','episode')"
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Answer {
    pub query: String,
    pub route: Route,
    pub terms: Vec<String>,
    pub sufficient: bool,
    pub sufficiency: String,
    pub records: Vec<Found>,
    pub tokens: usize,
    pub vector_used: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Found {
    pub record: RecordRow,
    pub score: f64,
    pub lineage: Vec<(i64, Option<String>)>,
    pub conflicts: Vec<i64>,
    pub commit: Option<String>,
}

fn lexical(
    db: &Db,
    terms: &[String],
    kinds: &str,
    include_invalid: bool,
    limit: usize,
) -> Result<Vec<(i64, f64)>> {
    if terms.is_empty() {
        return Ok(vec![]);
    }
    let q = fts_match_or(terms);
    let inv = if include_invalid {
        ""
    } else {
        "AND r.invalid = 0"
    };
    // invalid rows are not in the external FTS index, so `--all` searches record directly
    let sql = if include_invalid {
        format!(
            "SELECT r.id, 0.0 FROM record r WHERE r.kind IN {kinds} AND (r.subject LIKE ?1 OR r.object LIKE ?1 OR r.body LIKE ?1) ORDER BY r.id DESC LIMIT ?2"
        )
    } else {
        format!(
            "SELECT r.id, bm25(record_fts, 3.0, 2.0, 1.0) FROM record_fts JOIN record r ON r.id = record_fts.rowid \
             WHERE record_fts MATCH ?1 {inv} AND r.kind IN {kinds} ORDER BY 2 LIMIT ?2"
        )
    };
    let mut stmt = db.conn.prepare(&sql)?;
    let arg = if include_invalid {
        format!("%{}%", terms.first().cloned().unwrap_or_default())
    } else {
        q
    };
    let rows: Vec<(i64, f64)> = stmt
        .query_map(rusqlite::params![arg, limit as i64], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Answer a question or an id. `budget_tokens` caps the rendered output (default
/// 1 500); `include_invalid` is `--all`.
pub fn answer(
    db: &Db,
    query: &str,
    include_invalid: bool,
    limit: usize,
    budget_tokens: usize,
) -> Result<Answer> {
    let rt = route(query);
    let kinds = kinds_for(rt);
    let mut terms: Vec<String> = Vec::new();
    let mut ranked: Vec<(i64, f64)> = Vec::new();
    let mut vector_used = false;
    if rt == Route::Id {
        let id: i64 = query.trim().parse().unwrap_or(0);
        ranked.push((id, 1.0));
    } else {
        terms = select_terms(db, query, 8)?;
        let lex = lexical(db, &terms, kinds, include_invalid, 20)?;
        let mut fused: HashMap<i64, f64> = HashMap::new();
        for (rank, (id, _)) in lex.iter().enumerate() {
            *fused.entry(*id).or_default() += 1.0 / (60.0 + rank as f64 + 1.0);
        }
        // the sidecar: loaded here, on purpose, in an invoked tool — never in a hook
        if let Ok(emb) = muninn_embed::Embedder::load_default() {
            if let Ok(nn) = muninn_embed::knn(db, &emb.encode_one(query), 20, &emb.model_id) {
                vector_used = !nn.is_empty();
                for (rank, n) in nn.iter().enumerate() {
                    *fused.entry(n.id).or_default() += 1.0 / (60.0 + rank as f64 + 1.0);
                }
            }
        }
        ranked = fused.into_iter().collect();
        ranked.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
    }
    let mut records = Vec::new();
    let mut tokens = 0usize;
    for (id, score) in ranked {
        if records.len() >= limit {
            break;
        }
        let Some(row) = load(db, &format!("id = {id}"))?.pop() else {
            continue;
        };
        if row.invalid && !include_invalid {
            continue;
        }
        if !kinds.contains(&format!("'{}'", row.kind)) && rt != Route::Id {
            continue;
        }
        let t = estimate(&row.body) + 40;
        if tokens + t > budget_tokens && !records.is_empty() {
            break;
        }
        tokens += t;
        let commit = row
            .transcript_ref
            .as_deref()
            .and_then(|r| r.strip_prefix("git:"))
            .map(String::from)
            .or_else(|| row.subject.strip_prefix("commit:").map(String::from));
        records.push(Found {
            lineage: lineage(db, row.id)?,
            conflicts: conflicts_of(db, row.id)?,
            commit,
            score,
            record: row,
        });
    }
    let direct = records
        .iter()
        .find(|f| f.record.trust >= 2 && !f.record.invalid);
    let (sufficient, sufficiency) = match direct {
        Some(f) => (
            true,
            format!(
                "sufficient: #{} ({}, trust {}) answers directly",
                f.record.id, f.record.origin, f.record.trust
            ),
        ),
        None if records.is_empty() => (false, "insufficient: nothing recorded matches".into()),
        None => (
            false,
            "insufficient: only circumstantial records (trust < 2); do not fill the gap".into(),
        ),
    };
    Ok(Answer {
        query: query.to_string(),
        route: rt,
        terms,
        sufficient,
        sufficiency,
        records,
        tokens,
        vector_used,
    })
}

/// Plain-text rendering: one block per record, literal body, provenance, lineage.
pub fn render(a: &Answer) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "muninn why · route {:?} · {} record(s) · ~{} tokens · {}{}\n",
        a.route,
        a.records.len(),
        a.tokens,
        a.sufficiency,
        if a.vector_used {
            " · lexical+vector"
        } else {
            " · lexical only"
        }
    ));
    for f in &a.records {
        let r = &f.record;
        s.push_str(&format!(
            "\n[muninn:{}] #{} · {} {} · origin: {} · trust {} · {}{}{}\n",
            r.kind,
            r.id,
            r.subject,
            r.relation,
            r.origin,
            r.trust,
            muninn_core::project::iso(r.created_at),
            if r.invalid {
                format!(
                    " · RETIRED ({})",
                    r.invalid_reason.as_deref().unwrap_or("?")
                )
            } else {
                String::new()
            },
            f.commit
                .as_ref()
                .map(|c| format!(" · commit {c}"))
                .unwrap_or_default()
        ));
        s.push_str(r.body.trim_end());
        s.push('\n');
        if let Some(t) = &r.transcript_ref {
            s.push_str(&format!("  evidence: {t}\n"));
        }
        if f.lineage.len() > 1 {
            let chain: Vec<String> = f
                .lineage
                .iter()
                .map(|(i, reason)| match reason {
                    Some(rs) => format!("#{i}({rs})"),
                    None => format!("#{i}"),
                })
                .collect();
            s.push_str(&format!("  [muninn:lineage] {}\n", chain.join(" ← ")));
        }
        if !f.conflicts.is_empty() {
            let c: Vec<String> = f.conflicts.iter().map(|i| format!("#{i}")).collect();
            s.push_str(&format!(
                "  [muninn:conflict] disagrees with {} — both active, ask which stands\n",
                c.join(", ")
            ));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes() {
        assert_eq!(route("142"), Route::Id);
        assert_eq!(
            route("why did we choose exponential backoff"),
            Route::Decision
        );
        assert_eq!(route("por qué se descartó el napi addon"), Route::Deadend);
        assert_eq!(route("what changed in 1a2b3c4d"), Route::Commit);
        assert_eq!(route("history of src/auth/jwt.rs"), Route::File);
        assert_eq!(route("why is force push forbidden"), Route::Rule);
        assert_eq!(route("tell me about caching"), Route::All);
    }
}
