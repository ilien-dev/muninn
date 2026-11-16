//! `muninn why` (plan, Phase 4 §5-bis). Invoked, never injected. A deterministic
//! router picks the question's shape, lexical BM25 ranks what matches,
//! the F1 filter applies (invalid rows never, unless `--all`), and the answer is
//! literal records with origin, trust, transcript reference, lineage and conflicts —
//! no synthesis. A sufficiency marker says whether a trust ≥ 2 record answers directly.

mod es;

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

/// Every kind `why` answers with, whatever the route.
const ALL_KINDS: &str = "('invariant','decision','deadend','correction','claim','episode')";

/// The kinds a route puts first among equal matches of the same date. A route orders, it never excludes: each
/// one used to leave kinds out, so a correction that names a file or an invariant that says
/// "commit" could not be found by its own words (R-16: 0 of 15 such records on two stores).
fn kinds_for(r: Route) -> &'static str {
    match r {
        Route::Decision => "('decision','invariant','correction','claim')",
        Route::Deadend => "('deadend','correction','episode')",
        Route::Commit => "('decision','deadend','episode')",
        Route::File => "('decision','deadend','invariant','claim','episode')",
        Route::Rule => "('invariant','correction','decision')",
        Route::Id | Route::All => ALL_KINDS,
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
    preferred: &str,
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
    // invalid rows are not in the external FTS index, so `--all` searches record directly.
    // Equal scores go newest first, as in recall: memory answers with the latest statement, and
    // FTS5 returned a tie in rowid order. After the date, the route's kinds go first: a newer
    // statement of another kind is the one that prevails.
    let tie =
        format!("r.created_at DESC, CASE WHEN r.kind IN {preferred} THEN 0 ELSE 1 END, r.id DESC");
    if include_invalid {
        return every_word(db, terms, &tie, limit);
    }
    let sql = format!(
        "SELECT r.id, bm25(record_fts, 3.0, 2.0, 1.0) FROM record_fts JOIN record r ON r.id = record_fts.rowid \
         WHERE record_fts MATCH ?1 {inv} AND r.kind IN {ALL_KINDS} ORDER BY 2, {tie} LIMIT ?2"
    );
    let mut stmt = db.conn.prepare(&sql)?;
    let rows: Vec<(i64, f64)> = stmt
        .query_map(rusqlite::params![q, limit as i64], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// The SQL function `--all` compares record text through: `sanitize::fold`, so the text is
/// lowercased and its accents folded as a question's words are (`fts_term`). `LIKE` alone folds
/// ASCII case only, and "política" was searched as "politica" against "política".
const FOLD_FN: &str = "muninn_fold";

fn register_fold(db: &Db) -> Result<()> {
    use rusqlite::functions::FunctionFlags;
    db.conn.create_scalar_function(
        FOLD_FN,
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            Ok(ctx
                .get::<Option<String>>(0)?
                .map(|s| muninn_core::sanitize::fold(&s)))
        },
    )?;
    Ok(())
}

/// Whether a record's folded text holds parameter `?i`, a `like_pattern`.
fn folded_holds(i: usize) -> String {
    format!(
        "({FOLD_FN}(r.subject) LIKE ?{i} ESCAPE '\\' OR {FOLD_FN}(r.object) LIKE ?{i} ESCAPE '\\' \
         OR {FOLD_FN}(r.body) LIKE ?{i} ESCAPE '\\')"
    )
}

/// `%word%` with `\`, `%` and `_` escaped, so `slow_path` matches itself and not `slowXpath`.
fn like_pattern(word: &str) -> String {
    let mut p = String::from("%");
    for c in word.chars() {
        if matches!(c, '\\' | '%' | '_') {
            p.push('\\');
        }
        p.push(c);
    }
    p.push('%');
    p
}

/// `--all`: a `LIKE` per word over `record`, retired rows included. A record scores the summed
/// rarity of the words it holds, so every word typed counts and the rarest counts most; it used
/// to search the first word alone, and "what about PgBouncer" searched for "what" (R-16).
fn every_word(db: &Db, terms: &[String], tie: &str, limit: usize) -> Result<Vec<(i64, f64)>> {
    let holds = folded_holds;
    let n = db.count(&format!(
        "SELECT count(*) FROM record r WHERE r.kind IN {ALL_KINDS}"
    ))? as f64;
    let likes: Vec<String> = terms.iter().map(|t| like_pattern(t)).collect();
    let mut weighted = Vec::new();
    for (i, like) in likes.iter().enumerate() {
        let d: i64 = db.conn.query_row(
            &format!(
                "SELECT count(*) FROM record r WHERE r.kind IN {ALL_KINDS} AND {}",
                holds(1)
            ),
            [like],
            |r| r.get(0),
        )?;
        if d > 0 {
            let w = ((n + 1.0) / (d as f64 + 1.0)).ln();
            weighted.push(format!("CASE WHEN {} THEN {w:.6} ELSE 0 END", holds(i + 1)));
        }
    }
    if weighted.is_empty() {
        return Ok(vec![]);
    }
    let any: Vec<String> = (1..=likes.len()).map(holds).collect();
    let sql = format!(
        "SELECT r.id, {} AS score FROM record r WHERE r.kind IN {ALL_KINDS} AND ({}) \
         ORDER BY score DESC, {tie} LIMIT {limit}",
        weighted.join(" + "),
        any.join(" OR ")
    );
    let mut stmt = db.conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::ToSql> =
        likes.iter().map(|l| l as &dyn rusqlite::ToSql).collect();
    let rows: Vec<(i64, f64)> = stmt
        .query_map(params.as_slice(), |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// The question's words as `fts_term` gives them, three characters or more, each once.
fn words(query: &str) -> Vec<String> {
    let mut raw: Vec<String> = query
        .split_whitespace()
        .filter_map(muninn_core::sanitize::fts_term)
        .filter(|t| t.chars().count() >= 3)
        .collect();
    let mut seen = std::collections::HashSet::new();
    raw.retain(|t| seen.insert(t.clone()));
    raw
}

/// `words` without the scaffolding `select_terms` never searches for.
fn content_words(query: &str) -> Vec<String> {
    words(query)
        .into_iter()
        .filter(|t| !muninn_core::recall::is_stop_word(t))
        .collect()
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
    if rt == Route::Id {
        let id: i64 = query.trim().parse().unwrap_or(0);
        ranked.push((id, 1.0));
    } else {
        terms = select_terms(db, query, 8)?;
        // A Spanish word that changed its gender or number matches nothing: its stored forms
        // are asked for instead (`es`, `why` only). Not under `--all`, which finds them below.
        if !include_invalid {
            let mut unmatched = db
                .conn
                .prepare("SELECT count(*) FROM record_fts WHERE record_fts MATCH ?1")?;
            for w in content_words(query) {
                let n: i64 = unmatched
                    .query_row([format!("\"{w}\"")], |r| r.get(0))
                    .unwrap_or(1);
                if n == 0 {
                    for f in es::indexed_forms(db, &w)? {
                        if !terms.contains(&f) {
                            terms.push(f);
                        }
                    }
                }
            }
        }
        // `select_terms` drops a word no *servable* record holds, because the index it asks
        // holds only those. Under `--all` that is exactly backwards: the word you are asking
        // about — the value that was dropped — is the one word guaranteed to be missing from
        // it, so "why --all PgBouncer" found nothing about PgBouncer. Ask with the words as
        // typed instead; the query below is a LIKE over `record`, which does not need them to
        // be in any index.
        // The scaffolding words `select_terms` never searches for are left out here too, unless
        // they are all there is.
        if include_invalid {
            let raw = words(query);
            let content = content_words(query);
            if !content.is_empty() {
                terms = content;
            } else if !raw.is_empty() {
                terms = raw;
            }
            // a word no record holds as typed is asked by the forms of its stem that some
            // record, retired or not, does hold (`es`)
            register_fold(db)?;
            let mut forms = Vec::new();
            for w in &terms {
                let n = db.conn.query_row(
                    &format!("SELECT count(*) FROM record r WHERE {}", folded_holds(1)),
                    [like_pattern(w)],
                    |r| r.get::<_, i64>(0),
                )?;
                if n == 0 {
                    forms.extend(es::stored_forms(db, w)?);
                }
            }
            for f in forms {
                if !terms.contains(&f) {
                    terms.push(f);
                }
            }
        }
        let lex = lexical(db, &terms, kinds, include_invalid, 20)?;
        // a record's score is its rank in the lexical list, on the reciprocal scale (k = 60)
        let mut by_rank: HashMap<i64, f64> = HashMap::new();
        for (rank, (id, _)) in lex.iter().enumerate() {
            *by_rank.entry(*id).or_default() += 1.0 / (60.0 + rank as f64 + 1.0);
        }
        ranked = by_rank.into_iter().collect();
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
        if !ALL_KINDS.contains(&format!("'{}'", row.kind)) && rt != Route::Id {
            continue;
        }
        let t = estimate(&row.body) + 40;
        // too long for what is left: passed over, so a shorter record further down still fits
        if tokens + t > budget_tokens && !records.is_empty() {
            continue;
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
    })
}

/// Plain-text rendering: one block per record, literal body, provenance, lineage.
pub fn render(a: &Answer) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "muninn why · route {:?} · {} record(s) · ~{} tokens · {}\n",
        a.route,
        a.records.len(),
        a.tokens,
        a.sufficiency,
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
            // a file the client has since removed is named as gone (WP-21)
            let gone = muninn_core::recall::evidence_file(t).is_some_and(|f| !f.is_file());
            s.push_str(&format!(
                "  evidence: {t}{}\n",
                if gone {
                    muninn_core::recall::EVIDENCE_GONE
                } else {
                    ""
                }
            ));
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

    /// Of two records that match a question equally well, the newer one comes first: memory
    /// answers with the most recent statement. The dates here follow neither the ids nor their
    /// reverse, so the order cannot come from the id in either direction.
    #[test]
    fn equal_matches_come_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), muninn_core::db::Mode::ReadWrite).unwrap();
        for (subject, invalid, at) in [
            ("a", 0, 2_000),
            ("b", 0, 3_000),
            ("c", 0, 1_000),
            ("d", 1, 5_000),
            ("e", 1, 6_000),
            ("f", 1, 4_000),
        ] {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,invalid,invalid_reason,created_at) \
                     VALUES('decision',?1,'is','the queue uses Kestrel','the queue uses Kestrel','user_said',3,'s',?1,?2,CASE ?2 WHEN 1 THEN 'superseded' END,?3)",
                    rusqlite::params![subject, invalid, at],
                )
                .unwrap();
        }
        let subjects = |all: bool| -> Vec<String> {
            answer(&db, "Kestrel", all, 20, 1_500)
                .unwrap()
                .records
                .into_iter()
                .map(|f| f.record.subject)
                .collect()
        };
        assert_eq!(subjects(false), ["b", "a", "c"]);
        assert_eq!(subjects(true), ["e", "d", "f", "b", "a", "c"]);
    }

    /// A record too long for what is left of the budget is passed over, not the end of the
    /// answer: two long episodes that match better used to stop it before the short decision
    /// that answers the question.
    #[test]
    fn a_long_record_does_not_end_the_answer() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), muninn_core::db::Mode::ReadWrite).unwrap();
        let long = "user: the retry budget and the retry backoff again, ".repeat(18);
        for (i, (kind, body)) in [
            ("episode", long.as_str()),
            ("episode", long.as_str()),
            ("decision", "the retry budget is three attempts"),
        ]
        .iter()
        .enumerate()
        {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,?2,'is',?3,?3,'user_said',3,'s',?2,1)",
                    rusqlite::params![kind, format!("session:s#{i}"), body],
                )
                .unwrap();
        }
        let a = answer(&db, "the retry budget", false, 8, 500).unwrap();
        assert_eq!(
            a.records[0].record.kind, "episode",
            "the fixture must rank an episode first"
        );
        assert!(
            a.records.iter().any(|f| f.record.kind == "decision"),
            "{:?}",
            a.records.iter().map(|f| &f.record.kind).collect::<Vec<_>>()
        );
    }

    /// Among equal matches the date comes before the route's kinds: on the file route, which does
    /// not prefer corrections, a newer correction with the same words still comes before the older
    /// decision it replaces.
    #[test]
    fn a_newer_record_of_another_kind_comes_before_an_older_preferred_one() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), muninn_core::db::Mode::ReadWrite).unwrap();
        let text = "settings.toml holds the listen port";
        for (kind, invalid, at) in [
            ("decision", 0, 1_000),
            ("correction", 0, 2_000),
            ("decision", 1, 1_000),
            ("correction", 1, 2_000),
        ] {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,invalid,invalid_reason,created_at) \
                     VALUES(?1,?2,'is',?2,?2,'user_said',3,'s',?3,?4,CASE ?4 WHEN 1 THEN 'superseded' END,?5)",
                    rusqlite::params![kind, text, format!("{kind}{invalid}"), invalid, at],
                )
                .unwrap();
        }
        for all in [false, true] {
            let a = answer(&db, text, all, 8, 1_500).unwrap();
            assert_eq!(a.route, Route::File);
            assert_eq!(a.records[0].record.kind, "correction", "--all {all}");
        }
    }

    /// The route a question takes orders what it finds and never hides a kind. A file name sends
    /// a question to the file route, the word "commit" to the commit route, "dead end" to the
    /// dead-end route; each of them used to leave out kinds, so a correction that names a file,
    /// an invariant that says "commit" and a decision about a dead end could not be found by
    /// their own words.
    #[test]
    fn a_route_does_not_hide_a_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), muninn_core::db::Mode::ReadWrite).unwrap();
        let cases = [
            (
                "correction",
                "settings.toml holds the port, not the env file",
            ),
            ("invariant", "every commit is signed with the release key"),
            (
                "decision",
                "the dead end with polling is why we moved to webhooks",
            ),
        ];
        for (kind, text) in cases {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,?2,'is',?2,?2,'user_said',3,'s',?2,1)",
                    rusqlite::params![kind, text],
                )
                .unwrap();
        }
        for (kind, text) in cases {
            let a = answer(&db, text, false, 8, 1_500).unwrap();
            assert!(
                a.records.iter().any(|f| f.record.kind == kind),
                "the {kind} asked by its own words on route {:?}",
                a.route
            );
        }
    }

    /// Spanish changes a word's ending for gender and number, which the English stemmer leaves
    /// alone: asked "abierto", a store that says "abiertas" matched nothing (R-15: 0 of 69 such
    /// pairs on one store). `why` asks for the forms of the word's stem the index holds, and a
    /// form whose own stem is another word's does not come with it: "carta" does not reach
    /// "cartera".
    #[test]
    fn a_spanish_word_reaches_its_other_gender_and_number() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), muninn_core::db::Mode::ReadWrite).unwrap();
        for (i, (body, invalid)) in [
            ("las ventanas del panel quedan abiertas", 0),
            ("los despliegues rápidos van por la noche", 0),
            ("la cartera del cliente se guarda cifrada", 0),
            ("el informe semanal es obligatorio", 1),
        ]
        .iter()
        .enumerate()
        {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,invalid,invalid_reason,created_at) \
                     VALUES('decision',?1,'is',?2,?2,'user_said',3,'s',?1,?3,CASE ?3 WHEN 1 THEN 'superseded' END,1)",
                    rusqlite::params![format!("k{i}"), body, invalid],
                )
                .unwrap();
        }
        let found = |q: &str, all: bool| -> Vec<String> {
            answer(&db, q, all, 8, 1_500)
                .unwrap()
                .records
                .into_iter()
                .map(|f| f.record.object)
                .collect()
        };
        // masculine singular asked, feminine plural stored; feminine singular asked, masculine
        // plural stored
        assert!(found("abierto", false)
            .iter()
            .any(|o| o.contains("abiertas")));
        assert!(found("rápida", false).iter().any(|o| o.contains("rápidos")));
        // the guard: "carta" shares four letters with "cartera" and is not one of its forms
        assert!(
            found("carta", false).is_empty(),
            "{:?}",
            found("carta", false)
        );
        // and --all reaches the forms of a retired record's words too
        assert!(found("informes semanales obligatoria", true)
            .iter()
            .any(|o| o.contains("obligatorio")));
    }

    /// `--all` reads the records' own text, accents and all, while a question's words are folded
    /// as the index folds them: "política" was searched as "politica" and never matched the text
    /// that says "política", and no Spanish form of an accented stem was ever found. The text is
    /// folded the same way before it is compared, and `_` in a word is a letter, not a wildcard.
    #[test]
    fn all_finds_accented_words_and_their_forms() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), muninn_core::db::Mode::ReadWrite).unwrap();
        for (i, body) in [
            "la política de caché expira cada hora",
            "los despliegues rápidos van por la noche",
            "use the slow_path for imports",
            "use the slowXpath variant",
        ]
        .iter()
        .enumerate()
        {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,invalid,invalid_reason,created_at) \
                     VALUES('decision',?1,'is',?2,?2,'user_said',3,'s',?1,1,'superseded',1)",
                    rusqlite::params![format!("k{i}"), body],
                )
                .unwrap();
        }
        let found = |q: &str| -> Vec<String> {
            answer(&db, q, true, 8, 1_500)
                .unwrap()
                .records
                .into_iter()
                .map(|f| f.record.object)
                .collect()
        };
        assert!(
            found("política").iter().any(|o| o.contains("política")),
            "{:?}",
            found("política")
        );
        assert!(found("politica").iter().any(|o| o.contains("política")));
        // a form of an accented stem: "rápida" asked, "rápidos" stored
        assert!(
            found("rápida").iter().any(|o| o.contains("rápidos")),
            "{:?}",
            found("rápida")
        );
        assert_eq!(found("slow_path").len(), 1, "{:?}", found("slow_path"));
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

    /// `--all` searched the first word typed and nothing else, so "what about PgBouncer" looked
    /// for "what" and never found the retired value it names (R-16: 0 of 30, against 18 of 30
    /// asked by the value alone). Every word counts, the rarest the most.
    #[test]
    fn all_uses_every_word_typed() {
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
        for i in 0..30 {
            ins(&format!("what we know about the cache, note {i}"), 0);
        }
        let a = answer(&db, "what about PgBouncer", true, 8, 1_500).unwrap();
        assert!(
            a.records
                .first()
                .is_some_and(|f| f.record.object.contains("PgBouncer")),
            "{:?}",
            a.records
                .iter()
                .map(|f| &f.record.object)
                .collect::<Vec<_>>()
        );
    }
}
