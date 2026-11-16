//! Codex rollouts in the shape codex-cli 0.166.0 writes them. The fixture and every line built
//! here are invented; only the shape was read from real rollouts (line types, key names and
//! enumerated values).
//!
//! What these hold: a turn is the user's only where the rollout marks it as a person's, a
//! parse from a watermark knows the same things a parse from the first byte knows, and the
//! newest statement prevails across the two clients in either order.

use muninn_capture::ingest::ingest_transcript;
use muninn_capture::{parse_any, parse_any_with_tasks, Session};
use muninn_core::{Db, Mode};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const ROLLOUT: &str = "rollout-2026-11-14T10-00-00-019a7c00-0000-7000-8000-000000000001.jsonl";
const SID: &str = "019a7c00-0000-7000-8000-000000000001";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex")
        .join(ROLLOUT)
}

/// Rollout lines, as the fixture's generator writes them.
struct Rollout {
    lines: Vec<String>,
    day: u32,
}

impl Rollout {
    fn new(session: &str, day: u32, source: Value) -> Self {
        let mut r = Rollout {
            lines: Vec::new(),
            day,
        };
        r.push(
            "session_meta",
            json!({"id": session, "session_id": session, "cwd": "/work/tides", "originator": "codex-tui",
                   "cli_version": "0.166.0", "source": source, "thread_source": "user"}),
        );
        r
    }

    fn push(&mut self, ty: &str, payload: Value) {
        let n = self.lines.len();
        let ts = format!(
            "2026-11-{:02}T10:{:02}:{:02}.000Z",
            self.day,
            n / 60,
            n % 60
        );
        self.lines.push(
            json!({"timestamp": ts, "ordinal": n + 1, "type": ty, "payload": payload}).to_string(),
        );
    }

    fn started(&mut self, turn: &str, trigger: Option<&str>) {
        let attribution = trigger.map(|t| json!({"turn_id": turn, "turn_trigger": t}));
        self.push(
            "event_msg",
            json!({"type": "task_started", "turn_id": turn, "turn_attribution": attribution}),
        );
    }

    fn said(&mut self, turn: &str, text: &str) {
        self.push(
            "response_item",
            json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}),
        );
        self.push(
            "event_msg",
            json!({"type": "item_completed", "turn_id": turn,
                   "item": {"type": "UserMessage", "id": "u", "client_id": "c",
                            "content": [{"type": "text", "text": text, "text_elements": []}]}}),
        );
    }

    fn agent(&mut self, turn: &str, text: &str) {
        self.push(
            "event_msg",
            json!({"type": "item_completed", "turn_id": turn,
                   "item": {"type": "AgentMessage", "id": "a", "content": [{"type": "Text", "text": text}],
                            "phase": "final_answer"}}),
        );
    }

    fn mcp(&mut self, turn: &str, arguments: Value) {
        self.push(
            "event_msg",
            json!({"type": "item_completed", "turn_id": turn,
                   "item": {"type": "McpToolCall", "id": "m", "server": "fleet", "tool": "start_worker",
                            "arguments": arguments, "status": "completed"}}),
        );
    }

    fn text(&self) -> String {
        self.lines.join("\n") + "\n"
    }

    /// Bytes up to the line that first holds `marker`.
    fn before(&self, marker: &str) -> usize {
        let i = self
            .lines
            .iter()
            .position(|l| l.contains(marker))
            .unwrap_or_else(|| panic!("no line holds {marker:?}"));
        self.lines[..i].iter().map(|l| l.len() + 1).sum()
    }

    fn write(&self, dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(format!("rollout-{name}.jsonl"));
        std::fs::write(&p, self.text()).unwrap();
        p
    }
}

fn claude(dir: &Path, name: &str, lines: &[Value]) -> PathBuf {
    let p = dir.join(format!("{name}.jsonl"));
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&p, text).unwrap();
    p
}

fn typed_by_user(sid: &str, ts: &str, text: &str) -> Value {
    json!({"type": "user", "sessionId": sid, "timestamp": ts, "origin": {"kind": "human"},
           "message": {"role": "user", "content": text}})
}

