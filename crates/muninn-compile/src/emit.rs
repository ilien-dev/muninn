//! Emitters: permission fragment, PreToolUse rule file, coverage report.

use crate::classify::{Class, Decision};
use crate::CompiledRule;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Serialize)]
pub struct PermissionsFragment {
    pub permissions: PermissionLists,
}

#[derive(Debug, Default, Serialize)]
pub struct PermissionLists {
    pub deny: Vec<String>,
    pub ask: Vec<String>,
}

/// `settings.json` fragment: deduplicated, deterministic order.
pub fn permissions(rules: &[CompiledRule]) -> PermissionsFragment {
    let mut deny = std::collections::BTreeSet::new();
    let mut ask = std::collections::BTreeSet::new();
    for r in rules {
        for p in &r.classification.permissions {
            match p.decision {
                Decision::Deny => {
                    deny.insert(p.render());
                }
                Decision::Ask => {
                    ask.insert(p.render());
                }
            }
        }
    }
    // a deny rule for X makes an ask rule for X redundant
    let ask: Vec<String> = ask.into_iter().filter(|a| !deny.contains(a)).collect();
    PermissionsFragment {
        permissions: PermissionLists {
            deny: deny.into_iter().collect(),
            ask,
        },
    }
}

#[derive(Debug, Serialize)]
pub struct HookFile {
    pub version: u32,
    pub rules: Vec<HookEntry>,
}

#[derive(Debug, Serialize)]
pub struct HookEntry {
    pub id: String,
    pub source: String,
    pub text: String,
    #[serde(flatten)]
    pub rule: crate::classify::HookRule,
}

/// `pretooluse.json`: consumed by `muninn hook PreToolUse`.
pub fn hooks(rules: &[CompiledRule]) -> HookFile {
    let mut out = Vec::new();
    for r in rules {
        for (i, h) in r.classification.hooks.iter().enumerate() {
            out.push(HookEntry {
                id: format!(
                    "{}:{}#{}",
                    r.candidate.source_file, r.candidate.source_line, i
                ),
                source: format!("{}:{}", r.candidate.source_file, r.candidate.source_line),
                text: r.candidate.text.clone(),
                rule: h.clone(),
            });
        }
    }
    // deny before ask so the stricter decision wins on the first match
    out.sort_by_key(|e| match e.rule.decision {
        Decision::Deny => 0,
        Decision::Ask => 1,
    });
    HookFile {
        version: 1,
        rules: out,
    }
}

#[derive(Debug, Serialize)]
pub struct Coverage {
    pub total: usize,
    pub by_class: BTreeMap<String, usize>,
    pub interpretive_fraction: f64,
    pub permission_rules: usize,
    pub hook_rules: usize,
    pub without_rationale: usize,
}

pub fn coverage(rules: &[CompiledRule], without_rationale: usize) -> Coverage {
    let mut by_class = BTreeMap::new();
    for r in rules {
        *by_class
            .entry(r.classification.class.as_str().to_string())
            .or_insert(0) += 1;
    }
    let interp = by_class.get("interpretive_only").copied().unwrap_or(0);
    Coverage {
        total: rules.len(),
        interpretive_fraction: if rules.is_empty() {
            0.0
        } else {
            interp as f64 / rules.len() as f64
        },
        by_class,
        permission_rules: permissions(rules).permissions.deny.len()
            + permissions(rules).permissions.ask.len(),
        hook_rules: hooks(rules).rules.len(),
        without_rationale,
    }
}

