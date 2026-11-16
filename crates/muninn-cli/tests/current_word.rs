//! The newest thing the person said is the one that holds, whatever order the sessions are
//! read in, and nothing a tool or an agent wrote takes it away. Each test writes invented
//! transcripts and reads the store the hooks leave.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_muninn");

fn run(root: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let home = root.join("_home");
    let mut child = Command::new(BIN)
        .arg("--cwd")
        .arg(root)
        .args(args)
        .env("MUNINN_NO_PROJECT", "1")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("CODEX_HOME", home.join("no-codex"))
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    std::fs::create_dir_all(tmp.path().join("_home")).unwrap();
    let (code, _, err) = run(tmp.path(), &["init", "--keep-native"], "");
    assert_eq!(code, 0, "init: {err}");
    tmp
}

fn write(root: &Path, name: &str, lines: &[Value]) -> PathBuf {
    let p = root.join(name);
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&p, text).unwrap();
    p
}

fn append(path: &Path, lines: &[Value]) {
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

fn hook(root: &Path, event: &str, sid: &str, transcript: &Path) {
    let input = json!({"session_id": sid, "cwd": root, "transcript_path": transcript});
    let (code, _, err) = run(root, &["hook", event], &input.to_string());
    assert_eq!(code, 0, "{event}: {err}");
}

fn user(sid: &str, at: &str, text: &str) -> Value {
    json!({"type": "user", "sessionId": sid, "timestamp": at, "origin": {"kind": "human"},
           "promptSource": "typed", "message": {"role": "user", "content": text}})
}

fn say(sid: &str, text: &str) -> Value {
    json!({"type": "assistant", "sessionId": sid,
           "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}})
}

/// The agent of `sid` runs `cmd`; `failed` carries what the failing run printed.
fn bash(sid: &str, id: &str, cmd: &str, failed: Option<&str>) -> Vec<Value> {
    vec![
        json!({"type": "assistant", "sessionId": sid, "message": {"role": "assistant", "content": [
            {"type": "tool_use", "id": id, "name": "Bash", "input": {"command": cmd}}]}}),
        json!({"type": "user", "sessionId": sid, "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": failed.unwrap_or("ok"),
             "is_error": failed.is_some()}]},
            "toolUseResult": {"stdout": "", "stderr": "", "interrupted": false}}),
    ]
}

fn store(root: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(root.join(".muninn/muninn.db")).unwrap()
}

