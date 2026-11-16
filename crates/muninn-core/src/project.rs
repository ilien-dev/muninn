//! Portable projection (ENGINE.md §9, docs/format.md): every record as Markdown with
//! frontmatter under `.muninn/records/<kind>/<id>.md`, an `index.md` under the native
//! cap, JSONL export, and import from JSONL or Markdown. SQLite stays the operational
//! truth; these files are the truth a human or another tool can read.

use crate::caps::{INDEX_MAX_BYTES, INDEX_MAX_LINES};
use crate::db::{now_ms, Db};
use crate::paths::ProjectPaths;
use crate::Result;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordRow {
    pub id: i64,
    pub kind: String,
    pub subject: String,
    pub relation: String,
    pub object: String,
    pub body: String,
    pub origin: String,
    pub trust: i64,
    pub anchor_path: Option<String>,
    pub anchor_hash: Option<String>,
    pub session_id: String,
    pub transcript_ref: Option<String>,
    pub created_at: i64,
    pub invalid: bool,
    pub invalidated_by: Option<i64>,
    pub invalid_reason: Option<String>,
    /// When it was retired and by what (schema v3); `None` on an active row and on a
    /// retirement made before v3.
    #[serde(default)]
    pub invalidated_at: Option<i64>,
    #[serde(default)]
    pub invalidated_via: Option<String>,
    /// The earlier turns a restatement moved this record away from (`record_turn`), so that a
    /// store rebuilt from an export or the mirror still sweeps them when the record goes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub earlier_turns: Vec<String>,
}

const COLS: &str = "id, kind, subject, relation, object, body, origin, trust, anchor_path, anchor_hash, session_id, transcript_ref, created_at, invalid, invalidated_by, invalid_reason, invalidated_at, invalidated_via";
/// The same shape from a v2 store, which has no `invalidated_at` / `invalidated_via` yet.
const COLS_V2: &str = "id, kind, subject, relation, object, body, origin, trust, anchor_path, anchor_hash, session_id, transcript_ref, created_at, invalid, invalidated_by, invalid_reason, NULL AS invalidated_at, NULL AS invalidated_via";
/// The same shape read from `served_record`, which has no invalidation columns: a row that
/// comes back from there is active by construction, so the three are constants.
const COLS_SERVED: &str = "id, kind, subject, relation, object, body, origin, trust, anchor_path, anchor_hash, session_id, transcript_ref, created_at, 0 AS invalid, NULL AS invalidated_by, NULL AS invalid_reason, NULL AS invalidated_at, NULL AS invalidated_via";

fn row(r: &rusqlite::Row) -> rusqlite::Result<RecordRow> {
    Ok(RecordRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        subject: r.get(2)?,
        relation: r.get(3)?,
        object: r.get(4)?,
        body: r.get(5)?,
        origin: r.get(6)?,
        trust: r.get(7)?,
        anchor_path: r.get(8)?,
        anchor_hash: r.get(9)?,
        session_id: r.get(10)?,
        transcript_ref: r.get(11)?,
        created_at: r.get(12)?,
        invalid: r.get::<_, i64>(13)? == 1,
        invalidated_by: r.get(14)?,
        invalid_reason: r.get(15)?,
        invalidated_at: r.get(16)?,
        invalidated_via: r.get(17)?,
        earlier_turns: Vec::new(),
    })
}

fn tail_of(where_sql: &str) -> &'static str {
    if where_sql.to_ascii_uppercase().contains("ORDER BY") {
        ""
    } else {
        " ORDER BY id"
    }
}

/// Every record, retired ones included. For the Markdown mirror, `export --all` and
/// `muninn why`, which show the retired rows with their reason. **Not for a serving path**:
/// that is `load_served`.
pub fn load_all(db: &Db, where_sql: &str) -> Result<Vec<RecordRow>> {
    // A read-only open never migrates, so `why` and `export` can meet a v2 store written by
    // 1.2.0 until the next write path runs; it has neither retirement column yet.
    let cols = if db.schema_version()? < 3 {
        COLS_V2
    } else {
        COLS
    };
    let sql = format!(
        "SELECT {cols} FROM record WHERE {where_sql}{}",
        tail_of(where_sql)
    );
    let mut st = db.conn.prepare(&sql)?;
    let mut rows: Vec<RecordRow> = st.query_map([], row)?.filter_map(|r| r.ok()).collect();
    if cols == COLS {
        // a small table: only restatements write it
        let mut turns: std::collections::HashMap<i64, Vec<String>> =
            std::collections::HashMap::new();
        let mut t = db.conn.prepare(
            "SELECT record_id, transcript_ref FROM record_turn ORDER BY record_id, transcript_ref",
        )?;
        for (id, tref) in t
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .flatten()
        {
            turns.entry(id).or_default().push(tref);
        }
        for r in &mut rows {
            if let Some(v) = turns.remove(&r.id) {
                r.earlier_turns = v;
            }
        }
    }
    Ok(rows)
}

