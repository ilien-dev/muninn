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

/// Bytes to text, never failing: invalid sequences become U+FFFD.
pub fn from_bytes_lossy(b: &[u8]) -> String {
    clean_text(&String::from_utf8_lossy(b))
}

/// A single FTS5 MATCH token: alphanumerics and underscore only, lowercase.
/// Anything else would need quoting, and quoting was the failure [Q3].
pub fn fts_term(s: &str) -> Option<String> {
    let t: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .flat_map(|c| c.to_lowercase())
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
    fn lossy_never_panics() {
        let s = from_bytes_lossy(&[0xff, 0xfe, b'o', b'k']);
        assert!(s.ends_with("ok"));
    }

    #[test]
    fn truncate_on_boundary() {
        assert_eq!(truncate_chars("héllo", 2), "hé");
    }
}
