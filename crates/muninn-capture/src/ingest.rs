//! Write path: transcript → episodes → records, with byte-offset watermarks so a
//! hook that expires loses time, never data.

use crate::episode::from_turn_all;
use crate::parse_any;
use muninn_core::caps::MAX_ACTIVE_RECORDS;
use muninn_core::db::now_ms;
use muninn_core::{Db, Result};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Default, Serialize)]
pub struct IngestStats {
    pub turns: usize,
    pub inserted: usize,
    pub duplicates: usize,
    pub from_offset: u64,
    pub to_offset: u64,
    pub archived: usize,
}

fn watermark_key(transcript: &Path) -> String {
    format!(
        "wm:{}",
        blake3::hash(transcript.to_string_lossy().as_bytes()).to_hex()
    )
}

pub fn ingest_transcript(
    db: &Db,
    transcript: &Path,
    fallback_session: &str,
) -> Result<IngestStats> {
    let key = watermark_key(transcript);
    let from: u64 = db.meta_get(&key)?.and_then(|s| s.parse().ok()).unwrap_or(0);
    let session = parse_any(transcript, from).map_err(|e| muninn_core::Error::io(transcript, e))?;
    let sid = if session.session_id.is_empty() {
        fallback_session.to_string()
    } else {
        session.session_id.clone()
    };
    let mut stats = IngestStats {
        turns: session.turns.len(),
        from_offset: from,
        to_offset: session.end_offset,
        ..Default::default()
    };
    let now = now_ms();
    let tx = db.conn.unchecked_transaction()?;
    {
        let mut ins = tx.prepare(
            "INSERT OR IGNORE INTO record(kind, subject, relation, object, body, origin, trust, anchor_path, session_id, transcript_ref, dedup_hash, created_at) \
             VALUES('episode', ?1, 'happened', ?2, ?3, 'tool_observed', 1, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for (t, ep) in session
            .turns
            .iter()
            .flat_map(|t| from_turn_all(&sid, t).into_iter().map(move |e| (t, e)))
        {
            let hash = blake3::hash(
                format!("episode|{}|happened|{}|{}", ep.subject, ep.object, ep.body).as_bytes(),
            )
            .to_hex()
            .to_string();
            let created = t
                .timestamp
                .as_deref()
                .and_then(parse_rfc3339_ms)
                .unwrap_or(now);
            let anchor = ep.files.first().cloned();
            let n = ins.execute(rusqlite::params![
                ep.subject,
                ep.object,
                ep.body,
                anchor,
                sid,
                format!("{}:{}", transcript.display(), t.end_offset),
                hash,
                created
            ])?;
            if n == 1 {
                stats.inserted += 1;
            } else {
                stats.duplicates += 1;
            }
        }
    }
    // cap: archive the oldest episodes past the limit (retained, not served)
    let active: i64 = tx.query_row("SELECT count(*) FROM record WHERE invalid=0", [], |r| {
        r.get(0)
    })?;
    if active > MAX_ACTIVE_RECORDS {
        let over = active - MAX_ACTIVE_RECORDS;
        stats.archived = tx.execute(
            "UPDATE record SET invalid=1, invalid_reason='cap' WHERE id IN (SELECT id FROM record WHERE invalid=0 AND kind='episode' ORDER BY created_at ASC LIMIT ?1)",
            [over],
        )?;
    }
    tx.commit()?;
    db.meta_set(&key, &session.end_offset.to_string())?;
    db.meta_set("ingest_watermark_ms", &now.to_string())?;
    db.meta_set("records_changed_since_render", "1")?;
    Ok(stats)
}

/// RFC 3339 → epoch ms (UTC only, which is what both harnesses write).
fn parse_rfc3339_ms(s: &str) -> Option<i64> {
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
    // days from civil
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400_000 + hh * 3_600_000 + mm * 60_000 + (ss * 1000.0) as i64)
}

#[cfg(test)]
mod tests {
    #[test]
    fn rfc3339_roundtrip() {
        assert_eq!(
            super::parse_rfc3339_ms("2026-09-12T14:03:11.084Z"),
            Some(1_789_221_791_084)
        );
    }
}