/// Records that may be served, from the `served_record` view. The three invalidation columns
/// do not exist there, so they are filled with what the view already guarantees.
pub fn load_served(db: &Db, where_sql: &str) -> Result<Vec<RecordRow>> {
    let sql = format!(
        "SELECT {COLS_SERVED} FROM served_record WHERE {where_sql}{}",
        tail_of(where_sql)
    );
    let mut st = db.conn.prepare(&sql)?;
    let rows = st.query_map([], row)?.filter_map(|r| r.ok()).collect();
    Ok(rows)
}

fn yaml_str(s: &str) -> String {
    // double-quoted YAML scalar; enough for one-line values
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn yaml_opt(s: &Option<String>) -> String {
    s.as_deref().map(yaml_str).unwrap_or_else(|| "~".into())
}

pub fn iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
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

/// The Markdown file for one record: frontmatter, then the literal body.
pub fn markdown(r: &RecordRow) -> String {
    let mut s = String::with_capacity(r.body.len() + 512);
    s.push_str("---\n");
    let _ = writeln!(s, "id: {}", r.id);
    let _ = writeln!(s, "kind: {}", r.kind);
    let _ = writeln!(s, "subject: {}", yaml_str(&r.subject));
    let _ = writeln!(s, "relation: {}", yaml_str(&r.relation));
    let _ = writeln!(s, "object: {}", yaml_str(&r.object));
    let _ = writeln!(s, "origin: {}", r.origin);
    let _ = writeln!(s, "trust: {}", r.trust);
    let _ = writeln!(s, "anchor_path: {}", yaml_opt(&r.anchor_path));
    let _ = writeln!(s, "anchor_hash: {}", yaml_opt(&r.anchor_hash));
    let _ = writeln!(s, "session_id: {}", yaml_str(&r.session_id));
    let _ = writeln!(s, "transcript_ref: {}", yaml_opt(&r.transcript_ref));
    let _ = writeln!(s, "created_at: {}", iso(r.created_at));
    let _ = writeln!(s, "invalid: {}", r.invalid);
    let _ = writeln!(
        s,
        "invalidated_by: {}",
        r.invalidated_by
            .map(|i| i.to_string())
            .unwrap_or_else(|| "~".into())
    );
    let _ = writeln!(s, "invalid_reason: {}", yaml_opt(&r.invalid_reason));
    let _ = writeln!(
        s,
        "invalidated_at: {}",
        r.invalidated_at.map(iso).unwrap_or_else(|| "~".into())
    );
    let _ = writeln!(s, "invalidated_via: {}", yaml_opt(&r.invalidated_via));
    if !r.earlier_turns.is_empty() {
        let turns = serde_json::to_string(&r.earlier_turns).unwrap_or_default();
        let _ = writeln!(s, "earlier_turns: {}", yaml_str(&turns));
    }
    s.push_str("---\n");
    s.push_str(&r.body);
    if !r.body.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// The `meta` key that dates the last projection, in the milliseconds a retirement is dated in.
const PROJECTED_AT: &str = "projected_at_ms";
/// How far before the last projection a projection of some records still looks for
/// retirements: a writer whose transaction was open then dated its retirement before it.
const RETIRED_SLACK_MS: i64 = 60_000;

/// Bring the Markdown files for `ids` up to date (all records when `ids` is empty) and rebuild
/// `index.md`. Returns the number of files written.
pub fn project(paths: &ProjectPaths, db: &Db, ids: &[i64]) -> Result<usize> {
    // Experiment cells set this: the Markdown mirror would let an agent read the store
    // (retired flags included) around the hooks, which is what the arms must isolate.
    if std::env::var_os("MUNINN_NO_PROJECT").is_some() {
        return Ok(0);
    }
    let root = paths.records_dir();
    let mut n = 0;
    // A file is written when what is on disk is not what the record says, and a file that is
    // not there is written only when `missing` says so. `maintain` used to rewrite every file
    // of the store on every run: 4,214 files on a copy of one store, with nothing new (WP-11).
    let mut bring_up_to_date = |r: &RecordRow, missing: bool| -> Result<()> {
        let dir = root.join(&r.kind);
        let p = dir.join(format!("{}.md", r.id));
        let want = markdown(r);
        match std::fs::read_to_string(&p) {
            Ok(have) if have == want => return Ok(()),
            Err(_) if !missing => return Ok(()),
            _ => {}
        }
        std::fs::create_dir_all(&dir).map_err(|e| crate::Error::io(&dir, e))?;
        std::fs::write(&p, want).map_err(|e| crate::Error::io(&p, e))?;
        n += 1;
        Ok(())
    };
    // when the projection before this one ran, by the clock that dates a retirement
    let last: Option<i64> = db.meta_get(PROJECTED_AT)?.and_then(|s| s.parse().ok());
    let now: i64 = db.conn.query_row(
        "SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)",
        [],
        |r| r.get(0),
    )?;
    if ids.is_empty() {
        for r in &load_all(db, "1=1")? {
            bring_up_to_date(r, true)?;
        }
    } else {
        let list = ids
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        for r in &load_all(db, &format!("id IN ({list})"))? {
            bring_up_to_date(r, true)?;
        }
        // A record retired by this same ingest is not in `ids` — the caller passes what it
        // *wrote* — so its file kept saying `invalid: false` for as long as the store existed.
        // On disk that is a retired decision presented as current, in the one place
        // `docs/claims.md` discloses an agent could read around the hooks, and the disclosure
        // says retired records are "labelled as such" there. They were not.
        //
        // Bounded by what was retired since the projection before this one, with a minute to
        // spare for a writer that was mid-transaction then: the file of every retired record
        // the store ever held was read here on every ingest, 524 of them on one store. A
        // retirement that carries no date is older than the column, and its file was brought
        // up to date when it happened or is the whole projection's to mend.
        let retired = match last {
            Some(t) if db.schema_version()? >= 3 => {
                format!("invalid = 1 AND invalidated_at >= {}", t - RETIRED_SLACK_MS)
            }
            _ => "invalid = 1".to_string(),
        };
        for r in &load_all(db, &retired)? {
            bring_up_to_date(r, false)?;
        }
    }
    // a read-only caller cannot leave the date, and then the next projection reads further back
    let _ = db.meta_set(PROJECTED_AT, &now.to_string());
    write_index(paths, db)?;
    Ok(n)
}

/// `index.md`: active invariants first, then the newest decisions, dead ends and
/// corrections, one line each, under the native cap (200 lines / 25 KB) [O5].
pub fn write_index(paths: &ProjectPaths, db: &Db) -> Result<String> {
    if std::env::var_os("MUNINN_NO_PROJECT").is_some() {
        return Ok(String::new());
    }
    let mut s = String::new();
    s.push_str("# Muninn index\n\n");
    let active: i64 = db.count("SELECT count(*) FROM record WHERE invalid=0")?;
    let total: i64 = db.count("SELECT count(*) FROM record")?;
    let _ = writeln!(
        s,
        "{active} active of {total} records · regenerated {} · `muninn why <id>` for lineage\n",
        iso(now_ms())
    );
    let mut lines = 0usize;
    for (kind, cap) in [
        ("invariant", 60usize),
        ("decision", 60),
        ("deadend", 40),
        ("correction", 30),
        ("claim", 10),
    ] {
        // The newest `cap` of a kind, read whichever way reads fewer rows. Through the kind's
        // own index every row of the kind is read and sorted: 9 to 11 ms a kind on a store of
        // 20,000 records, three kinds, on every ingest. Down the newest-first index the scan
        // stops after `cap` rows of the kind, about `cap` times the store over the kind's
        // count; that is fewer when the kind is common and far more when it is rare.
        let of_kind: i64 = db.count(&format!(
            "SELECT count(*) FROM record WHERE invalid=0 AND kind='{kind}'"
        ))?;
        let common = of_kind.saturating_mul(of_kind) > (cap as i64).saturating_mul(active);
        let rows = load_served(
            db,
            &format!(
                "{}kind='{kind}' ORDER BY created_at DESC, id DESC LIMIT {cap}",
                // a unary plus keeps the planner off the kind's index
                if common { "+" } else { "" }
            ),
        )?;
        if rows.is_empty() {
            continue;
        }
        let _ = writeln!(s, "## {kind}\n");
        for r in rows {
            if lines >= INDEX_MAX_LINES - 12 || s.len() > INDEX_MAX_BYTES - 400 {
                break;
            }
            let obj: String = r.object.chars().take(100).collect();
            let _ = writeln!(
                s,
                "- #{} `{}` {} — {} · {} · trust {}",
                r.id,
                r.subject.chars().take(48).collect::<String>(),
                r.relation,
                obj,
                r.origin,
                r.trust
            );
            lines += 1;
        }
        s.push('\n');
    }
    let ep: i64 = db.count("SELECT count(*) FROM record WHERE invalid=0 AND kind='episode'")?;
    let _ = writeln!(
        s,
        "## episode\n\n{ep} literal episodes (not listed; searched by the read path)."
    );
    let p = paths.index_md();
    std::fs::write(&p, &s).map_err(|e| crate::Error::io(&p, e))?;
    Ok(s)
}

/// JSONL export: one object per record, the frontmatter fields plus `body`.
pub fn export_jsonl(db: &Db, out: &Path, include_invalid: bool) -> Result<usize> {
    let rows = load_all(db, if include_invalid { "1=1" } else { "invalid=0" })?;
    let mut s = String::new();
    for r in &rows {
        s.push_str(&serde_json::to_string(r).map_err(|e| crate::Error::Other(e.to_string()))?);
        s.push('\n');
    }
    std::fs::write(out, s).map_err(|e| crate::Error::io(out, e))?;
    Ok(rows.len())
}

/// A record as it arrives from outside: any field may be missing; origin defaults to
/// `imported` (trust 0) unless the file carries its own provenance.
#[derive(Debug, Default, Deserialize)]
pub struct Incoming {
    /// The id the record had where it was exported; used only to carry `invalidated_by`
    /// across, since ids change on import.
    pub id: Option<i64>,
    pub kind: Option<String>,
    pub subject: Option<String>,
    pub relation: Option<String>,
    pub object: Option<String>,
    pub body: Option<String>,
    pub origin: Option<String>,
    pub anchor_path: Option<String>,
    pub anchor_hash: Option<String>,
    pub session_id: Option<String>,
    pub transcript_ref: Option<String>,
    pub created_at: Option<serde_json::Value>,
    pub invalid: Option<bool>,
    pub invalid_reason: Option<String>,
    pub invalidated_by: Option<i64>,
    pub invalidated_at: Option<serde_json::Value>,
    pub invalidated_via: Option<String>,
    /// Absent from files written before `record_turn` existed.
    #[serde(default)]
    pub earlier_turns: Option<Vec<String>>,
    /// Explicit trigger conditions: `[{"kind":"keyword","key":"breakfast","grp":0}, ...]`
    #[serde(default)]
    pub cues: Option<Vec<IncomingCue>>,
}

#[derive(Debug, Deserialize)]
pub struct IncomingCue {
    pub kind: String,
    pub key: String,
    #[serde(default)]
    pub grp: i64,
}

fn parse_frontmatter(text: &str) -> (Incoming, String) {
    let mut inc = Incoming::default();
    let Some(rest) = text.strip_prefix("---\n") else {
        return (inc, text.to_string());
    };
    let Some(end) = rest.find("\n---\n") else {
        return (inc, text.to_string());
    };
    let fm = &rest[..end];
    let body = rest[end + 5..].to_string();
    for line in fm.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        let v = if v == "~" || v.is_empty() {
            None
        } else if v.starts_with('"') && v.ends_with('"') && v.len() >= 2 {
            Some(
                v[1..v.len() - 1]
                    .replace("\\\"", "\"")
                    .replace("\\n", "\n")
                    .replace("\\\\", "\\"),
            )
        } else {
            Some(v.to_string())
        };
        match k.trim() {
            "kind" => inc.kind = v,
            "subject" => inc.subject = v,
            "relation" => inc.relation = v,
            "object" => inc.object = v,
            "origin" => inc.origin = v,
            "anchor_path" => inc.anchor_path = v,
            "anchor_hash" => inc.anchor_hash = v,
            "session_id" => inc.session_id = v,
            "transcript_ref" => inc.transcript_ref = v,
            "created_at" => inc.created_at = v.map(serde_json::Value::String),
            "invalid" => inc.invalid = v.map(|x| x == "true"),
            "invalid_reason" => inc.invalid_reason = v,
            "id" => inc.id = v.and_then(|x| x.parse().ok()),
            "invalidated_by" => inc.invalidated_by = v.and_then(|x| x.parse().ok()),
            "invalidated_at" => inc.invalidated_at = v.map(serde_json::Value::String),
            "invalidated_via" => inc.invalidated_via = v,
            "earlier_turns" => inc.earlier_turns = v.and_then(|x| serde_json::from_str(&x).ok()),
            _ => {}
        }
    }
    (inc, body)
}

fn iso_to_ms(s: &str) -> Option<i64> {
    let (date, rest) = s.split_once('T')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let m: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let time = rest.trim_end_matches('Z');
    let mut t = time.split(':');
    let hh: i64 = t.next()?.parse().ok()?;
    let mm: i64 = t.next()?.parse().ok()?;
    let ss: f64 = t.next()?.split(['+', '-']).next()?.parse().ok()?;
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400_000 + hh * 3_600_000 + mm * 60_000 + (ss * 1000.0) as i64)
}

