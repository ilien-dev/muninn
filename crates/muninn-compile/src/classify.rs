//! Deterministic classifier: rule text → enforcement class + artefact.
//!
//! No LLM. Every classification names the pattern that produced it so the
//! coverage report is auditable. The core is *proximity*: a rule is enforceable
//! only when a negation, an action verb and its object sit together in one
//! sentence. A `never` three sentences away from a backticked command is prose,
//! not a rule — that was the dominant false positive on the public corpus.
//!
//! Safety rule: a wrongly-classified `deny` blocks the agent, so any rule that
//! carries an exception clause ("unless", "without asking", "only when") is
//! downgraded from `deny` to `ask`.

use crate::parse::Candidate;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    EnforceablePermission,
    EnforceableHook,
    EnforceableSandbox,
    InterpretiveOnly,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Class::EnforceablePermission => "enforceable_permission",
            Class::EnforceableHook => "enforceable_hook",
            Class::EnforceableSandbox => "enforceable_sandbox",
            Class::InterpretiveOnly => "interpretive_only",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Deny,
    Ask,
}

/// A Claude Code permission rule: `Tool(specifier)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub tool: String,
    pub specifier: Option<String>,
    pub decision: Decision,
}

impl PermissionRule {
    pub fn render(&self) -> String {
        match &self.specifier {
            Some(s) => format!("{}({})", self.tool, s),
            None => self.tool.clone(),
        }
    }
}

/// A PreToolUse rule evaluated by `muninn hook PreToolUse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRule {
    /// Regex on `tool_name`.
    pub tool_regex: String,
    /// Regex on `tool_input.command` (Bash) — optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_regex: Option<String>,
    /// Regex on `tool_input.file_path` / `path` — optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_regex: Option<String>,
    /// Extra runtime condition: `branch_in:main,master`, `new_file`, `root_file`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    pub decision: Decision,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classification {
    pub class: Class,
    /// Which pattern fired (for the audit trail).
    pub pattern_id: &'static str,
    pub permissions: Vec<PermissionRule>,
    pub hooks: Vec<HookRule>,
    /// Sandbox note (deferred artefact).
    pub sandbox: Option<String>,
    /// Human note when the mapping is partial.
    pub note: Option<String>,
}

fn re(s: &str) -> Regex {
    Regex::new(s).unwrap()
}

struct Rx {
    /// negation + optional adverbs + action verb
    neg_verb: Regex,
    /// negation immediately followed by a backticked object ("never `git push`")
    neg_tick: Regex,
    exception: Regex,
    backtick: Regex,
    sentence_end: Regex,
    positive_turn: Regex,
    force_push: Regex,
    git_push_kw: Regex,
    branch_kw: Regex,
    reset_hard: Regex,
    no_verify: Regex,
    add_all: Regex,
    amend: Regex,
    rm_rf: Regex,
    sudo: Regex,
    kill: Regex,
    secrets_file: Regex,
    root_words: Regex,
    doc_files: Regex,
    network: Regex,
    before_commit: Regex,
    before_push: Regex,
    before_pr: Regex,
    check_verb: Regex,
    ask_before: Regex,
    file_nouns: Regex,
    deps: Regex,
    full_suite: Regex,
    deploy: Regex,
}

const NEG: &str = r"(?:never|do not|don'?t|must not|mustn'?t|should not|shouldn'?t|not allowed to|forbidden to|no)";
const ADV: &str = r"(?:\s+(?:ever|directly|manually|automatically|proactively|silently|blindly|just|simply|casually|yourself|by hand))*";
const VERBS: &str = r"(run|runs|running|use|using|execute|call|invoke|type|start|launch|spawn|edit|modify|touch|change|hand-edit|hand-?edit|overwrite|update|delete|remove|commit|push|read|open|cat|create|save|write|add|install|introduce|rebase|amend|kill|access|fetch|configure|compile|build|reset|checkout|restore|clean|stage|force-push)";

