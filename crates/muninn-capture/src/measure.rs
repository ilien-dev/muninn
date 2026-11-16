//! Measurement only. usage-v4 reads each extraction rule alone, the filters together without
//! the rules that add records, and each of those rules on its own (ninth amendment). That
//! needs arms of one build in which named rules are off. Only the `extract_dump` example calls
//! [`disable`]; the product never does, so every rule is always on in it.
//!
//! Names: `agent_prompt`, `harness_origin`, `question_text`, `correction_guard`,
//! `pastes` (filters and origin fixes); `answer_pick`, `wish`,
//! `correction_no_opener` (rules that add records).

use std::sync::OnceLock;

static OFF: OnceLock<Vec<String>> = OnceLock::new();

/// The rules that add records, as `MUNINN_DUMP_FILTERS_ONLY` turns them off.
pub const RECALL_RULES: &[&str] = &["answer_pick", "wish", "correction_no_opener"];

/// Turn these rules off for the life of the process. The first call wins.
pub fn disable(names: &[&str]) {
    let _ = OFF.set(names.iter().map(|s| s.to_string()).collect());
}

/// Is this rule on? Always, unless a measurement turned it off.
pub fn on(name: &str) -> bool {
    OFF.get().is_none_or(|v| !v.iter().any(|n| n == name))
}
