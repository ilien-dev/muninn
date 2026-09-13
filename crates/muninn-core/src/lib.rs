//! Muninn core: a local, deterministic memory engine for coding agents.
//!
//! No server, no model and no LLM in the read path. See design/ENGINE.md.

pub mod caps;
pub mod db;
pub mod error;
pub mod filter;
pub mod health;
pub mod heartbeat;
pub mod paths;
pub mod project;
pub mod recall;
pub mod sanitize;
pub mod tokens;

pub use db::{Db, Mode};
pub use error::{Error, Result};
pub use paths::ProjectPaths;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
