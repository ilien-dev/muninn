//! Rule candidate extraction from Markdown instruction files.
//!
//! A candidate is any list item or sentence that carries an imperative marker
//! (never / always / do not / must / …). The literal text is preserved with its
//! source file, line and file hash so the compiled artefact can be traced back
//! and invalidated when the source changes.

use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Candidate {
    /// Path relative to the project root.
    pub source_file: String,
    pub source_line: usize,
    pub source_hash: String,
    /// The rule text, literal, single line, trimmed of list markers and emphasis.
    pub text: String,
    /// Nearest heading above the rule (context only; never used for enforcement).
    pub heading: Option<String>,
}

fn imperative_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(never|always|do not|don'?t|must not|must|mustn'?t|should not|shouldn'?t|forbidden|prohibited|not allowed|only run|only use|before committing|before pushing|before any commit|ask (?:the user )?before|ask first|nunca|siempre|no debes|debe|prohibido)\b",
        )
        .unwrap()
    })
}

/// Strip Markdown noise while keeping the words: list markers, numbering,
/// emphasis, checkbox, trailing colon-only headers.
fn clean_line(raw: &str) -> String {
    let mut s = raw.trim();
    // list markers and numbering
    loop {
        let before = s;
        s = s.trim_start_matches(['-', '*', '+', '>']).trim_start();
        if let Some(rest) = strip_numbering(s) {
            s = rest.trim_start();
        }
        s = s
            .trim_start_matches("[ ]")
            .trim_start_matches("[x]")
            .trim_start();
        if s == before {
            break;
        }
    }
    let s = s.replace("**", "").replace("__", "");
    let s = s
        .trim_matches(|c: char| c == '*' || c == '_' || c == '~')
        .trim();
    // collapse whitespace
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_numbering(s: &str) -> Option<&str> {
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    let rest = &s[digits..];
    rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))
}

/// Extract candidates from one Markdown document.
pub fn extract(source_file: &str, content: &str) -> Vec<Candidate> {
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    let mut out = Vec::new();
    let mut heading: Option<String> = None;
    let mut in_fence = false;
    let mut pending: Option<(usize, String)> = None; // multi-line list item accumulation

    let flush = |pending: &mut Option<(usize, String)>,
                 out: &mut Vec<Candidate>,
                 heading: &Option<String>| {
        if let Some((line, text)) = pending.take() {
            let text = clean_line(&text);
            if text.len() >= 12 && imperative_re().is_match(&text) {
                out.push(Candidate {
                    source_file: source_file.to_string(),
                    source_line: line,
                    source_hash: hash.clone(),
                    text,
                    heading: heading.clone(),
                });
            }
        }
    };

    for (i, raw) in content.lines().enumerate() {
        let line_no = i + 1;
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            flush(&mut pending, &mut out, &heading);
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if trimmed.starts_with('#') {
            flush(&mut pending, &mut out, &heading);
            heading = Some(trimmed.trim_start_matches('#').trim().to_string());
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut pending, &mut out, &heading);
            continue;
        }
        // Table rows are reference material, not rules; they are the main source of
        // long "rules" with a stray `never` in them.
        if trimmed.starts_with('|') {
            flush(&mut pending, &mut out, &heading);
            continue;
        }
        let is_item = trimmed.starts_with(['-', '*', '+'])
            || strip_numbering(trimmed).is_some()
            || trimmed.starts_with("[ ]")
            || trimmed.starts_with("[x]");
        let indent = raw.len() - trimmed.len();
        if is_item || indent == 0 || pending.is_none() {
            // a new item or a new paragraph line
            if is_item {
                flush(&mut pending, &mut out, &heading);
                pending = Some((line_no, trimmed.to_string()));
            } else if let Some((_, ref mut acc)) = pending {
                if indent > 0 {
                    acc.push(' ');
                    acc.push_str(trimmed);
                } else {
                    // paragraph continuation: keep sentences separate
                    flush(&mut pending, &mut out, &heading);
                    pending = Some((line_no, trimmed.to_string()));
                }
            } else {
                pending = Some((line_no, trimmed.to_string()));
            }
        } else if let Some((_, ref mut acc)) = pending {
            acc.push(' ');
            acc.push_str(trimmed);
        }
    }
    flush(&mut pending, &mut out, &heading);
    out
}

/// Instruction files Claude Code and Codex load for a project, in load order.
/// Follows `@path` imports from CLAUDE.md up to `max_depth`.
pub fn discover(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for name in ["CLAUDE.md", "AGENTS.md", ".claude/CLAUDE.md"] {
        let p = root.join(name);
        if p.is_file() {
            files.push(p);
        }
    }
    if let Ok(rd) = std::fs::read_dir(root.join(".claude/rules")) {
        let mut rules: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "md"))
            .collect();
        rules.sort();
        files.extend(rules);
    }
    // @imports (depth ≤ 4)
    let mut queue: Vec<(PathBuf, usize)> = files.iter().map(|f| (f.clone(), 0)).collect();
    let import_re = Regex::new(r"(?m)^@([^\s`]+)").unwrap();
    while let Some((f, depth)) = queue.pop() {
        if depth >= 4 {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&f) else {
            continue;
        };
        for cap in import_re.captures_iter(&content) {
            let rel = &cap[1];
            let target = if let Some(home) = rel.strip_prefix("~/") {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(home))
            } else {
                Some(f.parent().unwrap_or(root).join(rel))
            };
            if let Some(t) = target {
                if t.is_file() && !files.contains(&t) {
                    files.push(t.clone());
                    queue.push((t, depth + 1));
                }
            }
        }
    }
    files
}

/// Read and extract from every discovered file. Paths are reported relative to root.
pub fn extract_project(root: &Path) -> Vec<Candidate> {
    let mut out = Vec::new();
    for f in discover(root) {
        let rel = f
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| f.to_string_lossy().to_string());
        if let Ok(content) = std::fs::read(&f) {
            let content = muninn_core::sanitize::from_bytes_lossy(&content);
            out.extend(extract(&rel, &content));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_list_items_and_skips_fences() {
        let md = "# Rules\n\n- **Never** run `rm -rf` outside the repo\n- Use pnpm\n- Always run `cargo fmt`\n  before committing\n\n```bash\nnever do this in code\n```\nPlain prose that says do not push to main.\n";
        let c = extract("CLAUDE.md", md);
        let texts: Vec<&str> = c.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "Never run `rm -rf` outside the repo",
                "Always run `cargo fmt` before committing",
                "Plain prose that says do not push to main."
            ]
        );
        assert_eq!(c[0].source_line, 3);
        assert_eq!(c[0].heading.as_deref(), Some("Rules"));
    }

    #[test]
    fn numbering_and_checkboxes() {
        let md =
            "1. NEVER modify `.gitignore`\n2) Do run tests\n- [ ] Always ask before deploying\n";
        let c = extract("AGENTS.md", md);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].text, "NEVER modify `.gitignore`");
        assert_eq!(c[1].text, "Always ask before deploying");
    }
}
