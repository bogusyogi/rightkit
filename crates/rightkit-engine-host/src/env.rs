//! The engine's view of its own process environment: operating system, executable location, the
//! app's data root and the user's home. Fields are public and injectable, so a host builds it from
//! its own resolution (app ids, data roots).

use std::path::{Path, PathBuf};

pub use rightkit_roots::AppRoot;

/// Inputs to path resolution.
#[derive(Debug, Clone)]
pub struct Env {
    /// `std::env::consts::OS` (`macos`, `windows`, ...).
    pub os: String,
    /// The engine executable, when known.
    pub exe: Option<PathBuf>,
    /// The app's data root.
    pub app_root: AppRoot,
    /// The user's home folder, when known.
    pub home: Option<PathBuf>,
}

impl Env {
    /// An environment from explicit parts.
    pub fn new(
        os: impl Into<String>,
        exe: Option<PathBuf>,
        app_root: AppRoot,
        home: Option<PathBuf>,
    ) -> Self {
        Self {
            os: os.into(),
            exe,
            app_root,
            home,
        }
    }

    /// The folder the engine's bundled resources resolve from: see [`resource_dir`].
    pub fn resource_dir(&self) -> Option<PathBuf> {
        resource_dir(&self.os, self.exe.as_deref()?)
    }
}

/// Where an executable's bundled resources resolve from. On `macos`, an executable in a bundle
/// (`<App>.app/Contents/MacOS/<exe>`) resolves to `Contents/Resources`; on `windows`, to the
/// executable's folder. Anything else (a bare macOS binary, another system) is unresolved (`None`).
/// The one rule for engines and their `doctor` report.
pub fn resource_dir(os: &str, exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    match os {
        "macos" => {
            let in_bundle = dir.ends_with("Contents/MacOS")
                && dir
                    .parent()
                    .and_then(Path::parent)
                    .is_some_and(|app| app.extension().is_some_and(|e| e == "app"));
            in_bundle
                .then(|| dir.parent().map(|c| c.join("Resources")))
                .flatten()
        }
        "windows" => Some(dir.to_path_buf()),
        _ => None,
    }
}
