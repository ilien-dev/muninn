//! Gate 5a — does the control F2 emits actually refuse the call the rule forbids, and
//! refuse nothing else?
//!
//! Gate 1 measured the classifier: given a sentence, is it enforceable? It stopped
//! there. This measures the rest of the chain, on the shipped binary:
//!
//! ```text
//! rule text → muninn compile → muninn apply → { PreToolUse verdict, permission rules }
//! ```
//!
//! Each case is one rule from the public corpus and one tool call, with the verdict a
//! careful reader would give **from the rule's own words**. A control that is broader
//! than its rule therefore scores as a false block rather than disappearing into an
//! aggregate: that is the failure `GATE1.md` names and never counted.
//!
//! Two channels carry a control, and they are reported apart because only one of them is
//! ours to measure end to end:
//!
//! - **hook** — `.muninn/compiled/pretooluse.json`, evaluated by our own binary. Measured
//!   by running it.
//! - **permission** — `permissions.deny` / `permissions.ask` in `.claude/settings.json`,
//!   enforced by the harness, not by us. Matched here against our reading of the
//!   documented `Tool(specifier:*)` prefix semantics. That is a model of the harness, not
//!   the harness, and the report says so.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Deserialize)]
pub struct Case {
    pub id: String,
    pub pattern_id: String,
    pub corpus_file: String,
    pub source_line: u32,
    pub rule_text: String,
    pub tool: String,
    pub tool_input: serde_json::Value,
    /// The verdict the rule's own words call for: `deny`, `ask` or `allow`.
    pub expect: String,
    pub why: String,
    /// Branch the checkout is on, for rules conditioned on one. Default `main`.
    #[serde(default)]
    pub branch: Option<String>,
}

/// `deny` > `ask` > nothing. A call is "blocked" by either of the first two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Decision {
    None,
    Ask,
    Deny,
}

