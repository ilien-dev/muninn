//! Turn → literal episode. No summarisation by a model: a fixed template that
//! keeps the user's words, the assistant's stated conclusion, the commands
//! that ran and how they ended, and the files touched.

use crate::model::Turn;
use crate::redact::redact;
use muninn_core::caps::MAX_BODY_CHARS;
use muninn_core::sanitize::truncate_chars;

#[derive(Debug, Clone)]
pub struct Episode {
    pub subject: String,
    pub object: String,
    pub body: String,
    pub files: Vec<String>,
    pub had_failure: bool,
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn first_line(s: &str, max: usize) -> String {
    let l = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    truncate_chars(l, max).to_string()
}

pub fn from_turn(session_id: &str, t: &Turn) -> Option<Episode> {
    if t.user_prompt.trim().is_empty() {
        return None;
    }
    // slash-command and caveat wrappers are noise unless work was done under them
    let raw = t.user_prompt.trim();
    let tagged = raw.starts_with('<') && raw.contains("</");
    if tagged && t.tools.is_empty() {
        return None;
    }
    let stripped;
    let up: &str = if tagged {
        stripped = strip_tags(raw);
        stripped.trim()
    } else {
        raw
    };
    if up.is_empty() {
        return None;
    }
    let mut body = String::new();
    body.push_str("user: ");
    body.push_str(truncate_chars(up, 600));
    body.push('\n');
    if !t.assistant_text.trim().is_empty() {
        body.push_str("assistant: ");
        // last paragraph is usually the conclusion; keep head + tail
        let a = t.assistant_text.trim();
        if a.chars().count() > 700 {
            let head: String = a.chars().take(350).collect();
            let tail: String = a
                .chars()
                .rev()
                .take(300)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            body.push_str(&head);
            body.push_str(" … ");
            body.push_str(&tail);
        } else {
            body.push_str(a);
        }
        body.push('\n');
    }
    let mut had_failure = false;
    let cmds: Vec<String> = t
        .tools
        .iter()
        .filter(|c| c.name == "Bash" || c.name == "shell" || c.name.ends_with("_call"))
        .map(|c| {
            let status = match (c.exit_code, c.is_error) {
                (Some(0), _) => "ok".to_string(),
                (Some(n), _) => {
                    had_failure = true;
                    format!("exit {n}")
                }
                (None, true) => {
                    had_failure = true;
                    "error".to_string()
                }
                (None, false) => "ran".to_string(),
            };
            format!("$ {} → {}", truncate_chars(&c.target, 120), status)
        })
        .collect();
    if !cmds.is_empty() {
        body.push_str("commands:\n");
        for c in cmds.iter().take(12) {
            body.push_str("  ");
            body.push_str(c);
            body.push('\n');
        }
    }
    if !t.files_touched.is_empty() {
        body.push_str("files: ");
        body.push_str(
            &t.files_touched
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        );
        body.push('\n');
    }
    let body = redact(&body);
    let body = truncate_chars(&body, MAX_BODY_CHARS).to_string();
    Some(Episode {
        subject: format!("session:{}#{}", session_id, t.index),
        object: redact(&first_line(up, 160)),
        body,
        files: t.files_touched.iter().map(|f| redact(f)).collect(),
        had_failure,
    })
}
