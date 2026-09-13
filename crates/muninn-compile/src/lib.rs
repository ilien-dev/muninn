//! F2: rules in CLAUDE.md / AGENTS.md / .claude/rules → controls the harness
//! enforces (permission rules, PreToolUse deny/ask hooks), plus a coverage
//! report. Zero tokens injected. Deterministic; every classification names the
//! pattern that produced it.

pub mod classify;
pub mod emit;
pub mod parse;

pub use classify::{Class, Classification, Decision, HookRule, PermissionRule};
pub use parse::Candidate;

#[derive(Debug, Clone, serde::Serialize)]
pub struct CompiledRule {
    pub candidate: Candidate,
    pub classification: Classification,
}

/// Parse and classify every rule in the project.
pub fn compile_project(root: &std::path::Path) -> Vec<CompiledRule> {
    parse::extract_project(root)
        .into_iter()
        .map(|candidate| {
            let classification = classify::classify(&candidate);
            CompiledRule {
                candidate,
                classification,
            }
        })
        .collect()
}