fn rx() -> &'static Rx {
    static RX: OnceLock<Rx> = OnceLock::new();
    RX.get_or_init(|| Rx {
        neg_verb: re(&format!(r"(?i)\b{NEG}{ADV}\s+{VERBS}\b")),
        neg_tick: re(&format!(r"(?i)\b{NEG}{ADV}\s*[:—-]?\s*`")),
        exception: re(r"(?i)\b(unless|except|only (?:when|if|after)|without (?:explicit|the user|user|asking|confirmation|approval|permission|an explicit|being)|until (?:the user|asked|explicitly)|when (?:explicitly )?asked|if (?:the user )?asks|on your own|automatically|proactively|by default|without explicit|in a way that)\b|^(?:when|if|while|unless|during|for)\b"),
        backtick: re(r"`([^`]+)`"),
        sentence_end: re(r"[.!?]\s|\s[—–]\s|;\s| - "),
        positive_turn: re(r"(?i)\b(instead|use |edit |prefer|rather|unless|except|if you|first|then|until)\b"),
        force_push: re(r"(?i)(force[- ]push|push\s+(?:--force|-f)\b|--force\b)"),
        git_push_kw: re(r"(?i)\bgit push\b|\bpush(?:ing|es|ed)?\s+(?:(?:the|a|any|your|new|local|directly to|to|onto)\s+)*(?:remote|origin|main|master|upstream|github|branch(?:es)?|commits?|changes|code|tags?)\b|\bpush(?:ing)?\s+(?:without|unless|until|directly|automatically|yourself)\b|\bpush(?:ing)?\s*[.,;:]|\bpush(?:ing)?\s*$"),
        branch_kw: re(r"(?i)\b(?:directly\s+)?(?:to|on|into|onto)\s+`?(main|master)`?\b"),
        reset_hard: re(r"(?i)(git reset( --hard)?|git checkout( --)?( \.)?|git restore( \.)?|git clean\b|\breset --hard|git stash\b|`(checkout|restore|reset|stash|clean)`)"),
        no_verify: re(r"(?i)--no-verify"),
        add_all: re(r"(?i)git add (?:-A|\.|--all)\b"),
        amend: re(r"(?i)(--amend|\bamend(?:ing)? (?:a |the |pushed |existing )?commits?\b|\bamend\b\s*$|\bamend\s*[,.]|\bgit commit --amend\b)"),
        rm_rf: re(r"(?i)\brm -[rR]f\b|\brm -f[rR]\b|\brm -[rR]\b|\brm --recursive\b|recursive(?:ly)? (?:force[- ])?delet|force[- ]delet"),
        sudo: re(r"(?i)\bsudo\b|\bsu\b\s*[,`]"),
        kill: re(r"(?i)\b(pkill|killall|kill -9|kill\b)"),
        secrets_file: re(r"(?i)((?:^|[^\w.])\.env\b|\.env\.\*|\*\.pem|\*\.key|secrets/|credentials\*|credentials?\.json|id_rsa)"),
        root_words: re(r"(?i)(root (?:folder|directory|dir)|repo(?:sitory)? root|project root|top[- ]level)"),
        doc_files: re(r"(?i)\b(documentation files?|\*\.md|markdown files?|readme(?: files?)?|\.md files?|doc files?)\b"),
        network: re(r"(?i)\b(network access|internet access|access the internet|external (?:network|requests?|urls?|domains?)|make (?:http|network) (?:calls|requests)|web ?fetch|fetch (?:from )?(?:external|remote) )"),
        before_commit: re(r"(?i)(before (?:any |each |every |a |you )?commit(?:ting|s)?\b|prior to commit(?:ting)?|pre-?commit\b)"),
        before_push: re(r"(?i)(before (?:any |each |every |a |you )?push(?:ing|es)?\b|prior to push(?:ing)?|pre-?push\b)"),
        before_pr: re(r"(?i)before (?:opening|creating|submitting|sending) (?:a |the )?(?:pr|pull request|mr|merge request)\b"),
        check_verb: re(r"(?i)\b(run|runs|running|execute|pass(?:es)?|check|verify|lint|format|test|build|compile|typecheck|validate|fix)\b"),
        ask_before: re(r"(?i)\b(?:ask|check|confirm|get (?:explicit )?(?:approval|confirmation|permission))\b(?:\s+(?:the user|user|me|first|for (?:approval|confirmation|permission)|explicitly))*\s+(?:before|prior to)\s+(\w+(?:ing)?(?:\s+\w+){0,5})"),
        file_nouns: re(r"(?i)\b(files?|director(?:y|ies)|dirs?|folders?|data|untracked|artifacts?|worktree|branch(?:es)?)\b"),
        deps: re(r"(?i)\b(dependenc(?:y|ies)|(?:new|npm|external|third[- ]party|pip|cargo|node) (?:packages?|crates?|libraries?|modules?))\b"),
        full_suite: re(r"(?i)\b(?:full|entire|whole|complete)\b.{0,15}\b(?:test ?suite|suite|tests)\b|\b(?:test ?suite|tests)\b.{0,10}\b(?:in full|entirely)\b|\ball tests\b|\bevery test\b"),
        deploy: re(r"(?i)\b(deploy(?:ing|ment)?s?|publish(?:ing)?|release(?:s|ing)?|ship(?:ping)?)\b"),
    })
}

const CLI_WORDS: &[&str] = &[
    "npm",
    "pnpm",
    "yarn",
    "bun",
    "npx",
    "cargo",
    "go",
    "git",
    "make",
    "docker",
    "kubectl",
    "pip",
    "pip3",
    "uv",
    "poetry",
    "python",
    "python3",
    "node",
    "deno",
    "php",
    "rails",
    "bundle",
    "composer",
    "gradle",
    "mvn",
    "flutter",
    "dart",
    "ruff",
    "black",
    "eslint",
    "prettier",
    "tsc",
    "jest",
    "vitest",
    "pytest",
    "ctest",
    "cmake",
    "terraform",
    "aws",
    "gcloud",
    "az",
    "ssh",
    "curl",
    "wget",
    "rm",
    "sudo",
    "su",
    "chmod",
    "chown",
    "kill",
    "pkill",
    "killall",
    "vagrant",
    "task",
    "just",
    "bit",
    "ghstack",
    "gh",
    "task-master",
    "brew",
    "apt",
    "apt-get",
    "ant",
    "sbt",
    "bazel",
    "buck",
    "ninja",
    "xmake",
    "forge",
    "artisan",
    "trunk",
    "ruby",
    "gem",
    "swift",
    "xcodebuild",
    "xcrun",
    "idb",
    "adb",
    "wrangler",
    "vercel",
    "kamal",
    "helm",
    "dotnet",
    "nuget",
    "mix",
    "elixir",
    "cabal",
    "stack",
    "zig",
    "nix",
    "conda",
    "mamba",
    "jom",
    "clang",
    "gcc",
    "g++",
    "codesign",
    "e",
    "breeze",
    "bd",
    "gt",
    "dcg",
    "pscale",
    "openclaw",
    "dimos",
    "multica",
    "ralph",
    "bks",
    "mount",
    "chroot",
    "dd",
    "mkfs",
    "fdisk",
    "umount",
    "systemctl",
    "service",
    "launchctl",
    "tmux",
    "screen",
    "zellij",
    "vitest",
    "mocha",
    "nx",
    "turbo",
    "lerna",
    "rustup",
    "rustc",
    "clippy",
    "golangci-lint",
    "tools/format.js",
    "tools/lint.js",
];

fn first_word(cmd: &str) -> &str {
    cmd.split_whitespace().next().unwrap_or("")
}

fn looks_like_cli(cmd: &str) -> bool {
    let w = first_word(cmd);
    if w.is_empty()
        || w.contains('(')
        || w.contains('{')
        || w.contains('<')
        || w.starts_with('-')
        || w.starts_with('$')
    {
        return false;
    }
    if w.contains('.')
        && !w.starts_with("./")
        && !w.ends_with(".sh")
        && !w.ends_with(".py")
        && !w.ends_with(".js")
    {
        return false;
    }
    CLI_WORDS.contains(&w) || w.starts_with("./") || w.ends_with(".sh")
}

