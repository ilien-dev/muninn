//! Codex rollout (`~/.codex/sessions/**/rollout-*.jsonl`): lines of
//! `{timestamp, type, payload}`. Conversation lives in `response_item` payloads
//! (`message` with role user/assistant, `function_call`, `function_call_output`)
//! and `event_msg` payloads (`user_message`). Everything else is context.
//!
//! codex-cli 0.154 writes the conversation as typed `event_msg` items instead
//! (`item_completed`: `UserMessage`, `AgentMessage`, `CommandExecution` with its exit
//! code, `FileChange`), and its `response_item` tool calls are code-mode `exec`
//! scripts. Once a rollout shows those items, they are the only source read.

use crate::model::{Session, ToolCall, Turn};
use muninn_core::sanitize::{clean_text, truncate_chars};
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

pub fn parse(path: &std::path::Path, start_offset: u64) -> std::io::Result<Session> {
    let mut f = std::fs::File::open(path)?;
    if start_offset > 0 {
        f.seek(SeekFrom::Start(start_offset))?;
    }
    let mut reader = std::io::BufReader::new(f);
    let mut session = Session {
        harness: "codex".into(),
        end_offset: start_offset,
        ..Default::default()
    };
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
                if let Some(id) = p.get("id").and_then(|s| s.as_str()) {
                    session.session_id = id.to_string();
                }
                if let Some(c) = p.get("cwd").and_then(|s| s.as_str()) {
                    session.cwd = Some(c.to_string());
                }
            }
            "event_msg" if p.get("type").and_then(|t| t.as_str()) == Some("item_completed") => {
                let item = p.get("item").unwrap_or(&Value::Null);
                match item.get("type").and_then(|t| t.as_str()) {
                    Some("UserMessage") => {
                        let prompt =
                            clean_text(&content_text(item.get("content").unwrap_or(&Value::Null)));
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
                            ..Default::default()
                        });
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
                        });
                        t.end_offset = offset;
                    }
                    _ => {}
                }
            }
            "event_msg" if p.get("type").and_then(|t| t.as_str()) == Some("user_message") => {
                let prompt = clean_text(p.get("message").and_then(|m| m.as_str()).unwrap_or(""));
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
                    ..Default::default()
                });
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
}
