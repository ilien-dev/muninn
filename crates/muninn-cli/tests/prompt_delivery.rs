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
        // serde_json, not format!: a Windows path's backslashes must be escaped in JSON
        serde_json::json!({"session_id": sid, "cwd": root, "prompt": "which cache layer do we use", "hook_event_name": "UserPromptSubmit"})
            .to_string()
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
/// nothing and the next prompt re-served what the session had just been given.
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

    let prompt = |sid: &str| {
        // serde_json, not format!: a Windows path's backslashes must be escaped in JSON
        serde_json::json!({"session_id": sid, "cwd": root, "prompt": "which cache layer do we use", "hook_event_name": "UserPromptSubmit"})
            .to_string()
    };
    let first = run(root, &["hook", "UserPromptSubmit"], &prompt("S"));
    assert!(first.contains("redis"), "delivered once: {first}");

    // the fold `SessionStart` spawns, run in the foreground so the test is deterministic
    run(root, &["maintain"], "");

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

/// R-13: a catalogue line is a title, not the record. The record a session's catalogue named
/// is still offered in full when a prompt asks for it, before and after a fold; once given as
/// a block it is not given again.
#[test]
fn a_record_the_catalogue_named_is_still_offered_in_full() {
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
    let start = |sid: &str| {
        serde_json::json!({"session_id": sid, "cwd": root, "source": "startup", "hook_event_name": "SessionStart"})
            .to_string()
    };
    let prompt = |sid: &str| {
        serde_json::json!({"session_id": sid, "cwd": root, "prompt": "which cache layer do we use", "hook_event_name": "UserPromptSubmit"})
            .to_string()
    };
    for (sid, fold) in [("S", false), ("F", true)] {
        let catalogue = run(root, &["hook", "SessionStart"], &start(sid));
        assert!(catalogue.contains("#1 decision"), "listed: {catalogue}");
        if fold {
            run(root, &["maintain"], "");
        }
        let asked = run(root, &["hook", "UserPromptSubmit"], &prompt(sid));
        assert!(
            asked.contains("[muninn:decision] #1"),
            "offered in full (fold {fold}): {asked}"
        );
        let again = run(root, &["hook", "UserPromptSubmit"], &prompt(sid));
        assert!(!again.contains("redis"), "and only once: {again}");
    }
}

/// R-13: the session start's catalogue does not list the records its blocks show in full; a
/// subagent's neither.
#[test]
fn the_catalogue_beside_the_blocks_does_not_repeat_them() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    run(root, &["init", "--keep-native"], "");
    let sql = "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
               VALUES('invariant','never edit generated','must','never edit generated files','user: never edit generated files\n','user_said',3,'s','h1',2); \
               INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
               VALUES('decision','said:state:cache redis','is','we use redis for the cache layer','user: we use redis for the cache layer\n','user_said',3,'s','h2',1); \
               INSERT INTO cue(record_id,kind,key,grp) VALUES(1,'event','post_compact',0),(1,'event','session_start',1);";
    rusqlite::Connection::open(root.join(".muninn/muninn.db"))
        .unwrap()
        .execute_batch(sql)
        .unwrap();
    for (event, input) in [
        (
            "SessionStart",
            serde_json::json!({"session_id": "S", "cwd": root, "source": "startup", "hook_event_name": "SessionStart"}),
        ),
        (
            "SubagentStart",
            serde_json::json!({"session_id": "T", "cwd": root, "hook_event_name": "SubagentStart"}),
        ),
    ] {
        let out = run(root, &["hook", event], &input.to_string());
        assert!(out.contains("[muninn:invariant] #1"), "{event}: {out}");
        assert!(out.contains("#2 decision"), "{event}: {out}");
        assert!(!out.contains("#1 invariant"), "{event}: {out}");
    }
}

