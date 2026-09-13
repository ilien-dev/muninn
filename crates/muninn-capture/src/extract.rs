//! Typed candidates from a session: what the write path stores besides literal
//! episodes. Every extractor is a fixed rule over the transcript — no model, no
//! rewriting — and every candidate keeps its literal text and its origin, from which
//! trust is derived (ENGINE.md §2.1, §3).

use crate::model::{Session, Turn};
use crate::redact::redact;
use muninn_core::sanitize::truncate_chars;
use regex::Regex;
use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: &'static str,
    pub subject: String,
    pub relation: String,
    pub object: String,
    pub body: String,
    pub origin: &'static str,
    pub anchor_path: Option<String>,
    pub turn_index: usize,
    pub end_offset: u64,
}

/// Trust is a function of origin and nothing else [W3].
pub fn trust_of(origin: &str) -> i64 {
    match origin {
        "user_said" | "review_accepted" => 3,
        "commit_linked" => 2,
        "tool_observed" => 1,
        _ => 0,
    }
}

fn first_line(s: &str, max: usize) -> String {
    truncate_chars(
        s.lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim(),
        max,
    )
    .to_string()
}

/// Lowercase, alphanumerics and single spaces only — the supersession key for a
/// sentence-shaped subject.
pub fn norm_key(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for c in s.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_alphanumeric() {
            out.push(c);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    truncate_chars(out.trim(), max).to_string()
}

fn correction_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // a "no" family at the start of the prompt, or an explicit correction marker
        // within its first 80 characters: a "no quiero" three sentences in is a wish,
        // not a correction (SessionStart was reinjecting those every session)
        Regex::new(
            r"(?is)^\s*(no[,.:;! ]|nope\b|as[ií] no\b|eso no\b|don'?t\b|do not\b|not like that|wrong[,. ]|undo\b|revert\b|stop\b|no quiero\b|no uses\b|no hagas\b)|^.{0,80}?(\bas[ií] no\b|\ben vez de\b|\ben lugar de\b|\bya no\b|\brevierte\b|\brevertimos\b|\bdescart[ae]|\bte dije\b|\ba pesar de que\b|\bnot like that\b|\binstead of\b|\bi said\b|\bi told you\b|\bthat'?s not what\b)",
        )
        .unwrap()
    })
}

fn invariant_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)\b(siempre|nunca|jam[aá]s|never|always|must not|must\b|debe[sn]? de\b|debe[sn]?\b|no debe|prohibido|obligatorio)\b").unwrap()
    })
}

fn commit_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\[([\w./+\-]+)(?: \(root-commit\))? ([0-9a-f]{7,40})\] ([^\n]*)").unwrap()
    })
}

fn is_tagged(s: &str) -> bool {
    let r = s.trim();
    r.starts_with('<') && r.contains("</")
}

