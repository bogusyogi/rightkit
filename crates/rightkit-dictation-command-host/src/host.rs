//! The host app's identity, configured once at startup: its bundle identifier (excluded from
//! screenshots), the process-name prefix of its own processes and the titles of its own overlay
//! windows (never chosen as a paste or screenshot target), and its display name for messages.

use std::sync::OnceLock;

/// Identity of the dictation app hosting this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostIdentity {
    /// macOS bundle identifier, for example `com.example.app`.
    pub bundle_id: &'static str,
    /// Case-insensitive prefix of the host's own process names (shell and engine sidecars).
    pub process_name_prefix: &'static str,
    /// Exact (case-insensitive) titles of the host's own overlay windows.
    pub overlay_window_titles: &'static [&'static str],
    /// Name shown to the user in messages.
    pub display_name: &'static str,
}

const UNCONFIGURED: HostIdentity = HostIdentity {
    bundle_id: "",
    process_name_prefix: "",
    overlay_window_titles: &["pill"],
    display_name: "the app",
};

static IDENTITY: OnceLock<HostIdentity> = OnceLock::new();

/// Configure the host identity once. Returns false when one was already set.
pub fn configure(identity: HostIdentity) -> bool {
    IDENTITY.set(identity).is_ok()
}

/// The configured identity (an empty one until [`configure`]).
pub fn identity() -> &'static HostIdentity {
    IDENTITY.get().unwrap_or(&UNCONFIGURED)
}

/// True when `process_name` belongs to the host (empty prefix matches nothing).
pub fn is_own_process(process_name: &str) -> bool {
    let prefix = identity().process_name_prefix;
    !prefix.is_empty()
        && process_name
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
}
