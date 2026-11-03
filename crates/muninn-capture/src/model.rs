//! Harness-neutral view of a session transcript.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolCall {
    pub name: String,
    /// Bash: the command. Edit/Write/Read: the path. Others: a short JSON summary.
    pub target: String,
    pub is_error: bool,
    /// Exit code when the tool reported one (Bash).
    pub exit_code: Option<i64>,
    /// First bytes of the result, sanitised.
    pub result_head: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Turn {
    pub index: usize,
    pub timestamp: Option<String>,
    pub user_prompt: String,
    pub assistant_text: String,
    pub tools: Vec<ToolCall>,
    /// Files touched by Edit/Write/MultiEdit in this turn.
    pub files_touched: Vec<String>,
    /// Byte offset just past the last line of this turn (for watermarks).
    pub end_offset: u64,
    /// Byte offset of the line that opened this turn. Unlike `index`, which restarts at 0 on
    /// every parse from a watermark, this names one turn for the life of the transcript.
    pub start_offset: u64,
    /// The harness says a program sent this, not a person (`entrypoint: sdk-*`).
    pub automated: bool,
    /// An answer picked in AskUserQuestion: the person's choice, in a wrapper of question text
    /// that the person did not write.
    pub answered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Session {
    pub session_id: String,
    pub harness: String,
    pub cwd: Option<String>,
    pub turns: Vec<Turn>,
    /// Byte offset the parser stopped at (== file length when fully read).
    pub end_offset: u64,
}
