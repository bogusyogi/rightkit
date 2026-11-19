//! Host for transcript sync (feature `transcript-host`).
//!
//! `rightkit_sync::transcripts` holds the pure transcript pieces (mapper, ids, version book,
//! legacy migration, consent scope rule). This module is the running part:
//!
//! * [`TranscriptSync`]: the consent switch and account gate in front of the engine, the
//!   engine's lifetime, the guarded ledger, the account-scoped deletion outbox, the windowed
//!   history adapter and the pass.
//! * the scheduler (private): trigger coalescing, bounded retry, parking and the consent epoch.
//!   Timings come from [`SchedulerPolicy`].
//! * [`TranscriptHost`] / [`HistorySession`]: what the app provides.
//! * [`SyncTransport`] / [`NativeObserver`]: the backend seam; [`cloudkit::CloudKitTransport`]
//!   is the macOS implementation over the signed helper process.
//!
//! The module has no product name, copy, path, identifier or timing of its own: all of them are
//! inputs ([`TranscriptSyncConfig`], [`cloudkit::CloudKitConfig`]).

mod history;
mod ledger_guard;
mod outbox;
mod pending;
mod remote_window;
mod scheduler;
mod sync;
mod traits;

#[cfg(target_os = "macos")]
pub mod cloudkit;

pub use scheduler::SchedulerPolicy;
pub use sync::{
    DeleteOutcome, DeletionBacklog, StatusReport, TranscriptSync, TranscriptSyncConfig,
};
pub use traits::{
    HistoryKey, HistoryRow, HistorySession, KeyWindow, NativeObserver, Spawner, SyncTransport,
    TranscriptHost,
};

/// Make a diagnostic detail safe to hand to the host's event sink: tokens that look like paths
/// (contain `/` or `\`) become `<path>`, whitespace runs collapse to one space, and the result
/// is cut to 200 characters. Record text, keys and ciphertext are never passed here in the
/// first place; this is the backstop for error strings that come from the OS or the helper.
pub(crate) fn safe_detail(detail: &str) -> String {
    const MAX_CHARS: usize = 200;
    let mut out = String::new();
    for token in detail.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        if token.contains('/') || token.contains('\\') {
            out.push_str("<path>");
        } else {
            out.push_str(token);
        }
        if out.chars().count() >= MAX_CHARS {
            break;
        }
    }
    out.chars().take(MAX_CHARS).collect()
}
