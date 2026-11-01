#![cfg(target_os = "macos")]

//! macOS system glue, split by concern: one file per concern under this
//! directory, with every path resolving as `crate::desktop::macos::X`
//! via the `pub use` re-exports below.

/// Case-insensitive window-owner name prefix of the host's own processes, set once at startup.
static OWN_WINDOW_OWNER_PREFIX: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// Name the host's own window owners (case-insensitive prefix, for example the app name) so its
/// overlays and helpers are never chosen as a delivery destination. Returns false when a prefix
/// was already set.
pub fn set_own_window_owner_prefix(prefix: &'static str) -> bool {
    OWN_WINDOW_OWNER_PREFIX.set(prefix).is_ok()
}

pub(crate) fn own_window_owner_prefix() -> Option<&'static str> {
    OWN_WINDOW_OWNER_PREFIX.get().copied()
}

mod clipboard;
mod ffi;
mod focus;
mod frontmost;
mod keyboard;
mod observation;
mod qos;
mod window_query;

pub use clipboard::*;
pub use focus::*;
pub use frontmost::*;
pub use keyboard::*;
pub use observation::*;
pub use qos::*;
pub use window_query::{
    enable_isolated_window_queries, run_window_query_helper, set_window_query_diagnostic_hook,
    window_metadata,
};
