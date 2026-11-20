//! Where a `DeliveryHistoryStore` gets its connection from.
//!
//! Every store operation borrows exactly one live connection for the length of one closure. A raw
//! `rusqlite::Connection` is that connection itself (the host opened it and keeps it, as before).
//! A `ManagedConnection` owns an `rightkit_db::encrypted_lifecycle::EncryptedStore` and borrows
//! its managed writer (`EncryptedStore::with_writer`), so a key rotation, a restore or a recovery
//! that the same store runs drains and reopens the very connection the history uses. No connection
//! escapes the closure, so none can outlive a drain.

use crate::history::HistoryError;
use rightkit_db::rusqlite::Connection;

/// The access seam of [`super::DeliveryHistoryStore`]. Implemented for a raw connection and, with
/// the `managed-store` feature, for [`ManagedConnection`]. It is not meant to be implemented by a
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

/// An open encrypted store whose managed writer serves the history. The lifecycle owner stays
/// reachable (`DeliveryHistoryStore::store`) for backup, restore, recovery and rotation.
///
/// While a transition (rotation, restore, recovery) is draining the writer, an operation returns
/// a `HistoryError` naming the busy store instead of waiting; the next call after the transition
/// settles succeeds. A store left blocked by an unresolved transition answers every call with the
/// recovery-required error until `EncryptedStore::recover_rotation` settles it.
#[cfg(feature = "managed-store")]
pub struct ManagedConnection(pub(super) rightkit_db::encrypted_lifecycle::EncryptedStore);

#[cfg(feature = "managed-store")]
impl std::fmt::Debug for ManagedConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ManagedConnection").finish_non_exhaustive()
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
}
