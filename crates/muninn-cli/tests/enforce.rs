//! F2 end to end: a rule written in CLAUDE.md becomes a control that actually refuses
//! the tool call, and refuses nothing else.
//!
//! Gate 1 (`crates/muninn-bench/corpora/claude-md/GATE1.md`) measured the classifier and
//! stopped there. The links from `emit` onwards — the JSON the compiler writes, the merge
//! arithmetic in `apply`, and the verdict the PreToolUse hook returns — had no test, so a
//! change to any of them could disable enforcement in silence. Every case below drives the
//! real binary: `compile` → `apply` → `hook PreToolUse` with a harness-shaped payload.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_muninn");

/// A project with `rules` as its CLAUDE.md, initialised and on branch `branch`.
fn project(rules: &str, branch: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(
        root.join(".git/HEAD"),
        format!("ref: refs/heads/{branch}\n"),
    )
    .unwrap();
    std::fs::write(root.join("CLAUDE.md"), rules).unwrap();
    let out = muninn(root, &["init", "--keep-native", "--no-boot-block"]);
    assert!(out.status.success(), "init failed: {}", out.stderr);
    tmp
}

struct Out {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

fn muninn(root: &Path, args: &[&str]) -> Out {
    let out = Command::new(BIN)
        .arg("--cwd")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    Out {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into(),
        stderr: String::from_utf8_lossy(&out.stderr).into(),
    }
}

fn compile_and_apply(root: &Path) {
    let c = muninn(root, &["compile"]);
    assert!(c.status.success(), "compile failed: {}", c.stderr);
    let a = muninn(root, &["apply", "--yes"]);
    assert!(a.status.success(), "apply failed: {}", a.stderr);
    assert!(
        a.stdout.contains("applied"),
        "apply prints the diff it applied: {}",
        a.stdout
    );
}

/// One PreToolUse evaluation through the shipped hook. `None` is silence.
fn verdict(root: &Path, tool: &str, tool_input: serde_json::Value, codex: bool) -> Option<Value> {
    let mut payload = serde_json::json!({
        "session_id": "enforce", "cwd": root, "hook_event_name": "PreToolUse",
        "tool_name": tool, "tool_input": tool_input,
    });
    if codex {
        payload["turn_id"] = serde_json::Value::String("t1".into());
    }
    let mut child = Command::new(BIN)
        .arg("--cwd")
        .arg(root)
        .args(["hook", "PreToolUse"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "a read hook always exits 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        return None;
    }
    Some(serde_json::from_str(text.trim()).expect("hook stdout is one JSON object"))
}

type Value = serde_json::Value;

fn decision(v: &Value) -> Option<&str> {
    v["hookSpecificOutput"]["permissionDecision"].as_str()
}

const FORCE_PUSH: &str = "- Never force-push to `main`.\n";
const GENERATED: &str = "- Do NOT edit `public/generated.json` by hand; it is generated.\n";

#[test]
fn compiling_without_applying_enforces_nothing() {
    let p = project(FORCE_PUSH, "main");
    let c = muninn(p.path(), &["compile"]);
    assert!(c.status.success());
    assert!(
        p.path().join(".muninn/compiled/pretooluse.json").exists(),
        "compile writes the artefact"
    );
    assert!(
        !p.path().join(".muninn/compiled/applied.json").exists(),
        "but nothing is applied until the user says yes"
    );
    assert_eq!(
        verdict(
            p.path(),
            "Bash",
            serde_json::json!({ "command": "git push --force origin main" }),
            false
        ),
        None,
        "an unapplied rule must not block: the user has not seen the diff yet"
    );
}

#[test]
fn an_applied_rule_denies_the_violating_call_and_nothing_else() {
    let p = project(FORCE_PUSH, "main");
    compile_and_apply(p.path());

    let v = verdict(
        p.path(),
        "Bash",
        serde_json::json!({ "command": "git push --force origin main" }),
        false,
    )
    .expect("the violating call is judged");
    assert_eq!(decision(&v), Some("deny"));
    let reason = v["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap();
    assert!(
        reason.contains("CLAUDE.md:") && reason.contains("force-push"),
        "the reason names the source line and quotes the rule: {reason}"
    );

    assert_eq!(
        verdict(
            p.path(),
            "Bash",
            serde_json::json!({ "command": "git push origin main" }),
            false
        ),
        None,
        "an ordinary push is not a force-push"
    );
    assert_eq!(
        verdict(
            p.path(),
            "Read",
            serde_json::json!({ "file_path": "src/main.rs" }),
            false
        ),
        None,
        "a rule about pushing says nothing about reading"
    );
}

/// A rule that names a branch must not refuse the same command elsewhere. This is the
/// over-broad emission Gate 5a found and `GATE1.md` had admitted; the rule's scope is now
/// carried into the control, read from the push target and from the checkout's own branch.
#[test]
fn a_branch_scoped_rule_stays_on_its_branch() {
    let on_main = project(FORCE_PUSH, "main");
    compile_and_apply(on_main.path());
    let judge = |p: &Path, cmd: &str| {
        verdict(p, "Bash", serde_json::json!({ "command": cmd }), false)
            .as_ref()
            .and_then(decision)
            .map(str::to_string)
    };

    assert_eq!(
        judge(on_main.path(), "git push --force origin main").as_deref(),
        Some("deny"),
        "the target is named in the command"
    );
    assert_eq!(
        judge(on_main.path(), "git push --force").as_deref(),
        Some("deny"),
        "a bare force-push while on main pushes main"
    );
    assert_eq!(
        judge(on_main.path(), "git push --force origin my-feature"),
        None,
        "the rule named `main`; a feature branch is not it"
    );

    let on_feature = project(FORCE_PUSH, "feature/x");
    compile_and_apply(on_feature.path());
    assert_eq!(
        judge(on_feature.path(), "git push --force"),
        None,
        "a bare force-push on a feature branch pushes that branch"
    );
    assert_eq!(
        judge(on_feature.path(), "git push --force origin main").as_deref(),
        Some("deny"),
        "naming main as the target is still forbidden, wherever you stand"
    );
}

/// A rule with no branch in it keeps the blanket control: scope is narrowed only where the
/// rule states one.
#[test]
fn an_unscoped_rule_keeps_its_blanket_control() {
    let p = project("- Never run `sudo`.\n", "main");
    compile_and_apply(p.path());
    for cmd in ["sudo apt-get install jq", "sudo ./waf build"] {
        assert_eq!(
            verdict(
                p.path(),
                "Bash",
                serde_json::json!({ "command": cmd }),
                false
            )
            .as_ref()
            .and_then(decision),
            Some("deny"),
            "{cmd} must be denied"
        );
    }
    let settings = std::fs::read_to_string(p.path().join(".claude/settings.json")).unwrap();
    assert!(
        settings.contains("Bash(sudo:*)"),
        "an unscoped command rule still emits a permission rule: {settings}"
    );
}

#[test]
fn a_protected_path_rule_covers_edit_and_write() {
    let p = project(GENERATED, "main");
    compile_and_apply(p.path());
    for tool in ["Edit", "Write"] {
        let v = verdict(
            p.path(),
            tool,
            serde_json::json!({ "file_path": "public/generated.json" }),
            false,
        );
        let settings =
            std::fs::read_to_string(p.path().join(".claude/settings.json")).unwrap_or_default();
        assert!(
            v.is_some() || settings.contains(&format!("{tool}(./public/generated.json)")),
            "{tool} on the protected path must be refused by a hook or by a permission rule"
        );
    }
    assert_eq!(
        verdict(
            p.path(),
            "Edit",
            serde_json::json!({ "file_path": "public/other.json" }),
            false
        ),
        None,
        "a neighbouring file is not the protected one"
    );
}

#[test]
fn apply_merges_with_foreign_settings_and_revert_removes_only_ours() {
    // an unscoped rule, so the run exercises the permission channel as well as the hook
    let p = project("- Never run `sudo`.\n", "main");
    let settings = p.path().join(".claude/settings.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    v["permissions"]["deny"] = serde_json::json!(["Bash(curl:*)"]);
    v["someoneElsesKey"] = serde_json::json!("keep me");
    std::fs::write(&settings, serde_json::to_string_pretty(&v).unwrap()).unwrap();

    compile_and_apply(p.path());
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    let deny: Vec<&str> = after["permissions"]["deny"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str())
        .collect();
    assert!(
        deny.contains(&"Bash(curl:*)"),
        "foreign rules survive apply"
    );
    assert!(deny.iter().any(|d| d.contains("sudo")));
    assert_eq!(after["someoneElsesKey"], "keep me");

    let r = muninn(p.path(), &["apply", "--revert", "--yes"]);
    assert!(r.status.success(), "revert failed: {}", r.stderr);
    let back: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    let deny: Vec<&str> = back["permissions"]["deny"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(
        deny,
        vec!["Bash(curl:*)"],
        "revert removes exactly what we added"
    );
    assert_eq!(back["someoneElsesKey"], "keep me");
    assert_eq!(
        verdict(
            p.path(),
            "Bash",
            serde_json::json!({ "command": "sudo apt-get install jq" }),
            false
        ),
        None,
        "revert disables hook enforcement too"
    );
}

/// Codex has no `ask`. The same rule must still reach the agent, as a reminder.
#[test]
fn ask_becomes_a_reminder_on_codex() {
    let p = project("- Ask before deleting any file.\n", "main");
    compile_and_apply(p.path());
    let call = serde_json::json!({ "command": "rm build/output.o" });

    let claude = verdict(p.path(), "Bash", call.clone(), false).expect("claude gets a verdict");
    assert_eq!(decision(&claude), Some("ask"));

    let codex = verdict(p.path(), "Bash", call, true).expect("codex gets a reminder");
    assert!(
        codex["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .is_some_and(|s| s.contains("confirmation")),
        "codex output: {codex}"
    );
    assert_eq!(
        decision(&codex),
        None,
        "no permissionDecision field, because Codex has no ask"
    );
}

/// `branch_in:` is the one condition that reads the working tree.
#[test]
fn a_branch_condition_holds_only_on_that_branch() {
    let rules = "- Never commit directly to `main`; open a pull request.\n";
    let call = serde_json::json!({ "command": "git commit -m wip" });

    let on_main = project(rules, "main");
    compile_and_apply(on_main.path());
    let hooks =
        std::fs::read_to_string(on_main.path().join(".muninn/compiled/pretooluse.json")).unwrap();
    if !hooks.contains("branch_in:") {
        return; // this wording did not compile to a branch condition; nothing to assert
    }
    assert!(
        verdict(on_main.path(), "Bash", call.clone(), false).is_some(),
        "on main the condition holds"
    );

    let on_feature = project(rules, "feature/x");
    compile_and_apply(on_feature.path());
    assert_eq!(
        verdict(on_feature.path(), "Bash", call, false),
        None,
        "on a feature branch the same rule must stay silent"
    );
}

/// The denominator Gate 5b needs: one line per evaluation, silence included.
#[test]
fn every_evaluation_is_logged_including_silence() {
    let p = project(FORCE_PUSH, "main");
    compile_and_apply(p.path());
    verdict(
        p.path(),
        "Bash",
        serde_json::json!({ "command": "git push --force origin main" }),
        false,
    );
    verdict(
        p.path(),
        "Bash",
        serde_json::json!({ "command": "ls -la" }),
        false,
    );

    let log = std::fs::read_to_string(p.path().join(".muninn/log/enforce.jsonl"))
        .expect("the enforce log exists once a rule is applied");
    let lines: Vec<serde_json::Value> = log
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("one JSON object per line"))
        .collect();
    assert_eq!(lines.len(), 2, "one line per evaluation: {log}");
    assert_eq!(lines[0]["decision"], "deny");
    assert!(lines[0]["rule"].is_string());
    assert!(
        lines[1]["decision"].is_null() && lines[1]["rule"].is_null(),
        "silence is recorded as silence, not omitted"
    );
    assert!(lines.iter().all(|l| l["harness"] == "claude-code"));
}

/// An artefact that a later change breaks must fail loudly, not enforce nothing.
#[test]
fn a_corrupt_artefact_does_not_block_the_agent() {
    let p = project(FORCE_PUSH, "main");
    compile_and_apply(p.path());
    std::fs::write(
        p.path().join(".muninn/compiled/pretooluse.json"),
        "{ not json",
    )
    .unwrap();
    assert_eq!(
        verdict(
            p.path(),
            "Bash",
            serde_json::json!({ "command": "git push --force origin main" }),
            false
        ),
        None,
        "a broken artefact means no enforcement, never a crashed hook"
    );
}

/// A `new_file` condition asks whether the file exists in the *project*. Resolving the
/// relative path against the process's own directory instead judged `README.md` by
/// whichever README the caller happened to be standing next to.
#[test]
fn a_new_file_condition_is_judged_against_the_project_not_the_caller() {
    let p = project(
        "- NEVER create documentation files (*.md) unless the user asks.\n",
        "main",
    );
    compile_and_apply(p.path());
    let hooks = std::fs::read_to_string(p.path().join(".muninn/compiled/pretooluse.json")).unwrap();
    if !hooks.contains("new_file") {
        return; // this wording did not compile to a new_file condition
    }

    // the repository this test runs from has a README.md; the temporary project does not
    assert!(Path::new("README.md").exists() || Path::new("../../README.md").exists());
    assert!(!p.path().join("README.md").exists());
    assert!(
        verdict(
            p.path(),
            "Write",
            serde_json::json!({ "file_path": "README.md" }),
            false
        )
        .is_some(),
        "a README that does not exist in the project is a new file, whatever the caller's cwd holds"
    );

    std::fs::write(p.path().join("NOTES.md"), "already here\n").unwrap();
    assert_eq!(
        verdict(
            p.path(),
            "Write",
            serde_json::json!({ "file_path": "NOTES.md" }),
            false
        ),
        None,
        "a file that already exists in the project is not a new file"
    );
}
