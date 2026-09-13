//! Token accounting for delivery budgets.
//!
//! The hot path uses a cheap, deliberately conservative estimate (it overestimates,
//! so a budget expressed in it is never exceeded in real tokens). Exact counting
//! with `tiktoken-rs` (`cl100k_base`) is behind the `exact-tokens` feature and used
//! by CI tests and `muninn init --check-budget`; the real model tokeniser is not
//! public, so even the "exact" count is an approximation and is labelled as such.

/// Conservative estimate: ~3 characters per token for mixed prose/code.
pub fn estimate(s: &str) -> usize {
    s.chars().count().div_ceil(3)
}

#[cfg(feature = "exact-tokens")]
pub fn count_cl100k(s: &str) -> usize {
    use std::sync::OnceLock;
    static BPE: OnceLock<tiktoken_rs::CoreBPE> = OnceLock::new();
    let bpe = BPE.get_or_init(|| tiktoken_rs::cl100k_base().expect("cl100k_base"));
    bpe.encode_ordinary(s).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_is_conservative_on_prose() {
        let s = "the quick brown fox jumps over the lazy dog ".repeat(20);
        // ~9 tokens per repetition in cl100k; estimate must be >= that.
        assert!(estimate(&s) >= 180);
    }

    #[cfg(feature = "exact-tokens")]
    #[test]
    fn estimate_never_below_exact_on_samples() {
        for s in [
            "fn main() { println!(\"hi\"); }",
            "Nunca editar records/ a mano.",
            "[muninn:no-rebuild] src/webhooks/retry.rs ya implementa backoff exponencial.",
        ] {
            assert!(estimate(s) >= count_cl100k(s), "{s}");
        }
    }
}