fn answered(sid: &str, ts: &str, text: &str) -> Value {
    json!({"type": "assistant", "sessionId": sid, "timestamp": ts,
           "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}})
}

fn store(dir: &Path) -> Db {
    Db::open(&dir.join("m.db"), Mode::ReadWrite).unwrap()
}

#[derive(Debug)]
struct Row {
    id: i64,
    kind: String,
    origin: String,
    trust: i64,
    active: bool,
    subject: String,
    object: String,
    body: String,
    session: String,
    heir: Option<i64>,
}

fn rows(db: &Db, filter: &str) -> Vec<Row> {
    let sql = format!(
        "SELECT id, kind, origin, trust, invalid, subject, object, body, session_id, invalidated_by \
         FROM record WHERE {filter} ORDER BY id"
    );
    let mut q = db.conn.prepare(&sql).unwrap();
    let out = q
        .query_map([], |r| {
            Ok(Row {
                id: r.get(0)?,
                kind: r.get(1)?,
                origin: r.get(2)?,
                trust: r.get(3)?,
                active: r.get::<_, i64>(4)? == 0,
                subject: r.get(5)?,
                object: r.get(6)?,
                body: r.get(7)?,
                session: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
                heir: r.get(9)?,
            })
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    out
}

fn prompts(s: &Session) -> Vec<&str> {
    s.turns.iter().map(|t| t.user_prompt.as_str()).collect()
}

/// The fixture, read whole: five things a person or a command put to the agent open a turn;
/// what Codex itself writes in the user's role opens none.
#[test]
fn a_rollout_of_0_166_opens_a_turn_for_each_message_and_for_nothing_else() {
    let s = parse_any_with_tasks(&fixture(), 0).unwrap();
    assert_eq!(s.harness, "codex");
    assert_eq!(s.session_id, SID);
    assert_eq!(s.cwd.as_deref(), Some("/work/tides"));
    assert_eq!(
        prompts(&s),
        [
            "We go with SQLite for the tide cache. Never commit the generated tables under out/.",
            "No, heights are in metres, not feet, in every table.",
            "Do not rename the stations while you are in there.",
            "Always push straight to main after each change, never open a pull request.",
            "From now on the tide cache is on Postgres, not SQLite.",
        ]
    );
    let all = prompts(&s).join("\n");
    for injected in [
        "environment_context",
        "AGENTS.md instructions",
        "user_shell_command",
        "Summary of the work so far",
        "permissions",
    ] {
        assert!(!all.contains(injected), "{injected} opened a turn");
    }
    let t = &s.turns[0];
    assert_eq!(t.tools.len(), 3);
    assert_eq!(t.tools[0].exit_code, Some(101));
    assert!(t.tools[0].is_error);
    assert_eq!(t.tools[1].exit_code, Some(0));
    assert_eq!(t.files_touched, ["/work/tides/src/cache.rs"]);
}

/// Only the queued message is not a person's: the rollout says a command left it.
#[test]
fn a_turn_is_a_persons_only_where_the_rollout_says_one_started_it() {
    let s = parse_any(&fixture(), 0).unwrap();
    let automated: Vec<bool> = s.turns.iter().map(|t| t.automated).collect();
    assert_eq!(automated, [false, false, false, true, false]);
}

/// A session a person did not open in the terminal or the editor has no turn of theirs: a
/// prompt on a command line, a subagent's thread, a source the rollout does not name.
#[test]
fn a_session_no_person_opened_has_no_turn_of_theirs() {
    let tmp = tempfile::tempdir().unwrap();
    for (name, source, thread) in [
        ("exec", json!("exec"), "user"),
        ("mcp", json!("mcp"), "user"),
        (
            "subagent",
            json!({"subagent": {"thread_spawn": {"parent_thread_id": "p", "depth": 1}}}),
            "subagent",
        ),
        ("review", json!("cli"), "guardian_review"),
        ("unnamed", Value::Null, "user"),
    ] {
        let mut r = Rollout::new("s-auto", 14, source);
        r.lines[0] = r.lines[0].replace(
            "\"thread_source\":\"user\"",
            &format!("\"thread_source\":\"{thread}\""),
        );
        r.started("t1", Some("user"));
        r.said("t1", "Never commit the generated tables under out/.");
        r.agent("t1", "Understood.");
        let s = parse_any(&r.write(tmp.path(), name), 0).unwrap();
        assert_eq!(s.turns.len(), 1, "{name}");
        assert!(s.turns[0].automated, "{name}: stored as a person's turn");
    }
    // and the two a person does open, with and without the attribution older versions lack
    for (name, source, trigger) in [
        ("cli", "cli", Some("user")),
        ("vscode", "vscode", Some("user")),
        ("cli-old", "cli", None),
    ] {
        let mut r = Rollout::new("s-person", 14, json!(source));
        r.started("t1", trigger);
        r.said("t1", "Never commit the generated tables under out/.");
        r.agent("t1", "Understood.");
        let s = parse_any(&r.write(tmp.path(), name), 0).unwrap();
        assert!(!s.turns[0].automated, "{name}");
    }
}

/// `start_offset` names a turn for the life of the rollout: the byte its message's line
/// starts at, the same from the first byte and from a watermark.
#[test]
fn a_turn_starts_at_the_byte_of_its_message() {
    let path = fixture();
    let text = std::fs::read_to_string(&path).unwrap();
    let whole = parse_any(&path, 0).unwrap();
    for t in &whole.turns {
        let line = text[t.start_offset as usize..].lines().next().unwrap();
        let v: Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            v["payload"]["item"]["type"], "UserMessage",
            "turn {}",
            t.index
        );
        assert!(line.contains(&t.user_prompt[..20]), "turn {}", t.index);
    }
    let from = whole.turns[1].end_offset;
    let rest = parse_any(&path, from).unwrap();
    let starts = |s: &Session| s.turns.iter().map(|t| t.start_offset).collect::<Vec<_>>();
    assert_eq!(starts(&rest), starts(&whole)[2..]);
}

/// A parse from a watermark starts past the session's first line and, inside a turn, past the
/// line that says what started it. It still knows whose session this is and who started the
/// turn: here a watermark between a queued message's `task_started` and the message, and one
/// between a person's two messages of one turn, with a line longer than a read block between.
#[test]
fn a_parse_from_a_watermark_knows_who_is_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut r = Rollout::new("s-mark", 14, json!("cli"));
    r.started("t1", Some("user"));
    r.said("t1", "We go with SQLite for the tide cache.");
    r.agent("t1", &"x".repeat(300_000));
    r.said("t1", "Never commit the generated tables under out/.");
    r.agent("t1", "Understood.");
    r.started("t2", Some("queue"));
    r.said("t2", "Always push straight to main after each change.");
    r.agent("t2", "Noted.");
    let path = r.write(tmp.path(), "mark");

    let steer = parse_any(&path, r.before("Never commit") as u64).unwrap();
    assert_eq!(steer.session_id, "s-mark");
    assert_eq!(steer.cwd.as_deref(), Some("/work/tides"));
    assert_eq!(steer.turns.len(), 2);
    assert!(
        !steer.turns[0].automated,
        "the person's second message of the turn"
    );
    assert!(steer.turns[1].automated, "the queued message");

    let queued = parse_any(&path, r.before("Always push") as u64).unwrap();
    assert_eq!(queued.turns.len(), 1);
    assert!(
        queued.turns[0].automated,
        "the queued message, read from a watermark"
    );

    // an exec session read from a watermark is still nobody's
    let mut e = Rollout::new("s-exec", 14, json!("exec"));
    e.started("t1", Some("exec"));
    e.said("t1", "We go with SQLite for the tide cache.");
    e.agent("t1", "Done.");
    e.started("t2", Some("user"));
    e.said("t2", "Never commit the generated tables under out/.");
    e.agent("t2", "Understood.");
    let path = e.write(tmp.path(), "exec-mark");
    let later = parse_any(&path, e.before("Never commit") as u64).unwrap();
    assert!(later.turns[0].automated);
}

/// What the fixture leaves in a store: the person's statements at trust 3, and nothing at
/// trust 3 from the queued message, from a tool's output, from the agent or from what Codex
/// wrote in the user's role.
#[test]
fn only_what_the_person_typed_is_stored_as_theirs() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path());
    ingest_transcript(&db, &fixture(), "hook").unwrap();
    let typed = rows(&db, "kind <> 'episode'");
    let theirs: Vec<(&str, &str)> = typed
        .iter()
        .filter(|r| r.origin == "user_said")
        .map(|r| (r.kind.as_str(), r.object.as_str()))
        .collect();
    assert_eq!(
        theirs,
        [
            ("decision", "We go with SQLite for the tide cache"),
            ("invariant", "Never commit the generated tables under out/"),
            (
                "correction",
                "No, heights are in metres, not feet, in every table."
            ),
            (
                "correction",
                "Do not rename the stations while you are in there."
            ),
            (
                "decision",
                "From now on the tide cache is on Postgres, not SQLite"
            ),
        ]
    );
    for r in rows(&db, "trust = 3") {
        for not_theirs in [
            "push straight to main",
            "environment_context",
            "AGENTS.md",
            "Summary of the work so far",
            "ls out",
            "harbour table",
        ] {
            assert!(
                !r.body.contains(not_theirs) && !r.object.contains(not_theirs),
                "#{} holds {not_theirs:?} at trust 3",
                r.id
            );
        }
    }
    // the queued message is on record as what happened, not as a rule
    let queued = rows(&db, "body LIKE '%push straight to main%'");
    assert!(!queued.is_empty());
    assert!(
        queued.iter().all(|r| r.kind == "episode" && r.trust <= 1),
        "{queued:?}"
    );
    // the newest statement of the session prevails
    let cache: Vec<(bool, &str)> = typed
        .iter()
        .filter(|r| r.kind == "decision")
        .map(|r| (r.active, r.object.as_str()))
        .collect();
    assert_eq!(
        cache,
        [
            (false, "We go with SQLite for the tide cache"),
            (
                true,
                "From now on the tide cache is on Postgres, not SQLite"
            ),
        ]
    );
}

