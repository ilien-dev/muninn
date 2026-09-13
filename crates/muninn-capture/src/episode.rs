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

/// Split `text` into chunks of at most `max` chars, cutting on blank lines, then lines,
/// then whitespace. Literal: nothing is rewritten.
fn chunks(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        if rest.chars().count() <= max {
            out.push(rest.to_string());
            break;
        }
        let head = truncate_chars(rest, max);
        let cut = head
            .rfind("\n\n")
            .filter(|&i| i > max / 3)
            .or_else(|| head.rfind('\n').filter(|&i| i > max / 3))
            .or_else(|| head.rfind(' ').filter(|&i| i > max / 3))
            .unwrap_or(head.len());
        out.push(rest[..cut].trim().to_string());
        rest = rest[cut..].trim();
    }
    out
}

/// Every episode a turn yields: the summary episode from [`from_turn`], then the
/// rest of a long user prompt or assistant text as literal continuation chunks, so a
/// long turn (a pasted document, a compaction summary) is stored whole and not just
/// its head. At most `MAX_CHUNKS` continuations per turn.
pub fn from_turn_all(session_id: &str, t: &Turn) -> Vec<Episode> {
    const MAX_CHUNKS: usize = 12;
    const CHUNK: usize = 1_800;
    let Some(first) = from_turn(session_id, t) else {
        return Vec::new();
    };
    let mut out = vec![first];
    let mut k = 0usize;
    let mut push = |label: &str, text: &str| {
        for c in chunks(text, CHUNK) {
            if k >= MAX_CHUNKS {
                break;
            }
            k += 1;
            let body = redact(&format!("{label} (cont. {k}): {c}\n"));
            out.push(Episode {
                subject: format!("session:{}#{}.{}", session_id, t.index, k),
                object: redact(&first_line(c.as_str(), 160)),
                body: truncate_chars(&body, MAX_BODY_CHARS).to_string(),
                files: Vec::new(),
                had_failure: false,
            });
        }
    };
    let up = t.user_prompt.trim();
    if up.chars().count() > 600 && !(up.starts_with('<') && up.contains("</")) {
        let rest: String = up.chars().skip(600).collect();
        push("user", &rest);
    }
    let a = t.assistant_text.trim();
    if a.chars().count() > 700 {
        push("assistant", a);
    }
    out
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

#[cfg(test)]
mod chunk_tests {
    use super::*;

    #[test]
    fn long_turns_become_several_literal_episodes() {
        let t = Turn {
            user_prompt: (0..40)
                .map(|i| format!("paragraph {i} {}", "w".repeat(90)))
                .collect::<Vec<_>>()
                .join("\n\n"),
            assistant_text: "short".into(),
            ..Default::default()
        };
        let eps = from_turn_all("s", &t);
        assert!(eps.len() > 2, "{}", eps.len());
        assert_eq!(eps[0].subject, "session:s#0");
        assert_eq!(eps[1].subject, "session:s#0.1");
        assert!(eps[1].body.starts_with("user (cont. 1): "));
        for e in &eps {
            assert!(e.body.chars().count() <= MAX_BODY_CHARS);
        }
        // every paragraph survives somewhere
        let all: String = eps.iter().map(|e| e.body.clone()).collect();
        assert!(all.contains("paragraph 39"));
        let c = chunks("a b c", 10);
        assert_eq!(c, vec!["a b c"]);
    }
}
