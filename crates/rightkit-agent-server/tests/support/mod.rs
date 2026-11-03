//! Shared integration-test helpers, compiled once for the `e2e` test binary.

pub mod daemon_process;
pub(crate) mod e2e_api;
pub mod e2e_cli;
pub(crate) mod e2e_orch;
pub(crate) mod e2e_tools;
pub mod fake_membrane;
pub(crate) mod mock_provider;
pub mod scripted_provider;
pub(crate) mod tui_harness;

use std::sync::{Mutex, MutexGuard};

static PROCESS_GLOBAL_STATE: Mutex<()> = Mutex::new(());

/// Serialize tests that change process-wide environment or working directory.
pub(crate) fn process_global_lock() -> MutexGuard<'static, ()> {
    PROCESS_GLOBAL_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
