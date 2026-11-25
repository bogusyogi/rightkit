//! Where a `DeliveryHistoryStore` gets its connection from.
//!
//! Every store operation borrows exactly one live connection for the length of one closure. A raw
//! `rusqlite::Connection` is that connection itself (the host opened it and keeps it, as before).
//! A `ManagedConnection` owns an `rightkit_db::encrypted_lifecycle::EncryptedStore` and borrows
//! its managed writer (`EncryptedStore::with_writer`), so a key rotation, a restore or a recovery
//! that the same store runs drains and reopens the very connection the history uses. No connection
//! escapes the closure, so none can outlive a drain.

use super::EventSink;
use crate::history::HistoryError;
use rightkit_db::rusqlite::Connection;

/// The access seam of [`super::DeliveryHistoryStore`]. Implemented for a raw connection and, with
/// the `managed-store` feature, for `ManagedConnection`. It is not meant to be implemented by a
/// host.
pub trait HistoryConnection: Send {
    /// Run `operation` on the live connection for reading.
    fn read<R>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<R, HistoryError>,
    ) -> Result<R, HistoryError>;

    /// Run `operation` on the live connection for writing.
    fn write<R>(
        &mut self,
        operation: impl FnOnce(&mut Connection) -> Result<R, HistoryError>,
    ) -> Result<R, HistoryError>;

    /// Re-encrypt the store under a freshly minted key, reporting through `events`. Only a
    /// connection that owns its key lifecycle overrides this; the default refuses, as the
    /// `HistoryStore` default does.
    fn rotate_encryption_key(&self, _events: EventSink) -> Result<(), HistoryError> {
        Err(HistoryError::new("this history store is not encrypted"))
    }
}

impl HistoryConnection for Connection {
    fn read<R>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<R, HistoryError>,
    ) -> Result<R, HistoryError> {
        operation(self)
    }

    fn write<R>(
        &mut self,
        operation: impl FnOnce(&mut Connection) -> Result<R, HistoryError>,
    ) -> Result<R, HistoryError> {
        operation(self)
    }
}

/// What the host supplies so `HistoryStore::rotate_encryption_key` can rotate a managed store: how
/// a new key is minted and the telemetry names the rotation reports under. The event sink is the
/// one the store was opened with.
#[cfg(feature = "managed-store")]
pub struct KeyRotation {
    /// Mint the next key. An error is returned before the store is touched (no recovery pass, no
    /// rotation).
    pub mint_key: Box<dyn Fn() -> Result<String, String> + Send + Sync>,
    /// Event name for a recovery pass; the detail is the owner's report.
    pub recovery_event: &'static str,
    /// Event name for a rotation that committed but left cleanup for the next open.
    pub cleanup_pending_event: &'static str,
}

/// An open encrypted store whose managed writer serves the history. The lifecycle owner stays
/// reachable (`DeliveryHistoryStore::store`) for backup, restore, recovery and rotation.
///
/// While a transition (rotation, restore, recovery) is draining the writer, an operation returns
/// a `HistoryError` naming the busy store instead of waiting; the next call after the transition
/// settles succeeds. A store left blocked by an unresolved transition answers every call with the
/// recovery-required error until `EncryptedStore::recover_rotation` settles it.
#[cfg(feature = "managed-store")]
pub struct ManagedConnection(
    pub(super) rightkit_db::encrypted_lifecycle::EncryptedStore,
    pub(super) Option<KeyRotation>,
);

#[cfg(feature = "managed-store")]
impl std::fmt::Debug for ManagedConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedConnection")
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "managed-store")]
impl HistoryConnection for ManagedConnection {
    fn read<R>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<R, HistoryError>,
    ) -> Result<R, HistoryError> {
        self.0
            .with_writer(|conn| operation(conn))
            .map_err(|error| HistoryError::new(format!("history database: {error}")))?
    }

    fn write<R>(
        &mut self,
        operation: impl FnOnce(&mut Connection) -> Result<R, HistoryError>,
    ) -> Result<R, HistoryError> {
        self.0
            .with_writer(operation)
            .map_err(|error| HistoryError::new(format!("history database: {error}")))?
    }

    /// With a [`KeyRotation`]: mint a key, settle any leftover rotation state, rotate through the
    /// owner and report (`rightkit_db::encrypted_lifecycle::support::rotate_with_new_key`). `Ok`
    /// only when the new key is the stored primary key and opens the file; a rotation that is not
    /// settled returns an error that begins `key rotation pending`. Without one: the default
    /// refusal.
    fn rotate_encryption_key(&self, events: EventSink) -> Result<(), HistoryError> {
        let Some(rotation) = &self.1 else {
            return Err(HistoryError::new("this history store is not encrypted"));
        };
        rightkit_db::encrypted_lifecycle::support::rotate_with_new_key(
            &self.0,
            &*rotation.mint_key,
            |new_key| {
                let request = rightkit_db::encrypted_lifecycle::RotationRequest::new(new_key);
                self.0.rotate_key(request)
            },
            rightkit_db::encrypted_lifecycle::support::RotationEvents {
                recovery: rotation.recovery_event,
                cleanup_pending: rotation.cleanup_pending_event,
            },
            events,
        )
        .map_err(HistoryError::new)
    }
}
