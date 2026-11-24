//! The deadman wipe: a device that has not reached the owner's sync for a set number of days erases its synced copy and destroys
//! its keys itself. It narrows the limit stated in [`crate::revoke`] (a device that is switched off or never reconnects keeps its
//! readable copy) for a device that is running but cut off. It changes behaviour for a laptop left closed, so a host offers it as an
//! owner's switch and picks the period.
//!
//! * [`Deadman::record_heard`] is called after a pass that reached the store. [`Deadman::due`] is true when the last such pass was
//!   more than the period ago. The clock starts the first time it is asked, so a device is never wiped for a time before it began
//!   to sync.
//! * [`Deadman::self_wipe`] writes the intent to a marker file **outside** the folder the wipe erases, so a crash is finished by
//!   [`Deadman::resume`] at the next start, then erases every [`LocalCopy`], destroys the keys, and clears the marker. Data goes
//!   before keys so a failure leaves the keys for the retry; every step is safe to repeat.
//!
//! Nothing here names or logs content. The host registers every copy, exactly as for [`crate::RevokeWipe`].

use crate::manager::KeyManager;
use crate::revoke::LocalCopy;
use std::path::{Path, PathBuf};

/// Milliseconds in a day.
const DAY_MS: i64 = 86_400_000;

/// The deadman clock and wipe of one device.
pub struct Deadman {
    /// Where the "last heard" mark lives (a folder the wipe may erase).
    state_dir: PathBuf,
    /// The "wipe pending" marker, outside any folder a wipe erases.
    marker: PathBuf,
    days: i64,
}

impl Deadman {
    /// `state_dir` holds the last-heard mark, `marker` is the pending-wipe file (outside `state_dir` and every registered copy), and
    /// `days` is the period without reaching the owner's sync.
    pub fn new(state_dir: impl Into<PathBuf>, marker: impl Into<PathBuf>, days: i64) -> Self {
        Deadman {
            state_dir: state_dir.into(),
            marker: marker.into(),
            days,
        }
    }

    fn heard_path(&self) -> PathBuf {
        self.state_dir.join("last-heard")
    }

    /// Remembers that a pass reached the owner's sync just now (Unix milliseconds).
    pub fn record_heard(&self, now_ms: i64) {
        let _ = rightkit_fs::ensure_private_dir(&self.state_dir);
        let _ =
            rightkit_fs::atomic_write_private(&self.heard_path(), now_ms.to_string().as_bytes());
    }

    /// When a pass last reached the account. The clock starts the first time this is asked.
    fn last_heard(&self, now_ms: i64) -> i64 {
        match std::fs::read_to_string(self.heard_path())
            .ok()
            .and_then(|text| text.trim().parse::<i64>().ok())
        {
            Some(at) => at,
            None => {
                self.record_heard(now_ms);
                now_ms
            }
        }
    }

    /// Whether the whole period has passed without reaching the account.
    pub fn due(&self, now_ms: i64) -> bool {
        now_ms - self.last_heard(now_ms) > self.days * DAY_MS
    }

    /// A self-wipe was started and not finished.
    pub fn is_pending(&self) -> bool {
        self.marker.exists()
    }

    /// Erases every copy, then destroys the keys (`manager` is asked for after the copies are gone, so a key store that cannot be
    /// opened does not stop the erase), then clears the marker.
    pub fn finish(
        &self,
        copies: &[Box<dyn LocalCopy>],
        manager: impl FnOnce() -> std::result::Result<KeyManager, String>,
    ) -> std::result::Result<(), String> {
        for copy in copies {
            copy.erase().map_err(|e| format!("{}: {e}", copy.name()))?;
        }
        manager()?.forget_all().map_err(|e| e.to_string())?;
        remove_marker(&self.marker)
    }

    /// The deadman wipe: writes the marker, then [`Deadman::finish`].
    pub fn self_wipe(
        &self,
        copies: &[Box<dyn LocalCopy>],
        manager: impl FnOnce() -> std::result::Result<KeyManager, String>,
    ) -> std::result::Result<(), String> {
        std::fs::write(&self.marker, b"1").map_err(|e| e.to_string())?;
        self.finish(copies, manager)
    }

    /// Call at every start, before any store or the sync engine is opened: finishes a self-wipe that was started and interrupted.
    /// `true` when one was pending (whether or not it finished; on failure the marker stays and the next start tries again).
    pub fn resume(
        &self,
        copies: &[Box<dyn LocalCopy>],
        manager: impl FnOnce() -> std::result::Result<KeyManager, String>,
    ) -> bool {
        if !self.is_pending() {
            return false;
        }
        let _ = self.finish(copies, manager);
        true
    }
}

fn remove_marker(marker: &Path) -> std::result::Result<(), String> {
    match std::fs::remove_file(marker) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(()),
    }
}
