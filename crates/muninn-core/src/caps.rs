//! Hard caps and budgets. Every number here has a measured or published reason;
//! see design/ENGINE.md §3–§4 and the evidence ids next to each.

/// Active `invariant` records per project [K4].
pub const MAX_INVARIANTS: i64 = 60;
/// Active records of any kind [O5].
pub const MAX_ACTIVE_RECORDS: i64 = 20_000;
/// Characters of `record.body`; the rest stays in `transcript_ref`.
pub const MAX_BODY_CHARS: usize = 2_000;
/// Delivered tokens per turn, hard [P6].
pub const BUDGET_TURN_TOKENS: usize = 700;
/// Delivered tokens per block.
pub const BUDGET_BLOCK_TOKENS: usize = 200;
/// `index.md` limits (the native memory's own numbers, kept for compatibility).
pub const INDEX_MAX_LINES: usize = 200;
pub const INDEX_MAX_BYTES: usize = 25_000;
/// Boot block inserted in CLAUDE.md / AGENTS.md.
pub const BOOT_BLOCK_MAX_TOKENS: usize = 1_000;
pub const BOOT_BLOCK_MAX_CHARS: usize = 3_500;
/// Bytes of heartbeat log the health gate inspects.
pub const HEARTBEAT_TAIL_BYTES: u64 = 256 * 1024;
