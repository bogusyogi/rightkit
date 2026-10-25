//! Shell <-> engine wire contract. The envelope (`hello`, `request`, `event`, `response`, and errors
//! that carry `retryable`) and the built-in `ping`/`doctor`/`shutdown` methods are rightkit-job-queue's
//! serve contract. This crate names the app's own methods and job kinds on top of it, and the one
//! private data root that the shell and the engine both resolve.
use std::path::PathBuf;

use rightkit_daemon::mode::{self, AppId, Mode};

pub use rightkit_job_queue::{JobError, JobResult};

/// Engine name in the `hello` frame and in `doctor`.
pub const ENGINE_NAME: &str = "rightapp-engine";
/// Synchronous domain command: served by the CLI and the sidecar alike.
pub const METHOD_ECHO: &str = "echo";
/// Job kind for domain work submitted through `job.submit`. `echo` is the template's only kind.
pub const JOB_KIND_ECHO: &str = "echo";
/// This app's identity for `rightkit_daemon::mode`: the binding key and the folder name under the suite.
pub const APP: AppId = AppId::new("rightapp");

/// The release-or-test decision, made once per process. Every binary of the app reaches it before it opens a
/// store, a log or a credential (`app_data_root` calls it). A build without the `test-mode` feature always decides
/// "release" and reads no override at all; a half-configured test run exits with status 2 and one line.
pub fn init() -> &'static Mode {
    mode::init_or_exit(&APP)
}

/// This app's private data root: `<app folder>/data`, owner-only. In a release run the app folder is the per-user
/// suite location; in a QA run it is the isolated directory rightkit-qa created. Nothing else can move it.
pub fn app_data_root() -> Result<PathBuf, String> {
    let roots = mode::roots(&APP, init()).map_err(|e| e.to_string())?;
    let dir = roots.app_dir.join("data");
    rightkit_fs::ensure_private_dir(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}