/// One compaction runs `SessionStart` with `source=compact` and then `PostCompact`. It moves
/// the epoch once and logs the reinjection once; the session start, whose context is the one
/// the agent sees, carries it. `PostCompact` alone still does both.
#[test]
fn one_compaction_is_delivered_and_counted_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    run(root, &["init", "--keep-native"], "");
    let sql = "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
               VALUES('invariant','never edit generated','must','never edit generated files','user: never edit generated files\n','user_said',3,'s','h1',1); \
               INSERT INTO cue(record_id,kind,key,grp) VALUES(1,'event','session_start',0),(1,'event','post_compact',1);";
    rusqlite::Connection::open(root.join(".muninn/muninn.db"))
        .unwrap()
        .execute_batch(sql)
        .unwrap();
    let lines = |sid: &str, reason: &str| {
        reasons(root)
            .lines()
            .filter(|l| {
                l.contains(&format!("\"session\":\"{sid}\""))
                    && l.contains(&format!("\"reason\":\"{reason}"))
            })
            .count()
    };
    let epoch = |sid: &str| {
        std::fs::read_to_string(root.join(format!(".muninn/log/epoch-{sid}.txt"))).unwrap()
    };
    let start = serde_json::json!({"session_id": "C", "cwd": root, "source": "compact", "hook_event_name": "SessionStart"});
    let post = |sid: &str| {
        serde_json::json!({"session_id": sid, "cwd": root, "trigger": "auto", "hook_event_name": "PostCompact"})
            .to_string()
    };
    let shown = run(root, &["hook", "SessionStart"], &start.to_string());
    assert!(shown.contains("[muninn:invariant] #1"), "{shown}");
    run(root, &["hook", "PostCompact"], &post("C"));
    assert_eq!(epoch("C"), "1");
    assert_eq!(lines("C", "epoch:"), 1);
    assert_eq!(lines("C", "cue:event:post_compact"), 1);

    run(root, &["hook", "PostCompact"], &post("D"));
    assert_eq!(epoch("D"), "1");
    assert_eq!(lines("D", "cue:event:post_compact"), 1);

    // the other order: PostCompact first, then the session start, which still shows it
    run(root, &["hook", "PostCompact"], &post("E"));
    let start_e = serde_json::json!({"session_id": "E", "cwd": root, "source": "compact", "hook_event_name": "SessionStart"});
    let shown_e = run(root, &["hook", "SessionStart"], &start_e.to_string());
    assert!(shown_e.contains("[muninn:invariant] #1"), "{shown_e}");
    assert_eq!(epoch("E"), "1");
    assert_eq!(lines("E", "epoch:"), 1);
    assert_eq!(lines("E", "cue:event:post_compact"), 1);
}

/// R-14: `muninn recall` prints what the prompt hook delivers for the same prompt, block for
/// block, and with `--session` leaves out what that session was already given.
#[test]
fn recall_prints_what_the_prompt_hook_delivers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    run(root, &["init", "--keep-native"], "");
    let sql = "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
               VALUES('episode','session:e#1','said','cache','user: the cache layer is redis, the cache layer for every service, cache layer settled\n','tool_observed',1,'e','h1',1); \
               INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
               VALUES('decision','said:state:cache','is','a layer for the cache','user: a layer for the cache\n','user_said',3,'s','h2',2);";
    rusqlite::Connection::open(root.join(".muninn/muninn.db"))
        .unwrap()
        .execute_batch(sql)
        .unwrap();
    let ask = "which cache layer do we use";
    let hook = run(
        root,
        &["hook", "UserPromptSubmit"],
        &serde_json::json!({"session_id": "R", "cwd": root, "prompt": ask}).to_string(),
    );
    let hook: serde_json::Value = serde_json::from_str(&hook).unwrap();
    let delivered = hook["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        delivered.contains("#1") && delivered.contains("#2"),
        "{delivered}"
    );
    let recall: serde_json::Value =
        serde_json::from_str(&run(root, &["--json", "recall", ask], "")).unwrap();
    assert_eq!(recall["text"].as_str().unwrap(), delivered);
    let again: serde_json::Value =
        serde_json::from_str(&run(root, &["--json", "recall", "--session", "R", ask], "")).unwrap();
    assert_eq!(again["ids"], serde_json::json!([]), "{again}");
    // the gates too: a short prompt and a notification get nothing from either
    for gated in [
        "cache layer",
        "ok thanks",
        "<task-notification>cache layer redis</task-notification>",
    ] {
        let hook = run(
            root,
            &["hook", "UserPromptSubmit"],
            &serde_json::json!({"session_id": "G", "cwd": root, "prompt": gated}).to_string(),
        );
        assert_eq!(hook, "", "{gated}");
        let recall: serde_json::Value =
            serde_json::from_str(&run(root, &["--json", "recall", gated], "")).unwrap();
        assert_eq!(recall["text"], "", "{gated}: {recall}");
        assert_eq!(recall["gated"], "gated:intent", "{gated}: {recall}");
    }
}

#[test]
fn a_subagent_gets_the_memory_unless_it_is_switched_off() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    run(root, &["init", "--keep-native"], "");
    let db = root.join(".muninn/muninn.db");
    let sql = "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
               VALUES('invariant','never edit generated','must','never edit generated files','user: never edit generated files\n','user_said',3,'s','h1',1);";
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(sql)
        .unwrap();
    let input = r#"{"session_id":"s","hook_event_name":"SubagentStart","agent_type":"general"}"#;
    let out = run(root, &["hook", "SubagentStart"], input);
    assert!(out.contains("never edit generated files"), "{out}");
    assert!(out.contains("\"hookEventName\":\"SubagentStart\""), "{out}");
    run(root, &["config", "subagent-delivery", "off"], "");
    assert_eq!(run(root, &["hook", "SubagentStart"], input), "");
}

