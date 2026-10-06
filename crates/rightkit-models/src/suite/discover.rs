//! Find a usable copy of a model that another Right Suite app already
//! installed — read in place, never moved, copied, or deleted.
//!
//! The store is the preferred home. When it has no copy, the engine can load
//! the files another app keeps in its own data directory (HeardRight today),
//! so one machine holds one copy of the model instead of two.
//!
//! A copy qualifies only when every catalog file matches its trusted size and
//! SHA-256. Variant identities stay distinct; same-size corruption is rejected.

use crate::suite::catalog::ModelSpec;
use std::path::{Path, PathBuf};

/// Another app's data-directory name and the model sub-directories it uses.
#[derive(Debug, Clone, Copy)]
pub struct Source {
    pub app: &'static str,
    pub data_dir_name: &'static str,
    pub model_dirs: &'static [&'static str],
}

/// Sources compiled into the suite: HeardRight's own data dir and the previous
/// shared `RightSuite` layout (the latter only inside the legacy window).
pub fn default_sources() -> &'static [Source] {
    SOURCES
}

#[cfg(not(target_os = "macos"))]
const SOURCES: &[Source] = &[
    Source {
        app: "HeardRight",
        data_dir_name: "HeardRightNext",
        model_dirs: &["parakeet_tdt_v3_static1500_qint8_20260722"],
    },
    // Compatibility window for model copies installed by the previous shared
    // Right Suite store layout (`<data>/RightSuite/models/<model-id>`).
    Source {
        app: "RightSuite",
        data_dir_name: "RightSuite",
        model_dirs: &["parakeet_tdt_v3_static1500_qint8_20260722"],
    },
];

#[cfg(target_os = "macos")]
const SOURCES: &[Source] = &[
    Source {
        app: "HeardRight",
        data_dir_name: "HeardRightNext",
        model_dirs: &["parakeet-tdt-v3"],
    },
    Source {
        app: "RightSuite",
        data_dir_name: "RightSuite",
        model_dirs: &[
            "parakeet-tdt-v3-coreml-compiled-5f8888117f47",
            "parakeet-tdt-v3",
        ],
    },
];

/// A usable model copy owned by another app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingCopy {
    /// Display name of the app that owns the files, e.g. "HeardRight".
    pub app: &'static str,
    pub dir: PathBuf,
}

fn data_root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        dirs::data_local_dir()
    }
    #[cfg(not(windows))]
    {
        dirs::data_dir()
    }
}

/// Whether `dir` holds every file `spec` loads and matches trusted catalog
/// hashes. External app copies have no trusted local manifest, so verify
/// directly against the catalog at Full depth.
pub fn dir_has_model_files(dir: &Path, spec: &ModelSpec) -> bool {
    dir.is_dir()
        && crate::suite::manifest::verify_dir(
            dir,
            &spec.expected_manifest(),
            crate::suite::manifest::VerifyMode::Full,
        )
        .is_ready()
}

/// First usable copy of `spec` under `root` (a platform data directory that
/// contains each app's `<data_dir_name>/models/<sub-dir>`), searching the
/// suite's [`default_sources`]. `legacy_window_open` gates the `RightSuite`
/// compatibility source (see [`crate::suite::paths::version_within`]).
pub fn find_existing_under(
    root: &Path,
    spec: &ModelSpec,
    legacy_window_open: bool,
) -> Option<ExistingCopy> {
    find_existing_in_sources(root, spec, SOURCES, legacy_window_open)
}

/// As [`find_existing_under`] with caller-supplied sources.
pub fn find_existing_in_sources(
    root: &Path,
    spec: &ModelSpec,
    sources: &[Source],
    legacy_window_open: bool,
) -> Option<ExistingCopy> {
    for source in sources {
        if source.data_dir_name == "RightSuite" && !legacy_window_open {
            continue;
        }
        let models = root.join(source.data_dir_name).join("models");
        for sub in source.model_dirs {
            let dir = models.join(sub);
            if dir_has_model_files(&dir, spec) {
                return Some(ExistingCopy {
                    app: source.app,
                    dir,
                });
            }
        }
    }
    None
}

/// First usable copy of `spec` that another installed Right Suite app keeps.
pub fn find_existing_elsewhere(spec: &ModelSpec, legacy_window_open: bool) -> Option<ExistingCopy> {
    find_existing_under(&data_root()?, spec, legacy_window_open)
}