pub struct ImportStats {
    pub read: usize,
    pub inserted: usize,
    pub duplicates: usize,
    pub rejected: usize,
}

/// What one import carries from the first pass to the second: the old id of every row it
/// placed, mapped to the row that now holds it, and the heir links to restore.
#[derive(Default)]
struct Lineage {
    ids: std::collections::HashMap<i64, i64>,
    links: Vec<(i64, i64)>,
}

fn insert_incoming(
    db: &Db,
    mut inc: Incoming,
    body_fallback: String,
    st: &mut ImportStats,
    trust_origin: bool,
    lineage: &mut Lineage,
) -> Result<()> {
    st.read += 1;
    let explicit_cues = inc.cues.take();
    let body = inc.body.unwrap_or(body_fallback);
    let body = crate::sanitize::clean_text(&body);
    let body: String = body.chars().take(crate::caps::MAX_BODY_CHARS).collect();
    let kind = inc.kind.unwrap_or_else(|| "claim".into());
    if ![
        "invariant",
        "decision",
        "deadend",
        "correction",
        "claim",
        "episode",
    ]
    .contains(&kind.as_str())
    {
        st.rejected += 1;
        return Ok(());
    }
    let stated = inc.origin.filter(|o| {
        [
            "user_said",
            "review_accepted",
            "commit_linked",
            "tool_observed",
            "agent_inferred",
            "imported",
        ]
        .contains(&o.as_str())
    });
    // Trust derives from origin, and a file's `origin` is only the file's claim (WP-15). What
    // arrives from outside is `imported`, trust 0, as `docs/threat-model.md` says; the claim is
    // kept beside it. Only a file the user vouches for as their own export keeps it.
    let origin = match (&stated, trust_origin) {
        (Some(o), true) => o.clone(),
        _ => "imported".into(),
    };
    let trust = match origin.as_str() {
        "user_said" | "review_accepted" => 3,
        "commit_linked" => 2,
        "tool_observed" => 1,
        _ => 0,
    };
    let object = inc.object.unwrap_or_else(|| {
        body.lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(160)
            .collect()
    });
    let subject = inc
        .subject
        .unwrap_or_else(|| format!("imported:{}", &blake3::hash(body.as_bytes()).to_hex()[..12]));
    let relation = inc.relation.unwrap_or_else(|| "is".into());
    if body.trim().is_empty() && object.trim().is_empty() {
        st.rejected += 1;
        return Ok(());
    }
    // R-17: every newest-first path trusts this column. A date in a finer unit is read as the
    // millisecond it names; one that no unit makes sane is refused, never taken as given.
    let now = now_ms();
    let ms_of = |v: &Option<serde_json::Value>| match v {
        Some(serde_json::Value::Number(n)) => n.as_i64(),
        Some(serde_json::Value::String(s)) => iso_to_ms(s),
        _ => None,
    };
    let created = match &inc.created_at {
        None => now,
        v => match ms_of(v).and_then(|ms| crate::db::sane_created_at(ms, now)) {
            Some(ms) => ms,
            None => {
                st.rejected += 1;
                return Ok(());
            }
        },
    };
    let invalid = inc.invalid.unwrap_or(false);
    let invalidated_at = ms_of(&inc.invalidated_at)
        .and_then(|ms| crate::db::sane_created_at(ms, now))
        .filter(|_| invalid);
    let hash = blake3::hash(format!("{kind}|{subject}|{relation}|{object}|{body}").as_bytes())
        .to_hex()
        .to_string();
    let n = db.conn.execute(
        "INSERT OR IGNORE INTO record(kind, subject, relation, object, body, origin, trust, anchor_path, anchor_hash, session_id, transcript_ref, dedup_hash, created_at, invalid, invalid_reason, invalidated_at, invalidated_via) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        rusqlite::params![
            kind,
            subject,
            relation,
            object,
            body,
            origin,
            trust,
            inc.anchor_path,
            inc.anchor_hash,
            inc.session_id.unwrap_or_else(|| "imported".into()),
            inc.transcript_ref,
            hash,
            created,
            invalid as i64,
            inc.invalid_reason.filter(|r| ["superseded", "revoked", "anchor_changed", "reverted", "user", "cap"].contains(&r.as_str())),
            invalidated_at,
            inc.invalidated_via.filter(|_| invalid),
        ],
    )?;
    let id: Option<i64> = if n == 1 {
        st.inserted += 1;
        let id = db.conn.last_insert_rowid();
        if let Some(cues) = &explicit_cues {
            let rows: Vec<(String, String, i64)> = cues
                .iter()
                .map(|c| (c.kind.clone(), c.key.clone(), c.grp))
                .collect();
            let _ = crate::cue::set_explicit(db, id, &rows);
        }
        if let Some(o) = stated.filter(|o| *o != origin) {
            db.meta_set(&format!("imported_origin:{id}"), &o)?;
        }
        Some(id)
    } else {
        st.duplicates += 1;
        db.conn
            .query_row(
                "SELECT id FROM record WHERE dedup_hash = ?1",
                [&hash],
                |r| r.get(0),
            )
            .ok()
    };
    if let Some(new) = id {
        for tref in inc.earlier_turns.iter().flatten() {
            db.conn.execute(
                "INSERT OR IGNORE INTO record_turn(record_id, transcript_ref) VALUES (?1, ?2)",
                rusqlite::params![new, tref],
            )?;
        }
    }
    if let (Some(old), Some(new)) = (inc.id, id) {
        lineage.ids.insert(old, new);
        if let Some(heir) = inc.invalidated_by.filter(|_| invalid) {
            lineage.links.push((old, heir));
        }
    }
    Ok(())
}

