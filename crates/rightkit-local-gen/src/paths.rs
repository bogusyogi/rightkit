//! Path resolution shared by the desktop shell and the engine.
//!
//! Every function here is pure over its arguments: the app data root, the content root, the saved
//! `model_root`, the process environment (an [`EnvValues`] value) and the candidate directories for
//! the `sd-server` binary. [`EnvValues::from_process`] is the only place that reads the process
//! environment or the filesystem; tests build an [`EnvValues`] by hand.
//!
//! Rules, in the order each path is resolved (the shell's results, unchanged):
//!
//! - Workspace, content and tools roots: see [`crate::roots`]. `<PREFIX>` below is the host's
//!   environment prefix ([`crate::host::HostConfig::env_prefix`]).
//! - Local-image app folder: `<app data root>/local-image`. Database `local-image.sqlite3`, logs
//!   `logs`, staging `staging`, references `references` sit directly in it.
//! - Model root: `<PREFIX>_MODEL_ROOT`; else the saved `model_root` when it is absolute (a
//!   relative saved value is ignored); else `<app folder>/models`. LoRA folder: `<model root>/loras`.
//! - Outputs: `<content root>/<content subdir>/local-images` (the subdir is the host's
//!   [`crate::host::HostConfig::content_subdir`]).
//! - `sd-server` binary (`sd-server.exe` on Windows): `<PREFIX>_SD_SERVER` (not checked for
//!   existence); else the first candidate directory that holds the binary (`exists()`); else
//!   `<app folder>/bin/sd-server`.
//!
//! An empty environment variable counts as unset. The shell used to treat an empty value as set,
//! which resolved to an empty, relative path.
//!
//! This module adds no tests of its own: hosts cover it through their end-to-end suites.

use std::path::{Path, PathBuf};

pub use super::LocalImagePaths;

pub use crate::roots::{content_root, non_empty, tools_root, workspace_root, EnvValues};

/// The `sd-server` file name on this platform.
pub fn sd_server_file_name() -> &'static str {
    if cfg!(windows) {
        "sd-server.exe"
    } else {
        "sd-server"
    }
}

/// The `sd-server` path (see the module rules). `local_app_folder` is `<app data>/local-image`;
/// `candidate_dirs` are the directories the caller wants searched, in order (the shell passes the
/// folder of its executable, the engine the folder of its own).
pub fn sd_server_path(
    env: &EnvValues,
    local_app_folder: &Path,
    candidate_dirs: &[PathBuf],
) -> PathBuf {
    let exe = sd_server_file_name();
    if let Some(path) = non_empty(&env.sd_server) {
        return PathBuf::from(path);
    }
    for dir in candidate_dirs {
        let bundled = dir.join(exe);
        if bundled.exists() {
            return bundled;
        }
    }
    local_app_folder.join("bin").join(exe)
}

/// Every local-image path, resolved (see the module rules).
///
/// `app_data_dir` is the app data root (not the `local-image` folder under it). `content_root` is
/// the string from [`content_root`]. `saved_model_root` is the settings value, if any.
pub fn local_image(
    app_data_dir: &Path,
    content_root: &str,
    saved_model_root: Option<&Path>,
    env: &EnvValues,
    candidate_dirs: &[PathBuf],
) -> LocalImagePaths {
    let app_folder = app_data_dir.join("local-image");
    let model_root = non_empty(&env.model_root)
        .map(PathBuf::from)
        .or_else(|| {
            saved_model_root
                .filter(|p| p.is_absolute())
                .map(Path::to_path_buf)
        })
        .unwrap_or_else(|| app_folder.join("models"));
    LocalImagePaths {
        database_path: app_folder.join("local-image.sqlite3"),
        log_dir: app_folder.join("logs"),
        staging_dir: app_folder.join("staging"),
        reference_dir: app_folder.join("references"),
        lora_root: model_root.join("loras"),
        output_root: PathBuf::from(content_root)
            .join(&crate::host::config().content_subdir)
            .join("local-images"),
        backend_binary: sd_server_path(env, &app_folder, candidate_dirs),
        app_data_dir: app_folder,
        model_root,
    }
}
