//! Privacy-first crash capture for Right Suite apps.
//!
//! * `install_panic_hook` writes a **redacted** structured report on panic
//!   (works under `panic = "abort"`), optionally with a native process
//!   sample / minidump kept local.
//! * `pending` hands finished reports to the consented `rightkit_logs::Outbox`
//!   on the next launch; with no consent they stay on disk and are never sent.
//! * `bundle` builds a user-initiated support ZIP from already-redacted data
//!   (feature `support-bundle`, on by default).
//!
//! * `record_android_exits` turns the Android exit history (native crash tombstones and ANRs the OS
//!   already recorded) into the same redacted reports on the next launch.
//!
//! Merged from HeardRight `crash_dump.rs`, `crash_capture.rs`, `crash_report.rs`,
//! `panic_redact.rs`, `diagnostics_archive.rs` and `heardright-dump-helper`.

#![deny(unsafe_code)]

mod android;
#[cfg(feature = "support-bundle")]
mod archive;
#[cfg(feature = "support-bundle")]
mod bundle;
mod hook;
mod native;
mod pending;
mod report;
mod retention;
#[cfg(feature = "window-errors")]
mod window;

pub use android::{
    record_android_exits, reduce_trace, unseen_exits, AndroidExit, AndroidExitReason,
};
#[cfg(feature = "support-bundle")]
pub use archive::create_zip;
#[cfg(feature = "support-bundle")]
pub use bundle::{build_support_bundle, BundleManifest, SupportBundleOptions};
pub use hook::{
    capture_native_for_pid, fallback_crash_dir, install_panic_hook, install_process_hook,
    record_helper_exit, record_report, recording_enabled, set_recording_enabled, CrashConfig,
    ProcessKind,
};
pub use native::{capture_native, set_dump_helper, NativeCapture, DEFAULT_DUMP_HELPER};
pub use pending::{enqueue_pending, list_pending, EnqueueSummary, CRASH_EVENT_TYPE};
pub use report::{CrashReport, CRASH_SCHEMA_VERSION};
pub use retention::{prune, Retention};
#[cfg(feature = "window-errors")]
pub use window::{
    record_window_error, WindowErrorGate, WindowErrorReport, MAX_WINDOW_MESSAGE_CHARS,
    MAX_WINDOW_STACK_BYTES,
};