/// WP-20. One turn an ingest, as `Stop` does: the second batch starts past the session's
/// first line. Its records belong to the same session, and two corrections of different
/// things stand side by side under subjects of their own.
#[test]
fn a_later_batch_of_a_rollout_is_the_same_session_and_keeps_both_corrections() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path());
    let mut r = Rollout::new(SID, 14, json!("cli"));
    r.started("t1", Some("user"));
    r.said("t1", "No, heights are in metres, not feet, in every table.");
    r.agent("t1", "Switching the tables to metres.");
    let path = r.write(tmp.path(), "batches");
    ingest_transcript(&db, &path, "hook").unwrap();
    r.started("t2", Some("user"));
    r.said("t2", "Do not rename the stations while you are in there.");
    r.agent("t2", "The station names are untouched.");
    std::fs::write(&path, r.text()).unwrap();
    ingest_transcript(&db, &path, "hook").unwrap();

    let all = rows(&db, "1 = 1");
    assert!(all.iter().all(|x| x.session == SID), "{all:?}");
    let corrections = rows(&db, "kind = 'correction'");
    assert_eq!(corrections.len(), 2);
    assert!(corrections
        .iter()
        .all(|c| c.active && c.origin == "user_said"));
    let at = |marker: &str| {
        r.before(marker)
            + r.lines[r.lines.iter().position(|l| l.contains(marker)).unwrap()].len()
            + 1
    };
    assert_eq!(
        corrections
            .iter()
            .map(|c| c.subject.clone())
            .collect::<Vec<_>>(),
        [
            format!("correction:019a7c00#{}", at("No, heights")),
            format!("correction:019a7c00#{}", at("Do not rename")),
        ]
    );
}

