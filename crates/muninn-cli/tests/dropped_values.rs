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
    // rusqlite, not the sqlite3 command: the bundled SQLite has FTS5, which the store's
    // triggers need and macOS's system sqlite3 lacks, and Windows has no sqlite3 at all
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(&sql)
        .unwrap();
}

fn active(root: &Path, needle: &str) -> bool {
    let db = root.join(".muninn/muninn.db");
    let n: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM record WHERE invalid=0 AND object LIKE '%' || ?1 || '%'",
            [needle],
            |r| r.get(0),
        )
        .unwrap();
    n != 0
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

#[test]
fn a_word_the_repository_uses_everywhere_is_not_a_value() {
    let tmp = project("PgBouncer");
    let root = tmp.path();
    // `delivered` is vocabulary here: it is in five tracked files
    for i in 0..5 {
        std::fs::write(
            root.join(format!("doc{i}.md")),
            format!("the {i}th note about what is delivered\n"),
        )
        .unwrap();
    }
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "notes"]);
    decision(root, "delivered now looks the file up by session id");
    // a commit that changes one of those lines, one for one
    std::fs::write(root.join("doc2.md"), "the 2nd note about what is shipped\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "update dependencies"]);
    run(root, BIN, &["maintain"]);
    assert!(
        active(root, "looks the file up"),
        "a diff that touches an ordinary word says nothing about a decision that uses it"
    );
}

#[test]
fn a_number_that_changed_is_read_from_the_diff_too() {
    let tmp = project("10 connections");
    let root = tmp.path();
    decision(root, "the database pool holds 10 connections");
    // the unit stays on both lines and the number is not a word: nothing *went away*
    std::fs::write(
        root.join("config/stack.json"),
        "{\"value\": \"25 connections\"}\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "update dependencies"]);
    run(root, BIN, &["maintain"]);
    assert!(
        !active(root, "holds 10 connections"),
        "the same unit with a different number is a different decision"
    );
}

#[test]
fn a_hyphenated_value_matches_the_decision_that_named_it() {
    let tmp = project("async-std");
    let root = tmp.path();
    decision(root, "for the async runtime we are using async-std");
    std::fs::write(root.join("config/stack.json"), "{\"value\": \"tokio\"}\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "update dependencies"]);
    run(root, BIN, &["maintain"]);
    assert!(
        !active(root, "using async-std"),
        "a record's words come from text with the hyphen removed; the diff keeps it"
    );
}

/// A file that was deleted is not a decision that was reversed: the value may have moved,
/// been renamed past git's similarity threshold, or been split across new files. Measured on
/// the v5 head-to-head: one commit removing `config/decisions/` retired five live decisions,
/// four of them the only record of their answer, and the cells that asked those four
/// questions were then given nothing at all.
#[test]
fn a_deleted_file_retires_nothing() {
    let tmp = project("openssl");
    let root = tmp.path();
    decision(root, "use openssl for the tls backend");
    // the whole file goes, as a move or a rename past the similarity threshold would look
    std::fs::remove_file(root.join("config/stack.json")).unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "move the stack config elsewhere"]);
    run(root, BIN, &["maintain"]);
    assert!(
        active(root, "openssl"),
        "the decision survives its file being deleted"
    );
}

/// And the trigger still fires when the file survives: the guard above is about deletion, not
/// about removal. Without this the previous test passes on a broken capture path.
#[test]
fn a_value_removed_from_a_file_that_survives_still_retires() {
    let tmp = project("openssl");
    let root = tmp.path();
    decision(root, "use openssl for the tls backend");
    std::fs::write(root.join("config/stack.json"), "{\"value\": \"rustls\"}\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "update dependencies"]);
    run(root, BIN, &["maintain"]);
    assert!(
        !active(root, "openssl"),
        "a value replaced in a file that still exists is still retired"
    );
}

/// The record a swap writes is keyed on the retired decision's topic *minus the value that
/// left*. A hyphenated value is tokenised both joined and split, so filtering on one matched
/// word kept the other half: real stores held `said:change:runtime std` — half of `async-std`
/// — as the topic of the record that replaced it. The key is indexed and never rendered, so
/// this is a matching defect rather than a leak, and it is still the retired value in the key
/// of its own heir.
#[test]
fn a_swaps_topic_keeps_no_part_of_the_value_that_left() {
    let tmp = project("async-std");
    let root = tmp.path();
    decision(root, "for the async runtime we are using async-std");
    std::fs::write(root.join("config/stack.json"), "{\"value\": \"tokio\"}\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "update dependencies"]);
    run(root, BIN, &["maintain"]);
    let db = root.join(".muninn/muninn.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let mut q = conn
        .prepare("SELECT subject FROM record WHERE origin='commit_linked' AND subject LIKE 'said:change:%'")
        .unwrap();
    let subjects = q
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    assert!(
        !subjects.is_empty(),
        "the swap wrote its record: {subjects}"
    );
    for part in ["async", "std"] {
        assert!(
            !subjects.split_whitespace().any(|w| w == part),
            "no part of the retired value is the heir's topic: {subjects}"
        );
    }
}
