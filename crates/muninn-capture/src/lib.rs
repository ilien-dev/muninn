//! Transcript capture: Claude Code JSONL and Codex rollout → harness-neutral
//! turns → literal episodes, with secret redaction and byte-offset watermarks.

pub mod claude;
pub mod codex;
pub mod episode;
pub mod extract;
pub mod ingest;
pub mod model;
pub mod redact;

pub use model::{Session, ToolCall, Turn};

/// Is this "user" message the harness's own compaction summary?
///
/// It arrives in the user's role, and it is the assistant's earlier words condensed — not
/// something anyone typed. A third of this project's served episodes come from these turns,
/// and every one of them read `user:` at trust 1, which is the level for something seen in
/// the transcript rather than for a paraphrase that can be wrong in ways a literal excerpt
/// cannot. `extract` already refused to make typed records from it; episodes did not know.
/// The sentence is Claude Code's. Codex compacts differently and its marker is not known
/// here, so a Codex summary is still captured as a turn of its own; no rollout of this
/// project was available to read one from.
pub fn is_compaction_summary(prompt: &str) -> bool {
    prompt
        .trim_start()
        .starts_with("This session is being continued")
}

/// The file a record is about, from the files its evidence names — and nothing when the
/// evidence names more than one.
///
/// An anchor is a claim, not a label: `filter::validate_anchors` retires an anchored record
/// when that file's content changes, and `cue::derive` fires the record whenever the session
/// touches the file's directory. A commit or a turn that touched several files supports no
/// such claim about any one of them. Taking the first made the anchor a property of the
/// ordering rather than of the change — `git log --name-only` lists a commit's files
/// alphabetically, and on this project's own store that put 84 of 256 commit records on
/// whichever file sorted first, and 78 of its 187 directory cues on one directory.
pub fn sole_anchor<T: AsRef<str>>(files: &[T]) -> Option<String> {
    match files {
        [one] => Some(one.as_ref().to_string()),
        _ => None,
    }
}

/// Detect the harness from the file and parse from `start_offset`.
pub fn parse_any(path: &std::path::Path, start_offset: u64) -> std::io::Result<Session> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.starts_with("rollout-") || path.components().any(|c| c.as_os_str() == ".codex") {
        codex::parse(path, start_offset)
    } else {
        claude::parse(path, start_offset)
    }
}

#[cfg(test)]
mod anchor {
    #[test]
    fn an_anchor_needs_one_file() {
        assert_eq!(super::sole_anchor(&["src/a.rs"]), Some("src/a.rs".into()));
        assert_eq!(super::sole_anchor(&["src/a.rs", "src/b.rs"]), None);
        assert_eq!(super::sole_anchor::<&str>(&[]), None);
    }
}

#[cfg(test)]
mod smoke {
    #[test]
    fn parses_real_transcripts_when_present() {
        let home = std::env::var("HOME").unwrap_or_default();
        let dir =
            std::path::PathBuf::from(&home).join(".claude/projects/-home-ilien-Projects-muninn");
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "jsonl") {
                    let s = super::claude::parse(&p, 0).unwrap();
                    assert!(!s.turns.is_empty(), "{}", p.display());
                    let eps: Vec<_> = s
                        .turns
                        .iter()
                        .filter_map(|t| super::episode::from_turn(&s.session_id, t))
                        .collect();
                    eprintln!(
                        "{}: {} turns, {} episodes, {} tools",
                        p.file_name().unwrap().to_string_lossy(),
                        s.turns.len(),
                        eps.len(),
                        s.turns.iter().map(|t| t.tools.len()).sum::<usize>()
                    );
                    for t in s.turns.iter().take(12) {
                        eprintln!(
                            "  turn {} prompt[..90]={:?} tools={}",
                            t.index,
                            t.user_prompt.chars().take(90).collect::<String>(),
                            t.tools.len()
                        );
                    }
                    if let Some(e) = eps.get(3).or(eps.first()) {
                        eprintln!("--- sample episode ---\n{}\n{}", e.object, e.body);
                    }
                    break;
                }
            }
        }
    }
}
