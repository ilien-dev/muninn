//! Secret redaction before anything is stored: known token shapes plus a
//! Shannon-entropy check on long opaque strings. Home directories are
//! anonymised so records can be shared.

use regex::Regex;
use std::sync::OnceLock;

fn patterns() -> &'static [Regex] {
    static P: OnceLock<Vec<Regex>> = OnceLock::new();
    P.get_or_init(|| {
        [
            r"sk-ant-[A-Za-z0-9_\-]{20,}",
            r"sk-[A-Za-z0-9]{20,}",
            r"ghp_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}",
            r"AKIA[0-9A-Z]{16}",
            r"xox[baprs]-[A-Za-z0-9\-]{10,}",
            r"AIza[0-9A-Za-z_\-]{30,}",
            r"[A-Za-z0-9+/]{86}==",
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
            r#"(?i)\b(api[_-]?key|token|secret|password|passwd|authorization)\b\s*[:=]\s*['"]?[A-Za-z0-9_\-./+=]{12,}"#,
            r"eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}",
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    })
}

fn entropy(s: &str) -> f64 {
    let mut counts = [0usize; 256];
    let bytes = s.as_bytes();
    for &b in bytes {
        counts[b as usize] += 1;
    }
    let n = bytes.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// Replace secrets with `[redacted]` and `/home/<user>` with `~`.
pub fn redact(s: &str) -> String {
    let mut out = s.to_string();
    for re in patterns() {
        out = re.replace_all(&out, "[redacted]").to_string();
    }
    // opaque high-entropy tokens ≥ 32 chars with no spaces
    static OPAQUE: OnceLock<Regex> = OnceLock::new();
    let opaque = OPAQUE.get_or_init(|| Regex::new(r"[A-Za-z0-9_\-+/=]{32,}").unwrap());
    out = opaque
        .replace_all(&out, |c: &regex::Captures| {
            let t = &c[0];
            let has_digit = t.chars().any(|ch| ch.is_ascii_digit());
            let has_alpha = t.chars().any(|ch| ch.is_ascii_alphabetic());
            if has_digit && has_alpha && entropy(t) > 4.0 && !t.contains("--") {
                "[redacted]".to_string()
            } else {
                t.to_string()
            }
        })
        .to_string();
    static HOME: OnceLock<Regex> = OnceLock::new();
    let home = HOME.get_or_init(|| Regex::new(r"/(?:home|Users)/[A-Za-z0-9._\-]+").unwrap());
    home.replace_all(&out, "~").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_known_shapes_and_entropy() {
        let s = "key sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789 and ghp_abcdefghijklmnopqrstuvwxyz0123456789 in /home/alice/x";
        let r = redact(s);
        assert!(!r.contains("sk-ant-"));
        assert!(!r.contains("ghp_"));
        assert!(r.contains("~/x"));
        // a plain long identifier survives
        assert_eq!(
            redact("some_long_snake_case_identifier_name_here"),
            "some_long_snake_case_identifier_name_here"
        );
        // hex hashes (commits, blake3) are not secrets and survive: 16 symbols cap entropy at 4 bits
        let h = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        assert_eq!(redact(h), h);
        // an opaque mixed-case token is redacted
        let t = "qA7zP2mX9kL4vB8nR1tY6wC3eH5jD0sF2gK8uN4pQ7";
        assert!(redact(t).contains("[redacted]"));
    }

    #[test]
    fn redacts_azure_storage_account_key() {
        // poison_pill: well-known Azurite emulator key, not a real secret
        let s = "AccountKey=Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==;EndpointSuffix=core.windows.net";
        let r = redact(s);
        assert!(!r.contains("Eby8vdM02"));
        assert!(r.contains("[redacted]"));
    }
}
