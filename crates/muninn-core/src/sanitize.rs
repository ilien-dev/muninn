//! Input sanitisation at the trust boundary. Two published failure classes drive it:
//! 14 "unterminated string" FTS5 errors from stray quotes/control chars and 20
//! encoding failures on day one [Q3].

/// Strip C0/C1 control characters except `\n` and `\t`; drop U+FFFE/U+FFFF.
pub fn clean_text(s: &str) -> String {
    s.chars()
        .filter(|c| {
            let u = *c as u32;
            !(u < 0x20 && *c != '\n' && *c != '\t')
                && u != 0x7F
                && !(0x80..0xA0).contains(&u)
                && u != 0xFFFE
                && u != 0xFFFF
        })
        .collect()
}

/// The harness's own blocks, which arrive inside a `type: user` line and are not the user.
///
/// Claude Code injects task notifications, system reminders and slash-command scaffolding as
/// ordinary user content with no `isMeta` flag. Counted on this project's own transcripts: 134
/// `<task-notification>` blocks and 56 `<command-name>` blocks, every one of them a plain user
/// message. Captured, they become records at **trust 3 — "stated by the user"** — which is the
/// one thing that level is supposed to mean, and one of them tripped a change marker.
const HARNESS_TAGS: [&str; 8] = [
    "system-reminder",
    "task-notification",
    "local-command-stdout",
    "local-command-caveat",
    "command-name",
    "command-message",
    "command-args",
    "command-contents",
];

/// Lines the harness writes on the user's behalf. Not blocks, so they need their own list:
/// found in this project's own store as episodes whose whole content is one of them.
const HARNESS_LINES: [&str; 5] = [
    "[Request interrupted by user]",
    "[Request interrupted by user for tool use]",
    "User approved the plan.",
    "User rejected the plan.",
    "API Error: Request was aborted.",
];

/// Drop those blocks from text about to be attributed to the user.
pub fn strip_harness_blocks(s: &str) -> String {
    let mut out = s.to_string();
    for line in HARNESS_LINES {
        out = out.replace(line, "");
    }
    for tag in HARNESS_TAGS {
        let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
        while let Some(i) = out.find(&open) {
            let end = match out[i..].find(&close) {
                Some(j) => i + j + close.len(),
                // an unterminated block runs to the end of the message
                None => out.len(),
            };
            out.replace_range(i..end, "");
        }
    }
    out
}

/// Bytes to text, never failing: invalid sequences become U+FFFD.
pub fn from_bytes_lossy(b: &[u8]) -> String {
    clean_text(&String::from_utf8_lossy(b))
}

/// Fold a Latin letter onto its unaccented form, the way FTS5's `remove_diacritics 2`
/// folds it when indexing. The query side has to agree with the index side or an accented
/// word never matches: `móvil` is stored as `movil`, and a term that keeps the accent —
/// or, as this function once did, drops the accented letter altogether (`mvil`) — matches
/// nothing. Only the Latin-1 and Latin Extended-A ranges are covered, which is what
/// `remove_diacritics` itself handles.
fn fold_diacritic(c: char) -> Option<char> {
    let out = match c {
        'à'..='å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ď' | 'đ' => 'd',
        'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ĝ'..='ġ' | 'ģ' => 'g',
        'ĥ' | 'ħ' => 'h',
        'ì'..='ï' | 'ĩ'..='ı' => 'i',
        'ĵ' => 'j',
        'ķ' | 'ĸ' => 'k',
        'ĺ'..='ł' => 'l',
        'ñ' | 'ń'..='ŉ' => 'n',
        'ò'..='ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'ś' | 'ŝ' | 'ş' | 'š' => 's',
        'ţ' | 'ť' | 'ŧ' => 't',
        'ù'..='ü' | 'ũ'..='ų' => 'u',
        'ŵ' => 'w',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        _ => return None,
    };
    Some(out)
}

/// A single FTS5 MATCH token: alphanumerics and underscore only, lowercase, with Latin
/// diacritics folded exactly as the index folds them. Anything else would need quoting,
/// and quoting was the failure [Q3].
pub fn fts_term(s: &str) -> Option<String> {
    let t: String = s
        .chars()
        .flat_map(|c| c.to_lowercase())
        .filter_map(|c| match fold_diacritic(c) {
            // a script `unicode61` indexes but this function used to drop — Cyrillic, Greek,
            // CJK — was unsearchable for the same reason the accents were
            None if c.is_alphanumeric() || c == '_' => Some(c),
            folded => folded,
        })
        .collect();
    if t.len() < 2 || t.chars().all(|c| c == '_') {
        None
    } else {
        Some(t)
    }
}

