//! The session log: every hook that sees a transcript path notes it here, append-only, so the
//! write path can ingest a session whose own Stop/SessionEnd hook was cut short. Headless
//! sessions (`claude -p`) end before an asynchronous Stop hook finishes, and the head-to-head
//! smoke lost 17 of 20 sessions that way (PREREGISTRATION.md, 2026-09-17).

use muninn_core::ProjectPaths;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Transcripts older than this are not re-scanned by the write path.
const KEEP: usize = 64;

/// `maintain` rewrites the log only when that drops at least this many lines.
const COMPACT_MIN_DROPPED: usize = 128;

fn log_path(paths: &ProjectPaths) -> PathBuf {
    paths.log_dir().join("sessions.jsonl")
}

/// Append `transcript` for `session` (a read hook may call this: it touches no database).
pub fn note(paths: &ProjectPaths, session: &str, transcript: Option<&str>) {
    let Some(t) = transcript.filter(|t| !t.is_empty()) else {
        return;
    };
    let line = serde_json::json!({ "session": session, "transcript": t });
    let _ = muninn_core::logfold::append_line(&log_path(paths), &line);
}

/// One line of the log that reads: the session and the transcript a hook noted for it.
fn parse(line: &str) -> Option<(String, PathBuf)> {
    let v = serde_json::from_str::<serde_json::Value>(line).ok()?;
    Some((
        v["session"].as_str()?.to_string(),
        PathBuf::from(v["transcript"].as_str()?),
    ))
}

/// The distinct transcripts noted, most recent last, at most `KEEP`, existing files only.
///
/// One pass with the last line of each transcript looked up by its path. It used to drop the
/// earlier line of a transcript by comparing it with every transcript seen so far, for every
/// line: 8,627 lines for 636 transcripts cost 207 to 230 ms of every `Stop` on one store.
pub fn pending(paths: &ProjectPaths) -> Vec<(String, PathBuf)> {
    let text = std::fs::read_to_string(log_path(paths)).unwrap_or_default();
    let noted: Vec<(String, PathBuf)> = text.lines().filter_map(parse).collect();
    let mut last: HashMap<&Path, usize> = HashMap::new();
    for (i, (_, p)) in noted.iter().enumerate() {
        last.insert(p.as_path(), i);
    }
    let out: Vec<&(String, PathBuf)> = noted
        .iter()
        .enumerate()
        .filter(|(i, (_, p))| last[p.as_path()] == *i)
        .map(|(_, n)| n)
        .collect();
    let skip = out.len().saturating_sub(KEEP);
    out.into_iter()
        .skip(skip)
        .filter(|(_, p)| p.is_file())
        .cloned()
        .collect()
}

/// The transcript the session log holds for `session`: the one named after the session when
/// there is one (a subagent's transcript carries its parent's session id under another name),
/// otherwise the one noted last. `None` when the log names none that exists.
pub fn transcript_of(paths: &ProjectPaths, session: &str) -> Option<PathBuf> {
    let text = std::fs::read_to_string(log_path(paths)).unwrap_or_default();
    let mut noted: Vec<PathBuf> = Vec::new();
    for (s, p) in text.lines().filter_map(parse) {
        if s != session {
            continue;
        }
        noted.retain(|q| q != &p);
        noted.push(p);
    }
    noted.retain(|p| p.is_file());
    noted
        .iter()
        .rev()
        .find(|p| p.file_stem().is_some_and(|s| s == session))
        .or(noted.last())
        .cloned()
}

/// The sessions the log notes `transcript` for, oldest first, each with the path as the hook
/// wrote it. Two spellings of one file are the same file.
pub fn noted_as(paths: &ProjectPaths, transcript: &std::path::Path) -> Vec<(String, PathBuf)> {
    let same = |p: &std::path::Path| {
        p == transcript
            || matches!((std::fs::canonicalize(p), std::fs::canonicalize(transcript)), (Ok(a), Ok(b)) if a == b)
    };
    let text = std::fs::read_to_string(log_path(paths)).unwrap_or_default();
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for (s, p) in text.lines().filter_map(parse) {
        if same(&p) && !out.iter().any(|(o, _)| *o == s) {
            out.push((s, p));
        }
    }
    out
}

