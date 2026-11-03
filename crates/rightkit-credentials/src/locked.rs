//! Mutations of one credential target, each under that target's lock.
//!
//! The Windows Credential Manager has no atomic "write if absent" and no "delete if equal", so a
//! backend that wants either must exclude every other writer of the same native target while it
//! checks and acts. Two rules make that sound, and this module is where both live (it compiles on
//! every platform so they are tested everywhere; only the Windows backend uses it):
//!
//! * **Every mutator takes the lock.** If only `add_if_absent` locked, a concurrent `set` between
//!   its check and its write would be overwritten and `add_if_absent` would still report that it
//!   created the item.
//! * **The lock is named by the native target.** Two namespaces that share a Windows prefix write
//!   the same Credential Manager entry, so they must take the same lock; it is keyed on the
//!   normalised target name ([`Namespace::windows_lock_target`]), not on the service.
//!
//! The functions here hold the lock for the whole operation and call the *unlocked* primitives of
//! [`CredentialOps`], so nothing re-takes a lock it already holds.

use crate::backend::ConditionalDelete;
use crate::error::Result;
use crate::namespace::Namespace;
use crate::secret::Secret;

/// The unlocked primitives of one credential store, plus its lock.
pub(crate) trait CredentialOps {
    /// Held for the duration of one mutation; released on drop.
    type Guard;
    /// Block until this process holds the lock for the normalised native target name, excluding
    /// other threads and other processes. `ns` and `account` are the pair the target was built
    /// from: a store whose earlier versions named their lock by that pair takes that lock too.
    fn lock(&self, normalized_target: &str, ns: &Namespace, account: &str) -> Result<Self::Guard>;
    /// Read without locking. `want_blob == false` is a presence check.
    fn read(&self, ns: &Namespace, account: &str, want_blob: bool) -> Result<Option<Secret>>;
    /// Create or replace without locking.
    fn write(&self, ns: &Namespace, account: &str, secret: &Secret) -> Result<()>;
    /// Remove without locking; `Ok(false)` when there was nothing.
    fn remove(&self, ns: &Namespace, account: &str) -> Result<bool>;
}

fn lock_target<O: CredentialOps>(ops: &O, ns: &Namespace, account: &str) -> Result<O::Guard> {
    // The validated, normalised name (the same one the store's exclusion identity is built
    // from): a prefix with a NUL would otherwise lock (and write) a truncated target.
    let target = ns.windows_lock_target(account)?;
    ops.lock(&target, ns, account)
}

pub(crate) fn set<O: CredentialOps>(
    ops: &O,
    ns: &Namespace,
    account: &str,
    secret: &Secret,
) -> Result<()> {
    let _held = lock_target(ops, ns, account)?;
    ops.write(ns, account, secret)
}

pub(crate) fn add_if_absent<O: CredentialOps>(
    ops: &O,
    ns: &Namespace,
    account: &str,
    secret: &Secret,
) -> Result<bool> {
    let _held = lock_target(ops, ns, account)?;
    if ops.read(ns, account, false)?.is_some() {
        return Ok(false);
    }
    ops.write(ns, account, secret)?;
    Ok(true)
}

pub(crate) fn delete<O: CredentialOps>(ops: &O, ns: &Namespace, account: &str) -> Result<bool> {
    let _held = lock_target(ops, ns, account)?;
    ops.remove(ns, account)
}

pub(crate) fn delete_if_equals<O: CredentialOps>(
    ops: &O,
    ns: &Namespace,
    account: &str,
    expected: &Secret,
) -> Result<ConditionalDelete> {
    let _held = lock_target(ops, ns, account)?;
    match ops.read(ns, account, true)? {
        None => Ok(ConditionalDelete::Absent),
        Some(current) if current.ct_eq(expected) => {
            ops.remove(ns, account)?;
            Ok(ConditionalDelete::Deleted)
        }
        Some(_) => Ok(ConditionalDelete::Changed),
    }
}