/// Import from a JSONL file or a directory of Markdown files with frontmatter (the
/// harness's native topic files included: a file with no frontmatter is one `claim`).
/// Every record arrives as `imported`, trust 0; see `import_with`.
pub fn import(db: &Db, path: &Path) -> Result<ImportStats> {
    import_with(db, path, false)
}

/// `import`, and with `trust_origin` the file is the user's own export: each record keeps the
/// origin, and so the trust, it states. Either way a retired record comes back pointing at the
/// record that replaced it, when that record is in the same file (ids change on import, so
/// they are mapped in a second pass; WP-15).
pub fn import_with(db: &Db, path: &Path, trust_origin: bool) -> Result<ImportStats> {
    let mut st = ImportStats {
        read: 0,
        inserted: 0,
        duplicates: 0,
        rejected: 0,
    };
    let mut lineage = Lineage::default();
    let tx = db.write_tx()?;
    if path.is_dir() {
        let mut files: Vec<_> = walk_md(path);
        files.sort();
        for f in files {
            let text = std::fs::read_to_string(&f).map_err(|e| crate::Error::io(&f, e))?;
            let (inc, body) = parse_frontmatter(&text);
            insert_incoming(db, inc, body, &mut st, trust_origin, &mut lineage)?;
        }
    } else {
        let text = std::fs::read_to_string(path).map_err(|e| crate::Error::io(path, e))?;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            match serde_json::from_str::<Incoming>(line) {
                Ok(inc) => {
                    insert_incoming(db, inc, String::new(), &mut st, trust_origin, &mut lineage)?
                }
                Err(_) => st.rejected += 1,
            }
        }
    }
    for (old, heir) in &lineage.links {
        if let (Some(row), Some(heir)) = (lineage.ids.get(old), lineage.ids.get(heir)) {
            db.conn.execute(
                "UPDATE record SET invalidated_by = ?2 WHERE id = ?1 AND invalid = 1 AND invalidated_by IS NULL",
                [row, heir],
            )?;
        }
    }
    // after the rows are in: a record this import brought retired never met anything that
    // was done in this store before it, and nothing here gives it back
    crate::db::note_event(&tx, "import")?;
    tx.commit()?;
    let _ = crate::cue::derive_missing(db);
    db.meta_set("records_changed_since_project", "1")?;
    Ok(st)
}

