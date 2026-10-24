//! Shell <-> engine wire contract. The envelope (`hello`, `request`, `event`, `response`, and errors
//! that carry `retryable`) and the built-in `ping`/`doctor`/`shutdown` methods are rightkit-job-queue's
//! serve contract. This crate names the app's own methods and job kinds on top of it, the mode
//! identity both binaries start with, and the one private data root that the shell and the engine
//! both resolve.
use std::path::{Path, PathBuf};

pub use rightkit_job_queue::{JobError, JobResult};

/// Engine name in the `hello` frame and in `doctor`.
pub const ENGINE_NAME: &str = "rightapp-engine";
/// Synchronous domain command: served by the CLI and the sidecar alike.
pub const METHOD_ECHO: &str = "echo";
/// Job kind for domain work submitted through `job.submit`. `echo` is the template's only kind.
pub const JOB_KIND_ECHO: &str = "echo";
/// The installed folder name under the `Orthic Labs` data root (a dev build adds the `Dev` suffix).
pub const APP_NAME: &str = "RightApp";
/// The app id: the shell's single-instance id, the mode identity and the runner's test binding
/// (`rightkit-qa.toml` `app`).
pub const APP_ID: &str = "rightapp";
/// The identity both binaries pass to `rightkit_daemon::mode::init_or_exit` as their first call.
pub const MODE_APP: rightkit_daemon::mode::AppId = rightkit_daemon::mode::AppId::new(APP_ID);

/// This app's private data root, owner-only. `rightkit-fs` resolves it from the operating system's
/// account (never from the environment), so the shell and the engine get the same directory without
/// passing it between them. A build without debug assertions uses `<Orthic Labs>/RightApp`, a dev
/// build `<Orthic Labs>/RightAppDev`. Only a build with the `test-mode` feature honours the isolated
/// root that `rightkit-qa` binds for a scenario.
pub fn app_data_root() -> Result<PathBuf, String> {
    let app = rightkit_fs::AppId::new(APP_NAME).with_qa_app_id(APP_ID);
    let dirs = rightkit_fs::AppDirs::resolve(app).map_err(|e| e.to_string())?;
    rightkit_fs::private::ensure_private_dir(&dirs.data).map_err(|e| e.to_string())?;
    Ok(dirs.data)
}

/// Where the shell and the engine both write crash reports: a `crash` folder under the data root.
pub fn crash_dir(data_root: &Path) -> PathBuf {
    data_root.join("crash")
}

/// `"debug"` or `"release"`: the build as crash reports and diagnostics bundles state it.
pub fn build_label() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // rk-test: pure
    #[test]
    fn the_constants_are_what_the_scenario_file_expects() {
        assert_eq!(ENGINE_NAME, "rightapp-engine");
        assert_eq!(METHOD_ECHO, "echo");
        assert_eq!(JOB_KIND_ECHO, "echo");
    }

    // rk-test: pure
    #[test]
    fn the_mode_identity_is_the_runner_binding_id() {
        assert_eq!(APP_ID, "rightapp");
        assert_eq!(MODE_APP.id(), APP_ID);
    }

    // rk-test: pure
    #[test]
    fn crash_reports_live_in_a_crash_folder_under_the_data_root() {
        let root = Path::new("data-root");
        assert_eq!(crash_dir(root), root.join("crash"));
    }

    // rk-test: pure
    #[test]
    fn the_build_label_names_the_profile() {
        assert!(matches!(build_label(), "debug" | "release"));
    }
}
