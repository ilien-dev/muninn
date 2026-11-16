//! What usage-v4's section D took out, seen from outside the binary: the embedding sidecar,
//! the symbol graph, the directory and symbol cues, `PostToolUse` and the turn-context log.
//! No store in use held a vector, and no delivery in any store's ledger was made by one of
//! those cues. Their commands and switches are gone, a hooks file that still names
//! `PostToolUse` costs nothing, and the write path clears what they left on disk.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_muninn");

/// Run one command in `root`; returns (exit code, stdout, stderr).
fn run(root: &Path, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(BIN);
    cmd.arg("--cwd")
        .arg(root)
        .args(args)
        .env("MUNINN_NO_PROJECT", "1")
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

/// clap's exit code for a subcommand or a flag it does not know.
const USAGE: i32 = 2;

#[test]
fn the_commands_and_flags_of_the_removed_parts_are_gone() {
    let p = project();
    for args in [
        &["embed", "--status"][..],
        &["symbols", "--status"],
        &["init", "--cues"],
        &["cues", "--file", "src/a.rs"],
        &["cues", "--symbol", "verify_token"],
    ] {
        let (code, out, _) = run(p.path(), args, "", &[]);
        assert_eq!(code, USAGE, "`muninn {}` is refused: {out}", args.join(" "));
    }
    // the invoked form of the cues that remain still answers
    let (code, _, err) = run(
        p.path(),
        &["cues", "--event", "session_start", "--ungated"],
        "",
        &[],
    );
    assert_eq!(code, 0, "{err}");
}

/// A hooks file written by an earlier `muninn init --codex`, or an older plugin, still calls
/// `hook PostToolUse` after every tool. It gets exit 0 and nothing else: no output, no line in
/// the heartbeat log, no turn-context log.
#[test]
fn a_stale_post_tool_use_hook_exits_0_and_writes_nothing() {
    let p = project();
    std::fs::create_dir_all(p.path().join("src")).unwrap();
    std::fs::write(p.path().join("src/a.rs"), "fn a() {}\n").unwrap();
    let input = serde_json::json!({
        "session_id": "s1", "cwd": p.path(), "hook_event_name": "PostToolUse",
        "tool_name": "Read", "tool_input": {"file_path": p.path().join("src/a.rs")},
    })
    .to_string();
    let (code, out, err) = run(p.path(), &["hook", "PostToolUse"], &input, &[]);
    assert_eq!((code, out.trim(), err.trim()), (0, "", ""));
    let log = p.path().join(".muninn/log");
    assert!(!log.join("turn_context.jsonl").exists());
    let beats = std::fs::read_to_string(log.join("heartbeat.jsonl")).unwrap_or_default();
    assert!(!beats.contains("PostToolUse"), "{beats}");
}

/// Before an edit the hook gives the F2 verdict and nothing else. With the switch that used to
/// turn tool-time cues on, and a dead end recorded under the edited file's directory, an edit
/// is given no block.
#[test]
fn an_edit_is_given_no_cue_block() {
    let p = project();
    let c = store(p.path());
    c.execute_batch(
        "INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,anchor_path,session_id,dedup_hash,created_at) \
         VALUES(1,'deadend','cookie jwt','tried_and_failed','jwt in a cookie','user: jwt in a cookie broke the mobile client','user_said',3,'src/auth/jwt.rs','s','h1',1793000000000);",
    )
    .unwrap();
    // a store made before this build could hold these rows; this build's schema refuses them
    let _ = c.execute_batch(
        "INSERT INTO cue(record_id, kind, key, grp) VALUES (1,'event','pre_edit',0), (1,'dir','src/auth',0);",
    );
    drop(c);
    // the setting is refused now; a project set up by an earlier build still holds the key
    let (code, _, err) = run(p.path(), &["config", "cues", "on"], "", &[]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("cues") && err.contains("removed"), "{err}");
    let config = p.path().join(".muninn/config.json");
    assert!(
        !std::fs::read_to_string(&config)
            .unwrap_or_default()
            .contains("cues"),
        "a refused setting is not written"
    );
    std::fs::write(&config, "{\n  \"cues\": true\n}\n").unwrap();
    // read back, it is what the file holds
    let (code, out, _) = run(p.path(), &["config", "cues"], "", &[]);
    assert_eq!((code, out.trim()), (0, "cues = true"));
    let input = serde_json::json!({
        "session_id": "s1", "cwd": p.path(), "hook_event_name": "PreToolUse",
        "tool_name": "Edit", "tool_input": {"file_path": "src/auth/session.rs", "old_string": "a", "new_string": "b"},
    })
    .to_string();
    let (code, out, _) = run(
        p.path(),
        &["hook", "PreToolUse"],
        &input,
        &[("MUNINN_CUES", "1")],
    );
    assert_eq!((code, out.trim()), (0, ""), "an edit gets no memory block");
}

/// `maintain` builds no symbol graph, reports none of the removed passes, and deletes the
/// turn-context log an earlier build left behind: nothing reads it and nothing trims it now.
#[test]
fn maintain_builds_no_graph_and_clears_the_turn_context_log() {
    let p = project();
    std::fs::create_dir_all(p.path().join("src")).unwrap();
    std::fs::write(p.path().join("src/a.rs"), "pub fn verify_token() {}\n").unwrap();
    let log = p.path().join(".muninn/log/turn_context.jsonl");
    std::fs::write(
        &log,
        "{\"at\":1,\"session\":\"s\",\"files\":[\"src/a.rs\"],\"symbols\":[]}\n",
    )
    .unwrap();
    let (code, out, err) = run(p.path(), &["--json", "maintain"], "", &[]);
    assert_eq!(code, 0, "{err}");
    let stats: serde_json::Value = serde_json::from_str(out.trim()).expect(&out);
    for gone in ["symbols_indexed", "embedded", "variants_retired"] {
        assert!(stats.get(gone).is_none(), "{gone} is not reported: {out}");
    }
    assert!(!log.exists(), "the turn-context log is deleted");
    let tables: i64 = store(p.path())
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN \
             ('symbol', 'symbol_ref', 'symbol_file', 'turn_context', 'record_vec')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0);
}