/// R-04: a correction reaches a subagent and a compaction, not only a session start. Both asked
/// for the compaction cue, which only invariants carry. A subagent is given it even when its
/// parent was already shown it: what the parent saw is not what the subagent has.
#[test]
fn a_correction_reaches_a_subagent_and_a_compaction() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    run(root, &["init", "--keep-native"], "");
    // the cues the write path derives: an invariant gets both events, a correction one
    let sql = "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
               VALUES('invariant','never edit generated','must','never edit generated files','user: never edit generated files\n','user_said',3,'s','h1',1); \
               INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
               VALUES('correction','correction:s#2','is','no, the tests run with nextest','user: no, the tests run with nextest\n','user_said',3,'s','h2',2); \
               INSERT INTO cue(record_id,kind,key,grp) VALUES(1,'event','session_start',0),(1,'event','post_compact',1),(2,'event','session_start',0);";
    rusqlite::Connection::open(root.join(".muninn/muninn.db"))
        .unwrap()
        .execute_batch(sql)
        .unwrap();
    let start = serde_json::json!({"session_id": "P", "cwd": root, "source": "startup", "hook_event_name": "SessionStart"});
    assert!(
        run(root, &["hook", "SessionStart"], &start.to_string()).contains("[muninn:correction] #2")
    );
    for (event, input) in [
        (
            "SubagentStart",
            serde_json::json!({"session_id": "P", "cwd": root, "hook_event_name": "SubagentStart"}),
        ),
        (
            "PostCompact",
            serde_json::json!({"session_id": "Q", "cwd": root, "trigger": "auto", "hook_event_name": "PostCompact"}),
        ),
    ] {
        let out = run(root, &["hook", event], &input.to_string());
        assert!(out.contains("[muninn:correction] #2"), "{event}: {out}");
        assert!(out.contains("[muninn:invariant] #1"), "{event}: {out}");
    }
}

/// A subagent's delivery is logged under its parent's session id. The parent never saw it, so
/// it does not withhold the record from the parent's next answer (R-01).
#[test]
fn what_a_subagent_was_given_is_not_withheld_from_its_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    run(root, &["init", "--keep-native"], "");
    let sql = "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
               VALUES('invariant','never edit generated','must','never edit generated files','user: never edit generated files\n','user_said',3,'s','h1',1); \
               INSERT INTO cue(record_id,kind,key,grp) VALUES(1,'event','post_compact',0),(1,'event','session_start',1);";
    rusqlite::Connection::open(root.join(".muninn/muninn.db"))
        .unwrap()
        .execute_batch(sql)
        .unwrap();
    let sub =
        serde_json::json!({"session_id": "P", "cwd": root, "hook_event_name": "SubagentStart"})
            .to_string();
    assert!(
        run(root, &["hook", "SubagentStart"], &sub).contains("[muninn:invariant] #1"),
        "the subagent is given it as a block"
    );
    let prompt = serde_json::json!({"session_id": "P", "cwd": root, "prompt": "may we edit the generated files now", "hook_event_name": "UserPromptSubmit"})
        .to_string();
    let parent = run(root, &["hook", "UserPromptSubmit"], &prompt);
    assert!(
        parent.contains("never edit generated files"),
        "the parent is still answered: {parent}"
    );
}

/// An event delivery that cannot show every rule says so in one line, and that line is inside
/// the 700-token budget: on five store copies the blocks alone stayed under it and the line took
/// the delivery to 725 exact tokens.
#[cfg(feature = "exact-tokens")]
#[test]
fn an_event_delivery_with_its_disclosure_line_stays_inside_the_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    run(root, &["init", "--keep-native"], "");
    let mut sql = String::new();
    for i in 1..=40 {
        // dense text, Spanish with identifiers, as close to the estimate as real stores come
        let body = format!(
            "user: regla {i}: la configuración del servicio sigue la política número {i}, \
             nunca editar config_generado_v{i} a mano ni el módulo de índices"
        );
        sql.push_str(&format!(
            "INSERT INTO record(kind,subject,relation,object,body,origin,trust,session_id,dedup_hash,created_at) \
             VALUES('invariant','rule {i}','must','{body}','{body}','user_said',3,'s','h{i}',{i}); \
             INSERT INTO cue(record_id,kind,key,grp) VALUES({i},'event','post_compact',0),({i},'event','session_start',1);"
        ));
    }
    rusqlite::Connection::open(root.join(".muninn/muninn.db"))
        .unwrap()
        .execute_batch(&sql)
        .unwrap();
    let input = serde_json::json!({"session_id": "Q", "cwd": root, "trigger": "auto", "hook_event_name": "PostCompact"});
    let out: serde_json::Value =
        serde_json::from_str(&run(root, &["hook", "PostCompact"], &input.to_string())).unwrap();
    let text = out["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        text.contains("did not fit"),
        "the fixture must overflow: {text}"
    );
    let exact = muninn_core::tokens::count_cl100k(text);
    assert!(exact <= 700, "{exact} exact tokens");
}