/// (invalid, invalidated_via) of the one typed record whose text holds `needle`.
fn typed(root: &Path, needle: &str) -> (i64, Option<String>) {
    store(root)
        .query_row(
            "SELECT invalid, invalidated_via FROM record WHERE kind <> 'episode' AND instr(object, ?1) > 0",
            [needle],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or_else(|e| panic!("one typed record holding {needle:?}: {e}"))
}

const OLD: &str = "Let's use openssl for the TLS backend.";
const NEW: &str = "Actually, switch the TLS backend to rustls.";
const DAY1: &str = "2026-11-01T10:00:00Z";
const DAY2: &str = "2026-11-02T10:00:00Z";

/// I1 and R2. A decision of one day and the change made the next, each in its own session.
/// Read in the order of the dates the change retires the decision. Read the other way round
/// the older session is older word and retires nothing: the change stands in both orders. The
/// older decision then stays on record beside it, and wherever the two are shown they are
/// marked as disagreeing: in the session-start catalogue, in the prompt hook's blocks and in
/// `muninn why`.
#[test]
fn an_older_session_read_last_does_not_retire_the_later_change() {
    for older_last in [false, true] {
        let p = project();
        let root = p.path();
        let a = write(
            root,
            "a.jsonl",
            &[user("s-a", DAY1, OLD), say("s-a", "Done.")],
        );
        let b = write(
            root,
            "b.jsonl",
            &[user("s-b", DAY2, NEW), say("s-b", "Done.")],
        );
        if older_last {
            hook(root, "Stop", "s-b", &b);
            hook(root, "Stop", "s-a", &a);
        } else {
            hook(root, "Stop", "s-a", &a);
            hook(root, "Stop", "s-b", &b);
        }
        assert_eq!(typed(root, "rustls").0, 0, "older last: {older_last}");
        if !older_last {
            assert_eq!(typed(root, "openssl"), (1, Some("said".into())));
            continue;
        }
        assert_eq!(typed(root, "openssl").0, 0);
        let start = json!({"session_id": "s-c", "cwd": root, "source": "startup"});
        let (_, out, _) = run(root, &["hook", "SessionStart"], &start.to_string());
        let lines: Vec<&str> = out
            .split("\\n")
            .filter(|l| l.contains("decision") && (l.contains("openssl") || l.contains("rustls")))
            .collect();
        assert!(
            lines.len() == 2 && lines.iter().all(|l| l.contains("conflict")),
            "the catalogue marks both: {lines:?}"
        );
        let ask = json!({"session_id": "s-c", "cwd": root,
                         "prompt": "Which TLS backend do we use in this project now?"});
        let (_, out, _) = run(root, &["hook", "UserPromptSubmit"], &ask.to_string());
        assert_eq!(
            out.matches("[muninn:decision:conflict with #").count(),
            2,
            "the prompt hook marks both: {out}"
        );
        let (_, out, _) = run(root, &["why", "which TLS backend do we use"], "");
        assert!(out.contains("[muninn:conflict]"), "why says so: {out}");
    }
}

/// I1 through the idle release. The older session's last message was never answered: Stop
/// holds it, and `maintain` puts it on record once its transcript has been idle for an hour.
/// The change typed the next day in another session is on record by then, and stands.
#[test]
fn a_last_message_released_by_maintain_does_not_retire_a_later_change() {
    let p = project();
    let root = p.path();
    // the one-time passes of a new store have run, as on any store in use
    assert_eq!(run(root, &["maintain"], "").0, 0);
    let a = write(root, "a.jsonl", &[user("s-a", DAY1, OLD)]);
    let b = write(
        root,
        "b.jsonl",
        &[user("s-b", DAY2, NEW), say("s-b", "Done.")],
    );
    hook(root, "Stop", "s-a", &a);
    assert_eq!(
        store(root)
            .query_row("SELECT count(*) FROM record", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0,
        "held: nothing answered it"
    );
    hook(root, "Stop", "s-b", &b);
    let two_hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(7_200);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&a)
        .unwrap()
        .set_modified(two_hours_ago)
        .unwrap();
    assert_eq!(run(root, &["maintain"], "").0, 0);
    assert_eq!(
        typed(root, "openssl").0,
        0,
        "the released message is on record"
    );
    assert_eq!(typed(root, "rustls").0, 0, "the later change stands");
}

/// I1 through `muninn remember`. The agent keeps a sentence the person typed in an older
/// session the log names: it is theirs, at the date they typed it, and that is before the
/// change. The change stands.
#[test]
fn a_sentence_remembered_from_an_older_session_does_not_retire_a_later_change() {
    let p = project();
    let root = p.path();
    let a = write(root, "a.jsonl", &[user("s-a", DAY1, OLD)]);
    let b = write(
        root,
        "b.jsonl",
        &[user("s-b", DAY2, NEW), say("s-b", "Done.")],
    );
    // a read hook of the older session names its transcript in the session log
    let seen = json!({"session_id": "s-a", "cwd": root, "prompt": "hello", "transcript_path": a});
    assert_eq!(
        run(root, &["hook", "UserPromptSubmit"], &seen.to_string()).0,
        0
    );
    hook(root, "Stop", "s-b", &b);
    let (code, out, err) = run(
        root,
        &[
            "--json",
            "remember",
            "--kind",
            "decision",
            "--session",
            "s-a",
            OLD,
        ],
        "",
    );
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["origin"], "user_said", "{v}");
    assert_eq!(typed(root, "rustls").0, 0, "the later change stands");
}

/// The agent of `sid` starts a worker with `task`.
fn hand(sid: &str, task: &str) -> Vec<Value> {
    vec![
        json!({"type": "assistant", "sessionId": sid, "message": {"role": "assistant", "content": [
            {"type": "tool_use", "id": "t1", "name": "mcp__team__start", "input": {"task": task}}]}}),
        json!({"type": "user", "sessionId": sid, "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": "started"}]}}),
    ]
}