fn looks_like_path(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.contains(' ') || s.contains('(') || s.contains('<') || s.starts_with('-') {
        return false;
    }
    if s.starts_with('@') || s.contains("::") || s.starts_with("http") {
        return false;
    }
    if s.contains('*') {
        return true;
    }
    if s.contains('/') {
        // a route like `/login` or `/{noun}/{verb}` is not a file path
        let last = s.rsplit('/').next().unwrap_or("");
        return s.ends_with('/') || last.contains('.') || !s.starts_with('/') && !s.contains('{');
    }
    // a bare filename needs a stem and a known extension; `.ts` alone is an extension, not a path
    let Some((stem, ext)) = s.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && matches!(
            ext,
            "ts" | "js"
                | "tsx"
                | "jsx"
                | "json"
                | "md"
                | "lock"
                | "toml"
                | "yml"
                | "yaml"
                | "rs"
                | "py"
                | "go"
                | "h"
                | "c"
                | "cpp"
                | "sql"
                | "env"
                | "txt"
                | "snap"
                | "mjs"
                | "cjs"
                | "css"
                | "html"
                | "xml"
                | "gradle"
                | "swift"
                | "kt"
                | "rb"
                | "php"
                | "cs"
                | "java"
                | "prisma"
                | "graphql"
                | "proto"
        )
}

/// A path mention → gitignore-style specifier for Edit/Write/Read rules.
fn path_spec(p: &str) -> String {
    let p = p.trim().trim_end_matches(':');
    let p = p.strip_prefix("./").unwrap_or(p);
    if p.ends_with('/') {
        return format!("./{p}**");
    }
    if p.starts_with("**/") {
        return format!("./{p}");
    }
    if p.starts_with('/') {
        // absolute path: Claude Code's `//` prefix
        let base = p.trim_end_matches('/');
        return if p.ends_with('/') {
            format!("/{base}/**")
        } else {
            format!("/{p}")
        };
    }
    if p.starts_with("*.") {
        return format!("./**/{p}");
    }
    format!("./{p}")
}

fn cmd_spec(cmd: &str) -> String {
    let c = cmd.trim().trim_end_matches(['.', ',', ';']);
    let c = c
        .split(" <")
        .next()
        .unwrap_or(c)
        .trim_end_matches("...")
        .trim();
    format!("{c}:*")
}

fn perm(tool: &str, spec: Option<String>, decision: Decision) -> PermissionRule {
    PermissionRule {
        tool: tool.into(),
        specifier: spec,
        decision,
    }
}

fn hook(
    tool_regex: &str,
    command_regex: Option<&str>,
    path_regex: Option<&str>,
    condition: Option<&str>,
    decision: Decision,
    reason: String,
) -> HookRule {
    HookRule {
        tool_regex: tool_regex.into(),
        command_regex: command_regex.map(str::to_string),
        path_regex: path_regex.map(str::to_string),
        condition: condition.map(str::to_string),
        decision,
        reason,
    }
}

fn out(
    class: Class,
    pattern_id: &'static str,
    permissions: Vec<PermissionRule>,
    hooks: Vec<HookRule>,
) -> Classification {
    Classification {
        class,
        pattern_id,
        permissions,
        hooks,
        sandbox: None,
        note: None,
    }
}

fn interpretive(pattern_id: &'static str) -> Classification {
    out(Class::InterpretiveOnly, pattern_id, vec![], vec![])
}

/// One negated action: the verb and the text window it governs (same sentence,
/// cut at a positive turn like "— edit `x` instead").
struct Negated<'a> {
    verb: String,
    /// text from the verb to the end of its sentence (bounded)
    window: &'a str,
    /// text of the whole sentence containing the negation
    sentence: &'a str,
}

fn sentence_bounds(t: &str, pos: usize) -> (usize, usize) {
    let r = rx();
    let start = r
        .sentence_end
        .find_iter(&t[..pos])
        .last()
        .map(|m| m.end())
        .unwrap_or(0);
    let end = r
        .sentence_end
        .find(&t[pos..])
        .map(|m| pos + m.start())
        .unwrap_or(t.len());
    (start, end)
}

fn negated_actions(t: &str) -> Vec<Negated<'_>> {
    let r = rx();
    let mut v = Vec::new();
    for m in r.neg_verb.captures_iter(t) {
        let verb = m.get(1).unwrap();
        let (s, e) = sentence_bounds(t, verb.start());
        let mut win_end = e.min(verb.end() + 160);
        if let Some(p) = r.positive_turn.find(&t[verb.end()..win_end]) {
            // "never edit X — edit Y instead": stop before the positive part
            if p.start() > 0 {
                win_end = verb.end() + p.start();
            }
        }
        v.push(Negated {
            verb: verb.as_str().to_lowercase(),
            window: &t[verb.end()..win_end],
            sentence: &t[s..e],
        });
    }
    // "never `cmd`" with no verb
    for m in r.neg_tick.find_iter(t) {
        let (s, e) = sentence_bounds(t, m.start());
        let win_end = e.min(m.end() + 160);
        v.push(Negated {
            verb: String::new(),
            window: &t[m.end() - 1..win_end],
            sentence: &t[s..e],
        });
    }
    v
}

fn ticks_in(window: &str, max_gap: usize) -> Vec<String> {
    let r = rx();
    r.backtick
        .captures_iter(window)
        .filter(|c| c.get(0).unwrap().start() <= max_gap)
        .map(|c| c[1].to_string())
        .collect()
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n - 1).collect();
        format!("{cut}…")
    }
}

