//! Root resolution: the process facts (`EnvValues`) and the workspace, content and tools roots.
//!
//! Rules, in order: workspace root `<PREFIX>_WORKSPACE_ROOT`, else the host's known workspace folder
//! ([`EnvValues::known_workspace`]) when it is a directory, else `<home>/<workspace folder>`; content
//! root `<PREFIX>_CONTENT_ROOT`, else `<workspace>/Content`; tools root `<PREFIX>_TOOLS_ROOT`, else
//! `<workspace>/tools`. `<PREFIX>` is the host's environment prefix (see [`crate::host`]). An empty
//! variable counts as unset.

use std::path::{Path, PathBuf};

use crate::host;

/// The environment and filesystem facts path resolution reads. Build one with
/// [`EnvValues::from_process`] in a host, or set the fields directly in a test.
#[derive(Debug, Clone, Default)]
pub struct EnvValues {
    /// `<PREFIX>_WORKSPACE_ROOT`.
    pub workspace_root: Option<String>,
    /// `<PREFIX>_CONTENT_ROOT`.
    pub content_root: Option<String>,
    /// `<PREFIX>_TOOLS_ROOT`.
    pub tools_root: Option<String>,
    /// `<PREFIX>_MODEL_ROOT`.
    pub model_root: Option<String>,
    /// `<PREFIX>_SD_SERVER`.
    pub sd_server: Option<String>,
    /// `USERPROFILE` on Windows, `HOME` elsewhere.
    pub home: Option<String>,
    /// The temporary folder, used when `home` is unset.
    pub temp_dir: String,
    /// A workspace folder the host knows about, used when no variable names one and it exists.
    pub known_workspace: Option<String>,
    /// Whether `known_workspace` is an existing directory on this machine.
    pub known_workspace_is_dir: bool,
    /// Folder name of the default workspace under home (`<home>/<name>`).
    pub workspace_folder_name: String,
}

impl EnvValues {
    /// Reads the process environment. `known_workspace` is an optional host folder used when it
    /// exists; `workspace_folder_name` names the default workspace under home.
    pub fn from_process_with(known_workspace: Option<&str>, workspace_folder_name: &str) -> Self {
        Self {
            workspace_root: host::env_var("WORKSPACE_ROOT"),
            content_root: host::env_var("CONTENT_ROOT"),
            tools_root: host::env_var("TOOLS_ROOT"),
            model_root: host::env_var("MODEL_ROOT"),
            sd_server: host::env_var("SD_SERVER"),
            home: user_home(),
            temp_dir: std::env::temp_dir().to_string_lossy().to_string(),
            known_workspace: known_workspace.map(String::from),
            known_workspace_is_dir: known_workspace.is_some_and(|p| Path::new(p).is_dir()),
            workspace_folder_name: workspace_folder_name.to_string(),
        }
    }

    /// [`EnvValues::from_process_with`] with no known workspace and the folder name `RightKit`.
    pub fn from_process() -> Self {
        Self::from_process_with(None, "RightKit")
    }
}

fn user_home() -> Option<String> {
    let name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The workspace root as a string (see the module rules).
pub fn workspace_root(env: &EnvValues) -> String {
    if let Some(root) = non_empty(&env.workspace_root) {
        return root.to_string();
    }
    if env.known_workspace_is_dir {
        if let Some(known) = non_empty(&env.known_workspace) {
            return known.to_string();
        }
    }
    // Finder and Explorer launches do not start in a data folder.
    let home = non_empty(&env.home).unwrap_or(&env.temp_dir);
    let folder = if env.workspace_folder_name.is_empty() {
        "RightKit"
    } else {
        env.workspace_folder_name.as_str()
    };
    PathBuf::from(home)
        .join(folder)
        .to_string_lossy()
        .to_string()
}
/// The content root as a string (see the module rules).
pub fn content_root(env: &EnvValues) -> String {
    if let Some(root) = non_empty(&env.content_root) {
        return root.to_string();
    }
    format!(
        "{}/Content",
        workspace_root(env).replace('\\', "/").trim_end_matches('/')
    )
}

/// The tools root as a string (see the module rules).
pub fn tools_root(env: &EnvValues) -> String {
    if let Some(root) = non_empty(&env.tools_root) {
        return root.to_string();
    }
    format!("{}/tools", workspace_root(env))
}

pub fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|v| !v.is_empty())
}
