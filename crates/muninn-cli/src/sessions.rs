//! The session log: every hook that sees a transcript path notes it here, append-only, so the
//! write path can ingest a session whose own Stop/SessionEnd hook was cut short. Headless
//! sessions (`claude -p`) end before an asynchronous Stop hook finishes, and the head-to-head
//! smoke lost 17 of 20 sessions that way (PREREGISTRATION.md, 2026-09-17).

use muninn_core::ProjectPaths;
use std::io::Write;
use std::path::PathBuf;

/// Transcripts older than this are not re-scanned by the write path.
const KEEP: usize = 64;

fn log_path(paths: &ProjectPaths) -> PathBuf {
    paths.log_dir().join("sessions.jsonl")
}

/// Append `transcript` for `session` (a read hook may call this: it touches no database).
pub fn note(paths: &ProjectPaths, session: &str, transcript: Option<&str>) {
    let Some(t) = transcript.filter(|t| !t.is_empty()) else {
        return;
    };
    let _ = std::fs::create_dir_all(paths.log_dir());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(paths))
    {
        let line = serde_json::json!({ "session": session, "transcript": t });
        let _ = writeln!(f, "{line}");
    }
}

/// The distinct transcripts noted, most recent last, at most `KEEP`, existing files only.
pub fn pending(paths: &ProjectPaths) -> Vec<(String, PathBuf)> {
    let text = std::fs::read_to_string(log_path(paths)).unwrap_or_default();
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for l in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(l) else {
            continue;
        };
        let (Some(s), Some(t)) = (v["session"].as_str(), v["transcript"].as_str()) else {
            continue;
        };
        let p = PathBuf::from(t);
        out.retain(|(_, q)| q != &p);
        out.push((s.to_string(), p));
    }
    let skip = out.len().saturating_sub(KEEP);
    out.into_iter()
        .skip(skip)
        .filter(|(_, p)| p.is_file())
        .collect()
}

/// Ingest every noted transcript from its watermark (a no-op for finished ones).
pub fn ingest_pending(paths: &ProjectPaths, db: &muninn_core::Db) -> usize {
    let mut n = 0;
    for (session, p) in pending(paths) {
        if let Ok(st) =
            muninn_capture::ingest::ingest_transcript_with(db, &p, &session, Some(paths))
        {
            n += st.inserted;
        }
    }
    n
}
