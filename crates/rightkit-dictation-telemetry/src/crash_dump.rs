//! Cross-platform best-effort crash/hang dump capture.
//!
//! Called from three places: the panic hook ("panic"),
//! the main-thread hang watchdog ("hang"), and the sidecar unexpected-exit
//! detector in the engine supervisor ("sidecar_crash").
//!
//! MUST be panic-safe: no `unwrap`/`expect`, no allocation-heavy work that
//! could fail hard, no re-entrant panic. Every step is best-effort and
//! swallows its own errors — a broken dump path must never crash harder than
//! whatever it is trying to capture. Dumps carry no transcript/user text by
//! construction (process/thread state only), so no separate redaction pass
//! is needed before they travel in the diagnostics export.

use rightkit_crash::Retention;
use std::path::PathBuf;

/// The crash-directory caps, applied through `rightkit_crash::prune`
/// after every capture: newest 5 native dumps/samples, newest 10 crash logs,
/// and a 50 MiB overall cap (oldest files pruned first).
const CRASH_RETENTION: Retention = Retention {
    max_native_dumps: 5,
    max_reports: 10,
    max_dir_bytes: 50 * 1024 * 1024,
};

/// Windows dump helper shipped next to the shell executable (Tauri `externalBin`).
/// Same `--pid <pid> --output <path>` protocol as rightkit-crash's helper.
/// File name of the Windows dump helper the host ships next to its executable.
pub const DEFAULT_DUMP_HELPER: &str = "rightkit-dump-helper.exe";
static DUMP_HELPER_NAME: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// Name the dump helper executable when it is not [`DEFAULT_DUMP_HELPER`]. Returns false when a
/// name was already set.
pub fn configure_dump_helper_name(name: &'static str) -> bool {
    DUMP_HELPER_NAME.set(name).is_ok()
}

fn dump_helper() -> &'static str {
    DUMP_HELPER_NAME
        .get()
        .copied()
        .unwrap_or(DEFAULT_DUMP_HELPER)
}

#[cfg(target_os = "windows")]
const CAPTURE_EVENT_PREFIX: &str = "dump_helper";
#[cfg(not(target_os = "windows"))]
const CAPTURE_EVENT_PREFIX: &str = "sample_capture";

fn crash_dir() -> PathBuf {
    crate::app_paths::app_data_file("crash")
}

/// Select the Windows dump helper once at startup. Idempotent; ignored off Windows.
pub fn init_dump_helper() {
    rightkit_crash::set_dump_helper(dump_helper());
}

/// Best-effort crash/hang dump via `rightkit_crash::capture_native` (macOS
/// `sample`, Windows out-of-process minidump). Never panics; returns `None` on
/// any failure rather than surfacing an error a panic-hook or watchdog caller
/// would have to handle. Emits `dump_helper_*` (Windows) / `sample_capture_*`
/// lifecycle events, then prunes the crash directory.
pub fn write_dump(reason: &str) -> Option<PathBuf> {
    let dir = crash_dir();
    let started = std::time::Instant::now();
    let pid = std::process::id();
    crate::telemetry::log_message(
        &format!("{CAPTURE_EVENT_PREFIX}_started"),
        &format!("reason={reason} pid={pid}"),
    );
    let result = rightkit_crash::capture_native(&dir, reason);
    let elapsed_ms = started.elapsed().as_millis();
    match &result {
        Some(path) => {
            let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            crate::telemetry::log_message(
                &format!("{CAPTURE_EVENT_PREFIX}_succeeded"),
                &format!("reason={reason} pid={pid} bytes={bytes} elapsed_ms={elapsed_ms}"),
            );
        }
        None => crate::telemetry::log_message(
            &format!("{CAPTURE_EVENT_PREFIX}_failed"),
            &format!("reason={reason} pid={pid} elapsed_ms={elapsed_ms}"),
        ),
    }
    // Always prune, even on a failed capture — a half-written file from a
    // failed attempt should not be allowed to accumulate either.
    rightkit_crash::prune(&dir, CRASH_RETENTION);
    result
}