impl Decision {
    fn parse(s: &str) -> Self {
        match s {
            "deny" => Decision::Deny,
            "ask" => Decision::Ask,
            _ => Decision::None,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Decision::Deny => "deny",
            Decision::Ask => "ask",
            Decision::None => "allow",
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct Outcome {
    pub id: String,
    pub pattern_id: String,
    pub expect: String,
    pub got: String,
    pub hook: String,
    pub permission: String,
    /// The permission specifier that matched, when one did.
    pub specifier: Option<String>,
    pub rule_id: Option<String>,
    pub verdict: &'static str,
    pub source: String,
    /// Why the label is what it is — the reader's reasoning, carried into the report so a
    /// disputed case can be argued about rather than re-derived.
    pub why: String,
}

/// What happened to this case, in the four words the report groups by.
fn verdict_of(expect: Decision, got: Decision) -> &'static str {
    match (expect, got) {
        (Decision::None, Decision::None) => "correct_allow",
        (Decision::None, _) => "false_block",
        (_, Decision::None) => "missed",
        (e, g) if e == g => "correct_block",
        _ => "wrong_strength",
    }
}

fn run(bin: &Path, root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new(bin)
        .arg("--cwd")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("running muninn {}", args.join(" ")))?;
    anyhow::ensure!(
        out.status.success(),
        "muninn {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// One PreToolUse evaluation through the shipped hook.
fn hook_verdict(bin: &Path, root: &Path, case: &Case) -> Result<(Decision, Option<String>)> {
    let payload = serde_json::json!({
        "session_id": "gate5a", "cwd": root, "hook_event_name": "PreToolUse",
        "tool_name": case.tool, "tool_input": case.tool_input,
    });
    let mut child = Command::new(bin)
        .arg("--cwd")
        .arg(root)
        .args(["hook", "PreToolUse"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())?;
    let out = child.wait_with_output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        return Ok((Decision::None, None));
    }
    let v: serde_json::Value = serde_json::from_str(text.trim())?;
    let d = v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .map(Decision::parse)
        .unwrap_or(Decision::None);
    // the reason carries `Rule (file:line)`, which is how a verdict names its rule
    let rule = v["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .and_then(|r| {
            r.split_once(')')
                .map(|(a, _)| a.trim_start_matches("Rule (").to_string())
        });
    Ok((d, rule))
}

/// Our reading of `Tool(specifier)`: `Bash(prefix:*)` is a command prefix, at the start of
/// the command or after a `;`, `&&` or `|`; a path specifier is gitignore-shaped, with
/// `**` for a subtree and `*.ext` for a suffix. Not the harness's own matcher.
fn specifier_matches(spec: &str, tool: &str, input: &serde_json::Value) -> bool {
    let Some((spec_tool, rest)) = spec.split_once('(') else {
        return false;
    };
    let arg = rest.trim_end_matches(')');
    if spec_tool != tool {
        return false;
    }
    match tool {
        "Bash" => {
            let command = input.get("command").and_then(|c| c.as_str()).unwrap_or("");
            let prefix = arg.strip_suffix(":*").unwrap_or(arg);
            command.split(['\n', ';', '|', '&']).any(|seg| {
                let seg = seg.trim();
                seg == prefix
                    || seg
                        .strip_prefix(prefix)
                        .is_some_and(|r| r.is_empty() || r.starts_with(' '))
            })
        }
        _ => {
            let path = input
                .get("file_path")
                .or_else(|| input.get("path"))
                .and_then(|p| p.as_str())
                .unwrap_or("");
            path_matches(arg, path)
        }
    }
}

fn path_matches(spec: &str, path: &str) -> bool {
    let spec = spec.trim_start_matches("./").trim_start_matches('/');
    let path = path.trim_start_matches("./").trim_start_matches('/');
    if let Some(sub) = spec.strip_suffix("/**") {
        return path == sub || path.starts_with(&format!("{sub}/"));
    }
    if let Some(suffix) = spec.strip_prefix("**/*") {
        return path.ends_with(suffix);
    }
    if let Some(stem) = spec.strip_suffix(".*") {
        return path == stem || path.starts_with(&format!("{stem}."));
    }
    spec == path
}

#[derive(Debug, Deserialize, Default)]
struct Permissions {
    #[serde(default)]
    deny: Vec<String>,
    #[serde(default)]
    ask: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct PermissionsFile {
    #[serde(default)]
    permissions: Permissions,
}

fn permission_verdict(root: &Path, case: &Case) -> (Decision, Option<String>) {
    let text =
        std::fs::read_to_string(root.join(".muninn/compiled/permissions.json")).unwrap_or_default();
    let p: PermissionsFile = serde_json::from_str(&text).unwrap_or_default();
    for (list, decision) in [
        (&p.permissions.deny, Decision::Deny),
        (&p.permissions.ask, Decision::Ask),
    ] {
        for spec in list {
            if specifier_matches(spec, &case.tool, &case.tool_input) {
                return (decision, Some(spec.clone()));
            }
        }
    }
    (Decision::None, None)
}

/// Build a project holding exactly this rule, compile it, apply it, judge the call.
///
/// One rule per project on purpose: the question is whether the control emitted for *this*
/// rule covers what *this* rule says. A whole corpus file would let a neighbouring rule
/// block the benign call and hide an over-broad emission behind someone else's verdict.
fn evaluate_case(bin: &Path, work: &Path, case: &Case) -> Result<Outcome> {
    let root = &work.join(&case.id);
    if root.exists() {
        std::fs::remove_dir_all(root)?;
    }
    std::fs::create_dir_all(root.join(".git"))?;
    std::fs::write(
        root.join(".git/HEAD"),
        format!(
            "ref: refs/heads/{}\n",
            case.branch.as_deref().unwrap_or("main")
        ),
    )?;
    std::fs::write(
        root.join("CLAUDE.md"),
        format!("# Project rules\n\n- {}\n", case.rule_text),
    )?;
    run(bin, root, &["init", "--keep-native", "--no-boot-block"])?;
    run(bin, root, &["compile"])?;
    run(bin, root, &["apply", "--yes"])?;

    let (hook, rule_id) = hook_verdict(bin, root, case)?;
    let (permission, specifier) = permission_verdict(root, case);
    std::fs::remove_dir_all(root).ok();
    let got = hook.max(permission);
    let expect = Decision::parse(&case.expect);
    Ok(Outcome {
        id: case.id.clone(),
        pattern_id: case.pattern_id.clone(),
        expect: expect.name().into(),
        got: got.name().into(),
        hook: hook.name().into(),
        permission: permission.name().into(),
        specifier,
        rule_id,
        verdict: verdict_of(expect, got),
        source: format!("{}:{}", case.corpus_file, case.source_line),
        why: case.why.clone(),
    })
}

#[derive(Default)]
struct Tally {
    violating: usize,
    blocked: usize,
    strength_exact: usize,
    benign: usize,
    false_blocks: usize,
}

/// Gate 5a's decision rule, as pre-registered.
const BLOCK_FLOOR: f64 = 0.90;
const FALSE_BLOCK_CEILING: f64 = 0.10;

pub fn run_cmd(cases_path: &Path, bin: &Path, work: &Path, json: bool) -> Result<()> {
    let text = std::fs::read_to_string(cases_path)
        .with_context(|| format!("reading {}", cases_path.display()))?;
    let cases: Vec<Case> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).with_context(|| format!("parsing case: {l}")))
        .collect::<Result<_>>()?;
    anyhow::ensure!(!cases.is_empty(), "no cases in {}", cases_path.display());

    let mut outcomes = Vec::with_capacity(cases.len());
    for case in &cases {
        outcomes.push(evaluate_case(bin, work, case).with_context(|| format!("case {}", case.id))?);
    }

    let mut all = Tally::default();
    let mut by_pattern: BTreeMap<String, Tally> = BTreeMap::new();
    for o in &outcomes {
        let t = by_pattern.entry(o.pattern_id.clone()).or_default();
        for t in [&mut all, t] {
            if o.expect == "allow" {
                t.benign += 1;
                if o.verdict == "false_block" {
                    t.false_blocks += 1;
                }
            } else {
                t.violating += 1;
                if o.verdict != "missed" {
                    t.blocked += 1;
                }
                if o.verdict == "correct_block" {
                    t.strength_exact += 1;
                }
            }
        }
    }
    let rate = |n: usize, d: usize| {
        if d == 0 {
            f64::NAN
        } else {
            n as f64 / d as f64
        }
    };
    let block_rate = rate(all.blocked, all.violating);
    let false_rate = rate(all.false_blocks, all.benign);
    let strength_rate = rate(all.strength_exact, all.violating);
    let pass = block_rate >= BLOCK_FLOOR && false_rate <= FALSE_BLOCK_CEILING;

    if json {
        let v = serde_json::json!({
            "cases": cases.len(),
            "violating": all.violating, "blocked": all.blocked, "block_rate": block_rate,
            "benign": all.benign, "false_blocks": all.false_blocks, "false_block_rate": false_rate,
            "strength_exact": all.strength_exact, "strength_rate": strength_rate,
            "gate": { "block_floor": BLOCK_FLOOR, "false_block_ceiling": FALSE_BLOCK_CEILING, "pass": pass },
            "outcomes": outcomes,
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!(
            "Gate 5a — compiled controls against {} hand-labelled calls from {} rules\n",
            cases.len(),
            outcomes
                .iter()
                .map(|o| o.source.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        );
        println!(
            "{:<28} {:>9} {:>9} {:>8} {:>8}",
            "pattern", "blocked", "strength", "benign", "false"
        );
        for (p, t) in &by_pattern {
            println!(
                "{:<28} {:>4}/{:<4} {:>4}/{:<4} {:>8} {:>8}",
                p, t.blocked, t.violating, t.strength_exact, t.violating, t.benign, t.false_blocks
            );
        }
        println!(
            "\n{:<28} {:>4}/{:<4} {:>4}/{:<4} {:>8} {:>8}",
            "ALL",
            all.blocked,
            all.violating,
            all.strength_exact,
            all.violating,
            all.benign,
            all.false_blocks
        );
        println!(
            "\nblock rate        {:.3}  (gate: >= {BLOCK_FLOOR:.2})\nfalse-block rate  {:.3}  (gate: <= {FALSE_BLOCK_CEILING:.2})\nstrength exact    {:.3}  (deny where deny, ask where ask; not a gate condition)",
            block_rate, false_rate, strength_rate
        );

        let named = |v: &str, title: &str| {
            let rows: Vec<&Outcome> = outcomes.iter().filter(|o| o.verdict == v).collect();
            if rows.is_empty() {
                return;
            }
            println!("\n{title} ({}):", rows.len());
            for o in rows {
                println!(
                    "  {:<22} {:<26} expected {:<5} got {:<5}  {}",
                    o.id, o.pattern_id, o.expect, o.got, o.source
                );
                println!("      label: {}", o.why);
                if let Some(s) = &o.specifier {
                    println!("      via permission rule {s}");
                }
            }
        };
        named(
            "missed",
            "Not blocked — the rule forbids it and nothing stopped it",
        );
        named(
            "false_block",
            "Blocked wrongly — the rule allows it, the control does not",
        );
        named("wrong_strength", "Blocked at the wrong strength");

        println!(
            "\n{}",
            if pass {
                "GATE 5a: PASS"
            } else {
                "GATE 5a: FAIL"
            }
        );
    }
    anyhow::ensure!(pass, "Gate 5a not met");
    Ok(())
}