/// A task a Codex session hands to another agent through a tool is that agent's prompt, in
/// whatever client the worker runs: its text is nobody's statement there, in either order of
/// ingest.
#[test]
fn a_task_a_codex_session_hands_over_is_not_the_users_in_the_workers_session() {
    const TASK: &str =
        "Never rebuild the harbour index by hand, always run the nightly job and wait for it.";
    for worker_first in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let mut r = Rollout::new("s-coord", 14, json!("cli"));
        r.started("t1", Some("user"));
        r.said("t1", "Split the harbour work across two workers.");
        r.mcp("t1", json!({"prompt": TASK}));
        r.agent("t1", "Started a worker.");
        let coordinator = r.write(tmp.path(), "coord");
        let worker = claude(
            tmp.path(),
            "worker",
            &[
                typed_by_user("w1", "2026-11-14T10:05:00.000Z", TASK),
                answered("w1", "2026-11-14T10:05:10.000Z", "Running the nightly job."),
            ],
        );
        let order = if worker_first {
            [&worker, &coordinator]
        } else {
            [&coordinator, &worker]
        };
        for t in order {
            ingest_transcript(&db, t, "hook").unwrap();
        }
        let theirs = rows(
            &db,
            "invalid = 0 AND trust = 3 AND body LIKE '%nightly job%'",
        );
        assert!(theirs.is_empty(), "worker first {worker_first}: {theirs:?}");
    }
}