/// Classify one candidate. The `source` string is embedded in hook reasons so
/// the agent sees where a block came from.
pub fn classify(c: &Candidate) -> Classification {
    let r = rx();
    let t = c.text.as_str();
    let src = format!("{}:{}", c.source_file, c.source_line);
    let soft = re(r"(?i)\b(never|do not|don'?t)\s+(just|simply|blindly|casually)\b|\b(on a (?:pr|feature|release) branch|while (?:reviewing|debugging)|during (?:a |the )?(?:review|release))\b").is_match(t);
    let has_exception = r.exception.is_match(t) || soft;
    let downgrade = |d: Decision| if has_exception { Decision::Ask } else { d };
    let reason = |what: &str| format!("Rule ({src}): {what} — \"{}\"", truncate(t, 140));
    let actions = negated_actions(t);
    let is_verb = |n: &Negated, set: &[&str]| set.iter().any(|v| n.verb.starts_with(v));

    // ---- A. git operations ------------------------------------------------
    for n in &actions {
        let w = n.window;
        // force push
        if r.force_push.is_match(w)
            && (n.verb.is_empty() || is_verb(n, &["run", "use", "push", "force", "execute", "do"]))
            || n.verb.starts_with("force-push")
        {
            let d = downgrade(Decision::Deny);
            let allows_lease = n.sentence.to_lowercase().contains("force-with-lease")
                && !n.sentence.to_lowercase().contains("never plain");
            let hooks = if allows_lease {
                vec![hook(
                    "^Bash$",
                    Some(r"\bgit\s+push\b.*(\s--force\b(?:\s|$)|\s-f\b)"),
                    None,
                    None,
                    d,
                    reason("force-push without --force-with-lease is not allowed"),
                )]
            } else {
                vec![hook(
                    "^Bash$",
                    Some(r"\bgit\s+push\b.*(\s--force(?:\s|$)|\s-f\b|\s--force-with-lease\b)"),
                    None,
                    None,
                    d,
                    reason("force-push is not allowed"),
                )]
            };
            return out(
                Class::EnforceablePermission,
                "git.force_push",
                vec![
                    perm("Bash", Some("git push --force:*".into()), d),
                    perm("Bash", Some("git push -f:*".into()), d),
                ],
                hooks,
            );
        }
        if r.no_verify.is_match(w) && w.find("--no-verify").is_some_and(|p| p < 80) {
            let d = downgrade(Decision::Deny);
            return out(
                Class::EnforceableHook,
                "git.no_verify",
                vec![],
                vec![hook(
                    "^Bash$",
                    Some(r"\bgit\b.*--no-verify\b"),
                    None,
                    None,
                    d,
                    reason("bypassing hooks with --no-verify is not allowed"),
                )],
            );
        }
        if (is_verb(
            n,
            &[
                "run", "use", "execute", "reset", "checkout", "restore", "clean", "do",
            ],
        ) || n.verb.is_empty())
            && r.reset_hard.is_match(w)
            && r.reset_hard.find(w).is_some_and(|m| m.start() < 60)
        {
            let explicit =
                re(r"(?i)(--hard|-- \.|restore \.|clean -f|stash (drop|clear))").is_match(w);
            let d = if explicit {
                downgrade(Decision::Deny)
            } else {
                Decision::Ask
            };
            return out(
                Class::EnforceablePermission,
                "git.destructive",
                vec![
                    perm("Bash", Some("git reset --hard:*".into()), d),
                    perm("Bash", Some("git checkout -- .:*".into()), d),
                    perm("Bash", Some("git restore .:*".into()), d),
                    perm("Bash", Some("git clean:*".into()), d),
                ],
                vec![hook(
                    "^Bash$",
                    Some(r"\bgit\s+(reset\s+--hard|checkout\s+--\s+\.|restore\s+\.|clean\b)"),
                    None,
                    None,
                    d,
                    reason("destructive git operation"),
                )],
            );
        }
        if r.add_all.is_match(w) && r.add_all.find(w).is_some_and(|m| m.start() < 60) {
            let d = downgrade(Decision::Deny);
            return out(
                Class::EnforceablePermission,
                "git.add_all",
                vec![
                    perm("Bash", Some("git add -A:*".into()), d),
                    perm("Bash", Some("git add .:*".into()), d),
                    perm("Bash", Some("git add --all:*".into()), d),
                ],
                vec![hook(
                    "^Bash$",
                    Some(r"\bgit\s+add\s+(-A|\.|--all)\b"),
                    None,
                    None,
                    d,
                    reason("stage specific files, not everything"),
                )],
            );
        }
        if (n.verb.starts_with("amend")
            || r.amend.is_match(w) && r.amend.find(w).is_some_and(|m| m.start() < 40))
            && (n.verb.is_empty()
                || is_verb(n, &["amend", "run", "use", "do", "execute", "commit"]))
        {
            let d = downgrade(Decision::Deny);
            return out(
                Class::EnforceablePermission,
                "git.amend",
                vec![perm("Bash", Some("git commit --amend:*".into()), d)],
                vec![hook(
                    "^Bash$",
                    Some(r"\bgit\s+commit\b.*--amend\b"),
                    None,
                    None,
                    d,
                    reason("amending commits is not allowed"),
                )],
            );
        }
        if n.verb.starts_with("rebase")
            || (is_verb(n, &["run", "use", "do"]) && w.to_lowercase().starts_with(" `git rebase"))
        {
            let d = downgrade(Decision::Deny);
            return out(
                Class::EnforceablePermission,
                "git.rebase",
                vec![perm("Bash", Some("git rebase:*".into()), d)],
                vec![hook(
                    "^Bash$",
                    Some(r"\bgit\s+rebase\b"),
                    None,
                    None,
                    d,
                    reason("rebasing is not allowed"),
                )],
            );
        }
        if (is_verb(n, &["commit", "push"])
            || n.verb.is_empty() && w.starts_with("`git commit")
            || w.starts_with("`git push"))
            && r.branch_kw.is_match(w)
            && r.branch_kw.find(w).is_some_and(|m| m.start() < 60)
        {
            let d = downgrade(Decision::Deny);
            let on_push = n.verb.starts_with("push") || w.starts_with("`git push");
            let cmd = if on_push {
                r"\bgit\s+push\b"
            } else {
                r"\bgit\s+commit\b"
            };
            return out(
                Class::EnforceableHook,
                "git.protected_branch",
                vec![],
                vec![hook(
                    "^Bash$",
                    Some(cmd),
                    None,
                    Some("branch_in:main,master"),
                    d,
                    reason("main/master is protected; use a branch"),
                )],
            );
        }
        if n.verb.starts_with("push")
            || n.verb.is_empty() && w.starts_with("`git push")
            || is_verb(n, &["run", "use", "execute"]) && w.trim_start().starts_with("`git push")
        {
            if n.verb.starts_with("push") && !r.git_push_kw.is_match(n.sentence) {
                continue; // "do not push them into a generic utility"
            }
            return out(
                Class::EnforceablePermission,
                "git.push",
                vec![perm("Bash", Some("git push:*".into()), Decision::Ask)],
                vec![],
            );
        }
        if (n.verb.starts_with("commit") || n.verb.is_empty() && w.starts_with("`git commit"))
            && (has_exception
                || n.sentence.to_lowercase().contains("automatic")
                || n.sentence.to_lowercase().contains("yourself")
                || n.sentence.to_lowercase().contains("on your own")
                || n.sentence.to_lowercase().contains("on the user"))
        {
            // "never commit <path>" is handled below; this is the unconditional "never commit unless asked"
            let path_first = ticks_in(w, 20).iter().any(|p| looks_like_path(p));
            if !path_first {
                return out(
                    Class::EnforceablePermission,
                    "git.commit",
                    vec![perm("Bash", Some("git commit:*".into()), Decision::Ask)],
                    vec![],
                );
            }
        }
        if is_verb(n, &["modify", "change", "edit", "touch"])
            && w.to_lowercase().contains("git config")
            && w.to_lowercase().find("git config").is_some_and(|p| p < 40)
        {
            let d = downgrade(Decision::Deny);
            return out(
                Class::EnforceablePermission,
                "git.config",
                vec![perm("Bash", Some("git config:*".into()), d)],
                vec![],
            );
        }
    }

    for n in &actions {
        if is_verb(n, &["overwrite", "delete", "remove", "force-push", "push"])
            && re(r"(?i)^\s*(?:existing |release |git )?tags?\b").is_match(n.window)
        {
            let d = downgrade(Decision::Deny);
            return out(
                Class::EnforceableHook,
                "git.tags",
                vec![],
                vec![hook(
                    "^Bash$",
                    Some(
                        r"\bgit\s+tag\b.*\s(-f|--force|-d|--delete)\b|\bgit\s+push\b.*(--delete|:refs/tags/|--force.*\btags?\b)",
                    ),
                    None,
                    None,
                    d,
                    reason("tags must not be overwritten or deleted"),
                )],
            );
        }
    }

    // ---- B. destructive shell --------------------------------------------
    for n in &actions {
        let w = n.window;
        let near = |re_: &Regex| re_.find(w).is_some_and(|m| m.start() < 60);
        if (n.verb.is_empty() || is_verb(n, &["run", "use", "execute", "delete", "remove", "do"]))
            && near(&r.rm_rf)
        {
            let d = downgrade(Decision::Deny);
            return out(
                Class::EnforceablePermission,
                "shell.rm_rf",
                vec![
                    perm("Bash", Some("rm -rf:*".into()), d),
                    perm("Bash", Some("rm -fr:*".into()), d),
                ],
                vec![hook(
                    "^Bash$",
                    Some(
                        r"(^|[;&|]\s*)rm\s+-[a-zA-Z]*[rR][a-zA-Z]*\b|(^|[;&|]\s*)rm\s+-[a-zA-Z]*f[a-zA-Z]*\s+-[a-zA-Z]*r",
                    ),
                    None,
                    None,
                    d,
                    reason("recursive delete"),
                )],
            );
        }
        if (n.verb.is_empty() || is_verb(n, &["run", "use", "execute", "do", "call", "invoke"]))
            && near(&r.sudo)
        {
            let d = downgrade(Decision::Deny);
            return out(
                Class::EnforceablePermission,
                "shell.sudo",
                vec![perm("Bash", Some("sudo:*".into()), d)],
                vec![hook(
                    "^Bash$",
                    Some(r"(^|[;&|]\s*)sudo\b"),
                    None,
                    None,
                    d,
                    reason("sudo is not allowed"),
                )],
            );
        }
        let proc_obj = re(r"(?i)\b(process(?:es)?|server|servers|serves|daemon|pid|session|run|job|eval|zellij|tmux|`p?kill)\b").is_match(&w[..w.len().min(80)]);
        if (n.verb.is_empty() || is_verb(n, &["run", "use", "execute", "kill", "do", "call"]))
            && (n.verb.starts_with("kill") && proc_obj || near(&r.kill) && w.contains('`'))
        {
            let d = downgrade(Decision::Deny);
            return out(
                Class::EnforceablePermission,
                "shell.kill",
                vec![
                    perm("Bash", Some("kill:*".into()), d),
                    perm("Bash", Some("pkill:*".into()), d),
                    perm("Bash", Some("killall:*".into()), d),
                ],
                vec![hook(
                    "^Bash$",
                    Some(r"(^|[;&|]\s*)(kill|pkill|killall)\b"),
                    None,
                    None,
                    d,
                    reason("killing processes is not allowed"),
                )],
            );
        }
    }

    // ---- C. specific commands in backticks -------------------------------
    for n in &actions {
        if !(n.verb.is_empty()
            || is_verb(
                n,
                &[
                    "run", "use", "execute", "call", "invoke", "type", "start", "launch", "spawn",
                    "compile", "build", "do",
                ],
            ))
        {
            continue;
        }
        let cmds: Vec<String> = ticks_in(n.window, 40)
            .into_iter()
            .filter(|b| looks_like_cli(b))
            .collect();
        if cmds.is_empty() {
            continue;
        }
        if re(r"(?i)^\s*`[^`]*`(?:\s+\w+){0,2}\s+as\b").is_match(n.window) {
            continue; // "never use `pytest` commands as proof" — about evidence, not execution
        }
        if re(r"(?i)\b(in (?:those|these|such) contexts|in ci\b|in automation|on ci\b|in production|in prod\b)").is_match(n.sentence) {
            continue; // conditional on an execution context the hook cannot see
        }
        // the same command mentioned positively outside the negated window → ambiguous → ask
        let outside: String = {
            let w = n.window;
            let mut o = t.to_string();
            if let Some(p) = o.find(w) {
                o.replace_range(p..p + w.len(), "");
            }
            o
        };
        let ambiguous = cmds.iter().any(|c| outside.contains(&format!("`{c}`")));
        let d = if ambiguous {
            Decision::Ask
        } else {
            downgrade(Decision::Deny)
        };
        let (with_args, plain): (Vec<&String>, Vec<&String>) =
            cmds.iter().partition(|c| c.contains('<'));
        let mut perms: Vec<PermissionRule> = plain
            .iter()
            .map(|c| perm("Bash", Some(cmd_spec(c)), d))
            .collect();
        perms.dedup();
        let mut hooks: Vec<HookRule> = plain
            .iter()
            .map(|c| {
                let esc = regex::escape(cmd_spec(c).trim_end_matches(":*"));
                hook(
                    "^Bash$",
                    Some(&format!(r"(^|[;&|]\s*){esc}(\s|$)")),
                    None,
                    None,
                    d,
                    reason("this command is not allowed"),
                )
            })
            .collect();
        // `cmd <arg>`: the bare command is allowed, the form with an argument is not
        hooks.extend(with_args.iter().map(|c| {
            let esc = regex::escape(cmd_spec(c).trim_end_matches(":*"));
            hook(
                "^Bash$",
                Some(&format!(r"(^|[;&|]\s*){esc}\s+\S")),
                None,
                None,
                d,
                reason("this command form is not allowed"),
            )
        }));
        let class = if perms.is_empty() {
            Class::EnforceableHook
        } else {
            Class::EnforceablePermission
        };
        return out(class, "shell.named_command", perms, hooks);
    }

    // ---- D. files and paths ----------------------------------------------
    for n in &actions {
        let w = n.window;
        if is_verb(n, &["commit", "stage", "add", "push"]) {
            if is_verb(n, &["add"])
                && !re(
                    r"(?i)\b(repo|repository|git|version control|commit|stage|staging|tracked)\b",
                )
                .is_match(n.sentence)
            {
                continue;
            }
            let paths: Vec<String> = ticks_in(w, 30)
                .into_iter()
                .filter(|b| looks_like_path(b) || r.secrets_file.is_match(b))
                .collect();
            let mentions_secret = r.secrets_file.find(w).is_some_and(|m| m.start() < 40)
                || w.to_lowercase().find("secret").is_some_and(|p| p < 30)
                || w.to_lowercase().find("credential").is_some_and(|p| p < 30)
                || w.to_lowercase().find("api key").is_some_and(|p| p < 30);
            if !paths.is_empty() || mentions_secret {
                let d = downgrade(Decision::Deny);
                let mut pats: Vec<String> = paths
                    .iter()
                    .map(|p| regex::escape(p.trim_end_matches('/').trim_start_matches("./")))
                    .collect();
                if mentions_secret {
                    pats.push(r"\.env(\.|\s|$)".into());
                    pats.push(r"\.(pem|key)(\s|$)".into());
                }
                pats.dedup();
                let alt = pats.join("|");
                let mut c = out(
                    Class::EnforceableHook,
                    "git.commit_path",
                    vec![],
                    vec![hook(
                        "^Bash$",
                        Some(&format!(r"\bgit\s+(add|commit)\b.*({alt})")),
                        None,
                        None,
                        d,
                        reason("this path must not be committed"),
                    )],
                );
                c.note = Some("guards `git add`/`git commit` arguments only; `git add -A` bypasses it unless also denied".into());
                return c;
            }
        }
        if is_verb(
            n,
            &[
                "edit",
                "modify",
                "touch",
                "change",
                "hand-edit",
                "hand",
                "overwrite",
                "update",
                "delete",
                "remove",
                "write",
            ],
        ) {
            let mut paths: Vec<String> = ticks_in(w, 70)
                .into_iter()
                .filter(|b| looks_like_path(b))
                .collect();
            if paths.is_empty()
                && is_verb(n, &["modify", "edit", "touch", "change"])
                && w.trim().len() < 40
            {
                // object stated before the negation: "`Vagrantfile` - do not modify without permission"
                let before = &n.sentence[..n.sentence.find(w).unwrap_or(0)];
                paths = ticks_in(before, usize::MAX)
                    .into_iter()
                    .filter(|b| looks_like_path(b))
                    .collect();
            }
            if !paths.is_empty() {
                let d = downgrade(Decision::Deny);
                let generated = n.sentence.to_lowercase().contains("generated")
                    || n.sentence.to_lowercase().contains("lockfile")
                    || n.sentence.to_lowercase().contains("auto-gen");
                let mut perms = Vec::new();
                for p in &paths {
                    let spec = path_spec(p);
                    perms.push(perm("Edit", Some(spec.clone()), d));
                    perms.push(perm("Write", Some(spec), d));
                }
                let mut c = out(
                    Class::EnforceablePermission,
                    if generated {
                        "file.generated"
                    } else {
                        "file.protected_path"
                    },
                    perms,
                    vec![],
                );
                if !generated {
                    c.note = Some("path rule inferred from a backticked path; verify the glob before applying".into());
                }
                return c;
            }
        }
        if is_verb(n, &["read", "open", "cat", "access"])
            && r.secrets_file.find(w).is_some_and(|m| m.start() < 60)
        {
            let d = downgrade(Decision::Deny);
            return out(
                Class::EnforceablePermission,
                "file.secrets_read",
                vec![
                    perm("Read", Some("./.env".into()), d),
                    perm("Read", Some("./.env.*".into()), d),
                    perm("Read", Some("./**/*.pem".into()), d),
                    perm("Read", Some("./**/*.key".into()), d),
                ],
                vec![],
            );
        }
        if is_verb(n, &["create", "save", "write", "add"])
            && (w.to_lowercase().contains("file") || r.doc_files.is_match(w))
            && r.root_words.find(w).is_some_and(|m| m.start() < 80)
        {
            return out(
                Class::EnforceableHook,
                "file.root_create",
                vec![],
                vec![hook(
                    "^(Write|MultiEdit)$",
                    None,
                    None,
                    Some("root_file"),
                    Decision::Ask,
                    reason("new files must not be created at the repository root"),
                )],
            );
        }
        if is_verb(n, &["create", "add", "write"])
            && r.doc_files.find(w).is_some_and(|m| m.start() < 40)
        {
            return out(
                Class::EnforceableHook,
                "file.doc_create",
                vec![],
                vec![hook(
                    "^Write$",
                    None,
                    Some(r"(?i)\.(md|mdx|rst|txt)$"),
                    Some("new_file"),
                    Decision::Ask,
                    reason("do not create documentation files unless asked"),
                )],
            );
        }
        if is_verb(n, &["access", "use", "make", "fetch"])
            && r.network.find(w).is_some_and(|m| m.start() < 40)
            || r.network.is_match(t) && actions.iter().any(|n| n.verb.is_empty())
        {
            let d = downgrade(Decision::Deny);
            let mut c = out(
                Class::EnforceableSandbox,
                "net.no_network",
                vec![perm("WebFetch", None, d), perm("WebSearch", None, d)],
                vec![hook(
                    "^Bash$",
                    Some(r"(^|[;&|]\s*)(curl|wget|http|https)\b"),
                    None,
                    None,
                    d,
                    reason("network access is not allowed"),
                )],
            );
            c.sandbox = Some(
                "sandbox network=deny (not emitted in the MVP; permission + hook approximate it)"
                    .into(),
            );
            return c;
        }
        if is_verb(n, &["add", "install", "introduce"])
            && r.deps.find(w).is_some_and(|m| m.start() < 40)
        {
            return out(
                Class::EnforceableHook,
                "process.dependencies",
                vec![],
                vec![hook(
                    "^Bash$",
                    Some(
                        r"\b(npm|pnpm|yarn|bun) (add|install|i)\s+\S|\bcargo add\b|\b(pip|pip3|uv|poetry) (install|add)\s+\S|\bgo get\b|\bcomposer require\b|\bgem install\b",
                    ),
                    None,
                    None,
                    Decision::Ask,
                    reason("new dependencies need approval"),
                )],
            );
        }
        if is_verb(n, &["run", "execute"]) && r.full_suite.find(w).is_some_and(|m| m.start() < 30) {
            return out(
                Class::EnforceableHook,
                "process.full_suite",
                vec![],
                vec![hook(
                    "^Bash$",
                    Some(
                        r"(^|[;&|]\s*)(npm test|pnpm test|yarn test|bun test|cargo test|pytest|go test \./\.\.\.|make test)\s*$",
                    ),
                    None,
                    None,
                    Decision::Ask,
                    reason("run targeted tests, not the whole suite"),
                )],
            );
        }
    }
    // deploy/release/publish without instruction
    if let Some(n) = actions.iter().find(|n| {
        n.verb.is_empty() && r.deploy.find(n.window).is_some_and(|m| m.start() < 6)
            || r.deploy.is_match(&n.verb)
    }) {
        if has_exception || n.sentence.to_lowercase().contains("production") {
            return out(
                Class::EnforceableHook,
                "process.deploy",
                vec![],
                vec![hook(
                    "^Bash$",
                    Some(
                        r"\b(deploy|wrangler|vercel|kamal|fly deploy|npm publish|cargo publish|gh release)\b",
                    ),
                    None,
                    None,
                    Decision::Ask,
                    reason("deploys need confirmation"),
                )],
            );
        }
    }
    if let Some(m) = re(r"(?i)\b(never|do not|don'?t)\s+(deploy|release|publish)\b").find(t) {
        let (s, e) = sentence_bounds(t, m.start());
        if r.exception.is_match(&t[s..e])
            || t[s..e].to_lowercase().contains("production")
            || t[s..e].to_lowercase().contains("instruct")
        {
            return out(
                Class::EnforceableHook,
                "process.deploy",
                vec![],
                vec![hook(
                    "^Bash$",
                    Some(
                        r"\b(deploy|wrangler|vercel|kamal|fly deploy|npm publish|cargo publish|gh release)\b",
                    ),
                    None,
                    None,
                    Decision::Ask,
                    reason("deploys need confirmation"),
                )],
            );
        }
    }

    // ---- E. conditional / process ----------------------------------------
    if t.chars().count() <= 260 {
        if let Some(m) = r.before_pr.find(t) {
            let (s, e) = sentence_bounds(t, m.start());
            if r.check_verb.is_match(&t[s..e]) {
                return out(
                    Class::EnforceableHook,
                    "process.before_pr",
                    vec![],
                    vec![hook(
                        "^Bash$",
                        Some(r"\bgh\s+pr\s+create\b|\bglab\s+mr\s+create\b"),
                        None,
                        None,
                        Decision::Ask,
                        reason("a check is required before opening a PR"),
                    )],
                );
            }
        }
        let bc = r.before_commit.find(t);
        let bp = r.before_push.find(t);
        if let Some(m) = bc.or(bp) {
            let (s, e) = sentence_bounds(t, m.start());
            let sent = &t[s..e];
            let informational = re(r"(?i)(not before|no need|only need|don'?t need|optional|automatically (?:via|by|through|runs)|runs automatically|pre-commit hook runs|husky)").is_match(sent);
            if r.check_verb.is_match(sent) && !informational {
                let on_push = bc.is_none();
                let cmd = if on_push {
                    r"\bgit\s+push\b"
                } else {
                    r"\bgit\s+(commit|push)\b"
                };
                return out(
                    Class::EnforceableHook,
                    "process.before_commit",
                    vec![],
                    vec![hook(
                        "^Bash$",
                        Some(cmd),
                        None,
                        None,
                        Decision::Ask,
                        reason("a check is required before committing/pushing"),
                    )],
                );
            }
        }
        if let Some(cap) = r.ask_before.captures(t) {
            if re(r"(?i)\b(if|when)\b.{0,12}\b(unsure|uncertain|in doubt|not sure|doubt)\b")
                .is_match(t)
            {
                return interpretive("process.ask_if_unsure");
            }
            let what = cap[1].to_lowercase();
            let (tool, cmd, path): (&str, Option<&str>, Option<&str>) = if (what
                .starts_with("delet")
                || what.starts_with("remov"))
                && r.file_nouns.is_match(&what)
            {
                ("^(Bash)$", Some(r"(^|[;&|]\s*)(rm|git rm|rmdir)\b"), None)
            } else if what.starts_with("deploy")
                || what.starts_with("publish")
                || what.starts_with("releas")
            {
                (
                    "^Bash$",
                    Some(
                        r"\b(deploy|wrangler|vercel|kamal|fly deploy|gcloud .*deploy|npm publish|cargo publish|gh release)\b",
                    ),
                    None,
                )
            } else if what.starts_with("commit") {
                ("^Bash$", Some(r"\bgit\s+commit\b"), None)
            } else if what.starts_with("push") {
                ("^Bash$", Some(r"\bgit\s+push\b"), None)
            } else if what.contains("depend")
                || what.starts_with("install")
                || what.starts_with("add") && what.contains("packag")
            {
                (
                    "^Bash$",
                    Some(
                        r"\b(npm|pnpm|yarn|bun) (add|install|i)\b|\bcargo add\b|\b(pip|uv|poetry) (install|add)\b|\bgo get\b",
                    ),
                    None,
                )
            } else if what.contains("migrat") || what.contains("schema") {
                (
                    "^(Edit|Write|MultiEdit)$",
                    None,
                    Some(r"(?i)(migrations?/|schema\.(prisma|sql|rb|ts))"),
                )
            } else {
                return interpretive("process.ask_before_unmapped");
            };
            return out(
                Class::EnforceableHook,
                "process.ask_before",
                vec![],
                vec![hook(
                    tool,
                    cmd,
                    path,
                    None,
                    Decision::Ask,
                    reason("this action needs confirmation"),
                )],
            );
        }
    }

    interpretive("none")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(text: &str) -> Candidate {
        Candidate {
            source_file: "CLAUDE.md".into(),
            source_line: 1,
            source_hash: "h".into(),
            text: text.into(),
            heading: None,
        }
    }

    #[test]
    fn force_push_is_denied() {
        let c = classify(&cand("Never `git push --force` or `git push -f`"));
        assert_eq!(c.pattern_id, "git.force_push");
        assert!(c
            .permissions
            .iter()
            .any(|p| p.render() == "Bash(git push --force:*)" && p.decision == Decision::Deny));
    }

    #[test]
    fn push_without_instruction_is_ask() {
        let c = classify(&cand(
            "Never `git push` without an explicit instruction to push.",
        ));
        assert_eq!(c.pattern_id, "git.push");
        assert_eq!(c.permissions[0].decision, Decision::Ask);
    }

    #[test]
    fn push_in_other_sense_is_interpretive() {
        let c = classify(&cand(
            "Keep carve-outs at the boundary; do not push them into a generic lower-level utility.",
        ));
        assert_eq!(c.class, Class::InterpretiveOnly);
    }

    #[test]
    fn commit_unless_asked_is_ask() {
        let c = classify(&cand("NEVER commit unless user asks"));
        assert_eq!(c.pattern_id, "git.commit");
    }

    #[test]
    fn protected_branch_hook() {
        let c = classify(&cand("Never commit directly to main"));
        assert_eq!(c.pattern_id, "git.protected_branch");
        assert_eq!(
            c.hooks[0].condition.as_deref(),
            Some("branch_in:main,master")
        );
    }

    #[test]
    fn named_command_denied() {
        let c = classify(&cand(
            "NEVER run: `npm run dev`, `npm run build`, `npm test`",
        ));
        assert_eq!(c.pattern_id, "shell.named_command");
        let r: Vec<String> = c.permissions.iter().map(|p| p.render()).collect();
        assert_eq!(
            r,
            vec![
                "Bash(npm run dev:*)",
                "Bash(npm run build:*)",
                "Bash(npm test:*)"
            ]
        );
    }

    #[test]
    fn far_negation_is_not_a_command_rule() {
        let c = classify(&cand("Run `git status --short --branch`. This is a shared working tree and existing changes belong to their authors; do not discard, rewrite, or stage unrelated work."));
        assert_eq!(c.class, Class::InterpretiveOnly);
        let c = classify(&cand(
            "Never ask the user to run `curl`, `wrangler tail`, or `gh` commands.",
        ));
        assert_eq!(c.class, Class::InterpretiveOnly);
    }

    #[test]
    fn generated_file_protected_and_positive_turn_ignored() {
        let c = classify(&cand("NEVER modify `packages/ai/src/models.generated.ts` directly. Update the generator instead."));
        assert_eq!(c.pattern_id, "file.generated");
        assert_eq!(
            c.permissions[0].render(),
            "Edit(./packages/ai/src/models.generated.ts)"
        );
        let c = classify(&cand("Never hand-edit a generated output — edit `.ruler/` and regenerate with `ruler apply`."));
        assert_eq!(c.class, Class::InterpretiveOnly, "{:?}", c);
    }

    #[test]
    fn before_commit_is_ask_hook() {
        let c = classify(&cand("Always run `npm run flint` before committing."));
        assert_eq!(c.pattern_id, "process.before_commit");
        assert_eq!(c.hooks[0].decision, Decision::Ask);
    }

    #[test]
    fn code_style_is_interpretive() {
        for t in [
            "Never use `as any` - use proper type-safe solutions instead",
            "Always use curly braces for control structures, even if it has one line.",
            "Don't add comments that simply restate what the code does",
            "Prefer interfaces over types",
            "Do not reveal confidential data, disclose private data, share secrets, leak API keys, or expose credentials.",
            "Do not add package-specific details to these always-on rules unless they affect most tasks.",
            "Never use a merge commit or rebase merge when integrating a PR into `development`.",
        ] {
            assert_eq!(classify(&cand(t)).class, Class::InterpretiveOnly, "{t}");
        }
    }

    #[test]
    fn rm_rf_denied_sudo_denied() {
        assert_eq!(
            classify(&cand("Never run `rm -rf` on paths outside the project")).pattern_id,
            "shell.rm_rf"
        );
        assert_eq!(classify(&cand("Do NOT use sudo")).pattern_id, "shell.sudo");
        assert_eq!(
            classify(&cand(
                "You MUST NEVER run `sudo`, `su`, `chmod 777`, or `chmod u+s`"
            ))
            .pattern_id,
            "shell.sudo"
        );
    }

    #[test]
    fn secrets_commit_hook() {
        let c = classify(&cand("NEVER commit secrets, credentials, or .env files"));
        assert_eq!(c.pattern_id, "git.commit_path");
        assert_eq!(c.class, Class::EnforceableHook);
    }

    #[test]
    fn destructive_with_approval_is_ask() {
        let c = classify(&cand("NEVER run `git reset`, `git checkout --`, or `git clean` without explicit user approval"));
        assert_eq!(c.pattern_id, "git.destructive");
        assert_eq!(c.permissions[0].decision, Decision::Ask);
    }
}
