//! WP-21. A record's `evidence:` line names the transcript its words came from, and the
//! client removes old transcripts: on ten store copies 1,718 of 4,175 active records named a
//! file that was gone, most of the records in five of them. The record stays served, word for
//! word as stored. `show` and `why` say when the file is gone, so that nobody goes looking
//! for the rest of a message there, and `doctor` counts them.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_muninn");

fn run(root: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(BIN)
        .arg("--cwd")
        .arg(root)
        .args(args)
        .env("MUNINN_NO_PROJECT", "1")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

const GONE: &str = "no longer on disk";

#[test]
fn a_record_whose_transcript_is_gone_is_served_and_says_so_when_pulled() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let (code, _) = run(root, &["init", "--keep-native", "--no-boot-block"]);
    assert_eq!(code, 0);
    let kept = root.join("kept.jsonl");
    std::fs::write(&kept, "").unwrap();
    let removed = root.join("removed.jsonl");
    let db = rusqlite::Connection::open(root.join(".muninn/muninn.db")).unwrap();
    for (id, text, file) in [
        (1, "the retry budget is three attempts", &kept),
        (2, "release notes are written by hand", &removed),
    ] {
        db.execute(
            "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,transcript_ref,dedup_hash,created_at) \
             VALUES(?1,'decision',?2,'user_decision',?3,?3,'user_said',3,'s',?4,?2,?1)",
            rusqlite::params![
                id,
                format!("said:state:{id}"),
                text,
                format!("{}:120", file.display())
            ],
        )
        .unwrap();
    }
    drop(db);
    let evidence_of = |out: &str, text: &str| -> String {
        let at = out
            .find(text)
            .unwrap_or_else(|| panic!("{text} is shown: {out}"));
        out[at..]
            .lines()
            .find(|l| l.trim_start().starts_with("evidence:"))
            .unwrap_or_else(|| panic!("an evidence line follows {text}: {out}"))
            .to_string()
    };
    for args in [&["show", "1", "2"][..], &["why", "1"], &["why", "2"]] {
        let (code, out) = run(root, args);
        assert_eq!(code, 0, "{args:?}");
        if out.contains("retry budget") {
            let line = evidence_of(&out, "retry budget");
            assert!(!line.contains(GONE), "{args:?}: its file is there: {line}");
        }
        if out.contains("release notes") {
            let line = evidence_of(&out, "release notes");
            assert!(line.contains("removed.jsonl"), "{line}");
            assert!(line.contains(GONE), "{args:?}: its file is gone: {line}");
        }
        assert!(
            out.contains("retry budget") || out.contains("release notes"),
            "{args:?}: {out}"
        );
    }
    // a prompt is still given the record: what a hook delivers does not depend on the file
    let (_, out) = run(root, &["recall", "how are the release notes written"]);
    assert!(out.contains("release notes are written by hand"), "{out}");
    // and the live report counts them, without calling the store unhealthy for it
    let (code, out) = run(root, &["--json", "doctor"]);
    assert_eq!(code, 0);
    let report: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    let check = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "evidence")
        .unwrap_or_else(|| panic!("doctor has an evidence check: {out}"));
    assert_eq!(check["status"], "green", "{check}");
    let detail = check["detail"].as_str().unwrap();
    assert!(detail.contains("1 of 2 transcript"), "{detail}");
    assert!(detail.contains("1 record"), "{detail}");
    // the hook's own health line does not look at files: no such check on that path
    let (_, out) = run(root, &["--json", "status"]);
    assert!(!out.contains("\"evidence\""), "{out}");
}
