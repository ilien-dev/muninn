//! Error type shared by every crate in the workspace.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("database is busy after {attempts} attempts")]
    Busy { attempts: u32 },
    #[error("database integrity check failed: {0}")]
    Integrity(String),
    #[error("schema version {found} is newer than this binary supports ({supported})")]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
