//! The prompt hook can be turned off without turning the memory off.
//!
//! Muninn injects a block on every prompt; claude-mem injects once at session start, and that
//! difference is most of why Muninn occupies 2.6 times the window on the head-to-head grid.
//! Since the session catalogue arrived the agent is told what is on record up front and can
//! pull any of it by id, so per-prompt delivery may no longer be carrying its cost. Whether it
//! does is a question for a grid; what this fixes is that the switch exists, defaults to on,
//! and is visible in the ledger either way.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_muninn");

fn run(dir: &Path, args: &[&str], stdin: &str) -> String {
    use std::io::Write;
    let mut c = Command::new(BIN)
        .current_dir(dir)
        .args(args)
        .env("MUNINN_ROOT", dir)
        .env("MUNINN_NO_PROJECT", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    c.stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = c.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn reasons(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(".muninn/log/delivery.jsonl")).unwrap_or_default()
}

#[test]
fn prompt_delivery_is_on_by_default_and_can_be_switched_off() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    run(root, &["init", "--keep-native"], "");
    let db = root.join(".muninn/muninn.db");
    let sql = "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
               VALUES('decision','said:state:cache redis','is','we use redis for the cache layer','user: we use redis for the cache layer\n','user_said',3,'s','h1',1);";
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(sql)
        .unwrap();

    let prompt = |sid: &str| {
        format!(
            "{{\"session_id\":\"{sid}\",\"cwd\":\"{}\",\"prompt\":\"which cache layer do we use\",\"hook_event_name\":\"UserPromptSubmit\"}}",
            root.display()
        )
    };

    let on = run(root, &["hook", "UserPromptSubmit"], &prompt("a"));
    assert!(
        on.contains("redis"),
        "on by default, so nothing changes for anyone who does not ask: {on}"
    );

    run(root, &["config", "prompt-delivery", "off"], "");
    let off = run(root, &["hook", "UserPromptSubmit"], &prompt("b"));
    assert!(
        !off.contains("redis"),
        "switched off, the prompt hook delivers nothing: {off}"
    );
    assert!(
        reasons(root).contains("gated:prompt_delivery_off"),
        "and the ledger says why rather than looking like a miss"
    );

    // the memory itself is untouched: the same record is still there to be asked for
    let shown = run(root, &["show", "1"], "");
    assert!(
        shown.contains("redis"),
        "the store still serves it on request: {shown}"
    );
}

/// A record delivered earlier in a session is not delivered again in it — including after
/// `maintain` has folded the ledger, which `SessionStart` spawns on every session start.
///
/// The watermark moves past the pending file when the fold runs, so `delivered_ids` saw
/// nothing and the next prompt re-served what the catalogue had just named. Measured on the
/// head-to-head before the fix: 167 of the 224 records the prompt blocks served were already
/// in that session's catalogue.
#[test]
fn a_fold_does_not_reopen_what_the_session_already_saw() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    run(root, &["init", "--keep-native"], "");
    let sql = "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
               VALUES('decision','said:state:cache redis','is','we use redis for the cache layer','user: we use redis for the cache layer\n','user_said',3,'s','h1',1);";
    rusqlite::Connection::open(root.join(".muninn/muninn.db"))
        .unwrap()
        .execute_batch(sql)
        .unwrap();

    let start = format!(
        "{{\"session_id\":\"S\",\"cwd\":\"{}\",\"source\":\"startup\",\"hook_event_name\":\"SessionStart\"}}",
        root.display()
    );
    let catalogue = run(root, &["hook", "SessionStart"], &start);
    assert!(
        catalogue.contains("redis"),
        "the catalogue names it: {catalogue}"
    );

    // the fold `SessionStart` spawns, run in the foreground so the test is deterministic
    run(root, &["maintain"], "");

    let prompt = |sid: &str| {
        format!(
            "{{\"session_id\":\"{sid}\",\"cwd\":\"{}\",\"prompt\":\"which cache layer do we use\",\"hook_event_name\":\"UserPromptSubmit\"}}",
            root.display()
        )
    };
    let same = run(root, &["hook", "UserPromptSubmit"], &prompt("S"));
    assert!(
        !same.contains("redis"),
        "the same session does not get it twice: {same}"
    );
    let other = run(root, &["hook", "UserPromptSubmit"], &prompt("OTHER"));
    assert!(
        other.contains("redis"),
        "and another session still does: {other}"
    );
}
