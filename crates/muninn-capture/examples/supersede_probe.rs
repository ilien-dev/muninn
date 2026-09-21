//! Why does a replacement go unnoticed? A diagnostic, not a test.
//!
//! `eval_mechanism.py` reports how many held-out replacements were retired; it cannot say
//! which signal was missing. This prints, for every phrasing pair, exactly what
//! `extract::replaces_text` sees: the topic words each side contributes, how many they
//! share, whether the later message announces a change and whether it names a new value.
//!
//!   cargo run -p muninn-capture --example supersede_probe -- \
//!       crates/muninn-bench/experiment/loop6/heldout_phrasings.json
//!
//! Add `--misses` to print only the pairs that are not detected.

use muninn_capture::extract::{
    label_words, name_tokens, names_new_value, replaces_text, topic_words,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let misses_only = args.iter().any(|a| a == "--misses");
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .expect("usage: supersede_probe <heldout_phrasings.json> [--misses]");
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(path).expect("reading phrasings"))
            .expect("phrasings are a JSON list");

    let (mut hit, mut total) = (0, 0);
    for r in &rows {
        let key = r["key"].as_str().unwrap_or("?");
        let (a, b) = (r["a"].as_str().unwrap_or(""), r["b"].as_str().unwrap_or(""));
        // the ingest path asks `replaces_text` with the change flag the extractor derived;
        // here the flag is taken as true, which is the generous case: the pair was written
        // as a change, so anything still missed is missed on overlap alone
        let detected = replaces_text(a, b, true);
        total += 1;
        hit += detected as usize;
        if misses_only && detected {
            continue;
        }
        let (ta, tb) = (topic_words(a), topic_words(b));
        let common: Vec<&String> = label_words(a)
            .iter()
            .filter(|w| label_words(b).contains(w))
            .map(|w| Box::leak(w.clone().into_boxed_str()) as &str)
            .map(|w| Box::leak(Box::new(w.to_string())) as &String)
            .collect();
        let shared: Vec<&String> = ta.iter().filter(|w| tb.contains(w)).collect();
        println!(
            "{} {key}\n  a: {a}\n  b: {b}\n  topic(a)={ta:?}\n  topic(b)={tb:?}\n  \
             shared={shared:?} ({} of min {})  common_labels={common:?}\n  \
             names(b)={:?}  names_new_value={}\n",
            if detected { "HIT " } else { "MISS" },
            shared.len(),
            ta.len().min(tb.len()),
            name_tokens(b),
            names_new_value(a, b),
        );
    }
    println!("detected {hit}/{total}");
}
