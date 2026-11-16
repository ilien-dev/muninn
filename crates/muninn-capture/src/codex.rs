//! Codex rollout (`~/.codex/sessions/**/rollout-*.jsonl`): lines of
//! `{timestamp, type, payload}`. Conversation lives in `response_item` payloads
//! (`message` with role user/assistant, `function_call`, `function_call_output`)
//! and `event_msg` payloads (`user_message`). Everything else is context.
//!
//! codex-cli 0.154 writes the conversation as typed `event_msg` items instead
//! (`item_completed`: `UserMessage`, `AgentMessage`, `CommandExecution` with its exit
//! code, `FileChange`), and its `response_item` tool calls are code-mode `exec`
//! scripts. Once a rollout shows those items, they are the only source read.
//!
//! Who wrote a turn is read from the rollout's own marks, never from its words. A turn is a
//! person's when `session_meta` says a person opened the session in the terminal or the
//! editor (`source` is `cli` or `vscode`, `thread_source` is `user` or absent) and nothing
//! else started the turn (`task_started.turn_attribution.turn_trigger` is `user` or absent;
//! codex-cli 0.166 writes `exec` for a prompt from a command line and `queue` for a message
//! left with `codex queue`). Every other turn is `automated`: on record as an episode, never
//! as the user's statement.
//!
//! The queue is why `queue` is not `user`. Codex calls it a user-message queue, and a person
//! who types while a turn runs may well be its usual writer. But in codex-cli 0.166.0 anything
//! can add to it: `codex queue --thread <any session> --message <text>` is a plain command
//! (`cli/src/queue_cmd.rs`) that sends `thread/queue/add` to the app server, a request any
//! connected client can send, to a remote server too; the queue's store is polled for
//! "external messages"; and a queued item is an id and an input, with no field for who put
//! it there (`ext/queue/src/service.rs`). A rollout therefore cannot tell a person's queued
//! follow-up from a script's or another agent's, and trust is not given on a guess.
//!
//! What Codex itself writes in the user's role (the environment
//! block, the instruction files, a shell command the person ran, an internal continuation,
//! the history a compaction leaves) is not a `UserMessage` item and opens no turn.

use crate::model::{Session, ToolCall, Turn};
use muninn_core::sanitize::{clean_text, strip_harness_blocks, truncate_chars};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom};

fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()).map(str::to_string))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn exit_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)exit code:\s*(-?\d+)").unwrap())
}

/// What `session_meta` says of the session: which it is, where, and whether a person opened it.
#[derive(Default)]
struct Meta {
    id: Option<String>,
    cwd: Option<String>,
    person: bool,
}

fn read_meta(p: &Value) -> Meta {
    let text = |k: &str| p.get(k).and_then(|s| s.as_str()).map(str::to_string);
    // `source` is a word for a session someone opened and an object for a subagent's thread
    let opened_by_person = p
        .get("source")
        .and_then(|s| s.as_str())
        .is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "cli" | "vscode"));
    let own_thread = match p.get("thread_source") {
        None | Some(Value::Null) => true,
        Some(t) => t.as_str() == Some("user"),
    };
    Meta {
        id: text("id"),
        cwd: text("cwd"),
        person: opened_by_person && own_thread,
    }
}

/// Whether a `task_started` payload opens a turn a person started. A rollout older than the
/// attribution carries none, and the session's own mark decides alone.
fn started_by_person(p: &Value) -> bool {
    match p.get("turn_attribution") {
        None | Some(Value::Null) => true,
        Some(a) => match a.get("turn_trigger") {
            None | Some(Value::Null) => true,
            Some(t) => t.as_str() == Some("user"),
        },
    }
}

/// The payload of a line of this type and, when asked, of this payload type.
fn payload_of(raw: &[u8], ty: &str, payload_ty: Option<&str>) -> Option<Value> {
    let mut v = serde_json::from_slice::<Value>(raw).ok()?;
    if v.get("type").and_then(|t| t.as_str()) != Some(ty) {
        return None;
    }
    let p = v.get_mut("payload")?.take();
    match payload_ty {
        Some(want) if p.get("type").and_then(|t| t.as_str()) != Some(want) => None,
        _ => Some(p),
    }
}

