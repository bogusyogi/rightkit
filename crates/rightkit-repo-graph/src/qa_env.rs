//! Environment overrides that exist only in debug builds (tests, QA journeys and development
//! checkouts). A release build compiles the reads out entirely: every function returns `None`
//! and the variable names do not appear in the binary.

use std::ffi::OsString;

pub(crate) fn scip_index() -> Option<OsString> {
    None
}

pub(crate) fn embed_model_dir() -> Option<OsString> {
    None
}
