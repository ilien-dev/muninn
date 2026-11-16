//! `muninn-bench replay` on the two invented fixtures of this repository: the counts are the
//! ones the labels and the transcripts give by hand.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

const BENCH: &str = env!("CARGO_BIN_EXE_muninn-bench");

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/replay")
        .join(name)
}

fn replay(name: &str, extra: &[&str]) -> Value {
    let out = Command::new(BENCH)
        .arg("replay")
        .arg(fixture(name))
        .args(extra)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "replay {name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("replay prints one JSON object")
}

/// Five labelled records and six labelled messages. Of the records: two durable ones and one
/// noise one are still emitted, one was never said, and a dead end cannot be read here. Of
/// the messages: three of the four durable ones are emitted with the phrase that carries
/// them, the fourth yields a candidate that does not carry it, and one message that holds
/// nothing durable yields two candidates.
#[test]
fn an_extraction_fixture_gives_the_counts_of_its_labels() {
    let v = replay("extraction.json", &[]);
    assert_eq!(v["section"], "extraction");
    assert_eq!(v["split"], "all");
    let c = &v["counts"];
    assert_eq!(c["candidates_total"], 7);
    assert_eq!(c["items"]["precision"]["tides:90"], "not_emitted");
    assert_eq!(c["items"]["precision"]["tides:91"], "not_measurable");
    assert_eq!(
        c["items"]["recall"]["tides:tide-1:0"],
        json!(["durable", "a_other"])
    );
    assert_eq!(c["items"]["recall"]["tides:tide-2:2"], json!(["b", "b"]));
    assert_eq!(
        c["precision_set"],
        json!({
            "items": 5, "measurable": 4, "not_measurable_deadend": 1,
            "labels": {"a": 2, "b": 1, "c": 1, "d": 1},
            "emitted_by_label": {"a": 2, "b": 1}, "not_emitted_by_label": {"c": 1},
            "emitted_by_kind_label": {"decision:a": 1, "invariant:a": 1, "invariant:b": 1},
            "emitted": 3, "durable_emitted": 2, "noise_emitted": 1, "record_precision": 0.6667,
        })
    );
    assert_eq!(
        c["recall_set"],
        json!({
            "messages": 6, "labels": {"a": 4, "b": 2},
            "durable_messages": 4, "durable_emitted": 3, "durable_missed": 1, "recall": 0.75,
            "durable_by_kind": {"correction": 1, "decision": 2, "rule": 1},
            "durable_emitted_by_kind": {"correction": 1, "decision": 1, "rule": 1},
            "candidates_by_class": {"a_other": 2, "b": 2, "durable": 3},
            "messages_with_candidates_by_label": {"a": 4, "b": 1},
            "message_precision": 0.4286, "noise_candidates": 4,
        })
    );
}

/// `--split` reads the items that carry that split and no others.
#[test]
fn a_split_reads_its_own_items() {
    let v = replay("extraction.json", &["--split", "design"]);
    assert_eq!(v["split"], "design");
    let (p, r) = (&v["counts"]["precision_set"], &v["counts"]["recall_set"]);
    assert_eq!(
        (p["items"].clone(), p["emitted"].clone()),
        (json!(3), json!(2))
    );
    assert_eq!(p["record_precision"], 0.5);
    assert_eq!(
        (r["messages"].clone(), r["durable_emitted"].clone()),
        (json!(3), json!(2))
    );
    assert_eq!(r["recall"], 0.6667);
    assert_eq!(
        r["candidates_by_class"],
        json!({"a_other": 2, "durable": 2})
    );
}

/// Four prompts through the real hook of the `muninn` binary built beside this one. Three are
/// asked of the store as the two sessions left it; the fourth is asked of the store as it
/// stood between them, where the decision the second session replaced still stands.
#[test]
fn a_delivery_fixture_gives_the_counts_of_its_labels() {
    let muninn = Path::new(BENCH).with_file_name(format!("muninn{}", std::env::consts::EXE_SUFFIX));
    if !muninn.exists() {
        // `cargo test --workspace` builds it; a run of this package alone may not have
        eprintln!("no muninn binary beside muninn-bench; skipped");
        return;
    }
    let v = replay("delivery.json", &[]);
    assert_eq!(v["section"], "delivery");
    let items = &v["counts"]["items"];
    assert_eq!(items["Q1"]["ids"], json!([6, 10, 7, 9]));
    assert_eq!(items["Q2"]["ids"], json!([6, 5, 3, 10]));
    assert_eq!(items["Q3"]["ids"], json!([]));
    // rewound to before the second session: its records are gone and #4 is current again
    assert_eq!(items["Q4"]["ids"], json!([6, 4, 1]));
    let c = &v["counts"]["c12"];
    assert_eq!(
        json!([c["prompts"], c["prompts_with_delivery"], c["returned"]]),
        json!([4, 3, 11])
    );
    assert_eq!(
        json!([c["useful_returned"], c["useful_labelled"]]),
        json!([3, 3])
    );
    assert_eq!(
        json!([c["related_returned"], c["related_labelled"]]),
        json!([3, 3])
    );
    assert_eq!(
        json!([c["unrelated_returned"], c["unrelated_labelled"]]),
        json!([3, 3])
    );
    assert_eq!(
        json!([c["harmful_returned"], c["unlabelled_returned"]]),
        json!([0, 2])
    );
    // tokens are a third of the characters delivered, rounded up, averaged over every prompt
    let tokens: Vec<f64> = ["Q1", "Q2", "Q3", "Q4"]
        .iter()
        .map(|q| items[q]["tokens"].as_f64().unwrap())
        .collect();
    assert_eq!(tokens[2], 0.0);
    let mean = (tokens.iter().sum::<f64>() / 4.0 * 10.0).round() / 10.0;
    assert_eq!(c["mean_tokens"], mean);
    assert!(c["header_chars"].as_u64().unwrap() > 0);
    assert_eq!(c["evidence_chars"], 0, "hook blocks carry no evidence line");

    let design = replay("delivery.json", &["--split", "design"]);
    assert_eq!(design["counts"]["c12"]["prompts"], 2);
    assert_eq!(design["counts"]["c12"]["returned"], 4);
}