/// Build a MATCH expression `"a" OR "b" ...` from already-sanitised terms.
pub fn fts_match_or(terms: &[String]) -> String {
    terms
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Truncate to at most `max_chars` characters on a char boundary.
pub fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Read a file as text only if it is a regular file, and at most `max_bytes` of it.
/// A device, FIFO or symlink to one (`/dev/full` in the fault suite) never blocks a
/// hook; an oversized log is folded in pieces by successive runs.
pub fn read_regular_bounded(path: &std::path::Path, max_bytes: u64) -> std::io::Result<String> {
    use std::io::Read;
    let f = std::fs::File::open(path)?;
    let md = f.metadata()?;
    if !md.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let mut buf = Vec::with_capacity(md.len().min(max_bytes) as usize);
    f.take(max_bytes).read_to_end(&mut buf)?;
    Ok(from_bytes_lossy(&buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_control_keeps_newline() {
        assert_eq!(clean_text("a\u{0}b\nc\u{1B}[0m"), "ab\nc[0m");
    }

    #[test]
    fn term_rejects_quotes() {
        assert_eq!(fts_term("O'Reilly\""), Some("oreilly".into()));
        assert_eq!(fts_term("'\""), None);
        assert_eq!(fts_term("a"), None);
    }

    #[test]
    fn term_folds_diacritics_as_the_index_does() {
        // `remove_diacritics 2` stores these folded; a term that keeps the accent, or that
        // drops the accented letter, matches nothing
        assert_eq!(fts_term("móvil"), Some("movil".into()));
        assert_eq!(fts_term("Español"), Some("espanol".into()));
        assert_eq!(fts_term("ÜBER"), Some("uber".into()));
        assert_eq!(fts_term("año"), Some("ano".into()));
        // a script unicode61 indexes is kept rather than emptied
        assert_eq!(fts_term("日本語"), Some("日本語".into()));
        assert_eq!(fts_term("Кириллица"), Some("кириллица".into()));
    }

    #[test]
    fn lossy_never_panics() {
        let s = from_bytes_lossy(&[0xff, 0xfe, b'o', b'k']);
        assert!(s.ends_with("ok"));
    }

    #[test]
    fn truncate_on_boundary() {
        assert_eq!(truncate_chars("héllo", 2), "hé");
    }
}

#[cfg(test)]
mod harness_tests {
    use super::strip_harness_blocks;

    /// Claude Code injects its own blocks inside `type: user` lines with no `isMeta` flag.
    /// Counted on this project's transcripts: 134 `<task-notification>` and 56
    /// `<command-name>`, every one a plain user message. Captured, they become records at
    /// trust 3 — "stated by the user" — which is the one thing that level means.
    #[test]
    fn the_harness_is_not_the_user() {
        let s = "<task-notification>\n<task-id>b46</task-id>\n<summary>done</summary>\n</task-notification>";
        assert_eq!(strip_harness_blocks(s).trim(), "");

        let mixed =
            "please switch to rustls\n<system-reminder>be careful</system-reminder>\nthanks";
        let out = strip_harness_blocks(mixed);
        assert!(out.contains("switch to rustls"), "{out}");
        assert!(out.contains("thanks"), "{out}");
        assert!(!out.contains("be careful"), "{out}");

        // an unterminated block runs to the end rather than surviving
        let open = "ok\n<system-reminder>cut here and everything after";
        assert_eq!(strip_harness_blocks(open).trim(), "ok");

        // ordinary text with angle brackets is untouched
        let code = "use Vec<String> and keep it";
        assert_eq!(strip_harness_blocks(code), code);

        // and the lines the harness writes on the user's behalf are not the user either
        assert_eq!(
            strip_harness_blocks("[Request interrupted by user]").trim(),
            ""
        );
        assert_eq!(strip_harness_blocks("User approved the plan.").trim(), "");
        let keep = "the user approved the plan we discussed";
        assert_eq!(strip_harness_blocks(keep), keep);
    }
}
