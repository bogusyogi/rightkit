//! App-private paths injected from Kotlin at init. Pure string handling, no filesystem access.
//!
//! Android paths are always POSIX, so absoluteness is "starts with `/`" on every host (the unit
//! tests run on Windows and macOS too). Paths are never normalised through the host's `Path`.

use crate::error::AndroidError;

/// The three directories Kotlin hands over (`Context.getFilesDir()`, `getCacheDir()` and
/// `ApplicationInfo.nativeLibraryDir`).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AppPaths {
    pub files_dir: String,
    pub cache_dir: String,
    pub native_library_dir: String,
}

/// [`AppPaths`] plus the subdirectories Right Suite crates conventionally use under `files_dir`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AppDirs {
    pub files_dir: String,
    pub cache_dir: String,
    pub native_library_dir: String,
    /// `<files_dir>/models`: the `rightkit-models` root.
    pub models_dir: String,
    /// `<files_dir>/settings`: the `rightkit-app-settings` root.
    pub settings_dir: String,
    /// `<files_dir>/logs`: the `rightkit-logs` directory.
    pub logs_dir: String,
}

fn clean(label: &str, raw: &str) -> Result<String, AndroidError> {
    if raw.is_empty() {
        return Err(AndroidError::invalid(&format!("{label} is empty")));
    }
    if raw.contains('\0') {
        return Err(AndroidError::invalid(&format!("{label} contains a NUL")));
    }
    if !raw.starts_with('/') {
        return Err(AndroidError::invalid(&format!("{label} is not absolute")));
    }
    if raw.split('/').any(|c| c == "..") {
        return Err(AndroidError::invalid(&format!("{label} contains `..`")));
    }
    let trimmed = raw.trim_end_matches('/');
    Ok(if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    })
}

impl AppPaths {
    /// Validates and normalises (trailing slashes removed). Each path must be non-empty, absolute,
    /// free of NUL and of `..` components.
    pub fn new(
        files_dir: &str,
        cache_dir: &str,
        native_library_dir: &str,
    ) -> Result<Self, AndroidError> {
        Ok(AppPaths {
            files_dir: clean("files_dir", files_dir)?,
            cache_dir: clean("cache_dir", cache_dir)?,
            native_library_dir: clean("native_library_dir", native_library_dir)?,
        })
    }

    fn under_files(&self, name: &str) -> String {
        if self.files_dir == "/" {
            format!("/{name}")
        } else {
            format!("{}/{name}", self.files_dir)
        }
    }

    pub fn models_dir(&self) -> String {
        self.under_files("models")
    }

    pub fn settings_dir(&self) -> String {
        self.under_files("settings")
    }

    pub fn logs_dir(&self) -> String {
        self.under_files("logs")
    }

    /// `<files_dir>/crashes`: where [`crate::crash::install_panic_recorder`] writes reports.
    pub fn crash_dir(&self) -> String {
        self.under_files("crashes")
    }

    pub fn dirs(&self) -> AppDirs {
        AppDirs {
            files_dir: self.files_dir.clone(),
            cache_dir: self.cache_dir.clone(),
            native_library_dir: self.native_library_dir.clone(),
            models_dir: self.models_dir(),
            settings_dir: self.settings_dir(),
            logs_dir: self.logs_dir(),
        }
    }
}
