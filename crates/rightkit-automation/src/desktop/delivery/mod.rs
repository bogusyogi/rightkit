//! Text delivery to the focused field on macOS and Windows: clipboard stage, paste chord, Enter,
//! foreground-window snapshots, stop-destination validation and the real backend that wires them
//! together.
//!
//! The record shapes and pure predicates live in `rightkit_transcript_library::delivery`
//! (re-exported here). Enabled by the `desktop-delivery` feature.

use std::sync::OnceLock;

mod backend;
mod targets;

pub use backend::*;
pub use targets::*;

static DELIVERY_METHOD: OnceLock<fn() -> String> = OnceLock::new();

/// Install the provider of the configured delivery method name (the application's setting).
/// Returns false when one is already installed. Until then the method is `"paste"`.
pub fn set_delivery_method_provider(provider: fn() -> String) -> bool {
    DELIVERY_METHOD.set(provider).is_ok()
}

pub(crate) fn delivery_method() -> String {
    DELIVERY_METHOD
        .get()
        .map_or_else(|| "paste".to_string(), |provider| provider())
}
