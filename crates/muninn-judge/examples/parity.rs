//! Parity with judge-v3's Python rows: reads JSON lines {"cands": [[text, reply], …],
//! "b": [text, reply], "pick": k|null} and prints the pick distribution and the confirm P(yes)
//! for the given pick, one JSON line each.
use muninn_judge::{Judge, Said};
use std::io::BufRead;

fn said(v: &serde_json::Value) -> Said {
    Said {
        text: v[0].as_str().unwrap_or("").into(),
        reply: v[1].as_str().unwrap_or("").into(),
    }
}

fn main() {
    let j = Judge::load_default().expect("judge model");
    for line in std::io::stdin().lock().lines() {
        let v: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        let cands: Vec<Said> = v["cands"].as_array().unwrap().iter().map(said).collect();
        let b = said(&v["b"]);
        let p = j.pick_probs(&cands, &b).unwrap();
        let conf = v["pick"]
            .as_u64()
            .map(|k| j.confirm(&cands[k as usize], &b).unwrap());
        println!("{}", serde_json::json!({"p": p, "confirm": conf}));
    }
}
