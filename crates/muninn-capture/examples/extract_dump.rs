//! What the extractor makes of real messages: one JSON line per candidate the user's own
//! words produce (decision, invariant, correction). A measurement tool, not a test.
//!
//!   cargo run --release -p muninn-capture --example extract_dump -- \
//!       transcripts <project-name>=<dir-of-jsonl> [...]
//!   cargo run --release -p muninn-capture --example extract_dump -- \
//!       phrasings <muninn-experiments>/experiment/h2h/v6/seed_phrasings.json [...]
//!
//! `transcripts` parses every `*.jsonl` in each directory the way ingest does. `phrasings`
//! sends each message of a fixture as a one-turn session, `a` before `b`, so the guard set
//! the head-to-head grids were seeded from can be compared before and after a change.
//! (https://github.com/ilien-dev/muninn-experiments)
//!
//! `item` is a hash of the project, the kind and the body: the same candidate keeps its id
//! across builds, so two dumps join on it.

use muninn_capture::extract::extract;
use muninn_capture::model::{Session, Turn};

fn emit(project: &str, sid: &str, s: &Session) {
    for c in extract(s, sid) {
        if c.origin != "user_said" {
            continue;
        }
        let shape = match c.kind {
            "decision" => c.subject.split(':').take(2).collect::<Vec<_>>().join(":"),
            k => k.to_string(),
        };
        let item = blake3::hash(format!("{project}\u{0}{}\u{0}{}", c.kind, c.body).as_bytes())
            .to_hex()[..16]
            .to_string();
        let line = serde_json::json!({
            "item": item, "project": project, "session": sid, "turn": c.turn_index,
            "kind": c.kind, "shape": shape, "body": c.body,
        });
        println!("{line}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, rest) = args
        .split_first()
        .expect("usage: extract_dump transcripts|phrasings …");
    match mode.as_str() {
        "transcripts" => {
            for spec in rest {
                let (project, dir) = spec.split_once('=').expect("<project>=<dir>");
                let mut files: Vec<_> = std::fs::read_dir(dir)
                    .expect("reading transcript dir")
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
                    .collect();
                files.sort();
                for f in files {
                    let Ok(s) = muninn_capture::parse_any(&f, 0) else {
                        continue;
                    };
                    let sid = f.file_stem().unwrap().to_string_lossy().to_string();
                    emit(project, &sid, &s);
                }
            }
        }
        "phrasings" => {
            for path in rest {
                let text = std::fs::read_to_string(path).expect("reading phrasings");
                let rows: Vec<serde_json::Value> = serde_json::from_str(&text).expect("a list");
                for r in rows {
                    let key = r["key"].as_str().unwrap_or("?");
                    for side in ["a", "b", "old", "new"] {
                        let Some(msg) = r[side].as_str() else {
                            continue;
                        };
                        let s = Session {
                            session_id: format!("{key}-{side}"),
                            turns: vec![Turn {
                                user_prompt: msg.to_string(),
                                ..Default::default()
                            }],
                            ..Default::default()
                        };
                        emit(path, &format!("{key}-{side}"), &s);
                    }
                }
            }
        }
        other => panic!("unknown mode {other}"),
    }
}
