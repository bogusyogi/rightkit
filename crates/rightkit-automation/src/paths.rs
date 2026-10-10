//! Where evidence and captures go by default. Fail closed: when no absolute
//! app-data root can be determined the answer is an error, never a path relative
//! to the working directory (HeardRight's command-host `app_data_root()` could
//! fall back to cwd; that is the defect this replaces).

use crate::model::{Error, Result};
use rightkit_service::Suite;
use std::path::PathBuf;

/// `<suite>/automation`, created owner-only. Honours `RIGHTKIT_SUITE_ROOT`.
pub fn evidence_dir() -> Result<PathBuf> {
    let suite = Suite::resolve().map_err(|e| Error::Invalid(e.to_string()))?;
    suite
        .data_dir("automation")
        .map_err(|e| Error::Invalid(e.to_string()))
}

/// `evidence_dir()` inside an explicit per-run temp suite: the opt-in alternative
/// for runs that have no installed app-data root (CI, sandboxes).
pub fn per_run_evidence_dir(label: &str) -> Result<(Suite, PathBuf)> {
    let suite = Suite::per_run_temp(label).map_err(|e| Error::Invalid(e.to_string()))?;
    let d = suite
        .data_dir("automation")
        .map_err(|e| Error::Invalid(e.to_string()))?;
    Ok((suite, d))
}