/// A store that held a symbol graph is mostly that graph's pages. Its first `maintain` with
/// this build drops the tables and gives the pages back to the file system, once.
#[test]
fn maintain_gives_back_the_pages_of_a_dropped_graph_once() {
    let p = project();
    let db = p.path().join(".muninn/muninn.db");
    std::fs::remove_file(&db).unwrap();
    for wal in ["muninn.db-wal", "muninn.db-shm"] {
        let _ = std::fs::remove_file(p.path().join(".muninn").join(wal));
    }
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch(include_str!("../../muninn-core/src/testdata/schema_v4.sql"))
            .unwrap();
        c.execute_batch(
            "INSERT INTO meta(key, value) VALUES ('schema_version', '4') \
               ON CONFLICT(key) DO UPDATE SET value = excluded.value; \
             INSERT INTO record(id,kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
               VALUES(1,'invariant','never push to main','must','Never push to main without review.', \
                      'user: Never push to main without review.','user_said',3,'s','h1',1793000000000); \
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 4000) \
             INSERT INTO symbol(qualified_name, short_name, kind, path, line, file_hash) \
               SELECT 'module::function_number_' || i, 'function_number_' || i, 'fn', \
                      'src/some/long/path/file_' || i || '.rs', i, 'hash' FROM n;",
        )
        .unwrap();
    }
    let before = std::fs::metadata(&db).unwrap().len();
    let maintain = || {
        let (code, out, err) = run(p.path(), &["--json", "maintain"], "", &[]);
        assert_eq!(code, 0, "{err}");
        serde_json::from_str::<serde_json::Value>(out.trim()).expect(&out)
    };
    assert_eq!(maintain()["store_compacted"], true);
    let after = std::fs::metadata(&db).unwrap().len();
    assert!(after * 2 < before, "{before} bytes before, {after} after");
    assert_eq!(maintain()["store_compacted"], false, "once");
    let active: i64 = store(p.path())
        .query_row("SELECT count(*) FROM record WHERE invalid = 0", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(active, 1, "the record is still there");
}

/// No hooks file this repository ships, and none `init --codex` writes, registers
/// `PostToolUse`: the binary no longer serves it.
#[test]
fn no_hooks_file_registers_post_tool_use() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for file in [
        "plugin/hooks/hooks.json",
        "plugin/hooks/codex.json",
        "codex/hooks.json",
    ] {
        let text = std::fs::read_to_string(repo.join(file)).unwrap();
        assert!(!text.contains("PostToolUse"), "{file}");
        for event in ["SessionStart", "UserPromptSubmit", "PreToolUse", "Stop"] {
            assert!(text.contains(event), "{file} still registers {event}");
        }
    }
    let p = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(p.path().join(".git")).unwrap();
    let (code, _, err) = run(p.path(), &["init", "--keep-native", "--codex"], "", &[]);
    assert_eq!(code, 0, "{err}");
    let written = std::fs::read_to_string(p.path().join(".codex/hooks.json")).unwrap();
    assert!(!written.contains("PostToolUse"), "{written}");
    assert!(written.contains("PreToolUse"), "{written}");
}