/// The lines of the log that its readers can tell apart: for each session and transcript,
/// the first line that names the pair and the last. `noted_as` reads the order pairs first
/// appear in, `pending` and `transcript_of` the order they last appear in, and none of them
/// reads a repeat in between or a line that does not parse.
fn lines_that_count(text: &str) -> Vec<&str> {
    let parsed: Vec<(&str, (String, PathBuf))> = text
        .lines()
        .filter_map(|l| parse(l).map(|n| (l, n)))
        .collect();
    let mut first: HashMap<(&str, &Path), usize> = HashMap::new();
    let mut last: HashMap<(&str, &Path), usize> = HashMap::new();
    for (i, (_, (s, p))) in parsed.iter().enumerate() {
        first.entry((s.as_str(), p.as_path())).or_insert(i);
        last.insert((s.as_str(), p.as_path()), i);
    }
    parsed
        .iter()
        .enumerate()
        .filter(|(i, (_, (s, p)))| {
            let k = (s.as_str(), p.as_path());
            first[&k] == *i || last[&k] == *i
        })
        .map(|(_, (l, _))| *l)
        .collect()
}

/// Rewrite the log without the lines no reader can tell apart, and say how many went.
/// `maintain` calls this under its lock, so one process rewrites at a time.
///
/// Every hook of a session appends a line and nothing took one out: one store's log held
/// 8,627 lines for 636 transcripts. Hooks append while this runs, so what they add after the
/// read is carried over until the file stands still, and the new file then takes the old
/// one's place in one rename. A line appended in the instant between that last look and the
/// rename is lost; the next hook of its session notes the transcript again.
pub fn compact(paths: &ProjectPaths) -> usize {
    use std::io::Write;
    let log = log_path(paths);
    let Ok(bytes) = std::fs::read(&log) else {
        return 0;
    };
    // a line still being written has no newline yet and is carried over as it is
    let whole = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let text = String::from_utf8_lossy(&bytes[..whole]);
    let kept = lines_that_count(&text);
    let total = text.lines().count();
    let dropped = total - kept.len();
    if dropped < COMPACT_MIN_DROPPED {
        return 0;
    }
    let tmp = log.with_extension("jsonl.compact");
    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        let mut out = String::with_capacity(whole);
        for l in &kept {
            out.push_str(l);
            out.push('\n');
        }
        f.write_all(out.as_bytes())?;
        let mut carried = whole;
        loop {
            let now = std::fs::read(&log)?;
            if now.len() <= carried {
                break;
            }
            f.write_all(&now[carried..])?;
            carried = now.len();
        }
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &log)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return 0;
    }
    dropped
}

/// Ingest every noted transcript from its watermark (a no-op for finished ones).
pub fn ingest_pending(paths: &ProjectPaths, db: &muninn_core::Db) -> usize {
    ingest_noted(paths, db, &pending(paths))
}

/// Ingest these transcripts, each from its watermark.
pub fn ingest_noted(
    paths: &ProjectPaths,
    db: &muninn_core::Db,
    noted: &[(String, PathBuf)],
) -> usize {
    let mut n = 0;
    for (session, p) in noted {
        if let Ok(st) = ingest_one(db, p, session, paths) {
            n += st.inserted;
        }
    }
    n
}

