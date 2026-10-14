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

use crate::{Error, Result, Secret, Store};

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
        self.add_if_absent(&marker(account), new)
    }

    /// The pending new value, if a rotation was begun and not resolved.
    pub fn pending_rotation(&self, account: &str) -> Result<Option<Secret>> {
        Ok(self.get(&marker(account))?.filter(|s| !s.is_empty()))
    }

    /// Make the pending value the credential, then remove the marker. `Ok(false)` when nothing
    /// was pending. The marker is removed only after the primary write succeeded.
    pub fn commit_rotation(&self, account: &str) -> Result<bool> {
        let Some(new) = self.pending_rotation(account)? else {
            return Ok(false);
        };
        self.set(account, &new)?;
        self.delete(&marker(account))?;
        Ok(true)
    }

    /// Discard the pending value and leave the credential unchanged. `Ok(true)` if a marker was removed.
    pub fn abort_rotation(&self, account: &str) -> Result<bool> {
        self.delete(&marker(account))
    }
}
