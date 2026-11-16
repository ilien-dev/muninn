//! `muninn remember`: the session is found by its id in the session log the hooks keep, never
//! by the statement's words; the command prints the record's id and says when the statement
//! was not taken as the user's.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_muninn");

fn init(root: &Path) {
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let out = Command::new(BIN)
        .args([
            "--cwd",
            root.to_str().unwrap(),
            "init",
            "--keep-native",
            "--no-boot-block",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
}

/// A read hook that sees the transcript notes it in the session log.
fn prompt_hook(root: &Path, session: &str, transcript: &Path) {
    let input = serde_json::json!({"session_id": session, "cwd": root, "prompt": "hola",
                                   "transcript_path": transcript, "hook_event_name": "UserPromptSubmit"})
    .to_string();
    let mut child = Command::new(BIN)
        .args(["hook", "UserPromptSubmit"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}

fn remember(root: &Path, args: &[&str], session_env: Option<&str>) -> Output {
    let mut c = Command::new(BIN);
    c.args(["--cwd", root.to_str().unwrap(), "remember"])
        .args(args)
        .env_remove("CLAUDE_CODE_SESSION_ID");
    if let Some(s) = session_env {
        c.env("CLAUDE_CODE_SESSION_ID", s);
    }
    c.output().unwrap()
}

fn transcript(root: &Path, session: &str) -> std::path::PathBuf {
    let t = root.join(format!("{session}.jsonl"));
    let lines = [
        serde_json::json!({"type":"user","sessionId":session,"timestamp":"2026-11-05T10:00:00.000Z","message":{"role":"user","content":"Revisa el listado. Nunca borres una factura emitida."}}),
        serde_json::json!({"type":"assistant","sessionId":session,"message":{"role":"assistant","content":[{"type":"text","text":"Entendido."}]}}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&t, text).unwrap();
    t
}

fn origin_of(root: &Path, id: i64) -> (String, i64) {
    rusqlite::Connection::open(root.join(".muninn/muninn.db"))
        .unwrap()
        .query_row("SELECT origin, trust FROM record WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
}

fn json(o: &Output) -> serde_json::Value {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

#[test]
fn the_harness_session_is_found_in_the_session_log() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init(root);
    let t = transcript(root, "s1");
    prompt_hook(root, "s1", &t);
    let o = remember(
        root,
        &["--kind", "invariant", "Nunca borres una factura emitida"],
        Some("s1"),
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = String::from_utf8_lossy(&o.stdout);
    let id: i64 = text
        .split('#')
        .nth(1)
        .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("an id in: {text}"));
    assert_eq!(origin_of(root, id), ("user_said".into(), 3));
    // the counter `muninn status` shows
    let st = Command::new(BIN)
        .args(["--cwd", root.to_str().unwrap(), "status"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&st.stdout).contains("remember: 1"),
        "{}",
        String::from_utf8_lossy(&st.stdout)
    );
}

#[test]
fn a_session_that_cannot_be_identified_gives_the_lower_origin_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init(root);
    let t = transcript(root, "s1");
    prompt_hook(root, "s1", &t);
    for env in [None, Some("otra-sesion")] {
        let o = remember(
            root,
            &[
                "--json",
                "--kind",
                "invariant",
                "Nunca borres una factura emitida",
            ],
            env,
        );
        let v = json(&o);
        assert_eq!(v["origin"], "agent_inferred", "{v}");
        assert_eq!(v["trust"], 0);
        assert_eq!(v["lower"], "the session could not be identified");
    }
    let o = remember(
        root,
        &["--kind", "invariant", "Nunca borres una factura emitida"],
        None,
    );
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(text.contains("could not be identified"), "{text}");
}

/// Ruling 4 of Task 7's first review: no flag turns a file into the session. `--transcript`
/// is honoured only when the session log names that file for that session, `--session` only
/// for an id the log holds; otherwise the statement is the agent's, and the command says why.
#[test]
fn a_transcript_named_by_hand_counts_only_when_the_session_log_names_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init(root);
    let t = transcript(root, "s2");
    let path = t.to_str().unwrap();
    let rule = "Nunca borres una factura emitida";
    let by_hand = |session: Option<&str>| {
        let mut args = vec!["--json", "--kind", "invariant", "--transcript", path];
        if let Some(s) = session {
            args.extend(["--session", s]);
        }
        args.push(rule);
        json(&remember(root, &args, None))
    };
    // a file no hook noted: not the session, whatever it holds
    for session in [None, Some("s2")] {
        let v = by_hand(session);
        assert_eq!(
            (v["origin"].as_str(), v["trust"].as_i64()),
            (Some("agent_inferred"), Some(0)),
            "{v}"
        );
        assert!(v["lower"].as_str().unwrap().contains("session log"), "{v}");
    }
    let o = remember(
        root,
        &["--kind", "invariant", "--transcript", path, rule],
        None,
    );
    assert!(String::from_utf8_lossy(&o.stdout).contains("session log"));
    // noted by a hook for another session: not this one's
    prompt_hook(root, "s9", &t);
    let v = by_hand(Some("s2"));
    assert_eq!(v["origin"], "agent_inferred", "{v}");
    // noted for this session: honoured
    prompt_hook(root, "s2", &t);
    let v = by_hand(Some("s2"));
    assert_eq!(
        (v["origin"].as_str(), v["turn"].as_i64()),
        (Some("user_said"), Some(0)),
        "{v}"
    );
    // a paraphrase in a session the log names is still the agent's
    let v = json(&remember(
        root,
        &[
            "--json",
            "--kind",
            "invariant",
            "--session",
            "s2",
            "Las facturas emitidas no se borran nunca",
        ],
        None,
    ));
    assert_eq!(v["origin"], "agent_inferred");
    assert!(v["lower"].as_str().unwrap().contains("word for word"));
    let o = remember(root, &["--kind", "claim", "algo"], None);
    assert!(!o.status.success(), "an unknown kind is refused");
}

fn stop(root: &Path, session: &str, transcript: &Path) {
    let input =
        serde_json::json!({"session_id": session, "cwd": root, "transcript_path": transcript})
            .to_string();
    let mut child = Command::new(BIN)
        .args(["hook", "Stop"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}

/// I5. The sentence is on record and a later change retired it. Asked to keep it again from
/// the turn it was said in, the command leaves the record as it is, and its answer says the
/// record is retired and by what: "already on record" alone reads as "this is what holds".
#[test]
fn a_statement_on_record_and_retired_is_answered_as_retired() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init(root);
    let said = |session: &str, at: &str, text: &str| {
        let t = root.join(format!("{session}.jsonl"));
        let lines = [
            serde_json::json!({"type":"user","sessionId":session,"timestamp":at,"message":{"role":"user","content":text}}),
            serde_json::json!({"type":"assistant","sessionId":session,"message":{"role":"assistant","content":[{"type":"text","text":"Entendido."}]}}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&t, text).unwrap();
        t
    };
    let old = "We go with openssl for the TLS backend.";
    let t1 = said("s1", "2026-11-05T10:00:00.000Z", old);
    let t2 = said(
        "s2",
        "2026-11-06T10:00:00.000Z",
        "Actually, switch the TLS backend to rustls.",
    );
    stop(root, "s1", &t1);
    stop(root, "s2", &t2);
    let (heir, gone): (i64, i64) = rusqlite::Connection::open(root.join(".muninn/muninn.db"))
        .unwrap()
        .query_row(
            "SELECT invalidated_by, id FROM record WHERE kind = 'decision' AND invalid = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let v = json(&remember(
        root,
        &["--json", "--kind", "decision", "--session", "s1", old],
        None,
    ));
    assert_eq!(
        (v["id"].as_i64(), v["outcome"].as_str()),
        (Some(gone), Some("deduped")),
        "{v}"
    );
    assert_eq!(
        (v["retired"].as_bool(), v["heir"].as_i64()),
        (Some(true), Some(heir)),
        "{v}"
    );
    let o = remember(root, &["--kind", "decision", "--session", "s1", old], None);
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(
        text.contains(&format!("already on record as #{gone}"))
            && text.contains(&format!("retired (#{heir})")),
        "{text}"
    );
}

/// A session id is cut to eight bytes in a correction's subject. One whose eighth byte falls
/// inside a character was cut there, and the command panicked.
#[test]
fn a_session_id_with_letters_of_more_than_one_byte_is_kept_whole_letters() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init(root);
    let session = "añañañañ";
    let t = root.join("s.jsonl");
    let lines = [
        serde_json::json!({"type":"user","sessionId":session,"timestamp":"2026-11-05T10:00:00.000Z","message":{"role":"user","content":"No, the server port is 8443 behind the proxy."}}),
        serde_json::json!({"type":"assistant","sessionId":session,"message":{"role":"assistant","content":[{"type":"text","text":"Entendido."}]}}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&t, text).unwrap();
    prompt_hook(root, session, &t);
    let o = remember(
        root,
        &[
            "--json",
            "--kind",
            "correction",
            "--session",
            session,
            "No, the server port is 8443 behind the proxy.",
        ],
        None,
    );
    let v = json(&o);
    assert_eq!(v["origin"], "user_said", "{v}");
    // and the session's own Stop, which types the same turn
    stop(root, session, &t);
}