/// Sentences end at a newline, or at `.`, `!`, `;` followed by whitespace — so
/// `CLAUDE.md` and `v1.1` stay whole.
fn split_sentences(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let b = s.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        let end = c == b'\n'
            || (matches!(c, b'.' | b'!' | b';')
                && b.get(i + 1).is_none_or(|n| n.is_ascii_whitespace()));
        if end {
            let piece = s[start..i].trim();
            if !piece.is_empty() {
                out.push(piece);
            }
            start = i + 1;
        }
    }
    let tail = s[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

fn user_candidates(sid: &str, t: &Turn, out: &mut Vec<Candidate>) {
    let up = t.user_prompt.trim();
    if up.is_empty() || is_tagged(up) || up.starts_with("This session is being continued") {
        return;
    }
    if correction_re().is_match(up) && up.chars().count() <= 1_500 {
        let body = redact(&format!("user: {}\n", truncate_chars(up, 900)));
        out.push(Candidate {
            kind: "correction",
            subject: format!("correction:{}#{}", &sid[..sid.len().min(8)], t.index),
            relation: "user_said".into(),
            object: redact(&first_line(up, 160)),
            body,
            origin: "user_said",
            anchor_path: t.files_touched.first().cloned(),
            turn_index: t.index,
            end_offset: t.end_offset,
        });
    }
    for sent in split_sentences(up) {
        let n = sent.chars().count();
        if !(12..=220).contains(&n) || sent.contains('?') || !invariant_re().is_match(sent) {
            continue;
        }
        let key = norm_key(sent, 80);
        if key.split(' ').count() < 3 {
            continue;
        }
        out.push(Candidate {
            kind: "invariant",
            subject: key,
            relation: "must".into(),
            object: redact(sent),
            body: redact(&format!("user: {}\n", sent)),
            origin: "user_said",
            anchor_path: None,
            turn_index: t.index,
            end_offset: t.end_offset,
        });
    }
}

fn commit_candidates(t: &Turn, out: &mut Vec<Candidate>) {
    for c in &t.tools {
        if !(c.name == "Bash" || c.name == "shell") || !c.target.contains("git commit") {
            continue;
        }
        for cap in commit_re().captures_iter(&c.result_head) {
            let branch = &cap[1];
            let hash = &cap[2];
            let msg = cap[3].trim();
            let short: String = hash.chars().take(7).collect();
            let files = if t.files_touched.is_empty() {
                String::new()
            } else {
                format!(
                    "files: {}\n",
                    t.files_touched
                        .iter()
                        .take(12)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            out.push(Candidate {
                kind: "decision",
                subject: format!("commit:{short}"),
                relation: "is".into(),
                object: redact(truncate_chars(msg, 160)),
                body: redact(&format!("commit {short} on {branch}: {msg}\n{files}")),
                origin: "commit_linked",
                anchor_path: t.files_touched.first().cloned(),
                turn_index: t.index,
                end_offset: t.end_offset,
            });
        }
    }
}

fn norm_cmd(s: &str) -> String {
    truncate_chars(&s.split_whitespace().collect::<Vec<_>>().join(" "), 120).to_string()
}

fn looks_like_test(cmd: &str) -> bool {
    let c = cmd.to_lowercase();
    [
        "cargo test",
        "pytest",
        "npm test",
        "pnpm test",
        "yarn test",
        "go test",
        "make test",
        "cargo clippy",
        "cargo build",
        "npm run build",
        "tsc",
        "mvn ",
        "gradle",
    ]
    .iter()
    .any(|p| c.contains(p))
}

/// A command that failed repeatedly, or a build/test command whose last run in the
/// session failed, is a dead end: the anti-pattern is worth more than the example [J2].
fn deadend_candidates(session: &Session, out: &mut Vec<Candidate>) {
    #[derive(Default)]
    struct Hist {
        fails: usize,
        last_ok: bool,
        last_exit: Option<i64>,
        last_head: String,
        last_turn: usize,
        last_off: u64,
        cmd: String,
    }
    let mut hist: BTreeMap<String, Hist> = BTreeMap::new();
    for t in &session.turns {
        for c in &t.tools {
            if !(c.name == "Bash" || c.name == "shell") || c.target.trim().is_empty() {
                continue;
            }
            let key = norm_cmd(&c.target);
            let h = hist.entry(key.clone()).or_default();
            let failed =
                matches!(c.exit_code, Some(n) if n != 0) || (c.exit_code.is_none() && c.is_error);
            if failed {
                h.fails += 1;
            }
            h.last_ok = !failed;
            h.last_exit = c.exit_code;
            h.last_head = c.result_head.clone();
            h.last_turn = t.index;
            h.last_off = t.end_offset;
            h.cmd = key;
        }
    }
    for h in hist.values() {
        if h.last_ok {
            continue;
        }
        if h.fails >= 2 || (h.fails >= 1 && looks_like_test(&h.cmd)) {
            let exit = h
                .last_exit
                .map(|n| n.to_string())
                .unwrap_or_else(|| "error".into());
            out.push(Candidate {
                kind: "deadend",
                subject: format!("cmd:{}", h.cmd),
                relation: "tried_and_failed".into(),
                object: format!("exit {exit}, {} failed run(s)", h.fails),
                body: redact(&format!(
                    "$ {}\nexit {exit} after {} failed run(s); last output:\n{}\n",
                    h.cmd,
                    h.fails,
                    truncate_chars(&h.last_head, 700)
                )),
                origin: "tool_observed",
                anchor_path: None,
                turn_index: h.last_turn,
                end_offset: h.last_off,
            });
        }
    }
}

/// All typed candidates of a session, in transcript order (dead ends last).
pub fn extract(session: &Session, sid: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    for t in &session.turns {
        user_candidates(sid, t, &mut out);
        commit_candidates(t, &mut out);
    }
    deadend_candidates(session, &mut out);
    for c in &mut out {
        c.body = truncate_chars(&c.body, muninn_core::caps::MAX_BODY_CHARS).to_string();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ToolCall;

    fn turn(i: usize, prompt: &str) -> Turn {
        Turn {
            index: i,
            user_prompt: prompt.into(),
            ..Default::default()
        }
    }

    #[test]
    fn corrections_and_invariants_from_the_user() {
        let s = Session {
            turns: vec![
                turn(
                    0,
                    "No, así no. En vez de borrar el fichero, márcalo como invalid.",
                ),
                turn(
                    1,
                    "Nunca uses pkill en Bash. Siempre commitea como code@ilien.dev.",
                ),
                turn(2, "¿Debe el hook abrir la base en solo lectura?"),
                turn(3, "Veo que dice que treesitter sera parte de la version 1.1 a pesar de que te dije que tiene que ser parte del MVP"),
                turn(4, "Me gustaria que dentro de la definicion de la herramienta si consideremos tener nuestro propio motor de memoria. No importa si la herramienta ya tiene una. Podemos seguir el estandar pero no quiero depender de la memoria nativa."),
            ],
            ..Default::default()
        };
        let c = extract(&s, "abcdef12");
        let kinds: Vec<&str> = c.iter().map(|x| x.kind).collect();
        assert_eq!(kinds.iter().filter(|k| **k == "correction").count(), 2);
        assert_eq!(kinds.iter().filter(|k| **k == "invariant").count(), 2);
        let inv = c.iter().find(|x| x.kind == "invariant").unwrap();
        assert_eq!(inv.subject, "nunca uses pkill en bash");
        assert_eq!(trust_of(inv.origin), 3);
    }

    #[test]
    fn commits_become_decisions_and_repeated_failures_dead_ends() {
        let mut t0 = turn(0, "commit it");
        t0.tools.push(ToolCall {
            name: "Bash".into(),
            target: "git commit -m 'Phase 3: extract'".into(),
            result_head: "[master 1a2b3c4d] Phase 3: extract\n 2 files changed".into(),
            exit_code: Some(0),
            ..Default::default()
        });
        t0.files_touched.push("crates/x.rs".into());
        let mut t1 = turn(1, "run tests");
        for _ in 0..2 {
            t1.tools.push(ToolCall {
                name: "Bash".into(),
                target: "cargo   test -p muninn-core".into(),
                result_head: "error[E0308]".into(),
                exit_code: Some(101),
                ..Default::default()
            });
        }
        let s = Session {
            turns: vec![t0, t1],
            ..Default::default()
        };
        let c = extract(&s, "abcdef12");
        let d = c.iter().find(|x| x.kind == "decision").unwrap();
        assert_eq!(d.subject, "commit:1a2b3c4");
        assert_eq!(d.anchor_path.as_deref(), Some("crates/x.rs"));
        let de = c.iter().find(|x| x.kind == "deadend").unwrap();
        assert_eq!(de.subject, "cmd:cargo test -p muninn-core");
        assert!(de.object.starts_with("exit 101"));
        // a later success clears the dead end
        let mut s2 = s.clone();
        s2.turns[1].tools.push(ToolCall {
            name: "Bash".into(),
            target: "cargo test -p muninn-core".into(),
            exit_code: Some(0),
            ..Default::default()
        });
        assert!(extract(&s2, "abcdef12").iter().all(|x| x.kind != "deadend"));
    }
}
