//! Pending-rotation recovery for a credential that other state depends on (for example a
//! database key: the database is re-keyed with the new value, then the credential is updated).
//! A crash between the two steps must leave the new value recoverable.
//!
//! The marker is a second item in the same namespace named `<account>.rotation-pending` that
//! holds the NEW value. Sequence: [`Store::begin_rotation`] (marker written create-only), the
//! caller applies the new value to whatever depends on it, then [`Store::commit_rotation`]
//! (primary set, marker removed). After a crash the caller finds the marker with
//! [`Store::pending_rotation`] and either commits or aborts. `commit_rotation` is idempotent:
//! if it dies after the primary write, running it again sets the same value and clears the marker.
//!
//! `begin`, `commit` and `abort` all run under one whole-rotation lock for the account in its
//! bound store. Each invocation pins one store before taking its lock, then uses that store for
//! every read and mutation. Retain [`Store::pinned`] across begin, external rekeying, and commit
//! or abort to keep separate invocations on that same store. Ordinary handles resolve the default
//! again; after a restart reselect the original default keychain before committing recovery
//! (production explicit-file macOS backends are read/delete-only). Commit holds its lock
//! across pending read → primary write →
//! marker remove, so an abort/rebegin cannot land in between: without it, a stale commit would
//! overwrite a newer primary and its marker delete would remove the newer marker (a conditional
//! marker delete alone cannot prevent the stale primary write). The lock only serialises this
//! process; against other processes the create-only marker write and, where the backend supports
//! it, the conditional marker delete in `commit_rotation` are the remaining guards.

use crate::{ConditionalDelete, Error, Result, Secret, Store};

/// Suffix of the marker item's account name.
pub const ROTATION_PENDING_SUFFIX: &str = ".rotation-pending";

fn marker(account: &str) -> String {
    format!("{account}{ROTATION_PENDING_SUFFIX}")
}

impl Store {
    /// Record `new` as the pending value for `account`. `Ok(false)` when a rotation is already
    /// pending (nothing was changed; resolve it first). An empty value is rejected.
    pub fn begin_rotation(&self, account: &str, new: &Secret) -> Result<bool> {
        if account.ends_with(ROTATION_PENDING_SUFFIX) {
            return Err(Error::Invalid {
                what: "account",
                reason: "names a rotation marker".into(),
            });
        }
        let (store, lock) = self.rotation_store(account)?;
        let _held = lock
            .lock()
            .map_err(|_| Error::Unavailable("rotation lock poisoned".into()))?;
        store.rotation_ready(account)?;
        store.add_if_absent(&marker(account), new)
    }

    /// The pending new value, if a rotation was begun and not resolved.
    pub fn pending_rotation(&self, account: &str) -> Result<Option<Secret>> {
        Ok(self.get(&marker(account))?.filter(|s| !s.is_empty()))
    }

    /// Make the pending value the credential, then remove the marker. `Ok(false)` when nothing
    /// was pending. The marker is removed only after the primary write succeeded. The whole
    /// read → write → remove sequence runs under the account's rotation lock, shared with
    /// [`Store::begin_rotation`] and [`Store::abort_rotation`].
    pub fn commit_rotation(&self, account: &str) -> Result<bool> {
        let (store, lock) = self.rotation_store(account)?;
        let _held = lock
            .lock()
            .map_err(|_| Error::Unavailable("rotation lock poisoned".into()))?;
        store.rotation_ready(account)?;
        let Some(new) = store.pending_rotation(account)? else {
            return Ok(false);
        };
        store.set(account, &new)?;
        // Remove only the marker this commit read. `Changed` means another process began a
        // newer rotation after our primary write: its marker stays for its own commit.
        // Backends without an atomic conditional delete report `Unsupported`; the plain
        // delete is then still serialised against this process by the rotation lock.
        match store.delete_if_equals(&marker(account), &new)? {
            ConditionalDelete::Deleted | ConditionalDelete::Absent | ConditionalDelete::Changed => {
            }
            ConditionalDelete::Unsupported => {
                store.delete(&marker(account))?;
            }
        }
        Ok(true)
    }

    /// Discard the pending value and leave the credential unchanged. `Ok(true)` if a marker was removed.
    pub fn abort_rotation(&self, account: &str) -> Result<bool> {
        let (store, lock) = self.rotation_store(account)?;
        let _held = lock
            .lock()
            .map_err(|_| Error::Unavailable("rotation lock poisoned".into()))?;
        store.rotation_ready(account)?;
        store.delete(&marker(account))
    }
}
