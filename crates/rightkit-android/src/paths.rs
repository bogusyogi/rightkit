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

#[cfg(test)]
mod tests {
    use super::*;

    const F: &str = "/data/user/0/com.example/files";
    const C: &str = "/data/user/0/com.example/cache";
    const N: &str = "/data/app/~~x/com.example-y/lib/arm64";

    #[test]
    fn accepts_android_paths_and_derives_subdirectories() {
        let p = AppPaths::new(F, C, N).unwrap();
        assert_eq!(p.models_dir(), format!("{F}/models"));
        assert_eq!(p.settings_dir(), format!("{F}/settings"));
        assert_eq!(p.logs_dir(), format!("{F}/logs"));
        assert_eq!(p.crash_dir(), format!("{F}/crashes"));
        let d = p.dirs();
        assert_eq!(d.native_library_dir, N);
        assert_eq!(d.models_dir, p.models_dir());
    }

    #[test]
    fn trailing_slashes_are_removed() {
        let p = AppPaths::new(&format!("{F}//"), &format!("{C}/"), N).unwrap();
        assert_eq!(p.files_dir, F);
        assert_eq!(p.cache_dir, C);
    }

    #[test]
    fn root_is_kept_and_joins_without_a_double_slash() {
        let p = AppPaths::new("/", C, N).unwrap();
        assert_eq!(p.files_dir, "/");
        assert_eq!(p.models_dir(), "/models");
    }

    #[test]
    fn rejects_empty_relative_nul_and_parent_components() {
        for bad in ["", "files", "./files", "/a/../b", "/a/\0/b", "C:\\files"] {
            assert!(
                matches!(AppPaths::new(bad, C, N), Err(AndroidError::Invalid { .. })),
                "{bad:?} must be rejected"
            );
        }
        assert!(AppPaths::new(F, "", N).is_err());
        assert!(AppPaths::new(F, C, "lib").is_err());
    }

    #[test]
    fn a_dotted_name_is_not_a_parent_component() {
        assert!(AppPaths::new("/data/a..b/files", C, N).is_ok());
    }

    #[test]
    fn the_error_text_does_not_echo_the_path() {
        let e = AppPaths::new("relative/secret-name", C, N).unwrap_err();
        assert!(!e.to_string().contains("secret-name"));
    }
}
