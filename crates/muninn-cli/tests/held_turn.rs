//! WP-10: the last message of a session that nothing answered. Stop holds it back while the
//! session may still answer; SessionEnd, after which no answer is coming, puts it on record.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_muninn");

fn hook(root: &Path, event: &str, transcript: &Path) {
    let input = serde_json::json!({"session_id": "s1", "cwd": root,
                                   "transcript_path": transcript, "hook_event_name": event})
    .to_string();
    let mut child = Command::new(BIN)
        .args(["hook", event])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success(), "{event} exits 0");
}

fn typed(root: &Path) -> i64 {
    rusqlite::Connection::open(root.join(".muninn/muninn.db"))
        .unwrap()
        .query_row(
            "SELECT count(*) FROM record WHERE invalid=0 AND kind NOT IN ('episode') AND origin='user_said'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn session_end_stores_the_last_message_nothing_answered() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let out = Command::new(BIN)
        .args([
            "--cwd",
            root.to_str().unwrap(),
            "init",
            "--keep-native",
            "--no-boot-block",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let t = root.join("t.jsonl");
    let lines = [
        serde_json::json!({"type":"user","sessionId":"s1","message":{"role":"user","content":"Revisa el reporte"}}),
        serde_json::json!({"type":"assistant","sessionId":"s1","message":{"role":"assistant","content":[{"type":"text","text":"Revisado."}]}}),
        serde_json::json!({"type":"user","sessionId":"s1","message":{"role":"user","content":"No, nunca borres los reportes viejos del servidor."}}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&t, text).unwrap();
    hook(root, "Stop", &t);
    assert_eq!(typed(root), 0, "Stop holds the unanswered message back");
    hook(root, "SessionEnd", &t);
    assert!(typed(root) > 0, "SessionEnd puts it on record");
}
