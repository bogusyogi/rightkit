//! Environment overrides that exist only in debug builds (tests, QA journeys and development
//! checkouts). A release build compiles the reads out entirely: every function returns `None`
//! and the variable names do not appear in the binary, so no caller can redirect the stores,
//! identity files or workspace of an installed Membrane.

use std::ffi::OsString;

pub fn cortex_db() -> Option<OsString> {
    None
}

pub fn workspace_root() -> Option<OsString> {
    None
}

pub fn event_db() -> Option<OsString> {
    None
}

pub fn catalog() -> Option<OsString> {
    None
}

pub fn context_home() -> Option<OsString> {
    None
}

pub fn daily_analysis_dir() -> Option<OsString> {
    None
}

pub fn repo_root() -> Option<OsString> {
    None
}

pub fn anchor_dir() -> Option<OsString> {
    None
}

pub fn project_registry() -> Option<OsString> {
    None
}

pub fn push_telemetry_path() -> Option<OsString> {
    None
}

pub fn skill_event_path() -> Option<OsString> {
    None
}

pub fn state_root() -> Option<OsString> {
    None
}

pub fn runtime_origin() -> Option<OsString> {
    None
}

pub fn llmlingua_model() -> Option<OsString> {
    None
}

pub fn llmlingua_tokenizer() -> Option<OsString> {
    None
}

pub fn cache_root() -> Option<OsString> {
    None
}
