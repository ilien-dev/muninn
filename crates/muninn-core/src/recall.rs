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
    let all: Vec<String> = prompt
        .split_whitespace()
        .filter_map(fts_term)
        .filter(|t| t.len() >= 3)
        .collect();
    let mut terms: Vec<String> = all
        .iter()
        .filter(|t| !STOP.contains(&t.as_str()))
        .cloned()
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
    // `record_vocab` holds the tokens as the index stores them — stemmed, since schema 2 —
    // so a query word that is not its own stem ("releases", "pooler") is absent from it and
    // used to be dropped as unknown. The fallback asks the index itself, which applies the
    // same tokenizer to the query word; it runs only for words the vocab misses, so the
    // common case still costs one lookup.
    let mut matched = db
        .conn
        .prepare("SELECT count(*) FROM record_fts WHERE record_fts MATCH ?1")?;
    let mut scored: Vec<(f64, String)> = Vec::new();
    for t in terms {
        let mut df: f64 = stmt
            .query_row([&t], |r| r.get::<_, Option<f64>>(0))?
            .unwrap_or(0.0);
        if df <= 0.0 {
            df = matched
                .query_row([format!("\"{t}\"")], |r| r.get::<_, i64>(0))
                .unwrap_or(0) as f64;
        }
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
    // `STOP` exists so a prompt's scaffolding does not drag the whole store in, and the
    // df test drops a word no record holds. Between them they can take *every* word of a
    // question and leave silence, which is worse than a common word: a store that holds the
    // answer then returns nothing at all. Both are preferences, not vetoes — they apply
    // while something else survives them.
    if scored.is_empty() {
        for t in all {
            let df: f64 = stmt
                .query_row([&t], |r| r.get::<_, Option<f64>>(0))?
                .unwrap_or(0.0);
            // the rarity guard still applies: a word in a third of the store buys a long
            // posting list and no signal, and without this the full hook went past its
            // latency contract (11.9 ms against a limit of 10)
            if df > 0.0 && !(n >= 50.0 && df > n / 3.0) {
                scored.push((((n + 1.0) / (df + 1.0)).ln(), t));
            }
        }
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
/// A hit scoring worse than this fraction of the first hit's bm25 is padding, not an answer.
/// Half is the loosest round value that separated the two populations on the v4 `--code`
/// store (answers at -6.3, everything else at -4.0 and below); it is a preference over the
/// block's remaining slots, never a reason to serve nothing — the first hit always survives.
const RELEVANCE_FLOOR: f64 = 0.5;

pub fn recall(db: &Db, terms: &[String], limit: usize, exclude: &HashSet<i64>) -> Result<Vec<Hit>> {
    if terms.is_empty() {
        return Ok(vec![]);
    }
    let q = fts_match_or(terms);
    // A commit *log* entry — subject `commit:<hash>`, text a subject line and a file list —
    // is corroboration, and the hook has 700 tokens. Measured in a live grid: adding ten
    // commits to a store of twenty-eight records took the same arm from 18/27 to 6/27, because
    // the log entries match a question through their file names and say nothing when the
    // subject is ordinary ("update dependencies"). They stay in the store, `muninn why`
    // still reaches them, and what the *diff* observed — the record naming the value the file
    // now holds — is not one of them and is still served.
    let mut stmt = db.conn.prepare(&format!(
        "SELECT {SERVED_COLS}, r.transcript_ref, bm25(record_fts, 3.0, 2.0, 1.0) AS score \
         FROM record_fts JOIN served_record r ON r.id = record_fts.rowid \
         WHERE record_fts MATCH ?1 AND r.subject NOT LIKE 'commit:%' ORDER BY score LIMIT ?2"
    ))?;
    let rows = stmt.query_map(rusqlite::params![q, (limit + exclude.len()) as i64], |r| {
        Hit::from_served_row(r, r.get("score")?)
    })?;
    let mut hits: Vec<Hit> = rows
        .filter_map(|r| r.ok())
        .filter(|h| !exclude.contains(&h.id))
        .take(limit)
        .collect();
    // Half as good as the best match, or it is not served. Until now the block filled to its
    // limit whatever the scores were, so a store holding one good answer and a dozen weak ones
    // delivered the answer and then padded it with the weak ones. Measured on the v4 `--code`
    // store: a question about the compression codec matched its answer at -6.39 and then four
    // records about *other* decisions at -3.09 to -2.68, which took four of six slots — they
    // share only what every record Muninn writes about a commit shares
    // (`config/decisions/<id>.json now reads "value": ...`). bm25 here is negative and better
    // is more negative, so the test is against half the first hit's magnitude.
    //
    // It belongs here rather than in `deliver`, which is the CLI's path: the hooks fuse this
    // list with the cues in `hook::deliver_fused` and never call `deliver` at all. A floor in
    // `deliver` measured well on a `muninn recall` probe and changed nothing a cell was given.
    //
    // A trust-3 record — the user said it — is never cut: measured on the plain condition of
    // the same replica, the bare floor drops a correct record that ranks below a
    // better-matching one.
    if let Some(best) = hits.first().map(|h| h.score) {
        let floor = best * RELEVANCE_FLOOR;
        hits.retain(|h| h.score <= floor || h.trust >= 3);
    }
    Ok(hits)
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
/// A catalogue row: id, kind, the record's own text, and the file a commit confirmation is
/// anchored to.
type CatalogRow = (i64, String, String, Option<String>);

/// One line per active decision, newest first: what it says, and which record it retired.
///
/// Every other delivery path answers a question. This one answers a question the agent cannot
/// ask, because asking it requires knowing what is there: *what is recorded at all*. Measured
/// on the v4 and v5 grids, the engine put the current decision in front of the agent in 27
/// cells of 27 and the agent wrote "no current recorded decision" in most of them — it had a
/// filtered selection with no way to tell a memory that holds nothing from one whose query
/// missed, and it went to the checkout two to three times as often as the competitor's agent,
/// which is given a complete catalogue every session and pulls what it wants by id.
///
/// Deterministic and model-free: the line is the record's own `object`, and `replaces #n` is
/// read from `invalidated_by`. Retired rows contribute their id and nothing else — no text of
/// theirs is rendered, which is the same rule `why`'s lineage line already follows.
/// How many catalogue rows are read. One more than this is read to learn whether the list
/// is complete, which is the only thing about the remainder that changes an agent's reading.
const PAGE: usize = 120;

pub fn catalog(db: &Db, budget: usize) -> Result<Delivery> {
    let mut stmt = db.conn.prepare(&format!(
        "SELECT r.id, r.kind, r.object, \
                CASE WHEN r.origin = 'commit_linked' THEN r.anchor_path END AS anchored \
         FROM served_record r \
         WHERE r.kind IN ('decision', 'invariant', 'correction') \
           AND r.subject NOT LIKE 'commit:%' \
         ORDER BY r.created_at DESC, r.id DESC LIMIT {}",
        PAGE + 1
    ))?;
    let rows: Vec<CatalogRow> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .filter_map(|r| r.ok())
        .collect();
    // Which record retired which, in one grouped pass. As a correlated subquery this ran once
    // per row against a column with no index and took SessionStart's p95 to 439 ms against a
    // limit of 10; `record_heir` indexes it, and one pass needs no index at all.
    let mut heirs: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
    {
        let mut h = db.conn.prepare(
            "SELECT invalidated_by, group_concat(id) FROM record \
             WHERE invalidated_by IS NOT NULL GROUP BY invalidated_by",
        )?;
        for (heir, retired) in h
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(|r| r.ok())
        {
            heirs.insert(heir, retired);
        }
    }
    // Two active records with the same key and different values are a conflict, and the
    // block renderer has marked them since F1 was built. The catalogue did not, so the one
    // place an agent is shown everything at once was the one place it could not see that two
    // of them disagree. Asked once per line that is actually emitted — about thirteen of the
    // hundred and twenty read — and as an existence test, not a count: over the whole page,
    // counting them cost 43 ms of a 10 ms hook on a store whose subjects repeat.
    let mut clash_q = db.conn.prepare(
        "SELECT EXISTS(SELECT 1 FROM record r JOIN record o \
                         ON o.subject = r.subject AND o.relation = r.relation \
                        AND o.kind = r.kind \
                       WHERE r.id = ?1 AND o.id <> r.id AND o.invalid = 0 \
                         AND o.object <> r.object AND r.kind <> 'episode')",
    )?;
    let mut text = String::new();
    let mut ids = Vec::new();
    let cap = budget * 3;
    let mut cut = 0usize;
    for (id, kind, object, anchored) in rows.iter().take(PAGE) {
        let flat = object.trim().replace('\n', " ");
        let short = crate::sanitize::truncate_chars(&flat, 72);
        let one = if short.len() < flat.len() {
            format!("{short}\u{2026}")
        } else {
            short.to_string()
        };
        // a commit confirmation's `object` is the source line it found (`"value": "semver"`),
        // which names nothing on its own; the file it is anchored to is what names the subject
        let one = match anchored
            .as_deref()
            .and_then(|a| std::path::Path::new(a).file_stem())
            .and_then(|st| st.to_str())
        {
            Some(stem) => format!("{stem}: {one}"),
            None => one,
        };
        let repl = match heirs.get(id) {
            Some(r) => {
                let list: Vec<String> = r.split(',').map(|i| format!("#{i}")).collect();
                format!(" \u{b7} replaces {}", list.join(","))
            }
            None => String::new(),
        };
        const CLASH: &str = " \u{b7} conflict: another active record disagrees, ask";
        let line = format!("#{id} {kind} \u{b7} {one}{repl}\n");
        if text.len() + line.len() + CLASH.len() > cap {
            cut += 1;
            continue;
        }
        let clashes = clash_q.query_row([id], |r| r.get::<_, i64>(0)).unwrap_or(0) != 0;
        let line = if clashes {
            format!("#{id} {kind} \u{b7} {one}{repl}{CLASH}\n")
        } else {
            line
        };
        text.push_str(&line);
        ids.push(*id);
    }
    if text.is_empty() {
        return Ok(Delivery {
            text,
            ids,
            tokens: 0,
        });
    }
    // Whether this is the whole catalogue decides what an absence from it means, so the block
    // says which it is rather than leaving the reader to assume.
    //
    // It says *whether*, not *how many*. The exact number costs a scan of every active record
    // — 6 ms of a 10 ms hook on a store at the schema's cap — and counting only the page that
    // was read gives a number that is wrong, which is worse than none: a store holding twelve
    // thousand records reported "and 107 older". One row past the page answers the question
    // an agent actually has.
    let truncated = cut > 0 || rows.len() > PAGE;
    let more = if truncated {
        "\u{2026} and more, not listed \u{2014} a subject missing from this list may still be on \
         record; ask `muninn why \"<question>\"`. "
            .to_string()
    } else {
        "That is all of it: a subject missing from this list has nothing on record. ".to_string()
    };
    let text = format!(
        "[muninn:catalog] what is on record, newest first \u{2014} decisions, rules that stand, \
         corrections\n{text}{more}Ask for any of them by id: `muninn show <id> [<id> \u{2026}]`.\n"
    );
    let tokens = text.len() / 3;
    Ok(Delivery { text, ids, tokens })
}

/// The records with these ids, rendered as blocks. The pull half of the catalogue: the agent
/// names what it wants instead of hoping a query reaches it. `served_record` only, so a
/// retired id returns nothing rather than its text.
pub fn show(db: &Db, ids: &[i64], budget: usize) -> Result<Delivery> {
    // a pull of more than this is a dump, not a pull, and the budget would cut it anyway
    let ids = &ids[..ids.len().min(64)];
    if ids.is_empty() {
        return Ok(Delivery {
            text: String::new(),
            ids: vec![],
            tokens: 0,
        });
    }
    let places = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt = db.conn.prepare(&format!(
        "SELECT {SERVED_COLS}, r.transcript_ref, 0.0 AS score FROM served_record r          WHERE r.id IN ({places}) ORDER BY r.created_at DESC, r.id DESC"
    ))?;
    let params: Vec<&dyn rusqlite::ToSql> = ids.iter().map(|i| i as &dyn rusqlite::ToSql).collect();
    let hits: Vec<Hit> = stmt
        .query_map(params.as_slice(), |r| Hit::from_served_row(r, 0.0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(render(&hits, budget, &[]))
}

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

    /// A commit log entry matches a question through the file list in its body and says
    /// nothing when the subject is ordinary. Ten of them in a store of twenty-eight took a
    /// live grid's arm from 18/27 to 6/27. They stay in the store and out of the hook.
    #[test]
    fn a_commit_log_entry_is_not_served_by_the_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        let ins = |subject: &str, origin: &str, body: &str| {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('decision',?1,'is',?3,?3,?2,2,'s',?1,1)",
                    rusqlite::params![subject, origin, body],
                )
                .unwrap();
        };
        for i in 0..8 {
            ins(
                &format!("commit:{i:07x}"),
                "commit_linked",
                "update dependencies\nfiles: config/decisions/cache-eviction.json\n",
            );
        }
        ins(
            "said:change:cache eviction",
            "user_said",
            "user: LRU with a 300-second TTL for cache eviction\n",
        );
        let terms = select_terms(&db, "the cache eviction policy", 8).unwrap();
        let hits = recall(&db, &terms, 8, &HashSet::new()).unwrap();
        assert!(
            hits.iter().all(|h| !h.subject.starts_with("commit:")),
            "the hook serves no commit log entry: {:?}",
            hits.iter().map(|h| h.subject.clone()).collect::<Vec<_>>()
        );
        assert!(
            hits.iter().any(|h| h.body.contains("LRU")),
            "and the decision is still there"
        );
    }

    /// The block used to fill to its limit whatever the scores were, so a store holding one
    /// good answer and a dozen records that share a word with the question delivered the
    /// answer and then padded it with them. On the v4 `--code` store that padding was four of
    /// six blocks, every one of them about a different decision.
    #[test]
    fn padding_that_scores_half_the_answer_is_not_served() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        let ins = |subject: &str, body: &str| {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('decision',?1,'is',?2,?2,'commit_linked',2,'s',?1,1)",
                    rusqlite::params![subject, body],
                )
                .unwrap();
        };
        // each states a different decision, and all of them share the scaffolding Muninn
        // itself writes into a commit record
        for topic in ["versioning", "hashing", "runtime", "license", "wire"] {
            ins(
                &format!("said:change:{topic}"),
                &format!("config/decisions/{topic}.json now reads \"value\": \"something\"\n"),
            );
        }
        ins(
            "said:change:approach compression",
            "config/decisions/compression.json now reads \"value\": \"zstd\"\n",
        );
        let terms = select_terms(
            &db,
            "the current recorded decision on the compression codec",
            8,
        )
        .unwrap();
        let d = deliver(
            &db,
            "the current recorded decision on the compression codec",
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            d.text.contains("zstd"),
            "the answer is served: {terms:?}\n{}",
            d.text
        );
        assert!(
            !d.text.contains("hashing") && !d.text.contains("versioning"),
            "and the decisions it did not ask about are not: {terms:?}\n{}",
            d.text
        );
    }

    /// The floor protects the top of the block, so on its own it drops a correct record that
    /// ranks below a better-matching one — measured, on the plain condition of the same
    /// replica, as one answer lost. What the user said outranks how well it matches.
    #[test]
    fn the_floor_never_cuts_something_the_user_said() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        let ins = |subject: &str, trust: i64, body: &str| {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('decision',?1,'is',?3,?3,'user_said',?2,'s',?1,1)",
                    rusqlite::params![subject, trust, body],
                )
                .unwrap();
        };
        // the better match says nothing; the answer is the weaker match, and the user said it
        ins(
            "said:state:internal http hosts plain",
            1,
            "Architecture allows plain http for internal hosts, internal hosts only\n",
        );
        ins(
            "said:change:internal exception",
            3,
            "user: https everywhere, no plaintext exception\n",
        );
        let d = deliver(
            &db,
            "the current recorded decision on internal hosts",
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            d.text.contains("https everywhere"),
            "the user's own decision survives the floor:\n{}",
            d.text
        );
    }

    /// The catalogue exists so an agent can tell a memory that holds nothing about a subject
    /// from a query that missed it. Its three properties: every active record of the three
    /// kinds is in it, a record that retired another says so by id, and a retired record
    /// contributes its id and none of its text.
    #[test]
    fn the_catalogue_lists_what_is_on_record_and_what_replaced_what() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        let ins = |kind: &str, subject: &str, object: &str| {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,?2,'is',?3,?3,'user_said',3,'s',?2,1)",
                    rusqlite::params![kind, subject, object],
                )
                .unwrap();
            db.conn.last_insert_rowid()
        };
        let old = ins("decision", "said:state:tls", "TLS backend: openssl");
        let new = ins("decision", "said:change:tls", "Let's use rustls instead");
        ins(
            "invariant",
            "said:rule:https",
            "https everywhere, no plaintext exception",
        );
        ins("episode", "session:s#0", "some conversation");
        ins("decision", "commit:abc1234", "update dependencies");
        db.conn
            .execute(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=?2 WHERE id=?1",
                rusqlite::params![old, new],
            )
            .unwrap();
        let c = catalog(&db, 300).unwrap();
        assert!(
            c.text.contains("Let's use rustls instead"),
            "the active decision is listed:\n{}",
            c.text
        );
        assert!(
            c.text.contains("https everywhere"),
            "and so is the rule that stands:\n{}",
            c.text
        );
        assert!(
            c.text.contains(&format!("replaces #{old}")),
            "the record it retired is named by id:\n{}",
            c.text
        );
        assert!(
            !c.text.contains("openssl"),
            "and the retired record's own text is not in it:\n{}",
            c.text
        );
        assert!(
            !c.text.contains("some conversation"),
            "an episode is not a catalogue entry:\n{}",
            c.text
        );
        assert!(
            !c.text.contains("update dependencies"),
            "nor is a commit log entry:\n{}",
            c.text
        );
        // the pull half: named by id, and a retired id yields nothing rather than its text
        let d = show(&db, &[new], 700).unwrap();
        assert!(
            d.text.contains("rustls"),
            "show returns the record:\n{}",
            d.text
        );
        let r = show(&db, &[old], 700).unwrap();
        assert!(
            r.text.is_empty(),
            "a retired id is not servable, not even by name:\n{}",
            r.text
        );
    }

    /// The catalogue's last line decides what an absence from it means, so it has to be right
    /// about completeness on a store of any size. It said "and 107 older, not listed" on a
    /// store holding twelve thousand of them, because it counted the page it had read; and the
    /// exact number costs a scan of every active record, 6 ms of a 10 ms hook at the schema's
    /// cap. One row past the page answers the question an agent actually has.
    #[test]
    fn the_catalogue_says_whether_it_is_the_whole_list() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        {
            let tx = db.conn.unchecked_transaction().unwrap();
            for i in 0..400 {
                tx.execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('decision',?1,'is',?2,?2,'user_said',3,'s',?1,?3)",
                    rusqlite::params![format!("said:state:s{i}"), format!("decision number {i}"), i],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        let c = catalog(&db, 300).unwrap();
        assert!(c.ids.len() < 400, "the budget cut some: {}", c.ids.len());
        assert!(
            c.text.contains("and more, not listed"),
            "a truncated catalogue says so:\n{}",
            c.text
        );
        assert!(
            !c.text.contains("That is all of it"),
            "and never claims to be complete:\n{}",
            c.text
        );

        let tmp2 = tempfile::tempdir().unwrap();
        let db2 = Db::open(&tmp2.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        db2.conn
            .execute(
                "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                 VALUES('decision','said:state:one','is','the only decision','the only decision','user_said',3,'s','h',1)",
                [],
            )
            .unwrap();
        let c2 = catalog(&db2, 300).unwrap();
        assert!(
            c2.text.contains("That is all of it"),
            "a complete catalogue says so, which is what makes an absence mean something:\n{}",
            c2.text
        );
        assert!(
            c2.text.contains("the only decision"),
            "and the probe row past the page is never rendered as an entry:\n{}",
            c2.text
        );
    }

    /// Two active records with the same key and different values are a conflict, and the
    /// renderer has marked them since F1 was built. The catalogue did not, so the one place
    /// an agent is shown everything at once was the one place it could not see that two of
    /// them disagree — and it would have picked one.
    #[test]
    fn the_catalogue_marks_a_record_another_active_one_contradicts() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        let ins = |subject: &str, object: &str, h: &str| {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('decision',?1,'is',?2,?2,'user_said',3,'s',?3,1)",
                    rusqlite::params![subject, object, h],
                )
                .unwrap();
        };
        ins("said:state:tls", "we use openssl", "a");
        ins("said:state:tls", "we use rustls", "b");
        ins("said:state:cache", "we use redis", "c");
        let c = catalog(&db, 300).unwrap();
        let line_of = |needle: &str| {
            c.text
                .lines()
                .find(|l| l.contains(needle))
                .unwrap_or("")
                .to_string()
        };
        assert!(
            line_of("openssl").contains("conflict"),
            "both sides of the disagreement are marked:\n{}",
            c.text
        );
        assert!(
            line_of("rustls").contains("conflict"),
            "both sides of the disagreement are marked:\n{}",
            c.text
        );
        assert!(
            !line_of("redis").contains("conflict"),
            "and a record nothing contradicts is not:\n{}",
            c.text
        );
    }

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
