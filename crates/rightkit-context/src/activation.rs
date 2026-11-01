//! The activation lock.
//!
//! The host app's installer and updater own install, update and removal of this binary,
//! its plugins and its hooks. What stays here is the lock that serializes
//! anything mutating the state root.

use std::{path::Path, time::Duration};

const LOCK_WAIT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Held for as long as the caller mutates the state root. The kernel-backed guard is released by
/// the OS on drop or crash, so a killed activation never leaves the state root locked.
pub struct ActivationLock {
    _guard: rightkit_fs::lock::FileLock,
}

/// Take the activation lock under `state_root`, waiting up to 15 s.
pub fn acquire_lock(state_root: &Path) -> Result<ActivationLock, String> {
    acquire_lock_with_wait(state_root, LOCK_WAIT)
}

fn acquire_lock_with_wait(root: &Path, wait: Duration) -> Result<ActivationLock, String> {
    let guard_path = root.join(".activation.guard");
    let guard_options = rightkit_fs::lock::LockOptions {
        timeout: wait,
        poll: POLL_INTERVAL,
        label: "activation".to_string(),
    };
    let guard =
        rightkit_fs::lock::FileLock::acquire(&guard_path, &guard_options).map_err(|error| {
            match error {
                rightkit_fs::FsError::Locked { .. } | rightkit_fs::FsError::LockTimeout { .. } => {
                    "activation startup lock remained busy".to_string()
                }
                other => format!("open activation guard: {other}"),
            }
        })?;
    Ok(ActivationLock { _guard: guard })
}
