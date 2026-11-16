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

/// Whether `term` (as `fts_term` gives it) is scaffolding that `select_terms` never searches for.
pub fn is_stop_word(term: &str) -> bool {
    STOP.contains(&term)
}

/// Cheap gate: bare acknowledgements and slash commands get no memory.
///
/// A slash command carrying a request (`/design make the navbar sticky on mobile`) is gated
/// on that request, and a prompt that is a paste (`<pasted_content …>`) on the pasted text:
/// both used to be dropped whole, with harness notifications and `/clear`.
pub fn intent_gate(prompt: &str) -> bool {
    let mut p = prompt.trim();
    if let Some(cmd) = p.strip_prefix('/') {
        match cmd.split_once(char::is_whitespace) {
            Some((_, args)) => p = args.trim(),
            None => return false,
        }
    } else if p.starts_with("<pasted_content") {
        p = p.split_once('>').map(|(_, rest)| rest).unwrap_or("").trim();
        p = p.rfind("</pasted_content").map_or(p, |i| &p[..i]).trim();
    } else if p.starts_with('<') {
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
    // The active-record count is what the FTS triggers already keep in `meta`; counting it
    // here walked the whole index on every prompt for a number that is used as an IDF
    // denominator and a threshold.
    let n: f64 = match db
        .meta_get("fts_rows")
        .ok()
        .flatten()
        .and_then(|s| s.parse::<f64>().ok())
    {
        Some(v) if v >= 0.0 => v,
        _ => db.count("SELECT count(*) FROM record WHERE invalid=0")? as f64,
    };
    // A term's document frequency is the number of records the index matches it in. It was
    // read from `record_vocab`, which counts per column, summed: a typed record repeats its
    // value in subject, object and body, so its words counted three times and some were
    // dropped as too common (R-18: 7,195 of 17,781 terms of one store over their real count).
    // The index also applies the query's own tokenizer, so a word that is not its own stem
    // ("releases", "pooler") is counted as the query will match it.
    let mut matched = db
        .conn
        .prepare("SELECT count(*) FROM record_fts WHERE record_fts MATCH ?1")?;
    // One lookup per distinct word, not per appearance and not once more in the fallback.
    // Reading a term's document frequency means decoding its whole posting list, so a word
    // the store uses everywhere costs about 0.6 ms on a store at the schema's cap — and the
    // words a question repeats are exactly those. The second pass below used to ask again
    // for every word of the prompt, `STOP` words included.
    let mut df_of: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    let df = |t: &str,
              matched: &mut rusqlite::Statement,
              cache: &mut std::collections::HashMap<String, f64>|
     -> Result<f64> {
        if let Some(v) = cache.get(t) {
            return Ok(*v);
        }
        let d = matched
            .query_row([format!("\"{t}\"")], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as f64;
        cache.insert(t.to_string(), d);
        Ok(d)
    };
    // a term in more than a third of the store carries no signal and costs a long posting list
    let too_common = |d: f64| n >= 50.0 && d > n / 3.0;
    let mut scored: Vec<(f64, String)> = Vec::new();
    for t in &terms {
        let d = df(t, &mut matched, &mut df_of)?;
        if d <= 0.0 || too_common(d) {
            continue;
        }
        scored.push((((n + 1.0) / (d + 1.0)).ln(), t.clone()));
    }
    // `STOP` exists so a prompt's scaffolding does not drag the whole store in, and the
    // df test drops a word no record holds. Between them they can take *every* word of a
    // question and leave silence, which is worse than a common word: a store that holds the
    // answer then returns nothing at all. The df test is a preference and is lifted here;
    // `STOP` is not, and that is the correction.
    //
    // It used to iterate every word of the prompt, `STOP` included. On a store whose records
    // are one word each — `gzip`, `zstd` — no content word of "the transport compression
    // codec" exists, so the fallback matched on `the` and delivered four episodes about
    // bcrypt, TTLs and HTTPS. Silence is worse than a common word; four confident answers to
    // a question nobody asked are worse than silence, and they cost budget to say nothing.
    if scored.is_empty() {
        for t in all.iter().filter(|t| !STOP.contains(&t.as_str())) {
            // the rarity guard still applies: a word in a third of the store buys a long
            // posting list and no signal, and without this the full hook went past its
            // latency contract (11.9 ms against a limit of 10)
            let d = df(t, &mut matched, &mut df_of)?;
            if d > 0.0 && !too_common(d) {
                scored.push((((n + 1.0) / (d + 1.0)).ln(), t.clone()));
            }
        }
    }
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap()
            .then_with(|| b.1.len().cmp(&a.1.len()))
    });
    scored.dedup_by(|a, b| a.1 == b.1);
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
    pub kind: String,
    /// The active records this one disagrees with (F1), set by the read path and rendered as
    /// `:conflict with #n` after the kind. Kept apart from `kind`: written into it, a marked
    /// decision ranked as an episode (R-06).
    pub conflict: Vec<i64>,
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
            conflict: Vec::new(),
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

    /// The kind as a block header shows it: with `:conflict with #n,#m` when there is one.
    pub fn shown_kind(&self) -> String {
        if self.conflict.is_empty() {
            return self.kind.clone();
        }
        let ids: Vec<String> = self.conflict.iter().map(|i| format!("#{i}")).collect();
        format!("{}:conflict with {}", self.kind, ids.join(","))
    }

    /// Mark the records this one disagrees with (F1), unless the arm switches marking off.
    pub fn mark_conflicts(db: &Db, hits: &mut [Hit]) {
        let mark = std::env::var("MUNINN_ARM")
            .map(|a| a != "unfiltered")
            .unwrap_or(true);
        if !mark {
            return;
        }
        for h in hits.iter_mut().filter(|h| h.kind != "episode") {
            if let Ok(c) = crate::filter::conflicts_of(db, h.id) {
                h.conflict = c;
            }
        }
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
    //
    // Equal scores go newest first. With the score alone FTS5 returned a tie in rowid order,
    // oldest first, so of two records that say the same thing the older one took the slot
    // (R-07: the delivered set changed on 47 of 209 replayed prompts of one 1.2.0 store).
    let mut stmt = db.conn.prepare(&format!(
        "SELECT {SERVED_COLS}, r.transcript_ref, bm25(record_fts, 3.0, 2.0, 1.0) AS score \
         FROM record_fts JOIN served_record r ON r.id = record_fts.rowid \
         WHERE record_fts MATCH ?1 AND r.subject NOT LIKE 'commit:%' \
         ORDER BY score, r.created_at DESC, r.id DESC LIMIT ?2"
    ))?;
    // How many rows the loop below reads. Rows skipped as stale twins are read past it, from
    // the slack, so a skipped twin costs no slot and every other row is read as before.
    let reads = limit + exclude.len();
    let rows: Vec<Hit> = stmt
        .query_map(rusqlite::params![q, (reads + limit) as i64], |r| {
            Hit::from_served_row(r, r.get("score")?)
        })?
        .filter_map(|r| r.ok())
        .collect();
    // A statement replaced by a newer one is retired, and the episode of the turn that said it
    // is not: 1.2.0 stopped replacing episodes by their text. That episode holds the same
    // sentence and matches the same words, so the retired statement was served through it — on
    // 13 of 210 replayed prompts of one 1.2.0 store and 4 of 49 of another (R-05). It reads
    // `record` because what it asks about is a retired row; it returns no text of one. The
    // write path retires a short episode with its statement and keeps a long one, and a store
    // written before it did neither; neither may serve the retired sentence.
    //
    // One statement for every episode row read, excluded or not, rather than a probe per row: a
    // long session excludes hundreds of rows, and probing each of them one at a time cost more
    // than the rest of the query.
    let stale = stale_turns(db, &rows)?;
    let mut read = 0usize;
    // `best` is the best match this query has, not the best one left after exclusion. The
    // difference shows on a repeat: ask the same question twice and everything the first
    // answer used is excluded, so the second call's best remaining hit becomes the reference
    // the floor is measured against — and a set of weak records, all weak together, all pass
    // it. On this project's store, asking one question three times delivered three full
    // blocks, the third of them 588 tokens of records the floor would have cut on a fresh
    // ask. Holding the reference fixed makes the block empty instead: the good answer has
    // already been given.
    let mut best: Option<f64> = None;
    // the newest of everything this query reached, excluded or not — see the floor below
    let mut newest: Option<(i64, i64)> = None;
    let mut hits: Vec<Hit> = Vec::new();
    for h in rows {
        // a stale twin, excluded or not, counts toward neither the rows read, nor the floor's
        // reference, nor the newest the query reached
        if h.kind == "episode"
            && h.transcript_ref
                .as_deref()
                .is_some_and(|t| stale.contains(t))
        {
            continue;
        }
        if read == reads {
            break;
        }
        read += 1;
        if best.is_none() {
            best = Some(h.score);
        }
        if newest.is_none_or(|(at, id)| (h.created_at, h.id) > (at, id)) {
            newest = Some((h.created_at, h.id));
        }
        if exclude.contains(&h.id) {
            continue;
        }
        if hits.len() == limit {
            break;
        }
        hits.push(h);
    }
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
    if let Some(best) = best {
        let floor = best * RELEVANCE_FLOOR;
        // The newest record the query reached is never padding. The floor is measured against
        // the best match, and the best match is the one that repeats the question's words —
        // which is the record that *stated* the thing, not the one that later changed it. On
        // the plain head-to-head "the project license" reaches "Project license is GPL-3.0"
        // at full score and "Apache-2.0 is better for enterprise adoption" far below it, and
        // the floor then delivered the retired value alone. One block, the newest of what the
        // query found, so padding cannot come back with it.
        // `newest` is taken over everything the query reached, including what this session
        // has already been given. A record already delivered is not re-admitted by this — on
        // a repeat question the newest is excluded, the exemption finds nothing, and the
        // block is empty, which is what `a_repeat_question_does_not_lower_the_bar_to_fill_the
        // _block` asks for.
        // Strictly later than the best match, not merely last in the tie: a store where
        // everything arrived at once has no later statement in it, only weaker ones.
        let keep = newest
            .filter(|(at, id)| {
                hits.first()
                    .is_some_and(|top| top.id != *id && *at > top.created_at)
            })
            .map(|(_, id)| id);
        hits.retain(|h| h.score <= floor || h.trust >= 3 || Some(h.id) == keep);
        absolute_floor(db, terms, &mut hits, keep)?;
        beside_the_rules(&mut hits, keep);
    }
    Ok(hits)
}

/// How many matches a block holds beside its rules, its corrections and the newest record.
///
/// What passes both floors still came four blocks a prompt, and on the design half of usage-v4's
/// labelled prompts every useful block was among the first two by score; the third and fourth were
/// what the budget had room for. Rules and the newest record do not count against this: the
/// newest fact prevails, and a rule that matches is not cut for a better-scoring episode.
const OTHER_MATCHES: usize = 2;

/// Keep, in score order, the rules, corrections and `keep`, and the first `OTHER_MATCHES` turns
/// of everything else (chunks of one turn are one block, see `cue::merge`).
fn beside_the_rules(hits: &mut Vec<Hit>, keep: Option<i64>) {
    let mut turns: HashSet<String> = HashSet::new();
    hits.retain(|h| {
        if matches!(h.kind.as_str(), "invariant" | "correction") || Some(h.id) == keep {
            return true;
        }
        let t = turn_key(&h.subject);
        turns.contains(&t) || (turns.len() < OTHER_MATCHES && turns.insert(t))
    });
}

/// The share of a question's selected words a record must hold to be served: a third, rounded
/// up, so one word of up to three, two of four to six, three of seven or eight.
///
/// The floor above is relative to the best match, and when the best match is weak everything
/// passes it: one rare word in common was enough, and on one 1.2.0 store 39 % of the blocks
/// delivered for questions of four or more words held exactly one of them (R-08). Tuned on the
/// design half of usage-v4's labelled prompts only.
fn needed_terms(selected: usize) -> usize {
    selected.div_ceil(3).max(1)
}

/// Cut the hits that hold fewer of `terms` than `needed_terms` asks, with two exceptions, both
/// for the newest fact to prevail: the newest record the query reached (`keep`, see `recall`),
/// and a rule or correction that scores at least as well as a record this keeps — a floor on
/// words must not make a current rule lose its place to a weaker match.
fn absolute_floor(db: &Db, terms: &[String], hits: &mut Vec<Hit>, keep: Option<i64>) -> Result<()> {
    let need = needed_terms(terms.len());
    if need <= 1 || hits.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = hits.iter().map(|h| h.id).collect();
    let held = matched_terms(db, terms, &ids)?;
    let enough = |h: &Hit| held.get(&h.id).copied().unwrap_or(0) >= need || Some(h.id) == keep;
    // bm25 is negative and better is more negative: the weakest kept record has the largest score
    let weakest = hits
        .iter()
        .filter(|h| enough(h))
        .map(|h| h.score)
        .fold(None, |w: Option<f64>, s| Some(w.map_or(s, |w| w.max(s))));
    let rule = |h: &Hit| matches!(h.kind.as_str(), "invariant" | "correction");
    hits.retain(|h| enough(h) || (rule(h) && weakest.is_some_and(|w| h.score <= w)));
    Ok(())
}

/// The turns among `rows`' episodes whose typed statement is retired as superseded (`recall`).
/// Statements only: a dead end is retired the same way when its command passes later or fails
/// again, and that says nothing about what the person wrote in its turn.
fn stale_turns(db: &Db, rows: &[Hit]) -> Result<HashSet<String>> {
    let refs: Vec<&str> = rows
        .iter()
        .filter(|h| h.kind == "episode")
        .filter_map(|h| h.transcript_ref.as_deref())
        .collect();
    let mut out = HashSet::new();
    for chunk in refs.chunks(500) {
        let places = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut st = db.conn.prepare(&format!(
            "SELECT DISTINCT transcript_ref FROM record WHERE transcript_ref IN ({places}) \
               AND invalid = 1 AND invalid_reason = 'superseded' \
               AND kind IN ('decision', 'invariant', 'correction')"
        ))?;
        let params: Vec<&dyn rusqlite::ToSql> =
            chunk.iter().map(|t| t as &dyn rusqlite::ToSql).collect();
        out.extend(
            st.query_map(params.as_slice(), |r| r.get::<_, String>(0))?
                .flatten(),
        );
    }
    Ok(out)
}

/// For each of `ids`, how many of `terms` it holds, as the index matches them (the same
/// tokenizer and stemming as the query, so a count here agrees with what MATCH found). One
/// statement per term, restricted to the candidates' rowids.
pub fn matched_terms(
    db: &Db,
    terms: &[String],
    ids: &[i64],
) -> Result<std::collections::HashMap<i64, usize>> {
    let mut out: std::collections::HashMap<i64, usize> = ids.iter().map(|i| (*i, 0)).collect();
    if ids.is_empty() {
        return Ok(out);
    }
    let places = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let mut st = db.conn.prepare(&format!(
        "SELECT rowid FROM record_fts WHERE record_fts MATCH ?1 AND rowid IN ({places})"
    ))?;
    for t in terms {
        let phrase = format!("\"{t}\"");
        let mut params: Vec<&dyn rusqlite::ToSql> = vec![&phrase];
        params.extend(ids.iter().map(|i| i as &dyn rusqlite::ToSql));
        for id in st
            .query_map(params.as_slice(), |r| r.get::<_, i64>(0))?
            .flatten()
        {
            *out.entry(id).or_default() += 1;
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct Delivery {
    pub text: String,
    pub ids: Vec<i64>,
    pub tokens: usize,
}

/// `yyyy-mm-dd hh:mm` from epoch milliseconds, UTC. Used where a date alone is ambiguous —
/// two builds of the same day are the case this exists for.
pub fn date_time_of(ms: i64) -> String {
    let mins = (ms / 60_000).rem_euclid(1_440);
    format!("{} {:02}:{:02}", date_of(ms), mins / 60, mins % 60)
}

pub(crate) fn date_of(ms: i64) -> String {
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

/// The file a record's `transcript_ref` names (`<path>:<byte offset>`), when it names one: a
/// commit's reference (`git:<hash>`) names none.
pub fn evidence_file(transcript_ref: &str) -> Option<&std::path::Path> {
    if transcript_ref.starts_with("git:") {
        return None;
    }
    let path = match transcript_ref.rsplit_once(':') {
        Some((p, off)) if !off.is_empty() && off.bytes().all(|b| b.is_ascii_digit()) => p,
        _ => transcript_ref,
    };
    (!path.is_empty()).then(|| std::path::Path::new(path))
}

/// What an evidence line ends with when its file is gone (WP-21). The client removes old
/// transcripts; the record keeps what was stored and is served as before.
pub const EVIDENCE_GONE: &str =
    " (this file is no longer on disk; the text above is what was kept)";

/// The evidence line of a record someone pulled by hand (`show`, `why`): as a block's, and
/// saying so when the file is gone. Not for a hook: it looks at the file system.
pub fn evidence_line_checked(r: &Option<String>) -> String {
    let line = evidence_line(r);
    let gone = r
        .as_deref()
        .and_then(evidence_file)
        .is_some_and(|f| !f.is_file());
    match line.strip_suffix('\n') {
        Some(head) if gone => format!("{head}{EVIDENCE_GONE}\n"),
        _ => line,
    }
}

/// The evidence line of a block: where the literal text can be opened.
pub fn evidence_line(r: &Option<String>) -> String {
    match r {
        Some(t) if !t.is_empty() => {
            // HOME is unset in a native Windows process; USERPROFILE is its home there
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_default();
            match t.strip_prefix(home.as_str()) {
                Some(rest) if !home.is_empty() => format!("  evidence: ~{rest}\n"),
                _ => format!("  evidence: {t}\n"),
            }
        }
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
            "[muninn:{}] #{} · {} · session {} · origin: {} · trust {}{}\n{}\n{}",
            h.shown_kind(),
            h.id,
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
                "[muninn:{}] #{} · {} · session {} · origin: {} · trust {}{}\n{}…\n{}",
                h.shown_kind(),
                h.id,
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
/// A catalogue row: id, kind, the record's own text.
type CatalogRow = (i64, String, String);

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
    catalog_beside(db, budget, &[])
}

/// Every word of `line` is an acknowledgement (`ok, yes`, `vale, gracias`): it names nothing.
fn only_acknowledges(line: &str) -> bool {
    const ACK: [&str; 26] = [
        "ok", "okay", "yes", "yeah", "yep", "no", "sure", "thanks", "thank", "you", "fine", "good",
        "great", "perfect", "done", "si", "sí", "vale", "gracias", "perfecto", "genial", "bien",
        "dale", "listo", "hecho", "claro",
    ];
    let words: Vec<String> = line
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    !words.is_empty() && words.iter().all(|w| ACK.contains(&w.as_str()))
}

/// The catalogue, delivered beside blocks that already show the records `shown`: those are
/// not listed again. A line naming a record whose full block sits next to it spent a slot on
/// nothing new — 19 % of catalogue lines on one 1.2.0 store, 24 % on another (R-13).
pub fn catalog_beside(db: &Db, budget: usize, shown: &[i64]) -> Result<Delivery> {
    // What a commit's diff observed (`said:change:…`, `commit_linked`: "this file now reads this
    // line") is typed as a decision and is nobody's decision, so it is not listed. It stays on
    // record and a prompt that names it is still answered with it, so a list that left one out
    // is not the whole of what is on record (`left_out`). The page is the one read before, the
    // newest rows of the three kinds with the observations among them: an observation in it is
    // skipped, and no older row is read in its place.
    let mut stmt = db.conn.prepare(&format!(
        "SELECT r.id, r.kind, r.object, r.origin = 'commit_linked' \
         FROM served_record r \
         WHERE +r.kind IN ('decision', 'invariant', 'correction') \
           AND r.subject NOT LIKE 'commit:%' \
         ORDER BY r.created_at DESC, r.id DESC LIMIT {}",
        PAGE + 1
    ))?;
    let page: Vec<(CatalogRow, bool)> = stmt
        .query_map([], |r| Ok(((r.get(0)?, r.get(1)?, r.get(2)?), r.get(3)?)))?
        .filter_map(|r| r.ok())
        .collect();
    let past_page = page.len() > PAGE;
    let left_out = page.iter().any(|(_, observed)| *observed);
    let mut rows: Vec<CatalogRow> = page
        .into_iter()
        .take(PAGE)
        .filter(|(_, observed)| !observed)
        .map(|(row, _)| row)
        .collect();
    // Of the page, the rules that stand and the corrections come first, then the decisions,
    // each still newest first. Newest first over all three kinds, a store's list was its latest
    // requests typed as decisions: on usage-v4's labelled design session starts 6 % of those
    // lines were worth their place, and the rules and corrections on record were mostly neither
    // shown nor listed (R-04). Within the page that was read, so it costs no query: reading the
    // rules on their own, scattered through the store, cost about 3 ms of a cold SessionStart
    // on the bench store.
    rows.sort_by_key(|(_, kind, _)| kind == "decision");
    // A store can hold almost nothing this list is made of. Where a person types the value and
    // nothing else — `gzip`, then later `zstd` — no sentence carries a decision verb, capture
    // types nothing unless the reply states the replacement, and the one channel that tells an
    // agent what is on record shows two lines while twenty episodes hold every value the
    // questions are about. That fixture is the condition this engine loses.
    //
    // This was built once before and reverted (v32), and the reason it bought nothing is now
    // visible: at that time `first_line` skipped a one-word line, so eight of those twenty
    // episodes carried the harness's own trailing instruction as their object and the list it
    // produced was the same sentence over and over. With the object being the value, the list
    // is `zstd` above `gzip`, `cbor` above `msgpack`, `semver` above `calver` — the values
    // themselves, newest first, which is the ordering the question turns on.
    //
    // Only where there is almost nothing typed. A store with a working page of decisions keeps
    // the list that won the code condition, undiluted: the budget is spent before the episodes
    // are reached, and the threshold is what stops this being a change to every store in
    // exchange for one fixture.
    const SPARSE: usize = 5;
    let typed = rows.len();
    if typed < SPARSE {
        let mut ep = db.conn.prepare(&format!(
            "SELECT r.id, r.kind, r.object FROM served_record r \
             WHERE +r.kind = 'episode' AND length(r.object) <= 120 \
             ORDER BY r.created_at DESC, r.id DESC LIMIT {}",
            PAGE + 1
        ))?;
        // Two episodes with the same object say the same thing once. Before `first_line` read a
        // one-word line this was most of them; it is kept because the list is short and a
        // repeated line spends the budget that the next distinct value needs.
        let mut seen: std::collections::HashSet<String> = rows
            .iter()
            .map(|(_, _, o): &CatalogRow| o.trim().to_lowercase())
            .collect();
        let extra: Vec<CatalogRow> = ep
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .filter_map(|r| r.ok())
            .collect();
        for row in extra {
            if seen.insert(row.2.trim().to_lowercase()) {
                rows.push(row);
            }
        }
    }
    // The `+` on `kind` is not decoration. Without it SQLite takes the equality on
    // `record_kind`, cannot then satisfy the ORDER BY from an index, and sorts every active
    // decision in a temp B-tree before the LIMIT throws almost all of it away: 23 ms of a
    // 10 ms hook on a store at the schema's cap, against 0.3 ms through `record_recent`.
    // The unary plus makes the term unusable as an index constraint while leaving what it
    // selects exactly as written, so the planner is left with the one index that answers the
    // ORDER BY and stops at the page.
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
    //
    // `+o.kind` for the same reason as the page query above: with a bare equality the planner
    // takes `record_kind` and walks every active decision in the store to find one that shares
    // a subject, once per emitted line — 15.7 ms of a 10 ms hook at the schema's cap, against
    // 0.02 ms through the key. `record_active_kind_key` holds the kind too, so the kind is
    // tested in the index: through `record_active_key` every row with the same subject was read
    // to learn its kind, which on the bench store cost about 1 ms a line for the invariants the
    // catalogue lists first. A store not yet migrated falls back to `record_active_key`.
    let mut clash_q = db.conn.prepare(
        "SELECT EXISTS(SELECT 1 FROM record r JOIN record o \
                         ON o.subject = r.subject AND o.relation = r.relation \
                        AND +o.kind = r.kind \
                       WHERE r.id = ?1 AND o.id <> r.id AND o.invalid = 0 \
                         AND o.object <> r.object AND r.kind <> 'episode')",
    )?;
    let mut text = String::new();
    let mut ids = Vec::new();
    let mut listed_episodes = false;
    let cap = budget * 3;
    let mut cut = 0usize;
    // the same sentence on record under two kinds is listed once, under the first (newest) row
    // compared at the length capture cuts a correction to: a long first line is whole in the
    // decision and cut at 160 in the correction from the same message
    let key_of = |object: &str| {
        let key = object.lines().map(str::trim).find(|l| !l.is_empty());
        crate::sanitize::truncate_chars(key.unwrap_or(""), 160)
            .trim_end()
            .to_lowercase()
    };
    let mut objects_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    // A record shown in full beside the list claims its sentence before the list starts, so its
    // twin under another kind is not listed either, whichever of the two is newer.
    if !shown.is_empty() {
        let places = std::iter::repeat_n("?", shown.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut st = db.conn.prepare(&format!(
            "SELECT r.object FROM served_record r WHERE r.id IN ({places})"
        ))?;
        let params: Vec<&dyn rusqlite::ToSql> =
            shown.iter().map(|i| i as &dyn rusqlite::ToSql).collect();
        for o in st
            .query_map(params.as_slice(), |r| r.get::<_, String>(0))?
            .flatten()
        {
            objects_seen.insert(key_of(&o));
        }
    }
    for (id, kind, object) in rows.iter().take(PAGE) {
        if !objects_seen.insert(key_of(object)) || shown.contains(id) {
            continue;
        }
        // The first line of the object, not the whole of it flattened. A decision made from a
        // whole message carries every line of that message, and flattening them into one
        // string spends the line's 72 characters on whatever followed the statement — in the
        // fixtures, on the harness's own trailing instruction, so half of every catalogue line
        // was the same sentence and three or four fewer decisions fitted the budget. The
        // measured cost of that: on two fixtures the cache-eviction decision fell off the
        // bottom of the list, and its value then reached the session nowhere at all — nine of
        // eighteen cells. What a reader wants from one line is the first one.
        let first = object
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("");
        // …and of that line, the clause that carries the value. A decision's sentence is
        // usually the value and then the reason for it — "Going with LRU with a 300-second
        // TTL, easier to reason about", "Semver is better, our API compatibility matters way
        // more than publication dates" — and the reason is what a one-line index does not need.
        // Dropping it is not cosmetic: the list is cut by a token budget, and on two fixtures
        // the two oldest decisions fell off the bottom for want of about a hundred and fifty
        // characters, so the value an early question asks about reached the session nowhere at
        // all. The cut needs a floor, because "Actually, let's switch to zstd" and "Yes, use
        // LFU" open on a clause that names nothing. The floor is what the clause says rather
        // than how long it is: two words, one of them four characters or more and not a
        // function word. That keeps "Apache-2.0 instead" and "https everywhere", which a
        // character floor threw away, and still refuses "Actually" and "OK so".
        // Nor do the words of a clause that points back at what was said before: `En lugar de
        // eso` and `En cuanto a esta parte` pass the length test on `lugar`, `cuanto` and
        // `parte`, and a real catalogue listed clauses like these as titles.
        const POINTING: [&str; 12] = [
            "lugar", "cuanto", "parte", "donde", "dices", "dijiste", "esto", "esta", "este", "eso",
            "that", "this",
        ];
        let carries_a_value = |clause: &str| {
            let words: Vec<&str> = clause.split_whitespace().collect();
            words.len() >= 2
                && words.iter().any(|w| {
                    let bare = w.trim_matches(|c: char| !c.is_alphanumeric());
                    bare.chars().count() >= 4 && !POINTING.contains(&bare.to_lowercase().as_str())
                })
        };
        // A question, or an acknowledgement (`ok, yes`), typed as a decision is not what is on
        // record, and its line is the next record's. Labelled in usage-v4's design session
        // starts, lines like these were noise or fragments, never a rule or a decision. Only a
        // decision, and only words that are all acknowledgements: a short rule or correction
        // (`use uv`) is the rule, and `Yes, use LFU` names its value in short words.
        if kind == "decision"
            && (first.contains('?') || first.contains('\u{bf}') || only_acknowledges(first))
        {
            continue;
        }
        // Only where the line does not fit anyway. A first line inside the display width is
        // shown whole — on a real store those are ordinary prose and the clause before their
        // first comma is not what they are about. A line that is going to be cut is cut at
        // something that means something instead of at a character count.
        let flat = if first.chars().count() > 72 {
            first
                .char_indices()
                .filter(|(_, c)| *c == ',' || *c == ';')
                .map(|(i, _)| i)
                .find(|i| carries_a_value(&first[..*i]))
                .map_or(first, |i| &first[..i])
                .to_string()
        } else {
            first.to_string()
        };
        let short = crate::sanitize::truncate_chars(&flat, 72);
        let one = if short.len() < flat.len() {
            // on the last whole word, unless that leaves less than half the line: a title cut
            // mid-word reads as a different word
            let whole = match short.rfind(char::is_whitespace) {
                Some(i) if flat[i..].starts_with(char::is_whitespace) && i >= short.len() / 2 => {
                    short[..i].trim_end()
                }
                _ => short,
            };
            format!("{whole}\u{2026}")
        } else {
            short.to_string()
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
        let clashes = clash_q.query_row([id], |r| r.get::<_, i64>(0)).unwrap_or(0) != 0
            || (kind != "episode"
                && !crate::filter::marked_conflicts(db, *id)
                    .unwrap_or_default()
                    .is_empty());
        let line = if clashes {
            format!("#{id} {kind} \u{b7} {one}{repl}{CLASH}\n")
        } else {
            line
        };
        text.push_str(&line);
        ids.push(*id);
        listed_episodes |= kind == "episode";
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
    let truncated = cut > 0 || past_page || left_out;
    let more = if truncated {
        "\u{2026} and more, not listed \u{2014} a subject missing from this list may still be on \
         record; ask `muninn why \"<question>\"`. "
            .to_string()
    } else if listed_episodes {
        // The list holds the episodes themselves, so what an absence from it means is the
        // same as for any other line. What is worth saying is the one thing the lines carry
        // that a decision does not: they are in time order. That is a fact about the list,
        // not a reading of it — nothing here tells an agent that a later line retires an
        // earlier one, because nothing in the store says so and this block does not guess.
        "That is all of it. The lines marked `episode` are what was said, word for word, newest \
         first; where no decision covers a subject, the newest thing said about it is the latest \
         word on it. "
            .to_string()
    } else if db
        .conn
        .query_row(
            "SELECT 1 FROM served_record WHERE kind = 'episode' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .is_ok()
    {
        // The list is every typed record, and on a store that also holds episodes that is not
        // the same as everything on record. The old sentence said it was, and it was read on
        // exactly the store where it does most harm: where a person typed the value and
        // nothing else, nothing is typed, the list has two lines, and twenty episodes hold
        // every value the question is about. An agent told those subjects have nothing on
        // record has been told the opposite of the truth, by the one block whose job is to say
        // what is there.
        //
        // v34 read what the previous wording did, in the agent's own reports: it found the
        // values in the episodes, in order, and refused them as "unclassified excerpts, not
        // decisions", writing "no decision is currently recorded" with the answer in front of
        // it. The sentence was true and was read as a ranking. What it has to say instead is
        // also true: an episode is the person's own words, and when nothing on this list covers
        // a subject, the newest thing said about it is the latest word on it — which is a fact
        // about time, not an inference about which record retired which.
        "That is every decision, rule and correction on record. What was said in earlier \
         sessions is kept too, word for word, as episodes; a subject missing from this list may \
         be in one, and when it is, the newest thing said about it is the latest word on it \
         \u{2014} ask `muninn why \"<question>\"`. "
            .to_string()
    } else {
        "That is all of it: a subject missing from this list has nothing on record. ".to_string()
    };
    let head = if listed_episodes {
        "[muninn:catalog] what is on record: rules that stand and corrections, then decisions, \
         then what was said, each newest first"
    } else {
        "[muninn:catalog] what is on record: rules that stand and corrections, then decisions, \
         each newest first"
    };
    let text = format!(
        "{head}\n{text}{more}Ask for any of them by id: `muninn show <id> [<id> \u{2026}]`.\n"
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
    let mut d = render(&hits, budget, &[]);
    // A pull is where someone goes on to open the evidence: a line whose file is gone says so
    // (WP-21). Looked up here and not in `render`, which the hooks share.
    for h in hits.iter().filter(|h| d.ids.contains(&h.id)) {
        let (plain, checked) = (
            evidence_line(&h.transcript_ref),
            evidence_line_checked(&h.transcript_ref),
        );
        if plain != checked {
            d.text = d.text.replace(&plain, &checked);
        }
    }
    // An agent that asks for five ids and is handed three cannot tell which two it did not
    // get, and the two cases mean opposite things: a retired record was replaced by
    // something, and the catalogue's `replaces #n` is where those ids come from, while an
    // id that is in no row at all was a mistake. Saying which is not a leak — no text of a
    // retired record is rendered, only the fact that it is one, which the catalogue already
    // prints beside its heir.
    //
    // A third case is an active record the block left out. It is neither, and calling it retired
    // tells the agent a live decision is gone.
    let missing: Vec<i64> = ids.iter().copied().filter(|i| !d.ids.contains(i)).collect();
    if !missing.is_empty() {
        let over_budget: Vec<i64> = hits
            .iter()
            .map(|h| h.id)
            .filter(|i| missing.contains(i))
            .collect();
        let places = std::iter::repeat_n("?", missing.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut q = db.conn.prepare(&format!(
            "SELECT id FROM record WHERE invalid = 1 AND id IN ({places})"
        ))?;
        let params: Vec<&dyn rusqlite::ToSql> =
            missing.iter().map(|i| i as &dyn rusqlite::ToSql).collect();
        let retired: Vec<i64> = q
            .query_map(params.as_slice(), |r| r.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        let list = |v: &[i64]| {
            v.iter()
                .map(|i| format!("#{i}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        let unknown: Vec<i64> = missing
            .iter()
            .copied()
            .filter(|i| !retired.contains(i) && !over_budget.contains(i))
            .collect();
        let mut note = String::new();
        if !over_budget.is_empty() {
            note.push_str(&format!(
                "{} on record, not shown in this block: ask for it on its own. ",
                list(&over_budget)
            ));
        }
        // of the retired, the ones an agent's prompt retired while it read as the user's and
        // nothing gave back; a store an older binary wrote has no cause column, and none of these
        let unrestored: Vec<i64> = db
            .conn
            .prepare(&format!(
                "SELECT id FROM record WHERE invalid = 1 AND invalidated_via = '{}' AND id IN ({places})",
                crate::db::AGENT_UNRESTORED
            ))
            .and_then(|mut q| {
                q.query_map(params.as_slice(), |r| r.get(0))
                    .map(|rows| rows.filter_map(|r| r.ok()).collect())
            })
            .unwrap_or_default();
        let retired: Vec<i64> = retired
            .into_iter()
            .filter(|i| !unrestored.contains(i))
            .collect();
        if !retired.is_empty() {
            note.push_str(&format!(
                "{} retired: replaced or revoked, and not served. ",
                list(&retired)
            ));
        }
        if !unrestored.is_empty() {
            note.push_str(&format!(
                "{} retired by an agent's prompt that read as the user's, and not given back because the store changed before that was known: not served; say it again only if it still holds. ",
                list(&unrestored)
            ));
        }
        if !unknown.is_empty() {
            note.push_str(&format!(
                "{} is not a record in this store. ",
                list(&unknown)
            ));
        }
        d.text.push_str(note.trim_end());
        d.text.push('\n');
        d.tokens = d.text.len() / 3;
    }
    Ok(d)
}

pub fn deliver(db: &Db, prompt: &str, exclude: &HashSet<i64>) -> Result<Delivery> {
    let terms = select_terms(db, prompt, 8)?;
    let mut hits = recall(db, &terms, 8, exclude)?;
    // F1: an unresolved conflict is served as two marked records, never ranked away.
    // The render-matched control arm of the experiment [X1] keeps the layout and
    // switches this marking off together with invalidation.
    Hit::mark_conflicts(db, &mut hits);
    Ok(render(&hits, BUDGET_TURN_TOKENS, &terms))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalogue's two queries are written so that SQLite cannot use `record.kind` as an
    /// index constraint, because when it does it abandons the index that answers the ORDER BY
    /// and sorts the whole active set — 23 ms of a 10 ms hook at the schema's cap. Nothing in
    /// the SQL says so; the guard is a unary `+` that reads like a typo. This asserts the plan
    /// instead, so removing it fails here rather than in a latency contract months later.
    /// The floor asks "half as good as the best match", and the best match is the query's,
    /// not whatever survived exclusion. Ask the same question twice and the first answer is
    /// excluded from the second; if the reference moves with it, a set of records that are
    /// all weak together all pass, and the block fills up again.
    #[test]
    fn a_repeat_question_does_not_lower_the_bar_to_fill_the_block() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        let ins = |id: i64, object: &str, body: &str| {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,'decision',?2,'is',?3,?4,'tool_observed',1,'s',?2,1)",
                    rusqlite::params![id, format!("k{id}"), object, body],
                )
                .unwrap();
        };
        // one record answers the question; the rest share a single word with it
        ins(
            1,
            "pgbouncer",
            "the connection pooler is pgbouncer, pooler of connections",
        );
        for i in 2..8 {
            ins(
                i,
                "other",
                "an unrelated decision that happens to mention connection once",
            );
        }
        let terms = vec!["pooler".to_string(), "connection".to_string()];
        let first = recall(&db, &terms, 6, &HashSet::new()).unwrap();
        assert_eq!(first.first().map(|h| h.id), Some(1));
        let served: HashSet<i64> = first.iter().map(|h| h.id).collect();
        let second = recall(&db, &terms, 6, &served).unwrap();
        assert!(
            second.is_empty(),
            "everything left is weaker than half the best match: {:?}",
            second.iter().map(|h| (h.id, h.score)).collect::<Vec<_>>()
        );
    }

    /// A dead end is retired as `superseded` when its command passes later, or when a later
    /// failure of the same command takes its place. Neither says the turn's statement was
    /// replaced: the episode of that turn, the person's message and the reply, is still served.
    /// A statement of the turn retired the same way still hides it.
    #[test]
    fn a_retired_dead_end_does_not_hide_the_episode_of_its_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        let ins = |id: i64, kind: &str, text: &str, tref: &str| {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at,transcript_ref) \
                     VALUES(?1,?2,?3,'is',?4,?4,'tool_observed',1,'s',?3,1,?5)",
                    rusqlite::params![id, kind, format!("k{id}"), text, tref],
                )
                .unwrap();
        };
        ins(
            1,
            "episode",
            "user: the tide tables come from the almanac, check the parser",
            "t:1",
        );
        ins(2, "deadend", "cargo test tides failed", "t:1");
        ins(
            3,
            "episode",
            "user: the ferry timetable comes from the almanac, check the parser",
            "t:2",
        );
        ins(4, "deadend", "cargo test ferry failed", "t:2");
        ins(5, "deadend", "cargo test ferry failed again", "t:3");
        ins(
            6,
            "episode",
            "user: the signal lamp codes come from the almanac, check the parser",
            "t:4",
        );
        ins(
            7,
            "decision",
            "the signal lamp codes come from the almanac",
            "t:4",
        );
        db.conn
            .execute_batch(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_via='succeeded' WHERE id=2; \
                 UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_via='key', invalidated_by=5 WHERE id=4; \
                 UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_via='said', invalidated_by=1 WHERE id=7;",
            )
            .unwrap();
        let terms = vec!["almanac".to_string(), "parser".to_string()];
        let ids: HashSet<i64> = recall(&db, &terms, 8, &HashSet::new())
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        assert!(ids.contains(&1), "its command passed later: {ids:?}");
        assert!(ids.contains(&3), "a later failure replaced it: {ids:?}");
        assert!(!ids.contains(&6), "its statement was replaced: {ids:?}");
    }

    /// An excluded stale twin is as absent as a served one: it does not become the floor's
    /// reference, so a record the floor keeps without it is still kept.
    #[test]
    fn an_excluded_stale_twin_does_not_set_the_floor() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        let ins = |id: i64, kind: &str, text: &str, tref: Option<&str>| {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at,transcript_ref) \
                     VALUES(?1,?2,?3,'is',?4,?4,'tool_observed',1,'s',?3,1,?5)",
                    rusqlite::params![id, kind, format!("k{id}"), text, tref],
                )
                .unwrap();
        };
        ins(1, "decision", "the pooler is pgpool", Some("t:1"));
        ins(
            2,
            "episode",
            "pooler pooler pooler pooler connection connection pooler",
            Some("t:1"),
        );
        ins(
            3,
            "decision",
            "the pooler is pgbouncer, set by hand on every node of the cluster after a long review of the old setup and its failures, written down in the notes of the team that runs the database and the services around it every week of the year",
            None,
        );
        ins(
            4,
            "decision",
            "the pooler is pgcat, set by hand on every node of the cluster after a long review of the old setup and its failures, written down in the notes of the team that runs the database and the services around it every week of the year",
            None,
        );
        for i in 5..12 {
            ins(
                i,
                "decision",
                &format!("filler record number {i} about nothing"),
                None,
            );
        }
        db.conn
            .execute(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=3 WHERE id=1",
                [],
            )
            .unwrap();
        let terms = vec!["pooler".to_string(), "connection".to_string()];
        let ids = |ex: &[i64]| -> Vec<i64> {
            let ex: HashSet<i64> = ex.iter().copied().collect();
            recall(&db, &terms, 8, &ex)
                .unwrap()
                .iter()
                .map(|h| h.id)
                .collect()
        };
        let fresh = ids(&[]);
        assert_eq!(fresh, ids(&[2]), "excluding the twin changes nothing");
        assert!(fresh.contains(&4), "{fresh:?}");
    }

    fn insert_at(db: &Db, id: i64, kind: &str, text: &str, at: i64) {
        db.conn
            .execute(
                "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                 VALUES(?1,?2,?3,'is',?4,?4,'tool_observed',1,'s',?3,?5)",
                rusqlite::params![id, kind, format!("k{id}"), text, at],
            )
            .unwrap();
    }

    fn six_terms() -> Vec<String> {
        [
            "pooler", "replica", "failover", "quorum", "lease", "fencing",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    /// R-08: a record that shares one word of a long question is not an answer to it, however
    /// rare the word. Before, the only floor was half the best match, and one rare word in a
    /// short record scores as well as anything.
    #[test]
    fn one_word_of_a_long_question_is_not_enough() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        insert_at(
            &db,
            1,
            "episode",
            "the pooler sends writes to the replica after failover",
            1,
        );
        insert_at(&db, 2, "episode", "lease", 1);
        insert_at(&db, 3, "episode", "fencing", 1);
        insert_at(&db, 4, "episode", "quorum", 1);
        for i in 5..30 {
            insert_at(&db, i, "episode", &format!("filler note {i}"), 0);
        }
        let got: Vec<i64> = recall(&db, &six_terms(), 8, &HashSet::new())
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        assert_eq!(got, vec![1]);
    }

    /// The floor must not make a rule or correction that matches the question lose to a weaker
    /// record: one that scores at least as well as a record the floor keeps is kept, however few
    /// of the question's words it holds.
    #[test]
    fn a_correction_as_strong_as_what_is_kept_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        insert_at(
            &db,
            1,
            "episode",
            "the pooler sends writes to the replica after failover",
            3,
        );
        insert_at(
            &db,
            2,
            "correction",
            "fencing: fencing tokens, always fencing",
            1,
        );
        insert_at(
            &db,
            3,
            "episode",
            "a note that named the pooler and the replica among other things",
            2,
        );
        for i in 5..30 {
            insert_at(&db, i, "episode", &format!("filler note {i}"), 0);
        }
        let hits = recall(&db, &six_terms(), 8, &HashSet::new()).unwrap();
        let got: Vec<i64> = hits.iter().map(|h| h.id).collect();
        let score = |id: i64| hits.iter().find(|h| h.id == id).map(|h| h.score);
        assert!(got.contains(&3), "{got:?}");
        assert!(
            score(2) <= score(3),
            "the correction scores better than the kept record"
        );
        assert!(got.contains(&2), "{got:?}");
    }

    /// The newest record the query reached keeps its exemption under the absolute floor too: a
    /// later correction is often worded more briefly than the statement it changes.
    #[test]
    fn a_later_correction_is_served_however_briefly_worded() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        insert_at(
            &db,
            1,
            "decision",
            "the pooler sends writes to the replica after failover with a quorum lease",
            1,
        );
        insert_at(
            &db,
            2,
            "correction",
            "no: the writes wait for fencing, said much later than the first \
             statement and in a longer sentence than it, so it scores below it",
            9,
        );
        for i in 5..30 {
            insert_at(&db, i, "episode", &format!("filler note {i}"), 0);
        }
        let got: Vec<i64> = recall(&db, &six_terms(), 8, &HashSet::new())
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        assert_eq!(got, vec![1, 2]);
    }

    /// Beside the rules and corrections that pass the floors and the newest record the query
    /// reached, a block holds the two best other matches and no more.
    #[test]
    fn a_block_holds_two_matches_beside_its_rules_and_the_newest() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        // every record holds the same three of the six words; the shorter, the better it scores
        let pad = |n: usize| " and".repeat(n);
        insert_at(&db, 1, "episode", "pooler replica failover", 5);
        insert_at(
            &db,
            2,
            "episode",
            &format!("pooler replica failover{}", pad(1)),
            4,
        );
        insert_at(
            &db,
            3,
            "episode",
            &format!("pooler replica failover{}", pad(2)),
            3,
        );
        insert_at(
            &db,
            4,
            "correction",
            &format!("pooler replica failover{}", pad(3)),
            2,
        );
        insert_at(
            &db,
            5,
            "episode",
            &format!("pooler replica failover{}", pad(4)),
            1,
        );
        insert_at(
            &db,
            6,
            "episode",
            &format!("pooler replica failover{}", pad(5)),
            9,
        );
        for i in 10..40 {
            insert_at(&db, i, "episode", &format!("filler note {i}"), 0);
        }
        let mut got: Vec<i64> = recall(&db, &six_terms(), 8, &HashSet::new())
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        got.sort_unstable();
        assert_eq!(got, vec![1, 2, 4, 6]);
    }

    /// R-07: two records that match a question equally well are served newest first.
    #[test]
    fn equal_scores_go_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        for (id, at) in [(1, 30), (2, 10), (3, 20)] {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,'decision',?2,'is','the pooler is set','the pooler is set','user_said',3,'s',?2,?3)",
                    rusqlite::params![id, format!("k{id}"), at],
                )
                .unwrap();
        }
        let got: Vec<i64> = recall(&db, &["pooler".to_string()], 8, &HashSet::new())
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        // two of the three: a block holds two matches beside its rules and the newest
        assert_eq!(got, vec![1, 3]);
    }

    /// R-05: a statement replaced by a newer one is not served through the episode of the turn
    /// that said it. A twin whose statement left for another reason (the invariant cap) still
    /// is, and so is the newer statement.
    #[test]
    fn a_replaced_statement_is_not_served_through_its_episode() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        let ins = |id: i64, kind: &str, text: &str, tref: &str, at: i64| {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at,transcript_ref) \
                     VALUES(?1,?2,?3,'is',?4,?4,'user_said',3,'s',?3,?5,?6)",
                    rusqlite::params![id, kind, format!("k{id}"), text, at, tref],
                )
                .unwrap();
        };
        ins(1, "decision", "the connection pooler is pgpool", "t:1", 1);
        ins(
            2,
            "episode",
            "user: the connection pooler is pgpool",
            "t:1",
            1,
        );
        ins(
            3,
            "decision",
            "the connection pooler is pgbouncer now",
            "t:2",
            2,
        );
        ins(
            4,
            "invariant",
            "the connection pooler must stay pgcat",
            "t:3",
            3,
        );
        ins(
            5,
            "episode",
            "user: the connection pooler must stay pgcat",
            "t:3",
            3,
        );
        db.conn
            .execute_batch(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=3 WHERE id=1; \
                 UPDATE record SET invalid=1, invalid_reason='cap' WHERE id=4;",
            )
            .unwrap();
        let terms = vec!["pooler".to_string(), "connection".to_string()];
        let mut got: Vec<i64> = recall(&db, &terms, 8, &HashSet::new())
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        got.sort_unstable();
        assert_eq!(got, vec![3, 5]);
    }

    /// A word's document frequency is the number of records that hold it, not the sum over the
    /// three columns: a typed record repeats its value in subject, object and body and so counted
    /// three times, and a word in 12 of 60 records was dropped as "in more than a third of the
    /// store" (R-18).
    #[test]
    fn a_word_counts_once_per_record_whatever_the_columns() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        for i in 0..60 {
            let text = if i < 12 {
                format!("kestrel queue {i}")
            } else {
                format!("unrelated note {i}")
            };
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('decision',?1,'is',?1,?1,'user_said',3,'s',?1,1)",
                    rusqlite::params![text],
                )
                .unwrap();
        }
        assert!(
            select_terms(&db, "the kestrel setup", 8)
                .unwrap()
                .contains(&"kestrel".to_string()),
            "12 of 60 records is not a third of the store"
        );
    }

    /// A word the store has never seen is not a reason to match on `the`. The fallback
    /// lifts the document-frequency test when it has taken every candidate; it used to lift
    /// `STOP` with it, and on a store whose records are one word each — `gzip`, `zstd` — a
    /// question about the compression codec came back with four episodes about bcrypt, TTLs
    /// and HTTPS, because `the` was the only word of it the store held.
    #[test]
    fn a_word_the_store_never_saw_is_not_a_reason_to_match_on_the() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        for (i, body) in ["gzip", "zstd", "the bcrypt note", "the ttl note"]
            .iter()
            .enumerate()
        {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('episode',?1,'happened',?2,?2,'tool_observed',1,'s',?1,1)",
                    rusqlite::params![format!("k{i}"), body],
                )
                .unwrap();
        }
        assert!(
            select_terms(&db, "the transport compression codec", 8)
                .unwrap()
                .is_empty(),
            "no content word of that question is in this store"
        );
        // and a question the store can actually answer is unaffected
        assert!(!select_terms(&db, "is the bcrypt note still current", 8)
            .unwrap()
            .is_empty());
    }

    /// The floor is measured against the best match, and the best match repeats the
    /// question's words — which is the record that *stated* the thing, not the one that
    /// later changed it. On the plain head-to-head "the project license" reached "Project
    /// license is GPL-3.0" at full score and "Apache-2.0 is better for enterprise adoption"
    /// far below it, and the block delivered the retired value alone.
    #[test]
    fn the_newest_record_the_query_reached_is_never_cut_as_padding() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        let ins = |id: i64, at: i64, body: &str| {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,'episode',?2,'happened',?3,?3,'tool_observed',1,'s',?2,?4)",
                    rusqlite::params![id, format!("k{id}"), body, at],
                )
                .unwrap();
        };
        // 1 repeats the question's words, the way the record that *stated* the thing does
        ins(
            1,
            100,
            "project license: the project license is gpl-3.0, project license policy",
        );
        ins(
            2,
            200,
            "apache-2.0 is better for enterprise adoption, license-wise",
        );
        for i in 3..8 {
            ins(i, 150, "an unrelated note that mentions the project once");
        }
        let terms = vec!["license".to_string(), "project".to_string()];
        let hits = recall(&db, &terms, 8, &HashSet::new()).unwrap();
        let ids: Vec<i64> = hits.iter().map(|h| h.id).collect();
        assert_eq!(ids.first(), Some(&1), "{ids:?}");
        assert!(
            ids.contains(&2),
            "the later statement survives the floor: {ids:?}"
        );
        assert!(
            !ids.contains(&3),
            "padding does not come back with it: {ids:?}"
        );

        // and it is not re-admitted once it has been delivered
        let served: HashSet<i64> = hits.iter().map(|h| h.id).collect();
        assert!(recall(&db, &terms, 8, &served).unwrap().is_empty());
    }

    /// The boot summary tells the agent to pull entries by id. If it asks for five and is
    /// handed three, the two it did not get mean opposite things — one was replaced, one
    /// never existed — and silence made them look the same.
    #[test]
    fn a_pull_says_which_ids_it_could_not_serve() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        db.conn
            .execute(
                "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                 VALUES(1,'decision','k','is','rustls','user: rustls','user_said',3,'s','h1',1), \
                       (2,'decision','k','is','openssl','user: openssl','user_said',3,'s','h2',1)",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "UPDATE record SET invalid=1, invalid_reason='superseded' WHERE id=2",
                [],
            )
            .unwrap();
        let d = show(&db, &[1, 2, 404], 700).unwrap();
        assert_eq!(d.ids, vec![1]);
        assert!(d.text.contains("rustls"), "{}", d.text);
        assert!(
            !d.text.contains("openssl"),
            "a retired record's text is never served"
        );
        assert!(d.text.contains("#2 retired"), "{}", d.text);
        assert!(d.text.contains("#404 is not a record"), "{}", d.text);
        // nothing to say when every id was served
        assert!(!show(&db, &[1], 700).unwrap().text.contains("retired"));
    }

    #[test]
    fn the_catalogue_reads_an_index_and_never_sorts_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        let plan = |sql: &str| -> String {
            let mut st = db
                .conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap();
            st.query_map([], |r| r.get::<_, String>(3))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect::<Vec<_>>()
                .join(" | ")
        };
        let page = plan(
            "SELECT r.id, r.kind, r.object, r.origin = 'commit_linked' \
             FROM served_record r \
             WHERE +r.kind IN ('decision', 'invariant', 'correction') \
               AND r.subject NOT LIKE 'commit:%' \
             ORDER BY r.created_at DESC, r.id DESC LIMIT 121",
        );
        assert!(page.contains("record_recent"), "{page}");
        assert!(!page.contains("TEMP B-TREE"), "{page}");

        let clash_sql = "SELECT EXISTS(SELECT 1 FROM record r JOIN record o \
                             ON o.subject = r.subject AND o.relation = r.relation \
                            AND +o.kind = r.kind \
                           WHERE r.id = 1 AND o.id <> r.id AND o.invalid = 0 \
                             AND o.object <> r.object AND r.kind <> 'episode')";
        let clash = plan(clash_sql);
        assert!(clash.contains("record_active_kind_key"), "{clash}");
        // a store the write path has not opened since keeps the key, never the kind alone
        db.conn
            .execute_batch("DROP INDEX record_active_kind_key")
            .unwrap();
        let old = plan(clash_sql);
        assert!(old.contains("record_active_key"), "{old}");
    }

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
        // This store holds fewer than five typed records, which is the case where the list
        // would otherwise be three lines while the episodes hold everything the questions are
        // about, so what was said is listed after them.
        assert!(
            c.text.contains("some conversation"),
            "a catalogue with almost nothing typed lists what was said:\n{}",
            c.text
        );
        assert!(
            c.text.contains("then what was said") && c.text.contains("newest first"),
            "and says so, and that the order is time order:\n{}",
            c.text
        );

        // A line that fits is shown whole; one that does not is cut at a clause that says
        // something rather than at a character count, and never at one that says nothing.
        let tmp3 = tempfile::tempdir().unwrap();
        let db3 = Db::open(&tmp3.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        for (i, obj) in [
            "Going with LRU with a 300-second TTL, easier to reason about",
            "Semver is better, our API compatibility matters way more than publication dates",
            "Actually, let us switch to zstd because the ratios are better and it decompresses faster",
        ]
        .iter()
        .enumerate()
        {
            db3.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('decision',?1,'is',?2,?2,'user_said',3,'s',?1,?3)",
                    rusqlite::params![format!("said:state:s{i}"), obj, i as i64],
                )
                .unwrap();
        }
        let c5 = catalog(&db3, 300).unwrap();
        assert!(
            c5.text
                .contains("Going with LRU with a 300-second TTL, easier to reason about"),
            "a line inside the width is shown whole:\n{}",
            c5.text
        );
        assert!(
            c5.text.contains("Semver is better\n") || c5.text.contains("Semver is better ·"),
            "a line over it is cut at the clause that carries the value:\n{}",
            c5.text
        );
        assert!(
            !c5.text.contains("· Actually\n") && !c5.text.contains("· Actually ·"),
            "and never at one that carries none:\n{}",
            c5.text
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
        // A retired id yields no text of its own. It does yield the fact that it is retired:
        // the catalogue prints those ids beside their heir (`replaces #n`), so an agent that
        // follows one has asked a fair question, and silence answered it the same way a typo
        // would. F1 is about the record's text, and none of it is here.
        let r = show(&db, &[old], 700).unwrap();
        assert!(r.ids.is_empty());
        assert!(
            !r.text.contains("openssl"),
            "a retired record's text is never served:\n{}",
            r.text
        );
        assert!(r.text.contains(&format!("#{old} retired")), "{}", r.text);
    }

    /// What a commit's diff observed (`said:change:…`, `commit_linked`: "this file now reads
    /// this line") is on record as a decision and nobody decided it. The catalogue is the list
    /// of what the person stated, so it leaves the observation out, and then it is not the whole
    /// of what is on record and its last line says so.
    #[test]
    fn the_catalogue_does_not_list_what_a_commit_diff_observed() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        let ins = |subject: &str, origin: &str, object: &str, anchor: Option<&str>, at: i64| {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,anchor_path,session_id,dedup_hash,created_at) \
                     VALUES('decision',?1,'is',?3,?3,?2,2,?4,'s',?1,?5)",
                    rusqlite::params![subject, origin, object, anchor, at],
                )
                .unwrap();
            db.conn.last_insert_rowid()
        };
        ins(
            "said:state:hashing",
            "user_said",
            "We hash with blake3",
            None,
            1,
        );
        let whole = catalog(&db, 300).unwrap();
        assert!(whole.text.contains("That is all of it"), "{}", whole.text);
        let observed = ins(
            "said:change:test runner",
            "commit_linked",
            "\"vitest\": \"2.1.0\"",
            Some("package.json"),
            2,
        );
        let c = catalog(&db, 300).unwrap();
        assert!(c.text.contains("We hash with blake3"), "{}", c.text);
        assert!(
            !c.text.contains("vitest") && !c.ids.contains(&observed),
            "an observation of a file is not listed among the decisions:\n{}",
            c.text
        );
        assert!(
            c.text.contains("and more, not listed") && !c.text.contains("That is all of it"),
            "and a list that left one out does not say it is whole:\n{}",
            c.text
        );
        // it is still on record: a question that names it is answered with it
        let d = deliver(&db, "which test runner, vitest?", &HashSet::new()).unwrap();
        assert!(d.ids.contains(&observed), "{}", d.text);
    }

    /// Leaving the observations out changes nothing else about the list: the page it is made
    /// from is the one it read before, the newest rows of the three kinds, and an observation in
    /// it is skipped. Read past them instead, the page reached older rules, and since rules are
    /// listed first they took the lines of the newest decisions: on usage-v4's design session
    /// starts one labelled decision line of 18 went that way.
    #[test]
    fn an_observation_left_out_does_not_give_its_place_in_the_page_to_an_older_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        {
            let tx = db.conn.unchecked_transaction().unwrap();
            let ins = |kind: &str, origin: &str, i: i64| {
                tx.execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,?2,'is',?3,?3,?4,3,'s',?2,?5)",
                    rusqlite::params![kind, format!("said:change:{kind} {origin} {i}"), format!("{kind} {origin} number {i}"), origin, i],
                )
                .unwrap();
            };
            for i in 0..60 {
                ins("invariant", "user_said", i);
            }
            for i in 100..200 {
                ins("decision", "commit_linked", i);
            }
            for i in 200..230 {
                ins("decision", "user_said", i);
            }
            tx.commit().unwrap();
        }
        let c = catalog(&db, 300).unwrap();
        assert!(
            c.text.contains("decision user_said number 229"),
            "the newest decision is listed:\n{}",
            c.text
        );
        assert!(!c.text.contains("commit_linked"), "{}", c.text);
        assert!(c.text.contains("and more, not listed"), "{}", c.text);
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

        // …and it only means it where there is nothing else. One episode in the same store and
        // the sentence is false: the list holds the typed records and the episodes hold the
        // rest, which is the whole of what a store of bare values has.
        db2.conn
            .execute(
                "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                 VALUES('episode','said:seen:z','is','zstd','user: zstd','tool_observed',1,'s','e',2)",
                [],
            )
            .unwrap();
        let c3 = catalog(&db2, 300).unwrap();
        assert!(
            !c3.text.contains("nothing on record"),
            "a store with episodes in it does not say a missing subject has nothing:\n{}",
            c3.text
        );
        assert!(
            c3.text.contains("zstd") && c3.text.contains("what was said"),
            "with one decision on it, what was said is listed and named:\n{}",
            c3.text
        );
        // …and says how to read it. v34's agents found the values in the episodes and refused
        // them as "not decisions"; the sentence has to say that the newest one is the latest
        // word, which is true of any record ordered in time.
        assert!(
            c3.text.contains("word for word") && c3.text.contains("latest word"),
            "it says an episode is what was said and that the newest is the latest word:\n{}",
            c3.text
        );

        // …and a store with a working page of typed records keeps the list that won the code
        // condition, undiluted. Five is the threshold, so four more decisions reach it.
        for i in 0..4 {
            db2.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('decision',?1,'is',?2,?2,'user_said',3,'s',?1,3)",
                    rusqlite::params![format!("said:state:d{i}"), format!("decision {i}")],
                )
                .unwrap();
        }
        let c4 = catalog(&db2, 300).unwrap();
        assert!(
            !c4.text.contains("zstd"),
            "a catalogue with five typed records does not list episodes:\n{}",
            c4.text
        );
        assert!(
            c4.text.contains("as episodes") && c4.text.contains("muninn why"),
            "it names what else is there and how to reach it instead:\n{}",
            c4.text
        );
        assert!(
            c2.text.contains("the only decision"),
            "and the probe row past the page is never rendered as an entry:\n{}",
            c2.text
        );
    }

    /// A clause that only points back at what was said before names nothing: a real
    /// catalogue listed decisions titled with nothing but such a clause.
    #[test]
    fn a_catalogue_line_does_not_stop_at_a_clause_that_points_back() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        for (i, o) in [
            "En lugar de eso, usa un botón flotante para guardar, con el mismo color del encabezado",
            "Going with LRU with a 300-second TTL, it is easier to reason about and to tune later on",
        ]
        .iter()
        .enumerate()
        {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('decision',?1,'is',?2,?2,'user_said',3,'s',?1,1)",
                    rusqlite::params![format!("k{i}"), o],
                )
                .unwrap();
        }
        let c = catalog(&db, 300).unwrap();
        assert!(
            c.text
                .contains("En lugar de eso, usa un botón flotante para guardar"),
            "{}",
            c.text
        );
        assert!(
            c.text.contains("· Going with LRU with a 300-second TTL\n"),
            "{}",
            c.text
        );
    }

    /// A question and an acknowledgement typed as a decision are not what is on record: the
    /// catalogue leaves them out and spends their line on the next record. A one-word value is
    /// still listed.
    #[test]
    fn the_catalogue_leaves_out_questions_and_acknowledgements() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        for (i, o) in [
            "the cache layer is redis",
            "should the cache layer be redis?",
            "ok, yes",
            "zstd",
        ]
        .iter()
        .enumerate()
        {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('decision',?1,'is',?2,?2,'user_said',3,'s',?1,?3)",
                    rusqlite::params![format!("k{i}"), o, i as i64],
                )
                .unwrap();
        }
        let c = catalog(&db, 300).unwrap();
        assert_eq!(c.ids, vec![4, 1], "{}", c.text);
    }

    /// The rules that stand and the corrections are listed before the decisions, even when
    /// every decision is newer: the newest records of a store were mostly requests typed as
    /// decisions, and the rules on record were neither shown nor listed.
    #[test]
    fn the_catalogue_lists_rules_and_corrections_first() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        for (id, kind, object, at) in [
            (1, "invariant", "never push to main without review", 1),
            (2, "correction", "no, the tests run with nextest", 2),
            (3, "decision", "we pool connections with pgbouncer", 3),
            (4, "decision", "the cache layer is redis", 4),
        ] {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,?2,?1,'r',?3,?3,'user_said',3,'s',?1,?4)",
                    rusqlite::params![id, kind, object, at],
                )
                .unwrap();
        }
        let c = catalog(&db, 300).unwrap();
        assert_eq!(c.ids, vec![2, 1, 4, 3]);
        // and its header says so: a reader told "newest first" would take the rule for the
        // latest word
        assert!(
            c.text.starts_with(
                "[muninn:catalog] what is on record: rules that stand and corrections, then \
                 decisions, each newest first\n"
            ),
            "{}",
            c.text
        );
    }

    /// Only decisions are left out for saying nothing, and only when every word is an
    /// acknowledgement: a short rule or correction is the rule, and a decision that names its
    /// value in short words (`Yes, use LFU`) is the value.
    #[test]
    fn the_catalogue_keeps_short_rules_and_short_values() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        for (id, kind, object) in [
            (1, "correction", "use uv"),
            (2, "invariant", "why npm? use pnpm"),
            (3, "decision", "Yes, use LFU"),
            (4, "decision", "ok, yes"),
            (5, "decision", "should the cache be redis?"),
            (6, "decision", "the cache layer is redis"),
        ] {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,?2,?1,'r',?3,?3,'user_said',3,'s',?1,?1)",
                    rusqlite::params![id, kind, object],
                )
                .unwrap();
        }
        let mut ids = catalog(&db, 300).unwrap().ids;
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2, 3, 6]);
    }

    /// A title cut to the line's width ends on a whole word, then the ellipsis.
    #[test]
    fn a_cut_title_ends_on_a_whole_word() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        let o =
            "the release pipeline publishes every artefact to the internal registry before tagging";
        db.conn
            .execute(
                "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                 VALUES('decision','k','is',?1,?1,'user_said',3,'s','k',1)",
                [o],
            )
            .unwrap();
        let c = catalog(&db, 300).unwrap();
        assert!(
            c.text.contains(
                "· the release pipeline publishes every artefact to the internal registry…\n"
            ),
            "{}",
            c.text
        );
    }

    /// R-13: beside the blocks of a session start, the catalogue does not list what those blocks
    /// already show in full, nor that sentence's twin under another kind.
    #[test]
    fn the_catalogue_does_not_list_what_the_blocks_beside_it_show() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        for (id, kind, object, at) in [
            (1, "invariant", "never push to main without review", 3),
            (2, "decision", "never push to main without review", 2),
            (3, "decision", "we pool connections with pgbouncer", 1),
            (4, "decision", "always rebase before merging", 5),
            (5, "invariant", "always rebase before merging", 4),
        ] {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,?2,?1,'r',?3,?3,'user_said',3,'s',?1,?4)",
                    rusqlite::params![id, kind, object, at],
                )
                .unwrap();
        }
        // rules first, so a sentence on record as a rule and a decision is listed as the rule
        assert_eq!(catalog(&db, 300).unwrap().ids, vec![5, 1, 3]);
        // the twin of a shown record is left out when it is older (#2) and when it is newer (#4)
        let c = catalog_beside(&db, 300, &[1, 5]).unwrap();
        assert_eq!(c.ids, vec![3], "{}", c.text);
        assert!(!c.text.contains("push to main"), "{}", c.text);
        assert!(!c.text.contains("rebase"), "{}", c.text);
    }

    #[test]
    fn the_catalogue_lists_a_sentence_once_whatever_its_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        for (kind, subj) in [("decision", "said:change:a"), ("invariant", "siempre a")] {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,?2,'r','siempre usa la rama main',?2,'user_said',3,'s',?2,1)",
                    rusqlite::params![kind, subj],
                )
                .unwrap();
        }
        let c = catalog(&db, 300).unwrap();
        assert_eq!(
            c.text.matches("siempre usa la rama main").count(),
            1,
            "{}",
            c.text
        );
    }

    /// Two active records with the same key and different values are a conflict, and the
    /// renderer has marked them since F1 was built. The catalogue did not, so the one place
    /// an agent is shown everything at once was the one place it could not see that two of
    /// them disagree — and it would have picked one.
    /// A correction keeps 160 characters of a line the decision keeps whole, and the two are
    /// still one sentence.
    #[test]
    fn the_catalogue_lists_a_long_sentence_once() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        let long = "palabra ".repeat(40);
        let long = long.trim();
        let cut = crate::sanitize::truncate_chars(long, 160).to_string();
        for (id, kind, object) in [(1, "decision", long), (2, "correction", cut.as_str())] {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,?2,?1,'r',?3,?3,'user_said',3,'s',?1,1)",
                    rusqlite::params![id, kind, object],
                )
                .unwrap();
        }
        assert_eq!(catalog(&db, 300).unwrap().ids.len(), 1);
    }

    /// The budget cutting a live record off a pull is not the record being retired.
    #[test]
    fn show_does_not_call_a_record_the_budget_cut_retired() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("m.db"), crate::db::Mode::ReadWrite).unwrap();
        let body = "texto ".repeat(200);
        for id in 1..=2i64 {
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,'decision',?1,'r','o',?2,'user_said',3,'s',?1,1)",
                    rusqlite::params![id, body],
                )
                .unwrap();
        }
        let d = show(&db, &[1, 2], 200).unwrap();
        assert_eq!(d.ids.len(), 1);
        assert!(!d.text.contains("retired"), "{}", d.text);
        assert!(d.text.contains("on record, not shown"), "{}", d.text);
    }

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
        assert!(!intent_gate("/model opus"));
        assert!(intent_gate("/design dame un navbar fijo en móvil"));
        assert!(!intent_gate(
            "<task-notification>
<task-id>b1</task-id>"
        ));
        assert!(intent_gate(
            "<pasted_content id=\"9d05\">
Ajusta el script de build, necesito que tarde menos
</pasted_content>"
        ));
        assert!(!intent_gate(
            "<pasted_content id=\"1\">
ok
</pasted_content>"
        ));
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