/// And the other way round: a task a Claude Code session hands over is not the user's in the
/// Codex session it opens, though that rollout marks the turn as started by a user.
#[test]
fn a_task_handed_to_a_codex_session_is_not_the_users_there() {
    const TASK: &str =
        "Never rebuild the harbour index by hand, always run the nightly job and wait for it.";
    for worker_first in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let coordinator = claude(
            tmp.path(),
            "coord",
            &[
                typed_by_user(
                    "c1",
                    "2026-11-14T10:00:00.000Z",
                    "Split the harbour work across two workers.",
                ),
                json!({"type": "assistant", "sessionId": "c1", "timestamp": "2026-11-14T10:00:05.000Z",
                       "message": {"role": "assistant", "content": [
                           {"type": "tool_use", "id": "tu1", "name": "mcp__fleet__start_worker", "input": {"prompt": TASK}}]}}),
            ],
        );
        let mut r = Rollout::new("s-worker", 14, json!("vscode"));
        r.started("t1", Some("user"));
        r.said("t1", TASK);
        r.agent("t1", "Running the nightly job.");
        let worker = r.write(tmp.path(), "worker");
        let order = if worker_first {
            [&worker, &coordinator]
        } else {
            [&coordinator, &worker]
        };
        for t in order {
            ingest_transcript(&db, t, "hook").unwrap();
        }
        let theirs = rows(
            &db,
            "invalid = 0 AND trust = 3 AND body LIKE '%nightly job%'",
        );
        assert!(theirs.is_empty(), "worker first {worker_first}: {theirs:?}");
    }
}

/// What not trusting the queue costs, stated as a test. A queued message that restates a
/// decision is kept as something that was said, at trust 1, and the older decision stands:
/// a rollout does not say who queued it (`codex.rs`). The same message typed in the session
/// replaces the older decision, in a session a person opened and in no other.
#[test]
fn a_queued_restatement_is_kept_as_said_and_replaces_nothing() {
    for (trigger, source, replaced) in [
        ("queue", "cli", false),
        ("user", "cli", true),
        ("queue", "exec", false),
        ("user", "exec", false),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        let older = claude(
            tmp.path(),
            "older",
            &[
                typed_by_user(
                    "c1",
                    "2026-11-13T09:00:00.000Z",
                    "We go with SQLite for the tide cache.",
                ),
                answered("c1", "2026-11-13T09:00:10.000Z", "SQLite it is."),
            ],
        );
        let mut r = Rollout::new("s-queued", 14, json!(source));
        r.started("t1", Some(trigger));
        r.said(
            "t1",
            "From now on the tide cache is on Postgres, not SQLite.",
        );
        r.agent("t1", "Moving the tide cache to Postgres.");
        let newer = r.write(tmp.path(), "newer");
        ingest_transcript(&db, &older, "hook").unwrap();
        ingest_transcript(&db, &newer, "hook").unwrap();
        let d = cache_decisions(&db);
        let active: Vec<&str> = d.iter().filter(|x| x.0).map(|x| x.1.as_str()).collect();
        let expected: &[&str] = if replaced {
            &["From now on the tide cache is on Postgres, not SQLite"]
        } else {
            &["We go with SQLite for the tide cache"]
        };
        assert_eq!(active, expected, "{trigger} in a {source} session");
        let said = rows(&db, "body LIKE '%Postgres%' AND kind = 'episode'");
        assert!(
            said.iter().any(|e| e.active && e.trust == 1),
            "{trigger} in a {source} session: the message is on record as said: {said:?}"
        );
    }
}

fn cache_decisions(db: &Db) -> Vec<(bool, String, Option<i64>, i64)> {
    rows(db, "kind = 'decision' AND origin = 'user_said'")
        .into_iter()
        .map(|r| (r.active, r.object, r.heir, r.id))
        .collect()
}

