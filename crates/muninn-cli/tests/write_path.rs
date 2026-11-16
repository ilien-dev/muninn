//! What the write path costs and what it leaves behind, seen from outside the binary: the
//! log a read hook has to replay, the lock held while git runs, the session log and the
//! operational rows that grew without bound.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_muninn");

/// Run one command in `root` with the Markdown mirror off; returns (exit code, stdout, stderr).
fn run(root: &Path, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let mut env = env.to_vec();
    env.push(("MUNINN_NO_PROJECT", "1"));
    run_mirrored(root, args, stdin, &env)
}

/// Run one command in `root` as a user's store runs it, Markdown mirror included.
fn run_mirrored(
    root: &Path,
    args: &[&str],
    stdin: &str,
    env: &[(&str, &str)],
) -> (i32, String, String) {
    let mut cmd = Command::new(BIN);
    cmd.arg("--cwd")
        .arg(root)
        .args(args)
        .env_remove("MUNINN_NO_PROJECT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
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
    let (code, _, err) = run(tmp.path(), &["init", "--keep-native"], "", &[]);
    assert_eq!(code, 0, "init: {err}");
    tmp
}

fn store(root: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(root.join(".muninn/muninn.db")).unwrap()
}

fn hook(root: &Path, event: &str, extra: serde_json::Value) -> (i32, String, String) {
    let mut v = serde_json::json!({ "session_id": "s-write-path", "cwd": root });
    for (k, val) in extra.as_object().unwrap() {
        v[k] = val.clone();
    }
    run(root, &["hook", event], &v.to_string(), &[])
}

/// Bytes of write-ahead log beside the store: what the next connection has to read through
/// before its first query, when it is the only one connected.
fn log_bytes(root: &Path) -> u64 {
    std::fs::metadata(root.join(".muninn/muninn.db-wal")).map_or(0, |m| m.len())
}

/// A writer that closes while a read hook is connected cannot fold its log into the store on
/// the way out: SQLite does that only for the last connection. The log then stays, and every
/// read hook after it, each one the only connection for a few milliseconds, replays it whole
/// before its first query: 2.69 ms of `open` against 0.60 ms with a 4.2 MB log on a
/// 20,000-record store. The write path now folds the log itself before it closes.
#[test]
fn the_write_path_leaves_no_log_for_the_next_read_hook_to_replay() {
    let p = project();
    let root = p.path();
    for writer in ["maintain", "Stop"] {
        // enough written that the log is not empty: rows the writer's fold has to carry
        {
            let c = store(root);
            c.execute_batch(
                "CREATE TABLE IF NOT EXISTS filler(x BLOB); \
                 INSERT INTO filler VALUES (randomblob(200000)); DELETE FROM filler;",
            )
            .unwrap();
        }
        // a read hook's connection, idle between two queries, while the writer runs and exits
        let reader = rusqlite::Connection::open_with_flags(
            root.join(".muninn/muninn.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let _: i64 = reader
            .query_row("SELECT count(*) FROM meta", [], |r| r.get(0))
            .unwrap();
        let (code, _, err) = match writer {
            "maintain" => run(root, &["maintain"], "", &[]),
            _ => hook(root, writer, serde_json::json!({})),
        };
        assert_eq!(code, 0, "{writer}: {err}");
        assert_eq!(
            log_bytes(root),
            0,
            "{writer} left its log for the read hooks to replay"
        );
        drop(reader);
    }
}

/// WP-01. Every hook of a session appends a line to the session log and nothing took one out;
/// `maintain` now rewrites it down to the lines its readers can tell apart and says how many
/// went. What the log names is unchanged: the same session is found for a transcript after.
#[test]
fn maintain_takes_the_repeats_out_of_the_session_log() {
    let p = project();
    let root = p.path();
    let log = root.join(".muninn/log/sessions.jsonl");
    let transcript = root.join("s-write-path.jsonl");
    std::fs::write(&transcript, "").unwrap();
    // 200 hooks of one session, as a long session leaves them
    for _ in 0..200 {
        let (code, _, err) = hook(
            root,
            "UserPromptSubmit",
            serde_json::json!({ "prompt": "ok", "transcript_path": transcript }),
        );
        assert_eq!(code, 0, "{err}");
    }
    let lines = || std::fs::read_to_string(&log).unwrap().lines().count();
    assert_eq!(lines(), 200);
    let (code, out, err) = run(root, &["--json", "maintain"], "", &[]);
    assert_eq!(code, 0, "{err}");
    let st: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(st["session_lines_dropped"], 198, "{out}");
    assert_eq!(lines(), 2);
    // the session is still the one the log names for the transcript: `remember` reads it
    let (code, out, err) = run(
        root,
        &[
            "--json",
            "remember",
            "--kind",
            "decision",
            "--session",
            "s-write-path",
            "--transcript",
            transcript.to_str().unwrap(),
            "the retry budget is three attempts",
        ],
        "",
        &[],
    );
    assert_eq!(code, 0, "{out} {err}");
    assert!(!out.contains("the session log does not name"), "{out}");
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

/// WP-19. The commit pass asks git whether a word is still in the tree, tens of milliseconds
/// a question on Windows, and asked inside its closing transaction: a `Stop` that met it
/// waited 700 ms for the write lock and failed with `database is locked`. No write lock is
/// held while any git command of `maintain` runs: each time git is about to start, another
/// connection takes the write lock at once.
///
/// `maintain` is held before each git command by a seam only a debug build has: under
/// `cargo test --release` the test says so and does nothing.
#[test]
fn maintain_holds_no_write_lock_while_git_runs() {
    if !cfg!(debug_assertions) {
        eprintln!("skipped: the seam that holds maintain before git is in debug builds only");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(root.join("config/stack.json"), "{\"value\": \"openssl\"}\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "base"]);
    let (code, _, err) = run(root, &["init", "--keep-native", "--no-boot-block"], "", &[]);
    assert_eq!(code, 0, "{err}");
    // a decision naming a value, and a commit that swaps the value out: the pass asks git
    // how many files hold the word before it retires the decision
    store(root)
        .execute_batch(
            "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
             VALUES('decision','said:state:tls','user_decision','the tls backend is openssl',\
                    'user: the tls backend is openssl\n','user_said',3,'s','tls',1);",
        )
        .unwrap();
    std::fs::write(root.join("config/stack.json"), "{\"value\": \"rustls\"}\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "update dependencies"]);

    let hold = tempfile::tempdir().unwrap();
    let note = hold.path().join("held");
    let mut maintain = Command::new(BIN)
        .arg("--cwd")
        .arg(root)
        .arg("maintain")
        .env("MUNINN_NO_PROJECT", "1")
        .env("MUNINN_TEST_HOLD_GIT", hold.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // (the git subcommand about to run, whether the write lock could be taken at once)
    let mut probes: Vec<(String, bool)> = Vec::new();
    let other = store(root);
    other.busy_timeout(std::time::Duration::ZERO).unwrap();
    let status = loop {
        if let Some(st) = maintain.try_wait().unwrap() {
            break st;
        }
        if let Ok(sub) = std::fs::read_to_string(&note) {
            let free = other.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").is_ok();
            probes.push((sub, free));
            std::fs::remove_file(&note).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert!(status.success());
    assert!(
        probes.iter().any(|(sub, _)| sub == "grep"),
        "the pass asked git about a word: {probes:?}"
    );
    let locked: Vec<&str> = probes
        .iter()
        .filter(|(_, free)| !free)
        .map(|(sub, _)| sub.as_str())
        .collect();
    assert!(
        locked.is_empty(),
        "the write lock was held while git ran: {locked:?} of {probes:?}"
    );
    // and the pass did its work: the decision the swap replaced is retired
    let retired: i64 = store(root)
        .query_row(
            "SELECT count(*) FROM record WHERE invalid = 1 AND object LIKE '%openssl%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(retired, 1);
}

/// A transcript of `msgs`, each answered, for session `s-write-path`.
fn transcript(root: &Path, name: &str, msgs: &[&str]) -> std::path::PathBuf {
    let mut t = String::new();
    for (i, m) in msgs.iter().enumerate() {
        t += &serde_json::json!({"type":"user","sessionId":"s-write-path","cwd":root,
            "timestamp": format!("2026-11-10T10:{i:02}:00.000Z"),
            "message":{"role":"user","content":m}})
        .to_string();
        t.push('\n');
        t += &serde_json::json!({"type":"assistant","sessionId":"s-write-path",
            "timestamp": format!("2026-11-10T10:{i:02}:30.000Z"),
            "message":{"role":"assistant","content":[{"type":"text","text":"Understood."}]}})
        .to_string();
        t.push('\n');
    }
    let p = root.join(name);
    std::fs::write(&p, t).unwrap();
    p
}

/// WP-11. Every ingest said the records had changed, the 63 a `Stop` runs over finished
/// transcripts included, so every `maintain` rewrote the Markdown file of every record: 4,214
/// files and 0.6 s on a copy of one store, with nothing new. A run with nothing new writes
/// no file; a run after something was said writes that record's.
#[test]
fn maintain_writes_no_markdown_when_nothing_changed() {
    let p = project();
    let root = p.path();
    let said = transcript(
        root,
        "said.jsonl",
        &["We always run the linter before a commit, never skip it."],
    );
    let maintain = || -> serde_json::Value {
        let (code, out, err) = run_mirrored(root, &["--json", "maintain"], "", &[]);
        assert_eq!(code, 0, "{err}");
        serde_json::from_str(out.trim()).unwrap()
    };
    let stop = |t: &Path| {
        let input =
            serde_json::json!({ "session_id": "s-write-path", "cwd": root, "transcript_path": t });
        let (code, _, err) = run_mirrored(root, &["hook", "Stop"], &input.to_string(), &[]);
        assert_eq!(code, 0, "{err}");
    };
    stop(&said);
    let files = || -> Vec<(std::path::PathBuf, std::time::SystemTime)> {
        let mut out = Vec::new();
        for kind in std::fs::read_dir(root.join(".muninn/records"))
            .unwrap()
            .flatten()
        {
            if !kind.path().is_dir() {
                continue;
            }
            for f in std::fs::read_dir(kind.path()).unwrap().flatten() {
                out.push((f.path(), f.metadata().unwrap().modified().unwrap()));
            }
        }
        out.sort();
        out
    };
    maintain();
    let before = files();
    assert!(!before.is_empty(), "the turn left records and their files");
    // the session's transcript is noted, so this run ingests it again and finds nothing new
    let second = maintain();
    assert_eq!(second["projected"], 0, "{second}");
    assert_eq!(files(), before, "no file was written again");
    // and a `Stop` that ingests nothing does not say the records changed, so the next
    // `maintain` does not even compare the files
    stop(&said);
    let changed: String = store(root)
        .query_row(
            "SELECT value FROM meta WHERE key = 'records_changed_since_project'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(changed, "0");
    // something new is said: its files are written, the others are left as they are
    let more = transcript(
        root,
        "said.jsonl",
        &[
            "We always run the linter before a commit, never skip it.",
            "From now on the release notes are written by hand, never generated.",
        ],
    );
    stop(&more);
    maintain();
    let after = files();
    assert!(after.len() > before.len());
    for (path, at) in &before {
        assert!(
            after.contains(&(path.clone(), *at)),
            "{} was written again",
            path.display()
        );
    }
}

/// WP-13. The `heartbeat` table took a row for every hook run and nothing took one out:
/// 147,979 rows and 19.0 MB on one store, 30 % of its file. `maintain` keeps thirty days of
/// them. Its one reader is `muninn ledger`, which counts the prompt hooks ever recorded: the
/// rows that go are added to a count that stays, so the ledger says the same before and after.
#[test]
fn maintain_drops_old_heartbeat_rows_and_the_ledger_counts_the_same() {
    let p = project();
    let root = p.path();
    let day: i64 = 86_400_000;
    let now: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    {
        let c = store(root);
        let mut ins = c
            .prepare("INSERT INTO heartbeat(hook, session_id, started_at, ok, ms) VALUES (?1, 's', ?2, 1, 1.0)")
            .unwrap();
        // forty days old: 300 prompts and 200 other hooks; yesterday: 7 prompts and 5 others
        for i in 0..300 {
            ins.execute(rusqlite::params!["UserPromptSubmit", now - 40 * day + i])
                .unwrap();
        }
        for i in 0..200 {
            ins.execute(rusqlite::params!["PreToolUse", now - 40 * day + i])
                .unwrap();
        }
        for i in 0..7 {
            ins.execute(rusqlite::params!["UserPromptSubmit", now - day + i])
                .unwrap();
        }
        for i in 0..5 {
            ins.execute(rusqlite::params!["Stop", now - day + i])
                .unwrap();
        }
    }
    let ledger = || -> serde_json::Value {
        let (code, out, err) = run(root, &["--json", "ledger"], "", &[]);
        assert_eq!(code, 0, "{err}");
        serde_json::from_str(out.trim()).unwrap()
    };
    let rows = || -> i64 {
        store(root)
            .query_row(
                "SELECT count(*) FROM heartbeat WHERE session_id = 's'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    let before = ledger();
    assert_eq!(before["prompts"], 307);
    assert_eq!(rows(), 512);
    for run_no in 0..2 {
        let (code, out, err) = run(root, &["--json", "maintain"], "", &[]);
        assert_eq!(code, 0, "{err}");
        let st: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(
            st["heartbeats_dropped"],
            if run_no == 0 { 500 } else { 0 },
            "{out}"
        );
        assert_eq!(rows(), 12, "thirty days of rows are kept");
        assert_eq!(
            ledger(),
            before,
            "the ledger reads the same after run {run_no}"
        );
    }
}

/// R-10. SessionStart starts the detached `maintain` on another thread while it reads, and
/// joins it before it answers: the writer still runs, once, and the hook still answers with
/// its context without waiting for the writer to finish.
#[test]
fn a_session_start_still_starts_the_writer_and_answers_first() {
    let p = project();
    let root = p.path();
    let key = |k: &str| -> Option<String> {
        store(root)
            .query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| r.get(0))
            .ok()
    };
    // `init` leaves no integrity check on record; `maintain` leaves one
    assert_eq!(key("quick_check_result"), None);
    let _ = std::fs::remove_file(root.join(".muninn/log/maintain.stamp"));
    let (code, out, err) = hook(
        root,
        "SessionStart",
        serde_json::json!({ "source": "startup" }),
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("additionalContext"), "{out}");
    assert!(root.join(".muninn/log/maintain.stamp").is_file());
    let mut ran = false;
    for _ in 0..200 {
        if key("quick_check_result").is_some() {
            ran = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(ran, "the detached maintain ran");
    // inside the two minutes the next session start starts no writer
    let stamp = std::fs::read_to_string(root.join(".muninn/log/maintain.stamp")).unwrap();
    let (code, _, _) = hook(
        root,
        "SessionStart",
        serde_json::json!({ "source": "startup" }),
    );
    assert_eq!(code, 0);
    assert_eq!(
        std::fs::read_to_string(root.join(".muninn/log/maintain.stamp")).unwrap(),
        stamp
    );
}

/// R-19. The write hooks say where their time went in every heartbeat finish line. A read
/// hook says it only when `MUNINN_PHASES` is set: its finish line is otherwise as short as it
/// was, and the hook does not keep the steps.
#[test]
fn a_read_hook_keeps_its_phases_only_when_asked() {
    let p = project();
    let root = p.path();
    let last_finish = |hook_name: &str| -> serde_json::Value {
        std::fs::read_to_string(root.join(".muninn/log/heartbeat.jsonl"))
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .rfind(|v| v["ev"] == "finish" && v["hook"] == hook_name)
            .unwrap_or_else(|| panic!("{hook_name} left a finish line"))
    };
    let input = serde_json::json!({ "session_id": "s-write-path", "cwd": root }).to_string();
    for read_hook in ["SessionStart", "SubagentStart", "UserPromptSubmit"] {
        let (code, _, err) = run(root, &["hook", read_hook], &input, &[]);
        assert_eq!(code, 0, "{err}");
        assert!(
            last_finish(read_hook).get("phases").is_none(),
            "{read_hook} by default: {}",
            last_finish(read_hook)
        );
        let (code, _, err) = run(
            root,
            &["hook", read_hook],
            &input,
            &[("MUNINN_PHASES", "1")],
        );
        assert_eq!(code, 0, "{err}");
        assert!(
            last_finish(read_hook)["phases"]
                .as_array()
                .is_some_and(|a| !a.is_empty()),
            "{read_hook} when asked: {}",
            last_finish(read_hook)
        );
    }
    let (code, _, err) = run(root, &["hook", "Stop"], &input, &[]);
    assert_eq!(code, 0, "{err}");
    assert!(
        last_finish("Stop")["phases"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "{}",
        last_finish("Stop")
    );
}

fn mirror_is_asked_for(root: &Path) -> bool {
    store(root)
        .query_row(
            "SELECT value FROM meta WHERE key = 'records_changed_since_project'",
            [],
            |r| r.get::<_, String>(0),
        )
        .is_ok_and(|v| v == "1")
}

/// WP-11, the writers that are not an ingest. Only an ingest that wrote asks for the mirror
/// now, so every other writer of records has to ask for itself. A `Stop` whose ingests wrote
/// nothing and whose git capture stored a commit used to be covered by the ingests saying so
/// on every call.
#[test]
fn a_stop_that_only_captured_a_commit_asks_for_the_mirror() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "base"]);
    let (code, _, err) = run_mirrored(root, &["init", "--keep-native", "--no-boot-block"], "", &[]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = run_mirrored(root, &["maintain"], "", &[]);
    assert_eq!(code, 0, "{err}");
    assert!(
        !mirror_is_asked_for(root),
        "the mirror is up to date after maintain"
    );
    std::fs::write(root.join("a.txt"), "two\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "switch the value"]);
    let input = serde_json::json!({ "session_id": "s-write-path", "cwd": root }).to_string();
    let (code, _, err) = run_mirrored(root, &["hook", "Stop"], &input, &[]);
    assert_eq!(code, 0, "{err}");
    let commits: i64 = store(root)
        .query_row(
            "SELECT count(*) FROM record WHERE object = 'switch the value'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(commits, 1, "Stop stored the commit");
    assert!(mirror_is_asked_for(root), "and asked for its file");
    // the next maintain writes it and the request is settled
    let (code, out, err) = run_mirrored(root, &["--json", "maintain"], "", &[]);
    assert_eq!(code, 0, "{err}");
    let st: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert!(st["projected"].as_u64().unwrap() >= 1, "{out}");
    assert!(!mirror_is_asked_for(root));
}

/// WP-11. `muninn remember` stores a record outside any ingest: its file is written at once,
/// and what it retired or restated is left to the next `maintain`, which it asks for.
#[test]
fn remember_writes_its_records_file_and_asks_for_the_mirror() {
    let p = project();
    let root = p.path();
    let (code, _, err) = run_mirrored(root, &["maintain"], "", &[]);
    assert_eq!(code, 0, "{err}");
    assert!(!mirror_is_asked_for(root));
    let (code, out, err) = run_mirrored(
        root,
        &[
            "--json",
            "remember",
            "--kind",
            "decision",
            "--session",
            "s-write-path",
            "the retry budget is three attempts",
        ],
        "",
        &[],
    );
    assert_eq!(code, 0, "{out} {err}");
    let r: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    let file = root.join(format!(".muninn/records/decision/{}.md", r["id"]));
    assert!(file.is_file(), "{} is written", file.display());
    assert!(mirror_is_asked_for(root));
}

/// WP-11. `revoke` writes the revoked record's file at once and asks for the rest (what the
/// record had replaced is given back, and those files are the next `maintain`'s).
#[test]
fn revoke_writes_its_file_and_asks_for_the_mirror() {
    let p = project();
    let root = p.path();
    store(root)
        .execute_batch(
            "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
             VALUES(1,'decision','said:state:a','user_decision','logs go to stderr','logs go to stderr','user_said',3,'s','h1',1);",
        )
        .unwrap();
    let (code, _, err) = run_mirrored(root, &["maintain"], "", &[]);
    assert_eq!(code, 0, "{err}");
    // maintain did not know the row was put in by hand: ask, then settle
    store(root)
        .execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('records_changed_since_project', '1')",
            [],
        )
        .unwrap();
    let (code, _, err) = run_mirrored(root, &["maintain"], "", &[]);
    assert_eq!(code, 0, "{err}");
    let file = root.join(".muninn/records/decision/1.md");
    assert!(std::fs::read_to_string(&file)
        .unwrap()
        .contains("invalid: false"));
    let (code, _, err) = run_mirrored(root, &["revoke", "1"], "", &[]);
    assert_eq!(code, 0, "{err}");
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("invalid: true"),
        "a revoked record's file says so at once"
    );
    assert!(
        mirror_is_asked_for(root),
        "and what it gave back is asked for"
    );
}
