//! `replay`: a labelled fixture run against the current build, and the counts its section of
//! the usage-v4 experiment reads. Two kinds of fixture, both a JSON file whose paths are
//! relative to itself:
//!
//! - `extraction` (section A): transcripts and two labelled sets. The extractor runs here, in
//!   process, the way ingest runs it: every transcript of a project is parsed first, so a
//!   prompt another session handed over is known for what it is. Counted: which labelled
//!   records the build still emits, and what it emits from each labelled message.
//! - `delivery` (section C, rules C1 and C2): store copies and labelled prompts. Each store is
//!   copied, rewound to each prompt's moment and asked through the real `UserPromptSubmit`
//!   hook of the `muninn` binary. Counted: returned blocks by label, and what they cost.
//!
//! The counts carry the names the experiment's own scripts give them (`a_extract.py`,
//! `c_delivery.py` in `ilien-dev/muninn-experiments`), so a fixture built from that
//! experiment's private sets reproduces its figures. The fixtures in this repository are
//! invented and small. Nothing here is part of the `muninn` binary or of a hook.

use anyhow::{bail, Context, Result};
use muninn_capture::model::Session;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Deserialize)]
#[serde(tag = "section", rename_all = "lowercase")]
enum Fixture {
    Extraction(Extraction),
    Delivery(Delivery),
}

fn default_true() -> bool {
    true
}

// ---- extraction ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Extraction {
    /// Project label → directory of that project's transcripts (`*.jsonl`).
    transcripts: BTreeMap<String, PathBuf>,
    #[serde(default)]
    precision: Vec<PrecisionItem>,
    #[serde(default)]
    recall: Vec<RecallItem>,
}

/// A record an earlier build stored, labelled: `a` durable, anything else noise.
#[derive(Deserialize)]
struct PrecisionItem {
    id: String,
    label: String,
    kind: String,
    #[serde(default = "user_said")]
    origin: String,
    project: String,
    session: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    object: String,
    #[serde(default)]
    split: Option<String>,
}

fn user_said() -> String {
    "user_said".into()
}

/// A message of a session, labelled: `a` holds a durable statement (with the phrases that
/// carry it), `b` holds none, `nh` no person typed it, `opt` a bare option pick.
#[derive(Deserialize)]
struct RecallItem {
    id: String,
    label: String,
    #[serde(default)]
    kind: Option<String>,
    project: String,
    session: String,
    turn: usize,
    #[serde(default)]
    anchors: Vec<String>,
    #[serde(default)]
    split: Option<String>,
}

struct Emitted {
    turn: usize,
    kind: &'static str,
    body: String,
}

