//! The fourth invalidation trigger, end to end: a decision whose value the code stopped
//! holding is retired, and one whose value the code still holds is not.
//!
//! The grids in `experiment/loop8` and `loop9` (ilien-dev/muninn-experiments) measure how often this pays across thirty
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

/// A commit made before the statement took the word out before it was said.
#[test]
fn a_commit_older_than_the_statement_retires_nothing() {
    let tmp = project("PgBouncer");
    let root = tmp.path();
    std::fs::write(
        root.join("config/stack.json"),
        "{\"value\": \"Supavisor\"}\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "update dependencies"]);
    decision(root, "using PgBouncer for pooling");
    let db = root.join(".muninn/muninn.db");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute("UPDATE record SET created_at = 9000000000000", [])
        .unwrap();
    run(root, BIN, &["maintain"]);
    assert!(active(root, "PgBouncer"));
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
    let events = |root: &Path| -> i64 {
        let db = muninn_core::db::Db::open(
            &root.join(".muninn/muninn.db"),
            muninn_core::db::Mode::ReadOnly,
        )
        .unwrap();
        db.count("SELECT count(*) FROM store_event WHERE what = 'commits'")
            .unwrap_or(0)
    };
    assert_eq!(events(root), 0, "no commit took a value out yet");
    run(root, BIN, &["maintain"]);
    assert!(
        !active(root, "PgBouncer"),
        "the code stopped holding the value; the decision naming it must not be served"
    );
    // the pass leaves an event when it opens: a record an agent's prompt had retired was not
    // among the active ones it read, and its undo must not give that record back afterwards;
    // and one when it closes, having decided to retire a record
    assert_eq!(events(root), 2);
}

/// The commit pass opens (its event and its read of the active records, one transaction),
/// works with no transaction open, and closes (its retirements, and a second event when a
/// record of its read went during the work). A worker's turn retires the user's statement and
/// its coordinator's turn finds the worker's prompt out; each of the two lands before the
/// pass opens, during its work, or after it closes. Whatever the row, the statement whose
/// value the commit took out is not served at the end, and it is retired once: by the pass,
/// or by the worker's change and then left retired, marked.
///
/// The pass is held during its work by the seam in `maintain.rs`, which only a debug build
/// has: under `cargo test --release` the test says so and does nothing.
#[test]
fn the_commit_pass_and_a_prompt_found_out_in_every_order() {
    if !cfg!(debug_assertions) {
        eprintln!("skipped: the seam that holds the commit pass is in debug builds only");
        return;
    }
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum At {
        Before,
        During,
        After,
    }
    use At::{After, Before, During};
    // (the worker's turn, the coordinator's turn, the statement ends marked)
    let rows = [
        (Before, Before, false),
        (Before, During, true),
        (Before, After, true),
        (During, During, false),
        (During, After, true),
        (After, After, false),
    ];
    for (worker_at, coordinator_at, marked) in rows {
        let tmp = project("openssl");
        let root = tmp.path();
        let turn = |sid: &str, ts: &str, prompt: &str| -> String {
            format!(
                "{}\n{}\n",
                serde_json::json!({"type":"user","sessionId":sid,"timestamp":ts,"message":{"role":"user","content":prompt}}),
                serde_json::json!({"type":"assistant","sessionId":sid,"message":{"role":"assistant","content":[{"type":"text","text":"Understood."}]}})
            )
        };
        let task =
            "Write the upgrade notes for the proxy. Actually, switch the TLS backend to boringssl.";
        let own = root.join("own.jsonl");
        std::fs::write(
            &own,
            turn(
                "own",
                "2026-11-05T10:00:00Z",
                "We go with openssl for the TLS backend.",
            ),
        )
        .unwrap();
        let worker = root.join("w0.jsonl");
        std::fs::write(&worker, turn("w0", "2026-11-06T09:00:00Z", task)).unwrap();
        let coord = root.join("coord0.jsonl");
        std::fs::write(
            &coord,
            format!(
                "{}\n{}\n{}\n{}\n",
                serde_json::json!({"type":"user","sessionId":"coord0","timestamp":"2026-11-06T08:59:00Z","message":{"role":"user","content":"Start the proxy work."}}),
                serde_json::json!({"type":"assistant","sessionId":"coord0","message":{"role":"assistant","content":[{"type":"tool_use","id":"t0","name":"Agent","input":{"prompt":task}}]}}),
                serde_json::json!({"type":"user","sessionId":"coord0","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t0","content":"done"}]}}),
                serde_json::json!({"type":"assistant","sessionId":"coord0","message":{"role":"assistant","content":[{"type":"text","text":"Starting."}]}})
            ),
        )
        .unwrap();
        run(root, BIN, &["ingest", own.to_str().unwrap()]);
        assert!(active(root, "openssl"), "the user's statement is on record");
        // a commit takes the value out
        std::fs::write(root.join("config/stack.json"), "{\"value\": \"rustls\"}\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", "update dependencies"]);
        let land = |at: At| {
            if worker_at == at {
                run(root, BIN, &["ingest", worker.to_str().unwrap()]);
            }
            if coordinator_at == at {
                run(root, BIN, &["ingest", coord.to_str().unwrap()]);
            }
        };
        land(Before);
        let hold = tempfile::tempdir().unwrap();
        let mut pass = Command::new(BIN)
            .current_dir(root)
            .args(["maintain"])
            .env("MUNINN_ROOT", root)
            .env("MUNINN_NO_PROJECT", "1")
            .env("MUNINN_TEST_HOLD_COMMIT_PASS", hold.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        for _ in 0..600 {
            if hold.path().join("read").exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let row = format!("worker {worker_at:?}, coordinator {coordinator_at:?}");
        assert!(
            hold.path().join("read").exists(),
            "{row}: the pass opened: it had a commit that removed something"
        );
        land(During);
        std::fs::write(hold.path().join("go"), "").unwrap();
        assert!(pass.wait().unwrap().success(), "{row}");
        land(After);
        assert!(
            !active(root, "openssl"),
            "{row}: the statement the commit took the value from is not served"
        );
        let db = rusqlite::Connection::open(root.join(".muninn/muninn.db")).unwrap();
        let count = |sql: &str| -> i64 { db.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(
            count("SELECT count(*) FROM record WHERE invalid=1 AND invalidated_via='agent_unrestored' AND kind='decision'"),
            marked as i64,
            "{row}: left retired by the worker's change and marked, or retired by the pass"
        );
        assert_eq!(
            count("SELECT count(*) FROM record WHERE invalid=1 AND kind='decision' AND object LIKE '%openssl%'"),
            1,
            "{row}"
        );
    }
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
    // and the question that used to reach the old answer reaches this one (four words or more:
    // `recall` applies the prompt hook's intent gate)
    let out = run(
        root,
        BIN,
        &["recall", "what", "do", "we", "use", "for", "pooling"],
    );
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

/// I2 through the commit-diff pass: a decision said in one session and again in another, then
/// replaced by a config swap. The swap's sweep has to reach the episode of every turn that
/// stated it, the earlier one included; the hunk is the decision's, and the earlier episode
/// claims no hunk of its own.
#[test]
fn a_restated_decision_replaced_by_a_swap_takes_its_earlier_turns() {
    let tmp = project("PgBouncer");
    let root = tmp.path();
    std::fs::write(root.join("CHANGELOG.md"), "- we once used PgBouncer\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "changelog"]);
    for (i, sid) in ["s-one", "s-two"].iter().enumerate() {
        let mut t = String::new();
        t += &serde_json::json!({"type":"user","sessionId":sid,"cwd":root,
            "timestamp":format!("2026-11-0{}T10:00:00.000Z", i + 2),
            "message":{"role":"user","content":"We are using PgBouncer for pooling."}})
        .to_string();
        t.push('\n');
        t += &serde_json::json!({"type":"assistant","sessionId":sid,
            "timestamp":format!("2026-11-0{}T10:00:30.000Z", i + 2),
            "message":{"role":"assistant","content":[{"type":"text","text":"Understood."}]}})
        .to_string();
        t.push('\n');
        let p = root.join(format!("{sid}.jsonl"));
        std::fs::write(&p, t).unwrap();
        run(root, BIN, &["ingest", p.to_str().unwrap()]);
    }
    assert_eq!(
        count(
            root,
            "SELECT count(*) FROM record WHERE kind='episode' AND invalid=0 AND body LIKE '%PgBouncer for pooling%'"
        ),
        2,
        "one episode per session"
    );
    std::fs::write(
        root.join("config/stack.json"),
        "{\"value\": \"Supavisor\"}\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "update dependencies"]);
    run(root, BIN, &["maintain"]);
    assert_eq!(
        count(
            root,
            "SELECT count(*) FROM record WHERE invalid=0 AND body LIKE '%PgBouncer for pooling%'"
        ),
        0,
        "neither the decision nor either session's episode is still served"
    );
}

fn episode(root: &Path, text: &str, at: i64) {
    rusqlite::Connection::open(root.join(".muninn/muninn.db"))
        .unwrap()
        .execute(
            "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
             VALUES('episode',?1,'happened',?2,'user: ' || ?2,'tool_observed',1,'s',?1,?3)",
            rusqlite::params![format!("session:s#{at}"), text, at],
        )
        .unwrap();
}

/// The hunk's one statement is a decision when one named the value, however recent an episode
/// that mentions it; and a statement never typed as a decision is still reached through its
/// episode, or it stays active and is served as current.
#[test]
fn a_swap_prefers_the_decision_and_reaches_an_untyped_statement_through_its_episode() {
    let tmp = project("PgBouncer");
    let root = tmp.path();
    std::fs::write(root.join("CHANGELOG.md"), "- we once used PgBouncer\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "changelog"]);
    decision(root, "using PgBouncer for pooling");
    episode(root, "the PgBouncer logs were noisy today", 5);
    std::fs::write(
        root.join("config/stack.json"),
        "{\"value\": \"Supavisor\"}\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "update dependencies"]);
    run(root, BIN, &["maintain"]);
    assert!(
        !active(root, "using PgBouncer"),
        "the decision is the one replaced"
    );
    assert!(
        active(root, "logs were noisy"),
        "the passing mention is left"
    );

    let tmp = project("PgBouncer");
    let root = tmp.path();
    std::fs::write(root.join("CHANGELOG.md"), "- we once used PgBouncer\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "changelog"]);
    episode(root, "PgBouncer handles the pooling for now", 5);
    std::fs::write(
        root.join("config/stack.json"),
        "{\"value\": \"Supavisor\"}\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "update dependencies"]);
    run(root, BIN, &["maintain"]);
    assert!(
        !active(root, "handles the pooling"),
        "with no decision on record, the episode that stated it goes"
    );
}

/// A decision and its turn's episode are one statement. When the decision goes, the episode
/// goes with it, and it does not then claim a second hunk and write a second observation.
#[test]
fn a_statement_retired_once_writes_one_observation() {
    let tmp = project("PgBouncer");
    let root = tmp.path();
    std::fs::write(root.join("CHANGELOG.md"), "- we once used PgBouncer\n").unwrap();
    std::fs::write(
        root.join("config/replica.json"),
        "{\"value\": \"PgBouncer\"}\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "changelog and replica"]);
    decision(root, "using PgBouncer for pooling");
    episode(root, "using PgBouncer for pooling", 1);
    rusqlite::Connection::open(root.join(".muninn/muninn.db"))
        .unwrap()
        .execute("UPDATE record SET transcript_ref = 't.jsonl:10'", [])
        .unwrap();
    for f in ["config/stack.json", "config/replica.json"] {
        std::fs::write(root.join(f), "{\"value\": \"Supavisor\"}\n").unwrap();
    }
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "update dependencies"]);
    run(root, BIN, &["maintain"]);
    assert_eq!(
        count(
            root,
            "SELECT count(*) FROM record WHERE invalid=0 AND object LIKE '%PgBouncer%'"
        ),
        0
    );
    assert_eq!(
        count(
            root,
            "SELECT count(*) FROM record WHERE origin='commit_linked' AND subject LIKE 'said:%'"
        ),
        1,
        "one statement, one observation"
    );
}

/// A commit that swaps a value and its `git revert`, both seen by one `maintain`. `git log`
/// lists newest first, so 1.2.0 handled the revert before the commit it reverts was on record
/// and retired nothing, and then read the commit's diff as a value that changed. Every record
/// of a reverted commit is retired, and what the commit's diff would have retired stays.
#[test]
fn a_revert_seen_with_its_commit_retires_the_commit_and_nothing_it_touched() {
    let tmp = project("PgBouncer");
    let root = tmp.path();
    decision(root, "using PgBouncer for pooling");
    std::fs::write(
        root.join("config/stack.json"),
        "{\"value\": \"Supavisor\"}\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "use Supavisor for pooling"]);
    git(root, &["revert", "--no-edit", "HEAD"]);
    run(root, BIN, &["maintain"]);
    let of_it =
        "origin='commit_linked' AND object LIKE '%Supavisor%' AND object NOT LIKE 'Revert%'";
    let reverted = count(root, &format!("SELECT count(*) FROM record WHERE {of_it}"));
    assert!(reverted > 0, "the reverted commit is on record");
    assert_eq!(
        count(
            root,
            &format!("SELECT count(*) FROM record WHERE {of_it} AND invalid=0"),
        ),
        0,
        "and every record of it is retired"
    );
    assert!(
        active(root, "using PgBouncer"),
        "the revert put the value back, so the decision naming it holds"
    );
}

/// The same pair seen by two runs: the first reads the commit and retires the decision its
/// swap names, the second reads the revert. The revert retires the commit's records, its
/// observation included, and puts back what that observation had retired.
#[test]
fn a_revert_seen_later_puts_back_what_its_commit_retired() {
    let tmp = project("PgBouncer");
    let root = tmp.path();
    decision(root, "using PgBouncer for pooling");
    std::fs::write(
        root.join("config/stack.json"),
        "{\"value\": \"Supavisor\"}\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "use Supavisor for pooling"]);
    run(root, BIN, &["maintain"]);
    assert!(!active(root, "using PgBouncer"), "the swap retired it");
    git(root, &["revert", "--no-edit", "HEAD"]);
    run(root, BIN, &["maintain"]);
    assert_eq!(
        count(
            root,
            "SELECT count(*) FROM record WHERE origin='commit_linked' AND invalid=0 \
             AND object LIKE '%Supavisor%' AND object NOT LIKE 'Revert%'",
        ),
        0,
        "the commit and the observation its swap wrote are both retired"
    );
    assert!(
        active(root, "using PgBouncer"),
        "the revert undid what retired it"
    );
}

/// A transcript of `msgs`, each answered, written where `muninn ingest` can read it.
fn ingest(root: &Path, msgs: &[&str]) {
    let mut t = String::new();
    for (i, m) in msgs.iter().enumerate() {
        t += &serde_json::json!({"type":"user","sessionId":"s-reword","cwd":root,
            "timestamp":format!("2026-11-02T10:0{i}:00.000Z"),
            "message":{"role":"user","content":m}})
        .to_string();
        t.push('\n');
        t += &serde_json::json!({"type":"assistant","sessionId":"s-reword",
            "timestamp":format!("2026-11-02T10:0{i}:30.000Z"),
            "message":{"role":"assistant","content":[{"type":"text","text":"Understood."}]}})
        .to_string();
        t.push('\n');
    }
    let p = root.join("session.jsonl");
    std::fs::write(&p, t).unwrap();
    run(root, BIN, &["ingest", p.to_str().unwrap()]);
}

fn count(root: &Path, sql: &str) -> i64 {
    rusqlite::Connection::open(root.join(".muninn/muninn.db"))
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

/// WP-02. A commit that rewords one sentence of prose so an ordinary word leaves that line is
/// not a project changing its mind about anything. 1.2.0 retired every record that held the
/// word — the decision, the invariant and the correction typed from the same turns, and their
/// episodes — and wrote the new prose line as three decisions of its own.
#[test]
fn rewording_prose_retires_nothing_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    std::fs::write(
        root.join("README.md"),
        "# Demo\n\nThe heron mascot is on the cover page.\n\nSee docs/birds.md for the heron notes.\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(
        root.join("docs/birds.md"),
        "The heron scheduler runs the nightly batch jobs.\n",
    )
    .unwrap();
    std::fs::write(root.join(".gitignore"), ".muninn/\n*.jsonl\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "initial"]);
    run(root, BIN, &["init", "--keep-native", "--no-boot-block"]);
    ingest(
        root,
        &[
            "We go with the heron scheduler for the nightly batch jobs.",
            "Never deploy on Fridays without running the heron checklist first.",
            "No, the heron scheduler must keep its retry budget in the config file.",
        ],
    );
    let before = count(root, "SELECT count(*) FROM record WHERE invalid=0");
    assert!(
        before >= 6,
        "decision, invariant, correction and their episodes: {before}"
    );
    std::fs::write(
        root.join("README.md"),
        "# Demo\n\nThe mascot is on the cover page.\n\nSee docs/birds.md for the heron notes.\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "docs: reword the cover sentence"]);
    run(root, BIN, &["maintain"]);
    assert_eq!(
        count(root, "SELECT count(*) FROM record WHERE invalid=1"),
        0,
        "a reworded sentence of prose retires nothing"
    );
    assert_eq!(
        count(
            root,
            "SELECT count(*) FROM record WHERE origin='commit_linked' AND subject LIKE 'said:%'"
        ),
        0,
        "and no line of prose goes on record as a decision"
    );
}

/// WP-02 and WP-12. One hunk is evidence about one value: it retires the newest record that
/// named it and no other, and the observation it writes is that record's heir, so the
/// retirement can be traced to the commit and the catalogue's `replaces #n` has an end.
#[test]
fn a_swap_retires_one_record_and_is_its_heir() {
    let tmp = project("PgBouncer");
    let root = tmp.path();
    // the word survives elsewhere, so only the hunk speaks: a value gone from the whole tree
    // is the disappearance rule's, and that one is about every record that named it
    std::fs::write(root.join("CHANGELOG.md"), "- we once used PgBouncer\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "changelog"]);
    decision(root, "using PgBouncer for pooling");
    decision(root, "PgBouncer sits in front of the replicas");
    rusqlite::Connection::open(root.join(".muninn/muninn.db"))
        .unwrap()
        .execute(
            "UPDATE record SET created_at = 2 WHERE object LIKE 'PgBouncer sits%'",
            [],
        )
        .unwrap();
    std::fs::write(
        root.join("config/stack.json"),
        "{\"value\": \"Supavisor\"}\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "update dependencies"]);
    run(root, BIN, &["maintain"]);
    assert!(
        active(root, "using PgBouncer"),
        "the older statement is left"
    );
    assert!(!active(root, "PgBouncer sits"), "the newest one is retired");
    let (via, heir_origin): (String, String) =
        rusqlite::Connection::open(root.join(".muninn/muninn.db"))
            .unwrap()
            .query_row(
                "SELECT r.invalidated_via, h.origin FROM record r JOIN record h ON h.id = r.invalidated_by \
                 WHERE r.object LIKE 'PgBouncer sits%'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
    assert!(via.starts_with("swap:"), "{via}");
    assert_eq!(heir_origin, "commit_linked", "the observation is the heir");
}
