//! Environment overrides that exist only in debug builds (tests, QA journeys and development
//! checkouts). A release build compiles the reads out entirely: every function returns `None`
//! and the variable names do not appear in the binary, so no caller can redirect the stores,
//! identity files or workspace of an installed Membrane.

use std::ffi::OsString;

#[cfg(debug_assertions)]
pub fn event_db() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_EVENT_DB")
}

#[cfg(not(debug_assertions))]
pub fn event_db() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn memory_db() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_MEMORY_DB")
}

#[cfg(not(debug_assertions))]
pub fn memory_db() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn workspace_root() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_WORKSPACE_ROOT")
}

#[cfg(not(debug_assertions))]
pub fn workspace_root() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn workspace_config() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_WORKSPACE_CONFIG")
}

#[cfg(not(debug_assertions))]
pub fn workspace_config() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn telemetry_ingress() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_TELEMETRY_INGRESS")
}

#[cfg(not(debug_assertions))]
pub fn telemetry_ingress() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn installation_file() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_INSTALLATION_FILE")
}

#[cfg(not(debug_assertions))]
pub fn installation_file() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn mirror() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_MIRROR")
}

#[cfg(not(debug_assertions))]
pub fn mirror() -> Option<OsString> {
    None
}
