//! Does the *name* of the new value identify which decision it replaces?
//!
//! The evidence log closed the embedding route for supersession on a sentence-level
//! measurement [Z3]: the cosine between the superseding sentence and the superseded one
//! ranked the true pair first 4 times in 10, with the lowest true score below the highest
//! cross score, so no threshold existed. That measured whole sentences, which are mostly
//! prose about why — "cuts down on boilerplate", "less infrastructure to babysit" — and the
//! part that actually identifies the decision is the product name.
//!
//! This asks the untested half: embed only the named values (`extract::name_tokens`), and
//! see whether `gRPC → ConnectRPC` scores above `gRPC → OneSignal`. Two products of the same
//! kind may sit close together even when the sentences around them do not.
//!
//! Reports, over every scenario in the phrasing files given:
//!   * rank of the true pair among all candidate old-decisions (the trap the `c` messages set)
//!   * min true score against max cross score — if they do not separate, no threshold exists
//!     and the route is closed for names as it was for sentences
//!
//!   cargo run --release -p muninn-bench --example name_cosine -- <phrasings.json>...
//!
//! Development only: run it on development phrasings, never on the set a claim will cite.

use muninn_capture::extract::name_tokens;

fn cos(a: &[f32], b: &[f32]) -> f32 {
    let (mut d, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        d += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    d / (na.sqrt() * nb.sqrt())
}

fn main() -> anyhow::Result<()> {
    let files: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(!files.is_empty(), "usage: name_cosine <phrasings.json>...");
    let emb = muninn_embed::Embedder::load_default()?;

    let mut rows: Vec<(String, String, String)> = Vec::new(); // key, a, b
    for f in &files {
        let v: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(f)?)?;
        for r in v {
            rows.push((
                format!(
                    "{}::{}",
                    f.rsplit('/').nth(1).unwrap_or(""),
                    r["key"].as_str().unwrap_or("?")
                ),
                r["a"].as_str().unwrap_or("").to_string(),
                r["b"].as_str().unwrap_or("").to_string(),
            ));
        }
    }

    // the names only, joined; a side with no name cannot be scored and is reported as such
    let names = |s: &str| name_tokens(s).join(" ");
    let (mut scored, mut unnameable) = (0usize, 0usize);
    let mut firsts = 0usize;
    let mut min_true = f32::MAX;
    let mut max_cross = f32::MIN;
    let mut detail: Vec<(String, usize, f32, f32)> = Vec::new();

    let a_names: Vec<String> = rows.iter().map(|r| names(&r.1)).collect();
    let b_names: Vec<String> = rows.iter().map(|r| names(&r.2)).collect();
    let a_vecs = emb.encode(&a_names);
    let b_vecs = emb.encode(&b_names);

    for i in 0..rows.len() {
        if a_names[i].is_empty() || b_names[i].is_empty() {
            unnameable += 1;
            continue;
        }
        // every other scenario's original decision is a candidate the change must not match
        let mut scores: Vec<(f32, usize)> = (0..rows.len())
            .filter(|j| !a_names[*j].is_empty())
            .map(|j| (cos(&b_vecs[i], &a_vecs[j]), j))
            .collect();
        let truth = scores
            .iter()
            .find(|(_, j)| *j == i)
            .map(|(s, _)| *s)
            .unwrap_or(0.0);
        let cross = scores
            .iter()
            .filter(|(_, j)| *j != i)
            .map(|(s, _)| *s)
            .fold(f32::MIN, f32::max);
        scores.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap());
        let rank = scores
            .iter()
            .position(|(_, j)| *j == i)
            .unwrap_or(usize::MAX)
            + 1;
        scored += 1;
        firsts += (rank == 1) as usize;
        min_true = min_true.min(truth);
        max_cross = max_cross.max(cross);
        detail.push((rows[i].0.clone(), rank, truth, cross));
    }

    detail.sort_by_key(|d| std::cmp::Reverse(d.1));
    println!(
        "{:<46} {:>5} {:>8} {:>10}",
        "scenario", "rank", "true", "max cross"
    );
    for (k, rank, t, c) in detail.iter().take(40) {
        println!("{k:<46} {rank:>5} {t:>8.3} {c:>10.3}");
    }
    println!(
        "\nnames on both sides: {scored} pairs ({unnameable} skipped: one side names nothing)"
    );
    println!("true pair ranked first: {firsts}/{scored}");
    println!(
        "min true {min_true:.3}  vs  max cross {max_cross:.3}  →  {}",
        if min_true > max_cross {
            "separated: a threshold exists"
        } else {
            "overlapping: no single threshold separates them"
        }
    );
    Ok(())
}
