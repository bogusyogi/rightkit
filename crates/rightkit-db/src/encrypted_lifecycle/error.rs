//! Classified failures of the encrypted-store lifecycle.

use super::keystore::{KeySource, KeyStoreError};
use crate::{DbError, UnreadableCause};
use std::path::PathBuf;

/// Why an encrypted-store operation failed. Every variant says what was and was not touched;
/// none of them means "an empty database was created instead".
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LifecycleError {
    /// A cross-process or in-process conflict stopped the operation before any file or key was
    /// changed (the guard could not be taken, a reader or writer would not drain, the host would
    /// not stop, or another transition is running).
    #[error("store is busy, nothing was changed: {reason}")]
    Busy { reason: String },
    /// The database file is not there. No empty database was created (see
    /// `EncryptedStoreConfig::create_if_missing`).
    #[error("encrypted store {} does not exist; no empty database was created", .path.display())]
    StoreMissing { path: PathBuf },
    /// The file is a plaintext SQLite database. It was not touched and no key was applied.
    #[error("{} is a plaintext SQLite database, not an encrypted one; it was not touched", .path.display())]
    NotEncrypted { path: PathBuf },
    /// The file exists but no key is stored anywhere (primary, pending, legacy staging, recovery).
    #[error(
        "no database key is stored (primary, pending, legacy staging and recovery are all empty)"
    )]
    KeyMissing,
    /// Protected key storage refused or could not be reached (denied, unavailable, corrupt). There
    /// is never a plaintext fallback, and nothing was changed.
    #[error("protected key storage ({slot}) failed: {source}")]
    KeyStore {
        slot: String,
        #[source]
        source: KeyStoreError,
    },
    /// Keys are stored and none opens the file. SQLCipher cannot tell a wrong key from a damaged
    /// file, so the two are not distinguished here. The file was not touched, quarantined or
    /// replaced.
    #[error("no stored key opens {} (tried {tried:?}); the key is wrong or the file is damaged, and the file was not touched", .path.display())]
    NoKeyOpens {
        path: PathBuf,
        tried: Vec<KeySource>,
    },
    /// The file could not be opened for a reason that says nothing about the key (permission,
    /// I/O, lock, device not ready). Nothing was changed.
    #[error("database {} could not be opened: {source}", .path.display())]
    DatabaseUnavailable {
        path: PathBuf,
        cause: Option<UnreadableCause>,
        #[source]
        source: Box<DbError>,
    },
    /// An interrupted or unresolved transition (journal, indeterminate rotation) blocks ordinary
    /// use until `recover_rotation` resolves it. Recovery material is retained.
    #[error("an interrupted or unresolved transition must be recovered before use: {detail}")]
    RecoveryRequired { detail: String },
    /// A replacement or destination holds, or would receive, a key other than the store's key.
    #[error("wrong destination key for {}: {detail}", .path.display())]
    WrongDestinationKey { path: PathBuf, detail: String },
    /// A destination file already exists; nothing was overwritten.
    #[error("{} already exists; refusing to overwrite it", .path.display())]
    DestinationExists { path: PathBuf },
    /// Protected storage holds a different primary key than the one that opens the database.
    #[error("protected storage does not match the key that opens the database: {detail}")]
    KeyStoreMismatch { detail: String },
    #[error("invalid encrypted-store configuration: {0}")]
    InvalidConfig(String),
    /// The requested operation is not allowed in the store's current state.
    #[error("{0}")]
    Precondition(String),
    /// The caller's validation callback rejected the result.
    #[error("validation failed: {0}")]
    ValidationFailed(String),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Fs(#[from] rightkit_fs::FsError),
}

impl LifecycleError {
    pub(crate) fn busy(reason: impl Into<String>) -> Self {
        Self::Busy {
            reason: reason.into(),
        }
    }

    pub(crate) fn key_store(slot: impl Into<String>, source: KeyStoreError) -> Self {
        Self::KeyStore {
            slot: slot.into(),
            source,
        }
    }

    /// True for the conflicts that mean "nothing was changed, try again later".
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Busy { .. })
    }
}
