use rightkit_db::rusqlite;
use thiserror::Error;

/// Errors that can arise from storage operations.
#[derive(Debug, Error)]
pub enum StorageError {
    /// An underlying SQLite error.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A (de)serialization error while encoding/decoding a JSON column.
    #[error("serde error: {0}")]
    Serde(String),

    #[error("stale compaction snapshot")]
    StaleSnapshot,

    /// Shared SQLite bootstrap or backup operation failed.
    #[error("RightKit database error: {0}")]
    RightKitDb(#[from] rightkit_db::DbError),

    /// Shared owner-only filesystem operation failed.
    #[error("RightKit filesystem error: {0}")]
    RightKitFs(#[from] rightkit_fs::FsError),

    /// A requested row was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// The database's `user_version` is higher than any schema version this
    /// binary knows how to apply — it was created (or migrated) by a newer
    /// the host app. Refuse to open rather than risk running stale code against
    /// an unfamiliar schema.
    #[error(
        "database is from a newer version (schema v{current}, this build knows up to v{max}) — update the app to open it"
    )]
    NewerDatabase { current: i64, max: i64 },
}

/// Convenience result alias for storage operations.
pub type Result<T> = std::result::Result<T, StorageError>;

impl From<serde_json::Error> for StorageError {
    fn from(e: serde_json::Error) -> Self {
        StorageError::Serde(e.to_string())
    }
}
