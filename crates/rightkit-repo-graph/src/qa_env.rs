//! Environment overrides that exist only in debug builds (tests, QA journeys and development
//! checkouts). A release build compiles the reads out entirely: every function returns `None`
//! and the variable names do not appear in the binary.

use std::ffi::OsString;

#[cfg(debug_assertions)]
pub(crate) fn scip_index() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_REPO_GRAPH_SCIP_INDEX")
}

#[cfg(not(debug_assertions))]
pub(crate) fn scip_index() -> Option<OsString> {
    None
}

#[cfg(debug_assertions)]
pub(crate) fn embed_model_dir() -> Option<OsString> {
    std::env::var_os("RIGHTKIT_CONTEXT_EMBED_MODEL_DIR")
}

#[cfg(not(debug_assertions))]
pub(crate) fn embed_model_dir() -> Option<OsString> {
    None
}
