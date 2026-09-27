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

    /// Another connection holds the lock: the write path is at work, nothing is broken.
    pub fn is_busy(&self) -> bool {
        match self {
            Error::Busy { .. } => true,
            Error::Sqlite(rusqlite::Error::SqliteFailure(e, _)) => matches!(
                e.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ),
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    #[test]
    fn a_held_lock_is_busy_and_damage_is_not() {
        let locked = super::Error::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        ));
        assert!(locked.is_busy());
        assert!(super::Error::Busy { attempts: 6 }.is_busy());
        assert!(!super::Error::Integrity("page 3".into()).is_busy());
    }
}
