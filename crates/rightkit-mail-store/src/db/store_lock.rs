//! Store-wide advisory lock on the database, from rightkit-db (`StoreLock`).
//!
//! A process that keeps connections to `<db>` open holds the shared guard for as long as they
//! live. The lock file is `<db>.store-lock` beside the database (`rightkit_db::store_lock_path`).
use rightkit_db::StoreLock;
use rightkit_fs::LockOptions;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// How long to wait for a busy store before giving up: long enough to ride out another
/// process finishing its open or close, short enough that start-up never looks hung.
const WAIT: Duration = Duration::from_secs(10);

static HELD: Mutex<Option<(PathBuf, StoreLock)>> = Mutex::new(None);

fn options() -> LockOptions {
    LockOptions::wait(WAIT).labeled(crate::host::host().app_slug())
}

/// Take the shared guard for `db_path` and keep it until [`release`]. Returns true when this
/// call took it, false when this process already holds it for the same path.
pub(crate) fn hold_shared(db_path: &Path) -> Result<bool, String> {
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    if held.as_ref().is_some_and(|(path, _)| path == db_path) {
        return Ok(false);
    }
    let guard = StoreLock::shared(db_path, &options())
        .map_err(|e| format!("database store is busy (another process is replacing it): {e}"))?;
    // A guard for another path (tests, a moved folder) is dropped here.
    *held = Some((db_path.to_path_buf(), guard));
    Ok(true)
}

/// Drop the shared guard. Call after the writer and every pooled reader are closed.
pub(crate) fn release() {
    *HELD.lock().unwrap_or_else(|e| e.into_inner()) = None;
}