/// The newest statement prevails across clients: said in Claude Code, replaced in Codex.
#[test]
fn a_decision_restated_in_codex_replaces_the_one_from_claude_code() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path());
    let older = claude(
        tmp.path(),
        "older",
        &[
            typed_by_user(
                "c1",
                "2026-11-13T09:00:00.000Z",
                "We go with SQLite for the tide cache.",
            ),
            answered("c1", "2026-11-13T09:00:10.000Z", "SQLite it is."),
        ],
    );
    let mut r = Rollout::new("s-newer", 14, json!("cli"));
    r.started("t1", Some("user"));
    r.said(
        "t1",
        "From now on the tide cache is on Postgres, not SQLite.",
    );
    r.agent("t1", "Moving the tide cache to Postgres.");
    let newer = r.write(tmp.path(), "newer");
    ingest_transcript(&db, &older, "hook").unwrap();
    ingest_transcript(&db, &newer, "hook").unwrap();
    let d = cache_decisions(&db);
    assert_eq!(d.len(), 2, "{d:?}");
    assert_eq!(
        (d[0].0, d[0].1.as_str()),
        (false, "We go with SQLite for the tide cache")
    );
    assert_eq!(
        (d[1].0, d[1].1.as_str()),
        (
            true,
            "From now on the tide cache is on Postgres, not SQLite"
        )
    );
    assert_eq!(
        d[0].2,
        Some(d[1].3),
        "the retired decision names its replacement"
    );
}

/// And the other way round: said in Codex, replaced in Claude Code.
#[test]
fn a_decision_restated_in_claude_code_replaces_the_one_from_codex() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path());
    let mut r = Rollout::new("s-older", 13, json!("cli"));
    r.started("t1", Some("user"));
    r.said("t1", "We go with SQLite for the tide cache.");
    r.agent("t1", "SQLite it is.");
    let older = r.write(tmp.path(), "older");
    let newer = claude(
        tmp.path(),
        "newer",
        &[
            typed_by_user(
                "c1",
                "2026-11-14T09:00:00.000Z",
                "From now on the tide cache is on Postgres, not SQLite.",
            ),
            answered(
                "c1",
                "2026-11-14T09:00:10.000Z",
                "Moving the tide cache to Postgres.",
            ),
        ],
    );
    ingest_transcript(&db, &older, "hook").unwrap();
    ingest_transcript(&db, &newer, "hook").unwrap();
    let d = cache_decisions(&db);
    assert_eq!(d.len(), 2, "{d:?}");
    assert_eq!(
        (d[0].0, d[0].1.as_str()),
        (false, "We go with SQLite for the tide cache")
    );
    assert_eq!(
        (d[1].0, d[1].1.as_str()),
        (
            true,
            "From now on the tide cache is on Postgres, not SQLite"
        )
    );
    assert_eq!(d[0].2, Some(d[1].3));
}

/// A, then B, then A again in the same words, the three across the two clients: A is the
/// newest word and comes back, B is retired.
#[test]
fn a_decision_said_again_in_the_other_client_comes_back() {
    for codex_holds_b in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path());
        const A: &str = "We go with SQLite for the tide cache.";
        const B: &str = "From now on the tide cache is on Postgres, not SQLite.";
        let in_claude = |name: &str, sid: &str, day: u32, text: &str| {
            claude(
                tmp.path(),
                name,
                &[
                    typed_by_user(sid, &format!("2026-11-{day:02}T09:00:00.000Z"), text),
                    answered(sid, &format!("2026-11-{day:02}T09:00:10.000Z"), "Done."),
                ],
            )
        };
        let in_codex = |name: &str, sid: &str, day: u32, text: &str| {
            let mut r = Rollout::new(sid, day, json!("cli"));
            r.started("t1", Some("user"));
            r.said("t1", text);
            r.agent("t1", "Done.");
            r.write(tmp.path(), name)
        };
        let three = if codex_holds_b {
            [
                in_claude("a1", "c1", 11, A),
                in_codex("b", "x1", 12, B),
                in_claude("a2", "c2", 13, A),
            ]
        } else {
            [
                in_codex("a1", "x1", 11, A),
                in_claude("b", "c1", 12, B),
                in_codex("a2", "x2", 13, A),
            ]
        };
        for t in &three {
            ingest_transcript(&db, t, "hook").unwrap();
        }
        let active: Vec<String> = cache_decisions(&db)
            .into_iter()
            .filter(|d| d.0)
            .map(|d| d.1)
            .collect();
        assert_eq!(
            active,
            ["We go with SQLite for the tide cache"],
            "codex holds B: {codex_holds_b}"
        );
    }
}