fn walk_md(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk_md(&p));
        } else if p.extension().and_then(|x| x.to_str()) == Some("md")
            && p.file_name().and_then(|x| x.to_str()) != Some("index.md")
        {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_roundtrip() {
        let r = RecordRow {
            id: 7,
            kind: "decision".into(),
            subject: "retry.policy".into(),
            relation: "is".into(),
            object: "exponential \"backoff\"".into(),
            body: "literal body\nsecond line\n".into(),
            origin: "commit_linked".into(),
            trust: 2,
            anchor_path: Some("src/x.rs".into()),
            anchor_hash: None,
            session_id: "abc".into(),
            transcript_ref: None,
            created_at: 1_789_221_791_084,
            invalid: false,
            invalidated_by: None,
            invalid_reason: None,
            invalidated_at: None,
            invalidated_via: None,
            earlier_turns: Vec::new(),
        };
        let md = markdown(&r);
        assert!(md.starts_with("---\nid: 7\nkind: decision\n"));
        assert!(md.contains("created_at: 2026-09-12T14:03:11Z"));
        let (inc, body) = parse_frontmatter(&md);
        assert_eq!(inc.object.as_deref(), Some("exponential \"backoff\""));
        assert_eq!(inc.anchor_path.as_deref(), Some("src/x.rs"));
        assert_eq!(body, "literal body\nsecond line\n");
        assert_eq!(iso_to_ms("2026-09-12T14:03:11Z"), Some(1_789_221_791_000));
    }
}

