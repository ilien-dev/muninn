//! The fourth invalidation trigger, end to end: a decision whose value the code stopped
//! holding is retired, and one whose value the code still holds is not.
//!
//! The grids in `experiment/loop8` and `loop9` measure how often this pays across thirty
//! held-out replacements. This test fixes the three properties those grids cannot express as
//! a pass/fail, and that a refactor could quietly remove: a one-for-one swap is enough on its
//! own, a commit about something else retires nothing, and a value still present elsewhere in
//! the tree is not a disappearance.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_muninn");

fn run(dir: &Path, prog: &str, args: &[&str]) -> String {
    let out = Command::new(prog)
        .current_dir(dir)
        .args(args)
        .env("MUNINN_ROOT", dir)
        .env("MUNINN_NO_PROJECT", "1")
        .output()
        .unwrap_or_else(|e| panic!("{prog} {args:?}: {e}"));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn git(dir: &Path, args: &[&str]) {
    run(dir, "git", args);
}

/// A repository holding `value` in one tracked file, with Muninn initialised on it.
fn project(value: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join("config/stack.json"),
        format!("{{\"value\": \"{value}\"}}\n"),
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "base"]);
    run(root, BIN, &["init", "--keep-native", "--no-boot-block"]);
    tmp
}

/// A user decision naming `text`, as the capture path would have stored it.
fn decision(root: &Path, text: &str) {
    let db = root.join(".muninn/muninn.db");
    let sql = format!(
        "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
         VALUES('decision','said:state:{t}','user_decision','{text}','user: {text}\n','user_said',3,'s','{t}',1);",
        t = text.replace(' ', "-")
    );
    let out = Command::new("sqlite3")
        .arg(&db)
        .arg(&sql)
        .output()
        .expect("sqlite3");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn active(root: &Path, needle: &str) -> bool {
    let db = root.join(".muninn/muninn.db");
    let out = Command::new("sqlite3")
        .arg(&db)
        .arg(format!(
            "SELECT count(*) FROM record WHERE invalid=0 AND object LIKE '%{needle}%';"
        ))
        .output()
        .expect("sqlite3");
    String::from_utf8_lossy(&out.stdout).trim() != "0"
}

#[test]
fn a_one_for_one_swap_retires_the_decision_it_replaced() {
    let tmp = project("PgBouncer");
    let root = tmp.path();
    decision(root, "using PgBouncer for pooling");
    std::fs::write(
        root.join("config/stack.json"),
        "{\"value\": \"Supavisor\"}\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "update dependencies"]);
    run(root, BIN, &["maintain"]);
    assert!(
        !active(root, "PgBouncer"),
        "the code stopped holding the value; the decision naming it must not be served"
    );
}

#[test]
fn a_swap_is_enough_even_when_the_word_survives_elsewhere() {
    let tmp = project("PgBouncer");
    let root = tmp.path();
    // a changelog keeps the word alive long after the project stopped using it
    std::fs::write(root.join("CHANGELOG.md"), "- we once used PgBouncer\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "changelog"]);
    decision(root, "using PgBouncer for pooling");
    std::fs::write(
        root.join("config/stack.json"),
        "{\"value\": \"Supavisor\"}\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "update dependencies"]);
    run(root, BIN, &["maintain"]);
    assert!(
        !active(root, "PgBouncer"),
        "the hunk says what replaced what; the rest of the tree does not have to agree"
    );
}

#[test]
fn a_commit_about_something_else_retires_nothing() {
    let tmp = project("PgBouncer");
    let root = tmp.path();
    decision(root, "using PgBouncer for pooling");
    std::fs::write(root.join("config/other.json"), "{\"value\": \"ripgrep\"}\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "add a tool"]);
    std::fs::write(root.join("config/other.json"), "{\"value\": \"ugrep\"}\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "update dependencies"]);
    run(root, BIN, &["maintain"]);
    assert!(
        active(root, "PgBouncer"),
        "nothing in these commits says anything about the pooler"
    );
}

#[test]
fn when_nobody_says_what_replaced_it_the_file_does() {
    let tmp = project("PgBouncer");
    let root = tmp.path();
    decision(root, "using PgBouncer for pooling");
    std::fs::write(
        root.join("config/stack.json"),
        "{\"value\": \"Supavisor\"}\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    // a subject that says nothing: the diff is the only evidence
    git(root, &["commit", "-qm", "update dependencies"]);
    run(root, BIN, &["maintain"]);
    assert!(!active(root, "PgBouncer"), "the old decision is retired");
    assert!(
        active(root, "Supavisor"),
        "what the file now holds is on record, with the commit behind it"
    );
    // and the question that used to reach the old answer reaches this one
    let out = run(root, BIN, &["recall", "pooling"]);
    assert!(
        out.contains("Supavisor"),
        "the new value is served for the old topic: {out}"
    );
}

#[test]
fn an_unrelated_swap_writes_no_record_of_its_own() {
    let tmp = project("PgBouncer");
    let root = tmp.path();
    decision(root, "using PgBouncer for pooling");
    std::fs::write(root.join("config/other.json"), "{\"value\": \"ripgrep\"}\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "add a tool"]);
    std::fs::write(root.join("config/other.json"), "{\"value\": \"ugrep\"}\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "update dependencies"]);
    run(root, BIN, &["maintain"]);
    assert!(
        !active(root, "ugrep"),
        "a swap that retires nothing is ordinary churn and leaves no decision behind"
    );
}