/// The typed records active at the end that hold `needle`: (kind, origin, session).
fn standing(root: &Path, needle: &str) -> Vec<(String, String, String)> {
    let c = store(root);
    let mut st = c
        .prepare(
            "SELECT kind, origin, session_id FROM record WHERE invalid = 0 AND kind <> 'episode' \
             AND instr(object, ?1) > 0 ORDER BY id",
        )
        .unwrap();
    let rows = st
        .query_map([needle], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    rows.map(Result::unwrap).collect()
}

/// C1. The person states something in the turn whose agent starts a worker with a task that
/// copies the sentence. The worker stops first, as it does in a live session, and its turn
/// types the sentence; the coordinator's turn then shows that prompt was an agent's and revokes
/// the row. The person's own sentence, in the coordinator's turn, has the same hash: it is on
/// record as theirs, at their turn, whichever session is read first. 1.2.0 kept it in both
/// orders, and the build before this fix lost it when the worker was read first.
#[test]
fn what_the_person_says_in_the_turn_that_hands_a_copy_to_a_worker_is_kept() {
    const RULE: &str = "Never commit the local settings files to the repository.";
    const CHOICE: &str = "We go with rustls for the TLS backend of the proxy.";
    // (the kind, the person's prompt, the task, a word of the statement)
    let cases = [
        (
            "invariant",
            RULE.to_string(),
            format!("Prepare the release branch for the catalogue, version two. {RULE}"),
            "local settings files",
        ),
        (
            "invariant",
            format!("Start a worker for the catalogue branch, please. {RULE}"),
            format!("Prepare the release branch for the catalogue, version two. {RULE}"),
            "local settings files",
        ),
        (
            "decision",
            CHOICE.to_string(),
            format!("Write the upgrade notes for the proxy and its settings. {CHOICE}"),
            "rustls",
        ),
    ];
    for (kind, said, task, needle) in &cases {
        for worker_first in [true, false] {
            let p = project();
            let root = p.path();
            let mut lines = vec![user("coord", "2026-11-02T09:59:00Z", said)];
            lines.extend(hand("coord", task));
            lines.push(say("coord", "Started."));
            let coord = write(root, "coord.jsonl", &lines);
            let worker = write(
                root,
                "worker.jsonl",
                &[
                    user("worker", "2026-11-02T10:00:00Z", task),
                    say("worker", "Branch ready."),
                ],
            );
            if worker_first {
                hook(root, "Stop", "worker", &worker);
                hook(root, "Stop", "coord", &coord);
            } else {
                hook(root, "Stop", "coord", &coord);
                hook(root, "Stop", "worker", &worker);
            }
            let rows: Vec<_> = standing(root, needle)
                .into_iter()
                .filter(|(k, _, _)| k == kind)
                .collect();
            assert_eq!(
                rows,
                vec![(kind.to_string(), "user_said".into(), "coord".into())],
                "{kind}, {said:?}, worker first: {worker_first}"
            );
        }
    }
}

/// C1, said in an earlier turn of the coordinator's session than the one that hands the copy
/// over: the two turns read in one batch, and one Stop per turn with the worker's Stop before
/// the first, between the two and after both.
#[test]
fn what_the_person_said_a_turn_before_the_copy_was_handed_over_is_kept() {
    const CHOICE: &str = "We go with rustls for the TLS backend of the proxy.";
    let task = format!("Write the upgrade notes for the proxy and its settings. {CHOICE}");
    let first = vec![
        user("coord", "2026-11-02T09:50:00Z", CHOICE),
        say("coord", "Noted."),
    ];
    let mut second = vec![user(
        "coord",
        "2026-11-02T09:59:00Z",
        "Now hand the upgrade notes to a worker, please.",
    )];
    second.extend(hand("coord", &task));
    second.push(say("coord", "Started."));
    // W: the worker's Stop. 1 and 2: the coordinator's Stop after its first and second turn.
    // B: one Stop of the coordinator with both turns written.
    for order in ["WB", "BW", "W12", "1W2", "12W"] {
        let p = project();
        let root = p.path();
        let worker = root.join("worker.jsonl");
        let coord = root.join("coord.jsonl");
        for step in order.chars() {
            match step {
                'W' => {
                    write(
                        root,
                        "worker.jsonl",
                        &[
                            user("worker", "2026-11-02T10:00:00Z", &task),
                            say("worker", "Done."),
                        ],
                    );
                    hook(root, "Stop", "worker", &worker);
                }
                '1' => {
                    write(root, "coord.jsonl", &first);
                    hook(root, "Stop", "coord", &coord);
                }
                '2' => {
                    append(&coord, &second);
                    hook(root, "Stop", "coord", &coord);
                }
                _ => {
                    write(root, "coord.jsonl", &first);
                    append(&coord, &second);
                    hook(root, "Stop", "coord", &coord);
                }
            }
        }
        assert_eq!(
            standing(root, "rustls"),
            vec![("decision".into(), "user_said".into(), "coord".into())],
            "order {order}"
        );
    }
}

/// C2. A turn whose command failed gets a dead end. The same command passing later retires the
/// dead end, and a later dead end of the same command replaces it: either way the turn's episode,
/// the person's message and the reply, is still what `recall` and `why` answer with.
#[test]
fn a_command_that_failed_and_then_passed_keeps_its_turn_in_recall() {
    const ASK: &str = "The harbour tide tables for the lighthouse keepers come from the almanac \
                       of the northern pilots, check that the parser reads them.";
    const Q: &str = "harbour tide tables lighthouse keepers almanac northern pilots parser";
    for second in [None, Some("Exit code 101\nfailed on the footer row")] {
        let p = project();
        let root = p.path();
        let mut first = vec![user("s1", "2026-11-02T10:00:00Z", ASK)];
        first.extend(bash(
            "s1",
            "t1",
            "cargo test tides",
            Some("Exit code 101\nfailed"),
        ));
        first.push(say("s1", "The tide parser test fails on the header row."));
        let t = write(root, "s1.jsonl", &first);
        hook(root, "Stop", "s1", &t);
        let mut then = vec![user(
            "s1",
            "2026-11-02T10:05:00Z",
            "Skip the header row and run the tide parser test once more, please.",
        )];
        then.extend(bash("s1", "t2", "cargo test tides", second));
        if second.is_some() {
            // two failed runs: a dead end that says something else, under the same command
            then.extend(bash("s1", "t3", "cargo test tides", second));
        }
        then.push(say("s1", "Done with the second run."));
        append(&t, &then);
        hook(root, "Stop", "s1", &t);
        let c = store(root);
        let (retired, via): (i64, Option<String>) = c
            .query_row(
                "SELECT invalid, invalidated_via FROM record WHERE kind = 'deadend' ORDER BY id LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let how = if second.is_none() { "succeeded" } else { "key" };
        assert_eq!(
            (retired, via.as_deref()),
            (1, Some(how)),
            "the first dead end"
        );
        let (code, out, err) = run(root, &["recall", Q], "");
        assert_eq!(code, 0, "{err}");
        assert!(
            out.contains("almanac of the northern pilots"),
            "recall ({how}): {out:?} {err:?}"
        );
        let (code, out, err) = run(
            root,
            &[
                "why",
                "where do the harbour tide tables of the lighthouse keepers come from",
            ],
            "",
        );
        // 3: no typed record answers, the episodes are what there is
        assert!(code == 0 || code == 3, "{code} {err}");
        assert!(
            out.contains("almanac of the northern pilots"),
            "why ({how}): {out:?} {err:?}"
        );
    }
}

/// R1. The person revokes a rule. A session dated before the revocation, read after it, that
/// holds the same words is older word: the rule stays revoked. Only said again after the
/// revocation does it come back. 1.2.0 kept it revoked; the builds before this fix put it back.
#[test]
fn a_revoked_rule_stays_revoked_when_an_older_session_is_read_later() {
    const RULE: &str = "Never commit the local settings files to the repository.";
    let p = project();
    let root = p.path();
    let a = write(
        root,
        "a.jsonl",
        &[user("s-a", DAY1, RULE), say("s-a", "Done.")],
    );
    let b = write(
        root,
        "b.jsonl",
        &[user("s-b", DAY2, RULE), say("s-b", "Done.")],
    );
    hook(root, "Stop", "s-a", &a);
    let id: i64 = store(root)
        .query_row("SELECT id FROM record WHERE kind = 'invariant'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let (code, _, err) = run(root, &["revoke", &id.to_string()], "");
    assert_eq!(code, 0, "{err}");
    hook(root, "Stop", "s-b", &b);
    assert_eq!(
        typed(root, "local settings files"),
        (1, Some("revoked".into())),
        "said before the revocation, read after it"
    );
    // said again now, after the revocation: a turn with no date is dated when it is read
    let c = write(
        root,
        "c.jsonl",
        &[
            json!({"type": "user", "sessionId": "s-c", "message": {"role": "user", "content": RULE}}),
            say("s-c", "Done."),
        ],
    );
    hook(root, "Stop", "s-c", &c);
    assert_eq!(
        typed(root, "local settings files").0,
        0,
        "said again after it"
    );
}
