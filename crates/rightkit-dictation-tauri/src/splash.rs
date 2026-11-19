//! Launch-splash stall detection and recovery (issue #117 / M-29).
//!
//! The splash stays blocking until the engine is ready; it never reveals an unready app. When
//! preparation looks dead the splash offers Retry, Quit and Copy/Reveal diagnostics. Progress is a
//! monotonic stamp bumped by the prewarm thread; a recorded failure is surfaced at once.
//!
//! The stall window is the application's value: `is_stalled` takes it as an argument, and the
//! splash's window and its labels stay in the application. Retry and Quit are the shell's relaunch
//! and exit ([`crate::shell::DictationShell`]).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use crate::shell::Shell;
use crate::telemetry;

static LAST_PROGRESS_MS: AtomicU64 = AtomicU64::new(0);
static PREP_FAILED: AtomicBool = AtomicBool::new(false);

fn now_ms() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    // Reserve zero for "no progress" while using a monotonic clock.
    (EPOCH
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .min((u64::MAX - 1) as u128) as u64)
        + 1
}

/// Record that preparation advanced (a stage finished).
pub fn note_progress() {
    LAST_PROGRESS_MS.store(now_ms(), Ordering::Relaxed);
}

/// Record that preparation failed; the splash offers recovery at once.
pub fn note_failure() {
    PREP_FAILED.store(true, Ordering::Relaxed);
}

/// The splash's view of preparation, as the renderer reads it.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SplashStatus {
    pub failed: bool,
    /// Milliseconds since the last recorded progress; None before the first.
    pub since_progress_ms: Option<u64>,
}

/// The splash's current status.
pub fn status() -> SplashStatus {
    let last = LAST_PROGRESS_MS.load(Ordering::Relaxed);
    SplashStatus {
        failed: PREP_FAILED.load(Ordering::Relaxed),
        since_progress_ms: (last != 0).then(|| now_ms().saturating_sub(last)),
    }
}

/// The single stall rule for the native macOS splash: failed, or no progress for `stall_secs`
/// since launch. `waited_secs` is how long the splash has been up.
#[cfg(target_os = "macos")]
pub fn is_stalled(stall_secs: u64, waited_secs: u64) -> bool {
    if PREP_FAILED.load(Ordering::Relaxed) {
        return true;
    }
    let last = LAST_PROGRESS_MS.load(Ordering::Relaxed);
    let idle = if last == 0 {
        waited_secs
    } else {
        now_ms().saturating_sub(last) / 1000
    };
    waited_secs >= stall_secs && idle >= stall_secs
}

/// Retry: relaunch the whole application, which re-runs preparation from scratch. Goes through the
/// shell's helper relaunch, so the new process starts while this one still holds the single-instance
/// guard. When the helper cannot start, the application restarts directly.
pub fn retry(shell: &Shell) {
    telemetry::log_message("splash_stall_action", "action=retry");
    if let Err(err) = shell.relaunch_helper("splash_retry") {
        telemetry::log_message("splash_retry_helper_failed", &err);
        shell.restart_app();
    }
}

/// Quit the application from the splash.
pub fn quit(shell: &Shell) {
    telemetry::log_message("splash_stall_action", "action=quit");
    shell.exit_app(0);
}