/// `maintain`'s ingest of one transcript. Most sessions end with no SessionEnd, so a last
/// message nothing answered is released once the transcript has been idle for an hour
/// (`ingest::RELEASE_IDLE`) instead of being held for good (WP-10).
pub fn ingest_one(
    db: &muninn_core::Db,
    p: &std::path::Path,
    session: &str,
    paths: &ProjectPaths,
) -> muninn_core::Result<muninn_capture::ingest::IngestStats> {
    if muninn_capture::ingest::is_idle(p) {
        muninn_capture::ingest::ingest_transcript_released(db, p, session, Some(paths))
    } else {
        muninn_capture::ingest::ingest_transcript_with(db, p, session, Some(paths))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A log in the shape hooks leave: every hook of a session repeats its line, sessions
    /// interleave, a subagent's transcript goes under its parent's session, one transcript
    /// is noted by two sessions, some lines are torn.
    fn busy_log(root: &Path, repeats: usize) -> String {
        let t = |name: &str| root.join(name).to_string_lossy().to_string();
        let line = |s: &str, name: &str| {
            serde_json::json!({ "session": s, "transcript": t(name) }).to_string()
        };
        let mut lines: Vec<String> = Vec::new();
        for round in 0..repeats {
            for s in 0..70 {
                let sid = format!("sess-{s}");
                lines.push(line(&sid, &format!("sess-{s}.jsonl")));
                if s % 7 == 0 {
                    lines.push(line(&sid, &format!("agent-{s}.jsonl")));
                }
                if s % 11 == 0 && round % 2 == 1 {
                    // a file two sessions note, the second only on later rounds
                    lines.push(line(&sid, "shared.jsonl"));
                    lines.push(line("sess-other", "shared.jsonl"));
                }
            }
            lines.push("{\"session\":\"torn".to_string());
            lines.push("not json at all".to_string());
        }
        // and the newest session's lines come last, as they do
        for _ in 0..3 {
            lines.push(line("sess-3", "sess-3.jsonl"));
            lines.push(line("sess-0", "agent-0.jsonl"));
        }
        lines.join("\n") + "\n"
    }

    fn answers(paths: &ProjectPaths, root: &Path) -> String {
        let mut out = format!("pending {:?}\n", pending(paths));
        for s in [
            "sess-0",
            "sess-3",
            "sess-7",
            "sess-11",
            "sess-other",
            "nobody",
        ] {
            out.push_str(&format!("of {s} {:?}\n", transcript_of(paths, s)));
        }
        for f in [
            "shared.jsonl",
            "sess-7.jsonl",
            "agent-7.jsonl",
            "gone.jsonl",
        ] {
            out.push_str(&format!("as {f} {:?}\n", noted_as(paths, &root.join(f))));
        }
        out
    }

    /// WP-01. The rewrite takes out only what no reader can tell apart: `remember` (the
    /// session's transcript, the sessions of a transcript, the other transcripts) and the
    /// write path get the same answers from the log before it and after it.
    #[test]
    fn compacting_the_session_log_changes_no_readers_answer() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let paths = ProjectPaths::from_root(root);
        std::fs::create_dir_all(paths.log_dir()).unwrap();
        // most transcripts exist, a few are gone, as the client removes old ones
        for s in 0..70 {
            if s % 5 != 4 {
                std::fs::write(root.join(format!("sess-{s}.jsonl")), "").unwrap();
            }
            if s % 7 == 0 {
                std::fs::write(root.join(format!("agent-{s}.jsonl")), "").unwrap();
            }
        }
        std::fs::write(root.join("shared.jsonl"), "").unwrap();
        std::fs::write(log_path(&paths), busy_log(root, 12)).unwrap();
        let lines = |p: &ProjectPaths| {
            std::fs::read_to_string(log_path(p))
                .unwrap()
                .lines()
                .count()
        };

        let before = answers(&paths, root);
        let had = lines(&paths);
        assert!(pending(&paths).len() > 40, "{before}");
        let dropped = compact(&paths);
        assert_eq!(answers(&paths, root), before);
        assert_eq!(lines(&paths), had - dropped);
        // 70 sessions, 10 agents, 7 shared pairs and one more: at most two lines a pair
        assert!(
            lines(&paths) <= 2 * (70 + 10 + 7 + 1),
            "{} lines left",
            lines(&paths)
        );
        // nothing left to take out, so nothing is rewritten
        assert_eq!(compact(&paths), 0);
        // a hook appends afterwards as before
        note(
            &paths,
            "sess-new",
            Some(&root.join("sess-1.jsonl").to_string_lossy()),
        );
        assert_eq!(
            pending(&paths).last().map(|(s, _)| s.as_str()),
            Some("sess-new")
        );
    }

    /// A short log is left alone: rewriting it would save nothing a reader notices.
    #[test]
    fn a_log_with_few_repeats_is_not_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::from_root(tmp.path());
        std::fs::create_dir_all(paths.log_dir()).unwrap();
        let text = busy_log(tmp.path(), 1);
        std::fs::write(log_path(&paths), &text).unwrap();
        assert_eq!(compact(&paths), 0);
        assert_eq!(std::fs::read_to_string(log_path(&paths)).unwrap(), text);
    }

    /// WP-01. Two spellings of one path are one transcript, as they were when paths were
    /// compared one against the other, and the session of its last line is the one kept.
    #[test]
    fn pending_lists_a_transcript_once_under_its_last_session() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let paths = ProjectPaths::from_root(root);
        std::fs::write(root.join("a.jsonl"), "").unwrap();
        std::fs::write(root.join("b.jsonl"), "").unwrap();
        let a = root.join("a.jsonl").to_string_lossy().to_string();
        let b = root.join("b.jsonl").to_string_lossy().to_string();
        // the same file with a doubled separator, which a path comparison reads as equal
        let a2 = format!("{}//a.jsonl", root.to_string_lossy());
        note(&paths, "one", Some(&a));
        note(&paths, "two", Some(&b));
        note(&paths, "three", Some(&a2));
        let got = pending(&paths);
        let sessions: Vec<&str> = got.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(sessions, ["two", "three"]);
    }
}
