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
        "database is from a newer {} (schema v{current}, this build knows up to v{max}) — update {} to open it",
        newer_noun(),
        update_target()
    )]
    NewerDatabase { current: i64, max: i64 },
}

static PRODUCT_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Sets the product name the newer-database message uses (for example `MyApp`: "database is
/// from a newer MyApp (...) — update MyApp to open it"). Without it the message says "a newer
/// version" and "update the app". Call once at start-up; returns `false` when one was already
/// set.
pub fn set_product_name(name: &str) -> bool {
    PRODUCT_NAME.set(name.to_string()).is_ok()
}

fn newer_noun() -> &'static str {
    PRODUCT_NAME.get().map(String::as_str).unwrap_or("version")
}

fn update_target() -> &'static str {
    PRODUCT_NAME.get().map(String::as_str).unwrap_or("the app")
}

/// Convenience result alias for storage operations.
pub type Result<T> = std::result::Result<T, StorageError>;

impl From<serde_json::Error> for StorageError {
    fn from(e: serde_json::Error) -> Self {
        StorageError::Serde(e.to_string())
    }
}
