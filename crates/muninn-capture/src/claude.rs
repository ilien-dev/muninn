//! Claude Code transcript (`transcript_path`): one JSON object per line.
//!
//! `type: user` and `type: assistant` lines carry conversation, and so does one kind of
//! `attachment`: `queued_command`, a message typed while the agent was working. `isMeta` and
//! `isSidechain` lines are skipped; everything else (`mode`, `file-history-*`, `cost-state`, …)
//! is bookkeeping and ignored. Unknown shapes never fail the parse: a torn or foreign line is
//! dropped.

use crate::model::{Session, ToolCall, Turn};
use muninn_core::sanitize::{clean_text, strip_harness_blocks, truncate_chars};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom};

const RESULT_HEAD: usize = 400;
const ANSWERED: &str = "Your questions have been answered:";

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| match b.get("type").and_then(|t| t.as_str()) {
                Some("text") => b.get("text").and_then(|t| t.as_str()).map(str::to_string),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn tool_target(name: &str, input: &Value) -> String {
    let get = |k: &str| {
        input
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    match name {
        "Bash" => get("command"),
        "Edit" | "Write" | "MultiEdit" | "Read" | "NotebookEdit" => {
            let p = get("file_path");
            if p.is_empty() {
                get("notebook_path")
            } else {
                p
            }
        }
        "Grep" | "Glob" => format!("{} {}", get("pattern"), get("path"))
            .trim()
            .to_string(),
        _ => truncate_chars(&input.to_string(), 160).to_string(),
    }
}

/// Parse from `start_offset` (a previous watermark) to the end of the file.
/// User steering that Claude Code delivers inside a tool_result rather than as a
/// prompt: a rejected tool use (`... the user said:\n<text>`), answers to
/// AskUserQuestion (`Your questions have been answered: ...`), and a plan approval.
fn user_feedback(body: &str) -> Option<String> {
    const REJECT: &str = "The user doesn't want to proceed";
    const MARK: &str = "the user said:";
    const APPROVED: &str = "User has approved your plan";
    if body.starts_with(REJECT) {
        let i = body.find(MARK)?;
        let text = body[i + MARK.len()..].trim();
        return (!text.is_empty()).then(|| text.to_string());
    }
    if body.starts_with(ANSWERED) {
        return Some(truncate_chars(body.trim(), 1_500).to_string());
    }
    if body.starts_with(APPROVED) {
        return Some("User approved the plan.".to_string());
    }
    None
}

/// The text of a message the person queued while the agent worked. The same attachment also
/// carries task notifications, peer messages and reminders, which nobody typed; only
/// `commandMode: prompt` from a human, or from before the harness recorded an origin, counts.
fn queued_prompt(v: &Value) -> Option<String> {
    let a = v.get("attachment")?;
    if a.get("type").and_then(|t| t.as_str()) != Some("queued_command")
        || a.get("commandMode").and_then(|m| m.as_str()) != Some("prompt")
        || a.get("isMeta").and_then(|b| b.as_bool()).unwrap_or(false)
    {
        return None;
    }
    match a.get("origin").and_then(|o| o.get("kind")) {
        None | Some(Value::Null) => {}
        Some(kind) if kind.as_str() == Some("human") => {}
        Some(_) => return None,
    }
    Some(text_of(a.get("prompt")?))
}

pub fn parse(path: &std::path::Path, start_offset: u64) -> std::io::Result<Session> {
    let mut f = std::fs::File::open(path)?;
    if start_offset > 0 {
        f.seek(SeekFrom::Start(start_offset))?;
    }
    let mut reader = std::io::BufReader::new(f);
    let mut session = Session {
        harness: "claude-code".into(),
        end_offset: start_offset,
        ..Default::default()
    };
    let mut pending: HashMap<String, (usize, usize)> = HashMap::new(); // tool_use_id -> (turn idx, tool idx)
    let mut current: Option<Turn> = None;
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
            // torn last line: leave it for the next run
            offset -= n as u64;
            break;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&raw) else {
            continue;
        };
        if session.session_id.is_empty() {
            if let Some(s) = v.get("sessionId").and_then(|s| s.as_str()) {
                session.session_id = s.to_string();
            }
        }
        if session.cwd.is_none() {
            if let Some(c) = v.get("cwd").and_then(|s| s.as_str()) {
                session.cwd = Some(c.to_string());
            }
        }
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let automated = v
            .get("entrypoint")
            .and_then(|e| e.as_str())
            .is_some_and(|e| e.starts_with("sdk-"));
        if ty == "attachment" {
            let sidechain = v
                .get("isSidechain")
                .and_then(|b| b.as_bool())
                .unwrap_or(false);
            if let Some(text) = queued_prompt(&v).filter(|_| !sidechain) {
                let prompt = clean_text(&strip_harness_blocks(&text));
                if !prompt.trim().is_empty() {
                    if let Some(mut t) = current.take() {
                        t.end_offset = t.end_offset.max(session.end_offset);
                        session.turns.push(t);
                    }
                    current = Some(Turn {
                        index: session.turns.len(),
                        timestamp: v
                            .get("timestamp")
                            .and_then(|t| t.as_str())
                            .map(str::to_string),
                        user_prompt: prompt,
                        end_offset: offset,
                        start_offset: line_start,
                        automated,
                        ..Default::default()
                    });
                    session.end_offset = offset;
                }
            }
            continue;
        }
        if ty != "user" && ty != "assistant" {
            continue;
        }
        if v.get("isMeta").and_then(|b| b.as_bool()).unwrap_or(false)
            || v.get("isSidechain")
                .and_then(|b| b.as_bool())
                .unwrap_or(false)
        {
            continue;
        }
        let Some(msg) = v.get("message") else {
            continue;
        };
        let content = msg.get("content").cloned().unwrap_or(Value::Null);
        let ts = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .map(str::to_string);

        if ty == "user" {
            // tool results ride on user lines
            if let Value::Array(blocks) = &content {
                let results: Vec<&Value> = blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
                    .collect();
                if !results.is_empty() {
                    for r in results {
                        let id = r.get("tool_use_id").and_then(|s| s.as_str()).unwrap_or("");
                        if let Some(&(ti, ci)) = pending.get(id) {
                            let body = r.get("content").map(text_of).unwrap_or_default();
                            let is_error =
                                r.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false);
                            let exit = v
                                .get("toolUseResult")
                                .and_then(|t| t.get("exitCode").or_else(|| t.get("exit_code")))
                                .and_then(|x| x.as_i64());
                            let target = if let Some(t) = current.as_mut().filter(|t| t.index == ti)
                            {
                                t.tools.get_mut(ci)
                            } else {
                                session
                                    .turns
                                    .iter_mut()
                                    .find(|t| t.index == ti)
                                    .and_then(|t| t.tools.get_mut(ci))
                            };
                            if let Some(tc) = target {
                                tc.is_error = is_error;
                                tc.exit_code = exit;
                                tc.result_head = clean_text(truncate_chars(&body, RESULT_HEAD));
                            }
                        }
                    }
                    if let Some(t) = current.as_mut() {
                        t.end_offset = offset;
                    }
                    // A rejected tool use (plan rejection, denied edit) carries the
                    // user's steering text inside the tool_result; it is a user turn.
                    let feedback: Vec<String> = blocks
                        .iter()
                        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
                        .filter_map(|r| r.get("content").map(text_of))
                        .filter_map(|body| user_feedback(&body))
                        .collect();
                    if feedback.is_empty() {
                        continue;
                    }
                    let answered = feedback.iter().any(|f| f.starts_with(ANSWERED));
                    let prompt = clean_text(&strip_harness_blocks(&feedback.join("\n")));
                    if let Some(mut t) = current.take() {
                        t.end_offset = t.end_offset.max(session.end_offset);
                        session.turns.push(t);
                    }
                    let index = session.turns.len();
                    current = Some(Turn {
                        index,
                        timestamp: ts,
                        user_prompt: prompt,
                        end_offset: offset,
                        start_offset: line_start,
                        automated,
                        answered,
                        ..Default::default()
                    });
                    continue;
                }
            }
            let prompt = clean_text(&strip_harness_blocks(&text_of(&content)));
            if prompt.trim().is_empty() {
                continue;
            }
            // a new turn starts
            if let Some(mut t) = current.take() {
                t.end_offset = t.end_offset.max(session.end_offset);
                session.turns.push(t);
            }
            let index = session.turns.len();
            current = Some(Turn {
                index,
                timestamp: ts,
                user_prompt: prompt,
                end_offset: offset,
                start_offset: line_start,
                automated,
                ..Default::default()
            });
        } else {
            let Some(t) = current.as_mut() else { continue };
            if let Value::Array(blocks) = &content {
                for b in blocks {
                    match b.get("type").and_then(|x| x.as_str()) {
                        Some("text") => {
                            let s = b.get("text").and_then(|x| x.as_str()).unwrap_or("");
                            if !s.trim().is_empty() {
                                if !t.assistant_text.is_empty() {
                                    t.assistant_text.push('\n');
                                }
                                t.assistant_text.push_str(&clean_text(s));
                            }
                        }
                        Some("tool_use") => {
                            let name = b
                                .get("name")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            let input = b.get("input").cloned().unwrap_or(Value::Null);
                            let target = clean_text(&tool_target(&name, &input));
                            if matches!(
                                name.as_str(),
                                "Edit" | "Write" | "MultiEdit" | "NotebookEdit"
                            ) && !target.is_empty()
                                && !t.files_touched.contains(&target)
                            {
                                t.files_touched.push(target.clone());
                            }
                            let id = b
                                .get("id")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            pending.insert(id, (t.index, t.tools.len()));
                            t.tools.push(ToolCall {
                                name,
                                target,
                                ..Default::default()
                            });
                        }
                        _ => {}
                    }
                }
            }
            t.end_offset = offset;
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
mod queued_tests {
    use super::parse;
    use serde_json::json;

    fn parsed(lines: &[serde_json::Value]) -> crate::model::Session {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("t.jsonl");
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&p, text).unwrap();
        parse(&p, 0).unwrap()
    }

    fn queued(prompt: &str, origin: serde_json::Value) -> serde_json::Value {
        json!({"type": "attachment", "sessionId": "s", "timestamp": "2026-10-01T00:00:00Z",
               "attachment": {"type": "queued_command", "commandMode": "prompt",
                              "prompt": prompt, "origin": origin}})
    }

    /// A message typed while the agent worked is a turn. What else travels in the same
    /// attachment is not: nobody typed a task notification or a peer's message.
    #[test]
    fn a_queued_message_is_a_turn_and_a_notification_is_not() {
        let s = parsed(&[
            json!({"type": "user", "sessionId": "s", "message": {"content": "primero"}}),
            queued("nunca borres esto", json!({"kind": "human"})),
            queued("sin origen", serde_json::Value::Null),
            queued("de otro agente", json!({"kind": "peer"})),
            json!({"type": "attachment", "attachment": {"type": "queued_command",
                   "commandMode": "task-notification", "prompt": "terminó"}}),
        ]);
        let prompts: Vec<&str> = s.turns.iter().map(|t| t.user_prompt.as_str()).collect();
        assert_eq!(prompts, ["primero", "nunca borres esto", "sin origen"]);
        assert!(s.turns[1].start_offset > s.turns[0].start_offset);
    }

    #[test]
    fn a_program_prompt_is_marked_automated() {
        let s = parsed(&[
            json!({"type": "user", "entrypoint": "sdk-cli", "message": {"content": "hazlo"}}),
            json!({"type": "user", "entrypoint": "cli", "message": {"content": "hazlo tú"}}),
        ]);
        assert!(s.turns[0].automated);
        assert!(!s.turns[1].automated);
    }
}

#[cfg(test)]
mod feedback_tests {
    use super::user_feedback;

    #[test]
    fn user_feedback_extracts_steering_text() {
        let body = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). To tell you how to proceed, the user said:\nLos sidecar de embeddings deben ser parte del MVP.";
        assert_eq!(
            user_feedback(body).as_deref(),
            Some("Los sidecar de embeddings deben ser parte del MVP.")
        );
        assert_eq!(
            user_feedback("The user doesn't want to proceed with this tool use."),
            None
        );
        assert_eq!(user_feedback("ok"), None);
    }
}
