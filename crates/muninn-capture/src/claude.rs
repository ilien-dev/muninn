//! Claude Code transcript (`transcript_path`): one JSON object per line.
//!
//! Only `type: user` and `type: assistant` lines carry conversation; `isMeta`
//! and `isSidechain` lines are skipped; everything else (`attachment`, `mode`,
//! `file-history-*`, `cost-state`, …) is bookkeeping and ignored. Unknown
//! shapes never fail the parse: a torn or foreign line is dropped.

use crate::model::{Session, ToolCall, Turn};
use muninn_core::sanitize::{clean_text, truncate_chars};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom};

const RESULT_HEAD: usize = 400;

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
                    continue;
                }
            }
            let prompt = clean_text(&text_of(&content));
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
