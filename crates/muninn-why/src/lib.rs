//! `muninn why` (plan, Phase 4 §5-bis). Invoked, never injected. A deterministic
//! router picks the question's shape, lexical BM25 and the exact-kNN sidecar are fused
//! by RRF, the F1 filter applies (invalid rows never, unless `--all`), and the answer is
//! literal records with origin, trust, transcript reference, lineage and conflicts —
//! no synthesis. A sufficiency marker says whether a trust ≥ 2 record answers directly.

use muninn_core::filter::{conflicts_of, lineage};
use muninn_core::project::{load_all, RecordRow};
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
        // `select_terms` drops a word no *servable* record holds, because the index it asks
        // holds only those. Under `--all` that is exactly backwards: the word you are asking
        // about — the value that was dropped — is the one word guaranteed to be missing from
        // it, so "why --all PgBouncer" found nothing about PgBouncer. Ask with the words as
        // typed instead; the query below is a LIKE over `record`, which does not need them to
        // be in any index.
        if include_invalid {
            let raw: Vec<String> = query
                .split_whitespace()
                .filter_map(muninn_core::sanitize::fts_term)
                .filter(|t| t.chars().count() >= 3)
                .collect();
            if !raw.is_empty() {
                terms = raw;
            }
        }
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
        let Some(row) = load_all(db, &format!("id = {id}"))?.pop() else {
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
    // A commit log entry — `commit:<hash>`, whose text is the subject line and the files it
    // touched — is corroboration, not an answer. It was being reported as one: asked what the
    // cache eviction policy is, `why` replied "sufficient: #11 (commit_linked, trust 2)
    // answers directly" over `commit c9249e6: update dependencies`, and the agent went on to
    // write that nothing was recorded. It sinks below the records that carry a value, and it
    // never decides sufficiency on its own.
    let is_log = |f: &Found| f.record.subject.starts_with("commit:");
    records.sort_by_key(|f| is_log(f));
    let direct = records
        .iter()
        .find(|f| f.record.trust >= 2 && !f.record.invalid && !is_log(f));
    let (sufficient, sufficiency) = match direct {
        Some(f) => (
            true,
            format!(
                "sufficient: #{} ({}, trust {}) answers directly",
                f.record.id, f.record.origin, f.record.trust
            ),
        ),
        None if records.is_empty() => (false, "insufficient: nothing recorded matches".into()),
        // Only episodes, and at least one of them: the person's own words are below, and no
        // typed record covers the subject. v34's agents ran this, read "insufficient", and
        // wrote that nothing was recorded with the values in front of them; v37 measured what
        // the same true statement does in the catalogue, 56 against 38 of 108. It is said here
        // in the same words, and still opens with `insufficient`, because nothing of trust 2
        // answers this and an agent that goes on to write a value should know that.
        None if records
            .iter()
            .any(|f| f.record.kind == "episode" && !f.record.invalid) =>
        {
            (
                false,
                "insufficient: no typed record answers this — the episodes below are what was \
                 said, word for word, and the newest one about the subject is the latest word \
                 on it; name their trust, and do not invent a value"
                    .into(),
            )
        }
        None => (
            false,
            // what this has to prevent is an invented value, not an answer: an agent told
            // only "do not fill the gap" wrote that nothing was recorded while a trust-3
            // decision sat in the same output
            "insufficient: no record of trust 2 or more answers this — say what the records \
             below do show, name their trust, and do not invent a value"
                .into(),
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

    /// A commit log entry is corroboration, not an answer. Asked what the cache eviction
    /// policy is, `why` replied "sufficient: #11 (commit_linked, trust 2) answers directly"
    /// over `commit c9249e6: update dependencies`, and the agent went on to write that
    /// nothing was recorded — with the decision itself three lines further down.
    #[test]
    fn a_commit_subject_does_not_answer_the_question() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), muninn_core::db::Mode::ReadWrite).unwrap();
        let ins = |subject: &str, object: &str, origin: &str, trust: i64| {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('decision',?1,'is',?2,?2,?3,?4,'s',?1,1)",
                    rusqlite::params![subject, object, origin, trust],
                )
                .unwrap();
        };
        ins(
            "commit:c9249e6",
            "update dependencies\nfiles: config/cache-eviction.json\n",
            "commit_linked",
            2,
        );
        ins(
            "said:change:cache eviction lru",
            "LRU with a 300-second TTL for cache eviction",
            "user_said",
            3,
        );
        let a = answer(&db, "the cache eviction policy", false, 20, 1_500).unwrap();
        assert!(a.sufficient, "the decision answers it: {}", a.sufficiency);
        assert!(
            a.sufficiency.contains("user_said"),
            "the commit subject must not be what answers: {}",
            a.sufficiency
        );
        assert!(
            !a.records[0].record.subject.starts_with("commit:"),
            "and it must not lead the list"
        );
    }

    /// Only episodes answer: the verdict stays `insufficient`, and says what the episodes are.
    /// v34's agents read the older wording as "nothing is recorded" with the values below it.
    #[test]
    fn episodes_alone_are_named_as_what_was_said() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), muninn_core::db::Mode::ReadWrite).unwrap();
        for (i, v) in ["openssl for TLS", "rustls for TLS, better defaults"]
            .iter()
            .enumerate()
        {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('episode',?1,'happened',?2,?2,'tool_observed',1,'s',?1,?3)",
                    rusqlite::params![format!("session:s#{i}"), format!("user: {v}"), i as i64],
                )
                .unwrap();
        }
        let a = answer(&db, "the TLS backend", false, 20, 1_500).unwrap();
        assert!(!a.sufficient);
        assert!(
            a.sufficiency.starts_with("insufficient"),
            "{}",
            a.sufficiency
        );
        assert!(
            a.sufficiency.contains("word for word") && a.sufficiency.contains("latest word"),
            "{}",
            a.sufficiency
        );
        assert!(
            a.sufficiency.contains("do not invent a value"),
            "{}",
            a.sufficiency
        );
    }

    /// `--all` exists so a person can see what was retired. It asked the full-text index which
    /// words were worth searching for — and a retired record is not in that index, so the one
    /// word that would find it was always dropped as unknown: `why --all PgBouncer` returned
    /// everything except PgBouncer.
    #[test]
    fn all_finds_the_value_that_was_retired() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), muninn_core::db::Mode::ReadWrite).unwrap();
        let ins = |object: &str, invalid: i64| {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,invalid,invalid_reason,created_at) \
                     VALUES('decision',?1,'is',?2,?2,'user_said',3,'s',?1,?3,CASE ?3 WHEN 1 THEN 'superseded' END,1)",
                    rusqlite::params![object, object, invalid],
                )
                .unwrap();
        };
        ins("we go with PgBouncer for the pooling layer", 1);
        ins("we go with Supavisor for the pooling layer", 0);

        let seen = |all: bool| -> Vec<String> {
            answer(&db, "PgBouncer", all, 20, 1_500)
                .unwrap()
                .records
                .into_iter()
                .map(|f| f.record.object)
                .collect()
        };
        assert!(
            seen(true).iter().any(|o| o.contains("PgBouncer")),
            "--all must reach the retired value: {:?}",
            seen(true)
        );
        assert!(
            !seen(false).iter().any(|o| o.contains("PgBouncer")),
            "and without --all it must not be served at all"
        );
    }
}