/// `report.md`: what was compiled, what it enforces, and what it cannot.
pub fn report(rules: &[CompiledRule], without_rationale: &[&CompiledRule]) -> String {
    let cov = coverage(rules, without_rationale.len());
    let mut s = String::new();
    s.push_str("# Muninn rule compilation report\n\n");
    s.push_str(&format!(
        "{} rule candidates found. {:.1}% are interpretive only (the published corpus figure is 95.6%; anything enforceable is a gain).\n\n",
        cov.total,
        cov.interpretive_fraction * 100.0
    ));
    s.push_str("| class | rules |\n|---|---|\n");
    for (k, v) in &cov.by_class {
        s.push_str(&format!("| {k} | {v} |\n"));
    }
    s.push_str(&format!(
        "\nEmitted: {} permission rule(s), {} PreToolUse rule(s).\n\n",
        cov.permission_rules, cov.hook_rules
    ));

    let frag = permissions(rules);
    if !frag.permissions.deny.is_empty() || !frag.permissions.ask.is_empty() {
        s.push_str("## Permission rules (Claude Code `settings.json`)\n\n");
        for d in &frag.permissions.deny {
            s.push_str(&format!("- deny `{d}`\n"));
        }
        for a in &frag.permissions.ask {
            s.push_str(&format!("- ask `{a}`\n"));
        }
        s.push('\n');
    }

    s.push_str("## Enforceable rules, by source\n\n");
    for r in rules
        .iter()
        .filter(|r| r.classification.class != Class::InterpretiveOnly)
    {
        let c = &r.classification;
        s.push_str(&format!(
            "- **{}:{}** `{}` → {}\n  - \"{}\"\n",
            r.candidate.source_file,
            r.candidate.source_line,
            c.pattern_id,
            c.class.as_str(),
            r.candidate.text
        ));
        for p in &c.permissions {
            s.push_str(&format!(
                "  - permission {:?} `{}`\n",
                p.decision,
                p.render()
            ));
        }
        for h in &c.hooks {
            s.push_str(&format!(
                "  - hook {:?} on `{}`{}{}\n",
                h.decision,
                h.tool_regex,
                h.command_regex
                    .as_ref()
                    .map(|x| format!(" command~`{x}`"))
                    .unwrap_or_default(),
                h.condition
                    .as_ref()
                    .map(|x| format!(" if `{x}`"))
                    .unwrap_or_default()
            ));
        }
        if let Some(n) = &c.note {
            s.push_str(&format!("  - note: {n}\n"));
        }
        if let Some(n) = &c.sandbox {
            s.push_str(&format!("  - sandbox: {n}\n"));
        }
    }

    s.push_str("\n## Interpretive only (the agent must read these; nothing enforces them)\n\n");
    for r in rules
        .iter()
        .filter(|r| r.classification.class == Class::InterpretiveOnly)
    {
        s.push_str(&format!(
            "- {}:{} \"{}\"\n",
            r.candidate.source_file, r.candidate.source_line, r.candidate.text
        ));
    }

    if !without_rationale.is_empty() {
        s.push_str("\n## Rules without a recorded rationale\n\nThese have no linked `decision` record. They cannot be safely deleted or relaxed until someone records why they exist.\n\n");
        for r in without_rationale {
            s.push_str(&format!(
                "- {}:{} \"{}\"\n",
                r.candidate.source_file, r.candidate.source_line, r.candidate.text
            ));
        }
    }

    s.push_str("\n## Limits\n\n");
    s.push_str("- Tool-level enforcement does not cover indirect paths (a script that runs the forbidden command, an alias, a CI job).\n");
    s.push_str("- Permission rules are prefix matches; the PreToolUse hook covers flags anywhere in the command, but not commands hidden in files.\n");
    s.push_str("- Codex does not support `ask` from PreToolUse hooks: `ask` rules are delivered as a reminder there, not enforced.\n");
    s.push_str("- Enforcing a rule does not make stale memory safe; that is F1's job.\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::classify;
    use crate::parse::Candidate;

    #[test]
    fn deny_wins_over_ask_in_fragment() {
        let mk = |t: &str| {
            let c = Candidate {
                source_file: "CLAUDE.md".into(),
                source_line: 1,
                source_hash: "h".into(),
                text: t.into(),
                heading: None,
            };
            CompiledRule {
                classification: classify(&c),
                candidate: c,
            }
        };
        let rules = vec![
            mk("Never `git push --force`"),
            mk("Never `git push` without an explicit instruction"),
        ];
        let f = permissions(&rules);
        assert!(f
            .permissions
            .deny
            .contains(&"Bash(git push --force:*)".to_string()));
        assert!(f.permissions.ask.contains(&"Bash(git push:*)".to_string()));
        let r = report(&rules, &[]);
        assert!(r.contains("git.force_push"));
    }
}