#[cfg(test)]
mod import_tests {
    use super::*;
    use crate::db::Mode;

    fn fresh(dir: &Path, name: &str) -> Db {
        Db::open(&dir.join(name), Mode::ReadWrite).unwrap()
    }

    /// WP-15. `export --all` then `import`: the retired rows came back without the record
    /// that replaced them (ids change on import, and nothing mapped them), so lineage and the
    /// catalogue's `replaces #n` were lost on every round trip.
    #[test]
    fn a_round_trip_keeps_who_replaced_whom() {
        let tmp = tempfile::tempdir().unwrap();
        let a = fresh(tmp.path(), "a.db");
        // an unrelated row first, so ids differ between the two stores
        for (h, o) in [
            ("h0", "unrelated"),
            ("h1", "we use openssl"),
            ("h2", "we use rustls"),
        ] {
            a.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('decision',?1,'is',?2,?2,'user_said',3,'s',?1,1793000000000)",
                    rusqlite::params![h, o],
                )
                .unwrap();
        }
        a.conn
            .execute(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=3, invalidated_via='said' WHERE id=2",
                [],
            )
            .unwrap();
        let out = tmp.path().join("x.jsonl");
        export_jsonl(&a, &out, true).unwrap();
        let b = fresh(tmp.path(), "b.db");
        b.conn
            .execute(
                "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                 VALUES('decision','z','is','already here','already here','user_said',3,'s','z',1793000000000)",
                [],
            )
            .unwrap();
        let st = import_with(&b, &out, true).unwrap();
        assert_eq!(st.inserted, 3);
        // an import leaves an event dated after the rows it brought
        let newest = b.count("SELECT max(id) FROM record").unwrap();
        assert_eq!(
            b.count("SELECT after_record FROM store_event WHERE what = 'import'")
                .unwrap(),
            newest
        );
        let (via, heir): (String, String) = b
            .conn
            .query_row(
                "SELECT r.invalidated_via, h.object FROM record r JOIN record h ON h.id = r.invalidated_by \
                 WHERE r.object = 'we use openssl'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((via.as_str(), heir.as_str()), ("said", "we use rustls"));
    }