/// Lower case, accents folded, every run of anything but a letter, a digit, `_`, `%` or `+`
/// one space: the experiment's `norm`, with the engine's own folding of Latin accents where
/// the script decomposes every character (the two agree on the letters Spanish and English
/// use).
fn norm(s: &str) -> String {
    let folded = muninn_core::sanitize::fold(s);
    let mut out = String::with_capacity(folded.len());
    let mut gap = false;
    for c in folded.chars() {
        if c.is_alphanumeric() || matches!(c, '_' | '%' | '+') {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

/// Every `user_said` candidate of every transcript of a project, by session (the file's stem).
fn candidates(dir: &Path) -> Result<HashMap<String, Vec<Emitted>>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    files.sort();
    let sessions: Vec<(String, Session)> = files
        .iter()
        .filter_map(|f| {
            let s = muninn_capture::parse_any_with_tasks(f, 0).ok()?;
            Some((f.file_stem()?.to_string_lossy().to_string(), s))
        })
        .collect();
    let tasks: Vec<HashSet<String>> = sessions
        .iter()
        .map(|(_, s)| muninn_capture::agent::session_task_hashes(s))
        .collect();
    let mut out = HashMap::new();
    for (i, (sid, mut s)) in sessions.into_iter().enumerate() {
        muninn_capture::agent::mark_agent_written(&mut s, |h| {
            tasks
                .iter()
                .enumerate()
                .any(|(j, set)| j != i && set.contains(h))
        });
        let emitted = muninn_capture::extract::extract(&s, &sid)
            .into_iter()
            .filter(|c| c.origin == "user_said")
            .map(|c| Emitted {
                turn: c.turn_index,
                kind: c.kind,
                body: c.body,
            })
            .collect();
        out.insert(sid, emitted);
    }
    Ok(out)
}

fn ratio(a: usize, b: usize) -> Value {
    if b == 0 {
        Value::Null
    } else {
        json!(((a as f64 / b as f64) * 10_000.0).round() / 10_000.0)
    }
}

fn bump(m: &mut BTreeMap<String, usize>, k: &str) {
    *m.entry(k.to_string()).or_insert(0) += 1;
}

fn in_split(item: &Option<String>, split: Option<&str>) -> bool {
    split.is_none_or(|s| item.as_deref() == Some(s))
}

fn extraction(base: &Path, fx: &Extraction, split: Option<&str>) -> Result<Value> {
    let mut by_project: HashMap<&str, HashMap<String, Vec<Emitted>>> = HashMap::new();
    for (project, dir) in &fx.transcripts {
        by_project.insert(project.as_str(), candidates(&base.join(dir))?);
    }
    let none: Vec<Emitted> = Vec::new();
    let of = |project: &str, session: &str| -> &Vec<Emitted> {
        by_project
            .get(project)
            .and_then(|p| p.get(session))
            .unwrap_or(&none)
    };

    // the precision set: is each labelled record still emitted?
    let (mut labels, mut emitted, mut not_emitted) =
        (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    let mut emitted_by_kind = BTreeMap::new();
    let (mut items, mut not_measurable) = (0, 0);
    // what became of each item, by id: two builds are compared item by item on these
    let (mut precision_items, mut recall_items) = (Map::new(), Map::new());
    for p in fx.precision.iter().filter(|p| in_split(&p.split, split)) {
        items += 1;
        bump(&mut labels, &p.label);
        if p.kind == "deadend" || p.origin != "user_said" {
            not_measurable += 1;
            precision_items.insert(p.id.clone(), json!("not_measurable"));
            continue;
        }
        let (body, object) = (norm(&p.body), norm(&p.object));
        let hit = of(&p.project, &p.session).iter().any(|c| {
            let theirs = norm(&c.body);
            c.kind == p.kind && (theirs == body || (!object.is_empty() && theirs.contains(&object)))
        });
        if hit {
            bump(&mut emitted, &p.label);
            bump(&mut emitted_by_kind, &format!("{}:{}", p.kind, p.label));
        } else {
            bump(&mut not_emitted, &p.label);
        }
        let state = if hit { "emitted" } else { "not_emitted" };
        precision_items.insert(p.id.clone(), json!(state));
    }
    let n = |m: &BTreeMap<String, usize>, k: &str| m.get(k).copied().unwrap_or(0);
    let all_emitted: usize = emitted.values().sum();
    let precision_set = json!({
        "items": items,
        "measurable": items - not_measurable,
        "not_measurable_deadend": not_measurable,
        "labels": labels,
        "emitted_by_label": emitted,
        "not_emitted_by_label": not_emitted,
        "emitted_by_kind_label": emitted_by_kind,
        "emitted": all_emitted,
        "durable_emitted": n(&emitted, "a"),
        "noise_emitted": n(&emitted, "b") + n(&emitted, "c") + n(&emitted, "d"),
        "record_precision": ratio(n(&emitted, "a"), all_emitted),
    });

    // the recall set: what the build emits from each labelled message
    let (mut labels, mut classes, mut with_candidates) =
        (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    let (mut durable_by_kind, mut durable_emitted_by_kind) = (BTreeMap::new(), BTreeMap::new());
    let (mut messages, mut durable, mut carried) = (0, 0, 0);
    for r in fx.recall.iter().filter(|r| in_split(&r.split, split)) {
        messages += 1;
        bump(&mut labels, &r.label);
        let anchors: Vec<String> = r.anchors.iter().map(|a| norm(a)).collect();
        let mine: Vec<&Emitted> = of(&r.project, &r.session)
            .iter()
            .filter(|c| c.turn == r.turn)
            .collect();
        let mut carries = false;
        let mut mine_classes: Vec<&str> = Vec::new();
        for c in &mine {
            let class = match r.label.as_str() {
                "a" => {
                    let body = norm(&c.body);
                    if anchors.iter().any(|a| !a.is_empty() && body.contains(a)) {
                        carries = true;
                        "durable"
                    } else {
                        "a_other"
                    }
                }
                "nh" => "c",
                other => other,
            };
            bump(&mut classes, class);
            mine_classes.push(class);
        }
        recall_items.insert(r.id.clone(), json!(mine_classes));
        if !mine.is_empty() {
            bump(&mut with_candidates, &r.label);
        }
        if r.label == "a" {
            durable += 1;
            let kind = r.kind.as_deref().unwrap_or("none");
            bump(&mut durable_by_kind, kind);
            if carries {
                carried += 1;
                bump(&mut durable_emitted_by_kind, kind);
            }
        }
    }
    let scored =
        n(&classes, "durable") + n(&classes, "a_other") + n(&classes, "b") + n(&classes, "c");
    let recall_set = json!({
        "messages": messages,
        "labels": labels,
        "durable_messages": durable,
        "durable_emitted": carried,
        "durable_missed": durable - carried,
        "durable_by_kind": durable_by_kind,
        "durable_emitted_by_kind": durable_emitted_by_kind,
        "recall": ratio(carried, durable),
        "candidates_by_class": classes,
        "messages_with_candidates_by_label": with_candidates,
        "message_precision": ratio(n(&classes, "durable"), scored),
        "noise_candidates": n(&classes, "a_other") + n(&classes, "b") + n(&classes, "c"),
    });
    let total: usize = by_project
        .values()
        .flat_map(|p| p.values())
        .map(Vec::len)
        .sum();
    Ok(json!({
        "precision_set": precision_set,
        "recall_set": recall_set,
        "candidates_total": total,
        "items": { "precision": precision_items, "recall": recall_items },
    }))
}

// ---- delivery -----------------------------------------------------------------------------

#[derive(Deserialize)]
struct Delivery {
    /// Store label → where its records come from.
    stores: BTreeMap<String, StoreSource>,
    prompts: Vec<Prompt>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoreSource {
    /// A copy of a store's database.
    Db { db: PathBuf },
    /// Transcripts the `muninn` binary ingests into a new store, in this order.
    Ingest { ingest: Vec<PathBuf> },
}

#[derive(Deserialize)]
struct Prompt {
    id: String,
    store: String,
    prompt: String,
    /// The moment the prompt was typed, in milliseconds: the store is rewound to it. Absent,
    /// the prompt is asked of the store as it stands.
    #[serde(default)]
    at: Option<i64>,
    /// Record id → `u` useful, `r` related, `x` unrelated, `h` harmful.
    #[serde(default)]
    labels: BTreeMap<String, String>,
    #[serde(default)]
    split: Option<String>,
    #[serde(default = "default_true")]
    scored: bool,
}

fn muninn(bin: &Path, root: &Path, args: &[&str], stdin: &str) -> Result<String> {
    let mut cmd = Command::new(bin);
    cmd.arg("--cwd")
        .arg(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // a hook is measured as a user's session runs it, not as this shell is set up
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("MUNINN_") {
            cmd.env_remove(k);
        }
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("starting {}", bin.display()))?;
    child
        .stdin
        .take()
        .context("no stdin")?
        .write_all(stdin.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "`muninn {}` exited {}: {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Take the store back to `at`: what was recorded from five seconds before it on is gone, and
/// what those records had retired stands again. The experiment's `rewind`.
fn rewind(db: &Path, at: i64) -> Result<()> {
    let con = rusqlite::Connection::open(db)?;
    let t = at - 5_000;
    con.execute(
        "UPDATE record SET invalid = 0, invalidated_by = NULL, invalid_reason = NULL \
         WHERE invalid = 1 AND invalidated_by IN (SELECT id FROM record WHERE created_at >= ?1)",
        [t],
    )?;
    for table in ["cue", "record_vec", "fire_ledger"] {
        let there: bool = con.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |r| r.get(0),
        )?;
        if there {
            con.execute(
                &format!(
                    "DELETE FROM {table} WHERE record_id IN (SELECT id FROM record WHERE created_at >= ?1)"
                ),
                [t],
            )?;
        }
    }
    con.execute("DELETE FROM record WHERE created_at >= ?1", [t])?;
    Ok(())
}

/// What the hook rendered: the id of each block, and the characters its header lines, its
/// `evidence:` lines and its blocks as a whole take.
struct Rendered {
    ids: Vec<i64>,
    tokens: usize,
    header: usize,
    evidence: usize,
    blocks: usize,
}

fn rendered(text: &str) -> Rendered {
    let mut r = Rendered {
        ids: Vec::new(),
        tokens: text.chars().count().div_ceil(3),
        header: 0,
        evidence: 0,
        blocks: 0,
    };
    let mut in_block = false;
    for line in text.split('\n') {
        let len = line.chars().count() + 1;
        // `[muninn:<kind><anything>] #<id> `
        let id = line
            .strip_prefix("[muninn:")
            .and_then(|rest| rest.split_once("] #"))
            .filter(|(kind, _)| kind.starts_with(|c: char| c.is_ascii_lowercase() || c == '_'))
            .and_then(|(_, after)| {
                let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
                after[digits.len()..]
                    .starts_with(' ')
                    .then(|| digits.parse::<i64>().ok())
                    .flatten()
            });
        if let Some(id) = id {
            r.ids.push(id);
            in_block = true;
            r.header += len;
        } else if line.starts_with("[muninn:") {
            in_block = false;
        } else if in_block && line.starts_with("  evidence: ") {
            r.evidence += len;
        }
        if in_block {
            r.blocks += len;
        }
    }
    r
}

fn delivery(
    base: &Path,
    fx: &Delivery,
    split: Option<&str>,
    bin: &Path,
    work: &Path,
) -> Result<Value> {
    let mut results: HashMap<&str, Rendered> = HashMap::new();
    for (label, source) in &fx.stores {
        let root = work.join(label);
        std::fs::create_dir_all(root.join(".muninn/log"))?;
        let db = root.join(".muninn/muninn.db");
        match source {
            StoreSource::Db { db: from } => {
                std::fs::copy(base.join(from), &db)
                    .with_context(|| format!("copying the store {label}"))?;
            }
            StoreSource::Ingest { ingest } => {
                muninn(bin, &root, &["init", "--keep-native"], "")?;
                for t in ingest {
                    let t = base.join(t);
                    muninn(bin, &root, &["ingest", &t.to_string_lossy()], "")?;
                }
            }
        }
        // newest first: a rewind only ever goes further back
        let mut prompts: Vec<&Prompt> = fx.prompts.iter().filter(|p| &p.store == label).collect();
        prompts.sort_by_key(|p| std::cmp::Reverse(p.at.unwrap_or(i64::MAX)));
        for p in prompts {
            if let Some(at) = p.at {
                rewind(&db, at)?;
            }
            let input = json!({
                "session_id": format!("replay-{}", p.id), "prompt": p.prompt,
                "hook_event_name": "UserPromptSubmit", "cwd": root,
            });
            let out = muninn(
                bin,
                &root,
                &["hook", "UserPromptSubmit"],
                &input.to_string(),
            )?;
            let text = serde_json::from_str::<Value>(out.trim())
                .ok()
                .and_then(|v| {
                    v["hookSpecificOutput"]["additionalContext"]
                        .as_str()
                        .map(str::to_string)
                })
                .unwrap_or_default();
            results.insert(p.id.as_str(), rendered(&text));
        }
    }
    let name = |code: &str| match code {
        "u" => "useful",
        "r" => "related",
        "x" => "unrelated",
        "h" => "harmful",
        _ => "unlabelled",
    };
    let mut c: BTreeMap<String, usize> = BTreeMap::new();
    let mut labelled: BTreeMap<String, usize> = BTreeMap::new();
    let (mut tokens, mut header, mut evidence, mut blocks) = (Vec::new(), 0, 0, 0);
    let mut items = Map::new();
    for p in fx
        .prompts
        .iter()
        .filter(|p| p.scored && in_split(&p.split, split))
    {
        let Some(r) = results.get(p.id.as_str()) else {
            bail!("prompt {} names a store the fixture does not hold", p.id);
        };
        bump(&mut c, "prompts");
        if !r.ids.is_empty() {
            bump(&mut c, "prompts_with_delivery");
        }
        for id in &r.ids {
            bump(&mut c, "returned");
            let code = p
                .labels
                .get(&id.to_string())
                .map(String::as_str)
                .unwrap_or("");
            bump(&mut c, &format!("{}_returned", name(code)));
        }
        for code in p.labels.values() {
            bump(&mut labelled, name(code));
        }
        tokens.push(r.tokens);
        header += r.header;
        evidence += r.evidence;
        blocks += r.blocks;
        items.insert(p.id.clone(), json!({ "ids": r.ids, "tokens": r.tokens }));
    }
    let n = |m: &BTreeMap<String, usize>, k: &str| m.get(k).copied().unwrap_or(0);
    let mean = if tokens.is_empty() {
        Value::Null
    } else {
        json!(((tokens.iter().sum::<usize>() as f64 / tokens.len() as f64) * 10.0).round() / 10.0)
    };
    Ok(json!({
        "c12": {
            "prompts": n(&c, "prompts"),
            "prompts_with_delivery": n(&c, "prompts_with_delivery"),
            "returned": n(&c, "returned"),
            "useful_returned": n(&c, "useful_returned"),
            "useful_labelled": n(&labelled, "useful"),
            "related_returned": n(&c, "related_returned"),
            "related_labelled": n(&labelled, "related"),
            "unrelated_returned": n(&c, "unrelated_returned"),
            "unrelated_labelled": n(&labelled, "unrelated"),
            "harmful_returned": n(&c, "harmful_returned"),
            "unlabelled_returned": n(&c, "unlabelled_returned"),
            "mean_tokens": mean,
            "header_chars": header,
            "evidence_chars": evidence,
            "block_chars": blocks,
            "provenance_share": ratio(header + evidence, blocks),
        },
        "items": items,
    }))
}

/// Run the fixture and print its counts as JSON.
pub fn run(fixture: &Path, split: Option<&str>, bin: &Path, work: &Path) -> Result<()> {
    let text = std::fs::read_to_string(fixture)
        .with_context(|| format!("reading {}", fixture.display()))?;
    let fx: Fixture = serde_json::from_str(&text).context("the fixture is not a replay fixture")?;
    let base = fixture.parent().unwrap_or(Path::new("."));
    let (section, counts) = match &fx {
        Fixture::Extraction(e) => ("extraction", extraction(base, e, split)?),
        Fixture::Delivery(d) => {
            anyhow::ensure!(
                bin.exists(),
                "muninn binary not found at {} (build it with `cargo build --release -p muninn-cli`)",
                bin.display()
            );
            ("delivery", delivery(base, d, split, bin, work)?)
        }
    };
    let out = json!({ "section": section, "split": split.unwrap_or("all"), "counts": counts });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
