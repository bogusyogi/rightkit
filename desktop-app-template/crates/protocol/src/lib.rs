//! Shell <-> engine wire contract. The envelope (`hello`, `request`, `event`, `response`, and errors
//! that carry `retryable`) and the built-in `ping`/`doctor`/`shutdown` methods are rightkit-jobs'
//! serve contract. This crate names the app's own methods and job kinds on top of it, and the one
//! private data root that the shell and the engine both resolve.
use std::path::PathBuf;

use rightkit_service::Suite;

pub use rightkit_jobs::{JobError, JobResult};

/// Engine name in the `hello` frame and in `doctor`.
pub const ENGINE_NAME: &str = "rightapp-engine";
/// Synchronous domain command: served by the CLI and the sidecar alike.
pub const METHOD_ECHO: &str = "echo";
/// Job kind for domain work submitted through `job.submit`. `echo` is the template's only kind.
pub const JOB_KIND_ECHO: &str = "echo";
/// Harness override of the data root (`rightkit-qa.toml` `[engine] data_dir_env`). Tests set it to
/// isolate a run; nothing else in the app sets it.
pub const DATA_OVERRIDE_ENV: &str = "RIGHTAPP_APP_DATA";
/// The `rightkit-service` suite service name: `<suite>/runtime/rightapp/` is this app's directory.
pub const SERVICE: &str = "rightapp";
const PRIVATE_AREA: &str = "data";

/// This app's private data root: `<suite>/runtime/rightapp/data`, owner-only. The suite root comes
/// from `Suite::resolve` (the platform default, or `RIGHTKIT_SUITE_ROOT`), so the shell and the engine
/// resolve the same directory without passing it between them.
pub fn app_data_root() -> Result<PathBuf, String> {
    if let Some(v) = std::env::var_os(DATA_OVERRIDE_ENV).filter(|v| !v.is_empty()) {
        return Ok(v.into());
    }
    let suite = Suite::resolve().map_err(|e| e.to_string())?;
    suite.private_service_subdir(SERVICE, PRIVATE_AREA).map_err(|e| e.to_string())
}
