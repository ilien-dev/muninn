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

/// Detect the harness from the file and parse from `start_offset`.
pub fn parse_any(path: &std::path::Path, start_offset: u64) -> std::io::Result<Session> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.starts_with("rollout-") || path.to_string_lossy().contains("/.codex/") {
        codex::parse(path, start_offset)
    } else {
        claude::parse(path, start_offset)
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
