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
//! Merged from HeardRight `crash_dump.rs`, `crash_capture.rs`, `crash_report.rs`,
//! `panic_redact.rs`, `diagnostics_archive.rs` and `heardright-dump-helper`.

#![deny(unsafe_code)]

#[cfg(feature = "support-bundle")]
mod archive;
#[cfg(feature = "support-bundle")]
mod bundle;
mod hook;
mod native;
mod pending;
mod report;
mod retention;

#[cfg(feature = "support-bundle")]
pub use archive::create_zip;
#[cfg(feature = "support-bundle")]
pub use bundle::{build_support_bundle, BundleManifest, SupportBundleOptions};
pub use hook::{
    install_panic_hook, record_report, recording_enabled, set_recording_enabled, CrashConfig,
};
pub use native::{capture_native, set_dump_helper, NativeCapture, DEFAULT_DUMP_HELPER};
pub use pending::{enqueue_pending, list_pending, EnqueueSummary, CRASH_EVENT_TYPE};
pub use report::{CrashReport, CRASH_SCHEMA_VERSION};
pub use retention::{prune, Retention};