/// A parse from a watermark starts past the two lines that say who is writing: the session's
/// first line, and the `task_started` of the turn the watermark fell inside. Both are read
/// here: the first line as it stands, and the nearest `task_started` before `start_offset` by
/// walking back through the file a block at a time, so that a long rollout is not read again
/// from its first byte at every ingest.
fn before(f: &mut std::fs::File, start_offset: u64) -> std::io::Result<(Meta, bool)> {
    use std::io::Read;
    const BLOCK: u64 = 256 * 1024;
    const MARK: &[u8] = b"task_started";
    f.seek(SeekFrom::Start(0))?;
    let mut first = Vec::new();
    std::io::BufReader::new(&mut *f).read_until(b'\n', &mut first)?;
    let meta = payload_of(&first, "session_meta", None)
        .map(|p| read_meta(&p))
        .unwrap_or_default();
    // the bytes from `at` to `start_offset` that no whole line has been cut from yet
    let mut pending: Vec<u8> = Vec::new();
    let mut at = start_offset;
    while at > 0 {
        let from = at.saturating_sub(BLOCK);
        let mut block = vec![0u8; (at - from) as usize];
        f.seek(SeekFrom::Start(from))?;
        f.read_exact(&mut block)?;
        block.extend_from_slice(&pending);
        // a line may have started before this block: only what follows the first break is whole
        let whole_from = match (from, block.iter().position(|b| *b == b'\n')) {
            (0, _) => 0,
            (_, Some(i)) => i + 1,
            (_, None) => block.len(),
        };
        for line in block[whole_from..].rsplit(|b| *b == b'\n') {
            if line.windows(MARK.len()).any(|w| w == MARK) {
                if let Some(p) = payload_of(line, "event_msg", Some("task_started")) {
                    return Ok((meta, started_by_person(&p)));
                }
            }
        }
        block.truncate(whole_from);
        pending = block;
        at = from;
    }
    Ok((meta, true))
}

pub fn parse(path: &std::path::Path, start_offset: u64) -> std::io::Result<Session> {
    parse_with(path, start_offset, false)
}

/// As [`parse`], and a tool call that hands text to something else carries the hashes of the
/// long strings in its input (`agent::task_hashes`): for ingest, never for a read hook.
pub fn parse_with_tasks(path: &std::path::Path, start_offset: u64) -> std::io::Result<Session> {
    parse_with(path, start_offset, true)
}

