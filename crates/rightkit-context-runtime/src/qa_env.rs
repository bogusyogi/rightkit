//! Environment overrides that exist only in debug builds (tests, QA journeys and development
//! checkouts). A release build compiles the reads out entirely: every function returns `None`
//! and the variable names do not appear in the binary, so no caller can redirect the stores,
//! identity files or workspace of an installed Membrane.

use std::ffi::OsString;

#[cfg(debug_assertions)]
pub fn cortex_db() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_MEMORY_DB")
}

#[cfg(not(debug_assertions))]
pub fn cortex_db() -> Option<OsString> {
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
pub fn event_db() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_EVENT_DB")
}

#[cfg(not(debug_assertions))]
pub fn event_db() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn catalog() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_CATALOG")
}

#[cfg(not(debug_assertions))]
pub fn catalog() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn context_home() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_HOME")
}

#[cfg(not(debug_assertions))]
pub fn context_home() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn daily_analysis_dir() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_DAILY_ANALYSIS_DIR")
}

#[cfg(not(debug_assertions))]
pub fn daily_analysis_dir() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn repo_root() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_REPO_ROOT")
}

#[cfg(not(debug_assertions))]
pub fn repo_root() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn anchor_dir() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_ANCHOR_DIR")
}

#[cfg(not(debug_assertions))]
pub fn anchor_dir() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn project_registry() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_PROJECT_REGISTRY")
}

#[cfg(not(debug_assertions))]
pub fn project_registry() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn push_telemetry_path() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_PUSH_TELEMETRY_PATH")
}

#[cfg(not(debug_assertions))]
pub fn push_telemetry_path() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn skill_event_path() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_SKILL_EVENT_PATH")
}

#[cfg(not(debug_assertions))]
pub fn skill_event_path() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn state_root() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_STATE_ROOT")
}

#[cfg(not(debug_assertions))]
pub fn state_root() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn runtime_origin() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_RUNTIME_ORIGIN")
}

#[cfg(not(debug_assertions))]
pub fn runtime_origin() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn llmlingua_model() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_PUSH_LLMLINGUA_MODEL")
}

#[cfg(not(debug_assertions))]
pub fn llmlingua_model() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn llmlingua_tokenizer() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_PUSH_LLMLINGUA_TOKENIZER")
}

#[cfg(not(debug_assertions))]
pub fn llmlingua_tokenizer() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub fn cache_root() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_CACHE_ROOT")
}

#[cfg(not(debug_assertions))]
pub fn cache_root() -> Option<OsString> {
    None
}
