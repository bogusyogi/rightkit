//! Adapter from [`ExitObserver`] to `rightkit-crash`: each abnormal engine exit becomes a
//! redacted `sidecar_crash` report with the restart count, through
//! [`rightkit_crash::record_helper_exit`].

use crate::supervisor::ExitObserver;
use rightkit_crash::{record_helper_exit, CrashConfig};

/// Records abnormal engine exits as crash reports. Build the [`CrashConfig`] the same way the
/// app's own hook is built (same `crash_dir`, sink and known-secret list); the report is marked
/// as a helper exit.
pub struct CrashExitObserver {
    config: CrashConfig,
}

impl CrashExitObserver {
    pub fn new(config: CrashConfig) -> Self {
        Self { config }
    }
}

impl ExitObserver for CrashExitObserver {
    fn engine_exited(&self, helper: &str, exit: &str, restart_count: u32) {
        let _ = record_helper_exit(&self.config, helper, exit, restart_count);
    }
}