    /// The earlier turns a restatement keeps (`record_turn`) travel with the record, through
    /// the JSONL export and through the Markdown mirror that doctor's restore advice imports
    /// from; a file written before they existed still imports.
    #[test]
    fn a_round_trip_keeps_the_earlier_turns_of_a_restated_record() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::ProjectPaths::from_root(tmp.path());
        for d in paths.all_dirs() {
            std::fs::create_dir_all(d).unwrap();
        }
        let a = Db::open(&paths.db_path(), Mode::ReadWrite).unwrap();
        a.conn
            .execute(
                "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,transcript_ref,dedup_hash,created_at) \
                 VALUES('decision','k','is','we use openssl','we use openssl','user_said',3,'s2','t2.jsonl:9','h1',1793000000000)",
                [],
            )
            .unwrap();
        a.conn
            .execute(
                "INSERT INTO record_turn(record_id, transcript_ref) VALUES (1, 't1.jsonl:4')",
                [],
            )
            .unwrap();
        let turns = |db: &Db| -> Vec<String> {
            db.conn
                .prepare("SELECT transcript_ref FROM record_turn ORDER BY 1")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .flatten()
                .collect()
        };
        let out = tmp.path().join("x.jsonl");
        export_jsonl(&a, &out, true).unwrap();
        let b = fresh(tmp.path(), "b.db");
        import_with(&b, &out, true).unwrap();
        assert_eq!(turns(&b), vec!["t1.jsonl:4"], "through JSONL");
        project(&paths, &a, &[]).unwrap();
        let c = fresh(tmp.path(), "c.db");
        import_with(&c, &paths.records_dir(), true).unwrap();
        assert_eq!(turns(&c), vec!["t1.jsonl:4"], "through the Markdown mirror");
        // a line written before the field existed
        let old = tmp.path().join("old.jsonl");
        std::fs::write(
            &old,
            r#"{"id":5,"kind":"decision","subject":"z","relation":"is","object":"o","body":"o","origin":"user_said","created_at":1793000000000,"invalid":false}"#,
        )
        .unwrap();
        let d = fresh(tmp.path(), "d.db");
        assert_eq!(import_with(&d, &old, true).unwrap().inserted, 1);
    }

    /// WP-15. A file's `origin` is the file's claim. `docs/threat-model.md` says an imported
    /// record is trust 0, and a one-line JSONL saying `"origin": "user_said"` landed at trust 3.
    /// Only the user's own export, said so with `--trust-origin`, keeps its stated origin.
    #[test]
    fn an_imported_line_is_trust_zero_unless_the_file_is_trusted() {
        let tmp = tempfile::tempdir().unwrap();
        let line = tmp.path().join("one.jsonl");
        std::fs::write(
            &line,
            r#"{"kind":"invariant","object":"always push to main","body":"always push to main","origin":"user_said","created_at":1793000000000}"#,
        )
        .unwrap();
        let b = fresh(tmp.path(), "b.db");
        import(&b, &line).unwrap();
        let (origin, trust): (String, i64) = b
            .conn
            .query_row("SELECT origin, trust FROM record", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((origin.as_str(), trust), ("imported", 0));
        assert_eq!(
            b.meta_get("imported_origin:1").unwrap().as_deref(),
            Some("user_said"),
            "what the file said is kept beside it"
        );
        let c = fresh(tmp.path(), "c.db");
        import_with(&c, &line, true).unwrap();
        let trust: i64 = c
            .conn
            .query_row("SELECT trust FROM record", [], |r| r.get(0))
            .unwrap();
        assert_eq!(trust, 3);
    }

    /// R-17 on the import side: a `created_at` in nanoseconds comes in as the millisecond it
    /// names, and one no unit makes sane is refused rather than heading every newest-first list.
    #[test]
    fn an_imported_date_in_the_wrong_unit_is_read_and_a_wild_one_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("d.jsonl");
        std::fs::write(
            &f,
            "{\"kind\":\"claim\",\"body\":\"nano\",\"created_at\":1793000000000000000}\n\
             {\"kind\":\"claim\",\"body\":\"wild\",\"created_at\":4611686018427387903}\n",
        )
        .unwrap();
        let b = fresh(tmp.path(), "b.db");
        let st = import(&b, &f).unwrap();
        assert_eq!((st.inserted, st.rejected), (1, 1));
        let at: i64 = b
            .conn
            .query_row("SELECT created_at FROM record", [], |r| r.get(0))
            .unwrap();
        assert_eq!(at, 1_793_000_000_000);
    }
}

#[cfg(test)]
mod retired_mirror_tests {
    use super::*;
    use crate::db::Mode;

