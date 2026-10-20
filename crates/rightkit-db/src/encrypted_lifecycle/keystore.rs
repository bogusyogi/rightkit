//! The protected key storage contract that rightkit-db defines.
//!
//! rightkit-db owns the encrypted-store workflow; it never depends on rightkit-credentials.
//! rightkit-credentials implements [`ProtectedKeyStore`] in an adapter behind its own feature and
//! maps the methods to its rotation API (`begin_rotation`, `pending_rotation`,
//! `commit_rotation_of`, `abort_rotation_of`) and its recovery and legacy accounts.
//!
//! The workflow never assumes a method is atomic. After a settled write it reads state back,
//! including after an `Err`. `Unsettled` means the mutation can still run, so transient readback
//! cannot authorize rollback or destruction of retained keys.

use std::fmt;
use std::time::Duration;
use zeroize::Zeroizing;

/// A SQLCipher passphrase key (the same charset `DbOptions::key` accepts).
pub type Key = Zeroizing<String>;

/// Why protected key storage failed. None of these may fall back to plaintext storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyStoreErrorKind {
    /// The user or the OS refused access to the secret.
    Denied,
    /// The secret service is not reachable or not ready.
    Unavailable,
    /// The stored item is malformed.
    Corrupt,
    /// A mutation exceeded its bounded wait and may still change storage. Reads do not prove
    /// settlement; retain all viable keys and journal state until a later exclusive session.
    Unsettled,
    Other,
}

/// A failure of protected key storage. The message never contains key material.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{kind:?}: {message}")]
pub struct KeyStoreError {
    pub kind: KeyStoreErrorKind,
    pub message: String,
}

impl KeyStoreError {
    pub fn new(kind: KeyStoreErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn denied(message: impl Into<String>) -> Self {
        Self::new(KeyStoreErrorKind::Denied, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(KeyStoreErrorKind::Unavailable, message)
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::new(KeyStoreErrorKind::Other, message)
    }
}

/// An opaque token naming one exact staged (pending) key, so promotion and discard act on that
/// revision and never on a key staged later by someone else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRevision(pub String);

/// The staged key and its revision, as read back from storage.
#[derive(Clone)]
pub struct PendingKey {
    pub key: Key,
    pub revision: PendingRevision,
}

impl fmt::Debug for PendingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingKey")
            .field("key", &"<redacted>")
            .field("revision", &self.revision)
            .finish()
    }
}

/// Where a key that opened the database came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeySource {
    /// The canonical primary key.
    Primary,
    /// The staged key of a rotation (`<account>.rotation-pending` in credentials).
    Pending,
    /// An explicitly mapped legacy staging account such as HeardRight's `SQLCipher/DBKey.staging`.
    LegacyStaging(String),
    /// The recovery slot that retains the previous key while a rotation is unproven.
    Recovery,
}

/// Protected storage for one database's keys.
///
/// Contract: each `read_*` returns `Ok(None)` for an empty slot and `Err` only when the slot could
/// not be read (denied, unavailable, corrupt); an unreadable slot is never reported as empty.
/// Writes may fail after partially taking effect, so the workflow always reads back.
pub trait ProtectedKeyStore: Send + Sync {
    /// Acquire an owned, exclusive key session after the transition guard and before DB access.
    /// All slot access must use this capability until readback and journal settlement finish.
    fn lock_rotation(
        &self,
        timeout: Duration,
    ) -> Result<Box<dyn ProtectedKeySession + Send + 'static>, KeyStoreError>;
}

/// Exclusive access to one database's protected slots. Moving this owned capability to another
/// thread preserves its authority. It cannot be cloned. A mutation returning `Unsettled` must
/// keep subsequent session acquisition and slot access excluded until its worker has settled,
/// including after this session drops; transient reads cannot prove settlement.
pub trait ProtectedKeySession: Send {
    /// The canonical key that opens the database.
    fn read_primary(&mut self) -> Result<Option<Key>, KeyStoreError>;

    /// The staged key of an unfinished rotation, with its revision.
    fn read_pending(&mut self) -> Result<Option<PendingKey>, KeyStoreError>;

    /// Stage `key` as pending without touching the primary key; returns its revision.
    fn write_pending(&mut self, key: &Key) -> Result<PendingRevision, KeyStoreError>;

    /// Make the exact pending `revision` the primary key. The credentials adapter writes the
    /// primary key first and cleans its pending marker after, so an `Err` does not mean the
    /// primary key is unchanged; the caller reads the primary slot back.
    fn promote_pending(&mut self, revision: &PendingRevision) -> Result<(), KeyStoreError>;

    /// Remove the pending key of that exact revision. It is never the primary key.
    fn discard_pending(&mut self, revision: &PendingRevision) -> Result<(), KeyStoreError>;

    /// Read a mapped legacy staging account (`alias` is the account name the app configured).
    fn read_legacy_staging(&mut self, alias: &str) -> Result<Option<Key>, KeyStoreError>;

    /// Delete the legacy staging item. Called only after canonical storage and a database reopen
    /// readback prove recovery stays possible without it.
    fn delete_legacy_staging(&mut self, alias: &str) -> Result<(), KeyStoreError>;

    /// The recovery slot: the previous key, kept durably while a rotation is unproven.
    fn read_recovery(&mut self) -> Result<Option<Key>, KeyStoreError>;

    fn write_recovery(&mut self, key: &Key) -> Result<(), KeyStoreError>;

    fn clear_recovery(&mut self) -> Result<(), KeyStoreError>;

}
