//! Environment overrides that exist only in debug builds (tests, QA journeys and development
//! checkouts). A release build compiles the reads out entirely: every function returns `None`
//! and the variable names do not appear in the binary, so no caller can redirect the stores,
//! identity files or workspace of an installed Membrane.

use std::ffi::OsString;

pub fn event_db() -> Option<OsString> {
    None
}

pub fn memory_db() -> Option<OsString> {
    None
}

pub fn workspace_root() -> Option<OsString> {
    None
}

pub fn workspace_config() -> Option<OsString> {
    None
}

pub fn telemetry_ingress() -> Option<OsString> {
    None
}

pub fn installation_file() -> Option<OsString> {
    None
}

pub fn mirror() -> Option<OsString> {
    None
}