fn parse_with(
    path: &std::path::Path,
    start_offset: u64,
    with_tasks: bool,
) -> std::io::Result<Session> {
    let mut f = std::fs::File::open(path)?;
    let mut session = Session {
        harness: "codex".into(),
        end_offset: start_offset,
        ..Default::default()
    };
    // whether a person opened the session, and whether one started the turn being read
    let (mut by_person, mut turn_by_person) = (false, true);
    if start_offset > 0 {
        let (meta, started) = before(&mut f, start_offset)?;
        session.session_id = meta.id.unwrap_or_default();
        session.cwd = meta.cwd;
        by_person = meta.person;
        turn_by_person = started;
        f.seek(SeekFrom::Start(start_offset))?;
    }
    let mut reader = std::io::BufReader::new(f);
    let mut current: Option<Turn> = None;
    let mut pending: HashMap<String, (usize, usize)> = HashMap::new();
    // the rollout carries typed items (0.154+): `response_item` lines duplicate them
    let mut items = false;
    let mut offset = start_offset;
    let mut raw = Vec::new();
    loop {
        raw.clear();
        let n = reader.read_until(b'\n', &mut raw)?;
        if n == 0 {
            break;
        }
        let line_start = offset;
        offset += n as u64;
        if !raw.ends_with(b"\n") {
            offset -= n as u64;
            break;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&raw) else {
            continue;
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let p = v.get("payload").cloned().unwrap_or(Value::Null);
        let ts = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .map(str::to_string);
        match ty {
            "session_meta" => {
                let meta = read_meta(&p);
                if let Some(id) = meta.id {
                    session.session_id = id;
                }
                if meta.cwd.is_some() {
                    session.cwd = meta.cwd;
                }
                by_person = meta.person;
            }
            "event_msg" if p.get("type").and_then(|t| t.as_str()) == Some("task_started") => {
                turn_by_person = started_by_person(&p);
            }
            "event_msg" if p.get("type").and_then(|t| t.as_str()) == Some("item_completed") => {
                let item = p.get("item").unwrap_or(&Value::Null);
                match item.get("type").and_then(|t| t.as_str()) {
                    Some("UserMessage") => {
                        let prompt = clean_text(&strip_harness_blocks(&content_text(
                            item.get("content").unwrap_or(&Value::Null),
                        )));
                        if prompt.trim().is_empty() {
                            continue;
                        }
                        items = true;
                        if let Some(t) = current.take() {
                            session.turns.push(t);
                        }
                        let index = session.turns.len();
                        current = Some(Turn {
                            index,
                            timestamp: ts,
                            user_prompt: prompt,
                            end_offset: offset,
                            start_offset: line_start,
                            automated: !(by_person && turn_by_person),
                            ..Default::default()
                        });
                    }
                    Some("McpToolCall") => {
                        let Some(t) = current.as_mut() else { continue };
                        let part = |k: &str| item.get(k).and_then(|x| x.as_str()).unwrap_or("");
                        let name = format!("mcp__{}__{}", part("server"), part("tool"));
                        let input = item.get("arguments").cloned().unwrap_or(Value::Null);
                        let task_hashes = if with_tasks {
                            crate::agent::task_hashes(&name, &input)
                        } else {
                            Vec::new()
                        };
                        t.tools.push(ToolCall {
                            name,
                            target: clean_text(truncate_chars(&input.to_string(), 160)),
                            is_error: part("status") == "failed",
                            task_hashes,
                            ..Default::default()
                        });
                        t.end_offset = offset;
                    }
                    Some("AgentMessage") => {
                        let Some(t) = current.as_mut() else { continue };
                        let s = content_text(item.get("content").unwrap_or(&Value::Null));
                        if !s.trim().is_empty() {
                            if !t.assistant_text.is_empty() {
                                t.assistant_text.push('\n');
                            }
                            t.assistant_text.push_str(&clean_text(&s));
                        }
                        t.end_offset = offset;
                    }
                    Some("CommandExecution") => {
                        let Some(t) = current.as_mut() else { continue };
                        // ["/usr/bin/bash", "-lc", "<command>"]: the command is the script
                        let argv: Vec<&str> = item
                            .get("command")
                            .and_then(|c| c.as_array())
                            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                            .unwrap_or_default();
                        let command = match argv.as_slice() {
                            [_, flag, cmd] if flag.starts_with('-') && flag.ends_with('c') => {
                                cmd.to_string()
                            }
                            _ => argv.join(" "),
                        };
                        let exit = item.get("exit_code").and_then(|x| x.as_i64());
                        let failed = item.get("status").and_then(|x| x.as_str()) == Some("failed");
                        let out = item
                            .get("aggregated_output")
                            .or_else(|| item.get("formatted_output"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        t.tools.push(ToolCall {
                            name: "shell".into(),
                            target: clean_text(&command),
                            exit_code: exit,
                            is_error: exit.is_some_and(|e| e != 0) || failed,
                            result_head: clean_text(truncate_chars(out, 400)),
                            task_hashes: Vec::new(),
                        });
                        t.end_offset = offset;
                    }
                    Some("FileChange") => {
                        let Some(t) = current.as_mut() else { continue };
                        let files: Vec<String> = item
                            .get("changes")
                            .and_then(|c| c.as_object())
                            .map(|m| m.keys().map(|k| clean_text(k)).collect())
                            .unwrap_or_default();
                        for f in &files {
                            if !f.is_empty() && !t.files_touched.contains(f) {
                                t.files_touched.push(f.clone());
                            }
                        }
                        let out = item.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
                        t.tools.push(ToolCall {
                            name: "apply_patch".into(),
                            target: files.join(", "),
                            exit_code: None,
                            is_error: item.get("status").and_then(|x| x.as_str())
                                != Some("completed"),
                            result_head: clean_text(truncate_chars(out, 400)),
                            task_hashes: Vec::new(),
                        });
                        t.end_offset = offset;
                    }
                    _ => {}
                }
            }
            "event_msg" if p.get("type").and_then(|t| t.as_str()) == Some("user_message") => {
                let prompt = clean_text(&strip_harness_blocks(
                    p.get("message").and_then(|m| m.as_str()).unwrap_or(""),
                ));
                if prompt.trim().is_empty() {
                    continue;
                }
                if let Some(t) = current.take() {
                    session.turns.push(t);
                }
                let index = session.turns.len();
                current = Some(Turn {
                    index,
                    timestamp: ts,
                    user_prompt: prompt,
                    end_offset: offset,
                    start_offset: line_start,
                    automated: !(by_person && turn_by_person),
                    ..Default::default()
                });
            }
            // A typed item says nothing of a function tool's input. Beside typed items a call
            // is read here for one thing only: the task it may hand to another agent.
            "response_item"
                if items
                    && with_tasks
                    && p.get("type").and_then(|x| x.as_str()) == Some("function_call") =>
            {
                let Some(t) = current.as_mut() else { continue };
                let name = p.get("name").and_then(|x| x.as_str()).unwrap_or("");
                let input = p
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .and_then(|a| serde_json::from_str::<Value>(a).ok())
                    .unwrap_or(Value::Null);
                let task_hashes = crate::agent::task_hashes(name, &input);
                if !task_hashes.is_empty() {
                    t.tools.push(ToolCall {
                        name: name.to_string(),
                        target: clean_text(truncate_chars(&input.to_string(), 160)),
                        task_hashes,
                        ..Default::default()
                    });
                    t.end_offset = offset;
                }
            }
            "response_item" if !items => {
                let Some(t) = current.as_mut() else { continue };
                match p.get("type").and_then(|x| x.as_str()) {
                    Some("message")
                        if p.get("role").and_then(|r| r.as_str()) == Some("assistant") =>
                    {
                        let s = content_text(p.get("content").unwrap_or(&Value::Null));
                        if !s.trim().is_empty() {
                            if !t.assistant_text.is_empty() {
                                t.assistant_text.push('\n');
                            }
                            t.assistant_text.push_str(&clean_text(&s));
                        }
                    }
                    Some("function_call") | Some("custom_tool_call") | Some("local_shell_call") => {
                        let name = p
                            .get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("shell")
                            .to_string();
                        let args = p
                            .get("arguments")
                            .and_then(|a| a.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                p.get("input").map(|i| i.to_string()).unwrap_or_default()
                            });
                        let target = serde_json::from_str::<Value>(&args)
                            .ok()
                            .and_then(|a| a.get("cmd").or_else(|| a.get("command")).cloned())
                            .map(|c| match c {
                                Value::Array(parts) => parts
                                    .iter()
                                    .filter_map(|x| x.as_str())
                                    .collect::<Vec<_>>()
                                    .join(" "),
                                Value::String(s) => s,
                                other => other.to_string(),
                            })
                            .unwrap_or_else(|| truncate_chars(&args, 160).to_string());
                        let id = p
                            .get("call_id")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        pending.insert(id, (t.index, t.tools.len()));
                        t.tools.push(ToolCall {
                            name,
                            target: clean_text(&target),
                            ..Default::default()
                        });
                    }
                    Some("function_call_output")
                    | Some("custom_tool_call_output")
                    | Some("local_shell_call_output") => {
                        let id = p.get("call_id").and_then(|x| x.as_str()).unwrap_or("");
                        if let Some(&(_, ci)) = pending.get(id) {
                            let out = p
                                .get("output")
                                .map(|o| match o {
                                    Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                })
                                .unwrap_or_default();
                            // Codex embeds the exit code in the output text: "Exit code: N"
                            let exit = exit_re()
                                .captures(&out)
                                .and_then(|c| c[1].parse::<i64>().ok());
                            if let Some(tc) = t.tools.get_mut(ci) {
                                tc.exit_code = exit;
                                tc.is_error = exit.is_some_and(|e| e != 0);
                                tc.result_head = clean_text(truncate_chars(&out, 400));
                            }
                        }
                    }
                    _ => {}
                }
                t.end_offset = offset;
            }
            _ => {}
        }
        session.end_offset = offset;
    }
    if let Some(t) = current.take() {
        session.turns.push(t);
    }
    session.end_offset = offset;
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines as codex-cli 0.154.0 wrote them (a live session, trimmed): typed items,
    /// with the code-mode `exec` wrappers alongside.
    #[test]
    fn typed_items_of_codex_0_154() {
        let lines = [
            r#"{"timestamp":"t0","type":"session_meta","payload":{"id":"s154","cwd":"/r"}}"#,
            r#"{"timestamp":"t1","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"run ls"}]}}"#,
            r#"{"timestamp":"t1","type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"run ls -a. then add probe.txt"}]}}}"#,
            r#"{"timestamp":"t2","type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","content":[{"type":"Text","text":"Running it."}],"phase":"commentary"}}}"#,
            r#"{"timestamp":"t2","type":"response_item","payload":{"type":"custom_tool_call","call_id":"c1","name":"exec","input":"const r = await tools.exec_command({\"cmd\":\"ls -a.\"});"}}"#,
            r#"{"timestamp":"t3","type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","command":["/usr/bin/bash","-lc","ls -a."],"status":"failed","aggregated_output":"ls: invalid option -- '.'\n","exit_code":2}}}"#,
            r#"{"timestamp":"t4","type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","changes":{"/r/probe.txt":{"type":"add","content":"hello\n"}},"status":"completed","stdout":"Success. Updated the following files:\nA probe.txt\n"}}}"#,
            r#"{"timestamp":"t5","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}"#,
            r#"{"timestamp":"t5","type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","content":[{"type":"Text","text":"Done."}],"phase":"final_answer"}}}"#,
        ];
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("rollout-x.jsonl");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let s = parse(&path, 0).unwrap();
        assert_eq!(s.session_id, "s154");
        assert_eq!(s.turns.len(), 1);
        let t = &s.turns[0];
        assert_eq!(t.user_prompt, "run ls -a. then add probe.txt");
        assert_eq!(t.assistant_text, "Running it.\nDone.");
        assert_eq!(t.tools.len(), 2);
        assert_eq!(t.tools[0].name, "shell");
        assert_eq!(t.tools[0].target, "ls -a.");
        assert_eq!(t.tools[0].exit_code, Some(2));
        assert!(t.tools[0].is_error);
        assert_eq!(t.tools[1].name, "apply_patch");
        assert!(!t.tools[1].is_error);
        assert_eq!(t.files_touched, vec!["/r/probe.txt"]);
    }

    /// A function tool that hands a task to another agent leaves the task's hash for ingest
    /// to match the worker's prompt against, and leaves nothing for a read hook's parse. A
    /// call with no long string in its input (a wait, a sleep) is not a tool call on record.
    #[test]
    fn a_function_call_beside_typed_items_is_read_for_the_task_it_hands_over() {
        let task = "List every harbour table that has no station and say which file holds it.";
        let call = |name: &str, args: serde_json::Value| {
            serde_json::json!({"timestamp": "t2", "type": "response_item",
                "payload": {"type": "function_call", "name": name, "call_id": "c",
                            "arguments": args.to_string()}})
            .to_string()
        };
        let lines = [
            r#"{"timestamp":"t0","type":"session_meta","payload":{"id":"s","cwd":"/r","source":"cli"}}"#.to_string(),
            r#"{"timestamp":"t1","type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"split the harbour work"}]}}}"#.to_string(),
            call("wait", serde_json::json!({"cell_id": "7", "yield_time_ms": 1000})),
            call("spawn_agent", serde_json::json!({"message": task})),
        ];
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("rollout-y.jsonl");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let s = parse_with_tasks(&path, 0).unwrap();
        let tools = &s.turns[0].tools;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "spawn_agent");
        assert_eq!(
            tools[0].task_hashes,
            vec![crate::agent::text_hash(task).unwrap()]
        );
        assert!(parse(&path, 0).unwrap().turns[0].tools.is_empty());
    }
}