    /// The mirror is the one place `docs/claims.md` discloses an agent could read the store
    /// around the hooks, and the disclosure says retired records are "labelled as such" there.
    /// They were not: `project` writes the ids its caller passed, a record retired by the same
    /// ingest is not among them, and its file kept saying `invalid: false` for as long as the
    /// store existed — a retired decision presented on disk as current.
    #[test]
    fn a_retired_record_is_labelled_retired_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::ProjectPaths::from_root(tmp.path());
        for d in paths.all_dirs() {
            std::fs::create_dir_all(d).unwrap();
        }
        let db = Db::open(&paths.db_path(), Mode::ReadWrite).unwrap();
        for (h, o) in [("h1", "we use openssl"), ("h2", "we use rustls")] {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('decision',?1,'is',?2,?2,'user_said',3,'s',?1,1)",
                    rusqlite::params![h, o],
                )
                .unwrap();
        }
        // written while both are active, which is what the ingest path does
        project(&paths, &db, &[1, 2]).unwrap();
        let file = paths.records_dir().join("decision/1.md");
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .contains("invalid: false"),
            "active to begin with"
        );

        db.conn
            .execute(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=2 WHERE id=1",
                [],
            )
            .unwrap();
        // the next projection names only the record it wrote, as the caller does
        project(&paths, &db, &[2]).unwrap();
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .contains("invalid: true"),
            "and the retired one is relabelled even though nobody asked for it"
        );
    }

    fn two_decisions() -> (tempfile::TempDir, crate::ProjectPaths, Db) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::ProjectPaths::from_root(tmp.path());
        for d in paths.all_dirs() {
            std::fs::create_dir_all(d).unwrap();
        }
        let db = Db::open(&paths.db_path(), Mode::ReadWrite).unwrap();
        for (h, o) in [("h1", "we use openssl"), ("h2", "we use rustls")] {
            db.conn
                .execute(
                    "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES('decision',?1,'is',?2,?2,'user_said',3,'s',?1,1)",
                    rusqlite::params![h, o],
                )
                .unwrap();
        }
        (tmp, paths, db)
    }

    fn says(file: &Path, what: &str) -> bool {
        std::fs::read_to_string(file).unwrap().contains(what)
    }

    /// WP-11. `maintain` projected every record on every run: 4,214 files rewritten each
    /// time on a copy of one store, with nothing new. A file is written when what is on
    /// disk is not what the record says, and not otherwise.
    #[test]
    fn projecting_the_whole_store_writes_only_the_files_that_changed() {
        let (_tmp, paths, db) = two_decisions();
        assert_eq!(project(&paths, &db, &[]).unwrap(), 2, "both are new");
        assert_eq!(project(&paths, &db, &[]).unwrap(), 0, "nothing changed");
        db.conn
            .execute(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=2 WHERE id=1",
                [],
            )
            .unwrap();
        assert_eq!(project(&paths, &db, &[]).unwrap(), 1, "one was retired");
        let file = paths.records_dir().join("decision/1.md");
        assert!(says(&file, "invalid: true"));
        // a file someone removed comes back
        std::fs::remove_file(&file).unwrap();
        assert_eq!(project(&paths, &db, &[]).unwrap(), 1);
        assert!(file.is_file());
    }

    /// `index.md` lists the newest of each kind, newest first, whether the kind is most of
    /// the store (read down the newest-first index) or a few rows of it (read through the
    /// kind's own index).
    #[test]
    fn the_index_lists_the_newest_of_a_common_kind_and_of_a_rare_one() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::ProjectPaths::from_root(tmp.path());
        for d in paths.all_dirs() {
            std::fs::create_dir_all(d).unwrap();
        }
        let db = Db::open(&paths.db_path(), Mode::ReadWrite).unwrap();
        for i in 1..=400i64 {
            // three claims among 400 records; two decisions share each date
            let kind = if i % 150 == 0 { "claim" } else { "decision" };
            db.conn
                .execute(
                    "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
                     VALUES(?1,?2,?3,'is',?3,?3,'user_said',3,'s',?3,?4)",
                    rusqlite::params![i, kind, format!("statement {i}"), i / 2],
                )
                .unwrap();
        }
        let index = write_index(&paths, &db).unwrap();
        let ids: Vec<i64> = index
            .lines()
            .filter_map(|l| l.strip_prefix("- #")?.split(' ').next()?.parse().ok())
            .collect();
        let decisions: Vec<i64> = (1..=400).rev().filter(|i| i % 150 != 0).take(60).collect();
        assert_eq!(ids[..60], decisions[..], "{index}");
        assert_eq!(ids[60..], [300, 150], "{index}");
    }

    /// WP-11. An ingest's projection looked at the file of every retired record the store
    /// had ever held: 524 on one store, most of that ingest's time. It looks at the ones
    /// retired since the projection before it; an older file is the whole projection's.
    #[test]
    fn an_ingests_projection_looks_at_what_was_retired_since_the_last_one() {
        let (_tmp, paths, db) = two_decisions();
        project(&paths, &db, &[1, 2]).unwrap();
        // retired long before the last projection, with a file never brought up to date:
        // not this projection's to mend
        db.conn
            .execute(
                "UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=2, invalidated_at=1000 WHERE id=1",
                [],
            )
            .unwrap();
        let file = paths.records_dir().join("decision/1.md");
        assert_eq!(project(&paths, &db, &[2]).unwrap(), 0);
        assert!(says(&file, "invalid: false"));
        // the whole projection mends it
        assert_eq!(project(&paths, &db, &[]).unwrap(), 1);
        assert!(says(&file, "invalid: true"));
    }
}
