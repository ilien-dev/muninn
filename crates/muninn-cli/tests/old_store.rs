//! A store written by 1.2.0 (schema v2), or by a build of schema v3 or v4, read by this binary
//! before any write path has migrated it: the read-only commands and hooks run between the
//! binary update and the first Stop, SessionEnd or `maintain`, and none of them may fail on a
//! column or a table a later schema added, or on one a later schema took away.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_muninn");

fn old_store(root: &Path, schema: &str, version: &str) {
    std::fs::create_dir_all(root.join(".muninn/log")).unwrap();
    let c = rusqlite::Connection::open(root.join(".muninn/muninn.db")).unwrap();
    c.execute_batch(schema).unwrap();
    c.execute(
        "INSERT INTO meta(key, value) VALUES ('schema_version', ?1) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [version],
    )
    .unwrap();
    c.execute_batch(
        "
         INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at)
           VALUES(1,'decision','said:state:pooling pgbouncer','user_decision','we pool connections with PgBouncer',
                  'user: we pool connections with PgBouncer','user_said',3,'s','h1',1793000000000);
         INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at)
           VALUES(2,'invariant','never push to main','must','Never push to main without review.',
                  'user: Never push to main without review.','user_said',3,'s','h2',1793000000001);
         INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at,transcript_ref)
           VALUES(3,'decision','said:state:pooling pgpool','user_decision','we pool connections with pgpool',
                  'user: we pool connections with pgpool','user_said',3,'s','h3',1792000000000,'t:3');
         INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at,transcript_ref)
           VALUES(4,'episode','session:s#3','said','we pool connections with pgpool',
                  'user: we pool connections with pgpool','tool_observed',1,'s','h4',1792000000000,'t:3');
         UPDATE record SET invalid=1, invalid_reason='superseded', invalidated_by=1 WHERE id=3;",
    )
    .unwrap();
}

fn version(root: &Path) -> String {
    rusqlite::Connection::open(root.join(".muninn/muninn.db"))
        .unwrap()
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

/// Run one command; returns (exit code, stdout + stderr).
fn run(root: &Path, args: &[&str], stdin: Option<&str>) -> (i32, String) {
    let mut cmd = Command::new(BIN);
    cmd.arg("--cwd")
        .arg(root)
        .args(args)
        .env("MUNINN_NO_PROJECT", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.unwrap_or("").as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
    )
}

#[test]
fn every_read_only_path_works_on_a_1_2_0_store_before_it_is_migrated() {
    read_only_paths_work_before_migration(
        include_str!("../../muninn-core/src/testdata/schema_1_2_0.sql"),
        "2",
    );
}

#[test]
fn every_read_only_path_works_on_a_v3_store_before_it_is_migrated() {
    read_only_paths_work_before_migration(
        include_str!("../../muninn-core/src/testdata/schema_v3.sql"),
        "3",
    );
}

#[test]
fn every_read_only_path_works_on_a_v4_store_before_it_is_migrated() {
    read_only_paths_work_before_migration(
        include_str!("../../muninn-core/src/testdata/schema_v4.sql"),
        "4",
    );
}

fn read_only_paths_work_before_migration(schema: &str, v: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    old_store(root, schema, v);
    let out = root.join("x.jsonl");
    let hook = |event: &str| {
        serde_json::json!({"session_id": "s-old", "cwd": root, "prompt": "how do we pool connections",
                           "hook_event_name": event, "source": "startup"})
        .to_string()
    };
    let read_only: Vec<(Vec<&str>, Option<String>)> = vec![
        (
            vec!["export", "--all", "--out", out.to_str().unwrap()],
            None,
        ),
        (vec!["why", "how", "do", "we", "pool", "connections"], None),
        (vec!["why", "--all", "pgpool"], None),
        (vec!["show", "1"], None),
        (vec!["recall", "pool", "connections"], None),
        (vec!["status"], None),
        (vec!["doctor"], None),
        (
            vec!["hook", "UserPromptSubmit"],
            Some(hook("UserPromptSubmit")),
        ),
    ];
    for (args, stdin) in &read_only {
        let (code, text) = run(root, args, stdin.as_deref());
        assert!(
            !text.contains("no such column") && !text.contains("no such table"),
            "`muninn {}` on a v{v} store: {text}",
            args.join(" ")
        );
        // `why` exits 3 when the answer is insufficient and `doctor`/`status` 1 when not green;
        // what may not happen is the store failing to answer
        assert!(
            !text.contains("muninn export:") && !text.contains("muninn why:"),
            "`muninn {}` failed: {text}",
            args.join(" ")
        );
        let _ = code;
    }
    assert_eq!(version(root), v, "nothing above migrated the store");
    // the stale-twin test reads an index a v2 store has not got: it still answers, and the
    // replaced statement's episode is not what it answers with
    let fresh = serde_json::json!({"session_id": "s-twin", "cwd": root, "prompt": "how do we pool connections"})
        .to_string();
    let (_, prompt) = run(root, &["hook", "UserPromptSubmit"], Some(&fresh));
    assert!(prompt.contains("PgBouncer"), "{prompt}");
    assert!(!prompt.contains("pgpool"), "{prompt}");
    let exported = std::fs::read_to_string(&out).unwrap();
    assert_eq!(exported.lines().count(), 4, "every record is exported");
    assert!(
        exported.contains("\"invalidated_by\":1"),
        "with its lineage: {exported}"
    );
    let (_, why) = run(root, &["why", "--all", "pgpool"], None);
    assert!(
        why.contains("pgpool"),
        "`why --all` reads retired rows: {why}"
    );
    // the session-start hooks last: SessionStart spawns `maintain`, which migrates
    for event in ["SubagentStart", "SessionStart"] {
        let (code, text) = run(root, &["hook", event], Some(&hook(event)));
        assert_eq!(code, 0, "{event}: {text}");
        assert!(!text.contains("no such column"), "{event}: {text}");
    }
}
