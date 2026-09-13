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
}

const COLS: &str = "id, kind, subject, relation, object, body, origin, trust, anchor_path, anchor_hash, session_id, transcript_ref, created_at, invalid, invalidated_by, invalid_reason";

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
    })
}

pub fn load(db: &Db, where_sql: &str) -> Result<Vec<RecordRow>> {
    let tail = if where_sql.to_ascii_uppercase().contains("ORDER BY") {
        ""
    } else {
        " ORDER BY id"
    };
    let sql = format!("SELECT {COLS} FROM record WHERE {where_sql}{tail}");
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
    s.push_str("---\n");
    s.push_str(&r.body);
    if !r.body.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// Write the Markdown files for `ids` (all records when `ids` is empty) and rebuild
/// `index.md`. Returns the number of files written.
pub fn project(paths: &ProjectPaths, db: &Db, ids: &[i64]) -> Result<usize> {
    // Experiment cells set this: the Markdown mirror would let an agent read the store
    // (retired flags included) around the hooks, which is what the arms must isolate.
    if std::env::var_os("MUNINN_NO_PROJECT").is_some() {
        return Ok(0);
    }
    let rows = if ids.is_empty() {
        load(db, "1=1")?
    } else {
        let list = ids
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        load(db, &format!("id IN ({list})"))?
    };
    let root = paths.records_dir();
    let mut n = 0;
    for r in &rows {
        let dir = root.join(&r.kind);
        std::fs::create_dir_all(&dir).map_err(|e| crate::Error::io(&dir, e))?;
        let p = dir.join(format!("{}.md", r.id));
        std::fs::write(&p, markdown(r)).map_err(|e| crate::Error::io(&p, e))?;
        n += 1;
    }
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
        let rows = load(
            db,
            &format!("invalid=0 AND kind='{kind}' ORDER BY created_at DESC LIMIT {cap}"),
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
    let rows = load(db, if include_invalid { "1=1" } else { "invalid=0" })?;
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

fn insert_incoming(
    db: &Db,
    inc: Incoming,
    body_fallback: String,
    st: &mut ImportStats,
) -> Result<()> {
    st.read += 1;
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
    let origin = inc
        .origin
        .filter(|o| {
            [
                "user_said",
                "review_accepted",
                "commit_linked",
                "tool_observed",
                "agent_inferred",
                "imported",
            ]
            .contains(&o.as_str())
        })
        .unwrap_or_else(|| "imported".into());
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
    let created = match inc.created_at {
        Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or_else(now_ms),
        Some(serde_json::Value::String(s)) => iso_to_ms(&s).unwrap_or_else(now_ms),
        _ => now_ms(),
    };
    let hash = blake3::hash(format!("{kind}|{subject}|{relation}|{object}|{body}").as_bytes())
        .to_hex()
        .to_string();
    let n = db.conn.execute(
        "INSERT OR IGNORE INTO record(kind, subject, relation, object, body, origin, trust, anchor_path, anchor_hash, session_id, transcript_ref, dedup_hash, created_at, invalid, invalid_reason) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
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
            inc.invalid.unwrap_or(false) as i64,
            inc.invalid_reason.filter(|r| ["superseded", "revoked", "anchor_changed", "reverted", "user", "cap"].contains(&r.as_str())),
        ],
    )?;
    if n == 1 {
        st.inserted += 1;
    } else {
        st.duplicates += 1;
    }
    Ok(())
}

/// Import from a JSONL file or a directory of Markdown files with frontmatter (the
/// harness's native topic files included: a file with no frontmatter is one `claim`).
pub fn import(db: &Db, path: &Path) -> Result<ImportStats> {
    let mut st = ImportStats {
        read: 0,
        inserted: 0,
        duplicates: 0,
        rejected: 0,
    };
    let tx = db.write_tx()?;
    if path.is_dir() {
        let mut files: Vec<_> = walk_md(path);
        files.sort();
        for f in files {
            let text = std::fs::read_to_string(&f).map_err(|e| crate::Error::io(&f, e))?;
            let (inc, body) = parse_frontmatter(&text);
            insert_incoming(db, inc, body, &mut st)?;
        }
    } else {
        let text = std::fs::read_to_string(path).map_err(|e| crate::Error::io(path, e))?;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            match serde_json::from_str::<Incoming>(line) {
                Ok(inc) => insert_incoming(db, inc, String::new(), &mut st)?,
                Err(_) => st.rejected += 1,
            }
        }
    }
    tx.commit()?;
    let _ = crate::cue::derive_missing(db);
    db.meta_set("records_changed_since_render", "1")?;
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
