//! Installed-or-missing status for the local ASR models: backend alias canonicalization, the
//! on-disk subdir per platform, the model-family label and where a found copy lives.
//!
//! Resolution reads the shared store and the app-data tree through
//! [`crate::model_paths::resolve_model_dir`]. The host passes the user-visible family names as
//! [`LocalAsrLabels`] and whether a replacement download is pending as a plain bool.

use serde::Serialize;
use std::path::Path;

use crate::{model_paths, ultra_model};

/// Backend id of the current-generation model.
pub const BACKEND_CURRENT: &str = "parakeet-tdt";

/// Backend id of the retired previous-generation model.
pub const BACKEND_RETIRED: &str = "parakeet-unified";

/// Retired previous-generation model directory on macOS.
pub const RETIRED_MACOS_SUBDIR: &str = "parakeet-unified-en-0.6b";

/// Retired previous-generation model directory on Windows and other platforms.
pub const RETIRED_DESKTOP_SUBDIR: &str = "unified_static15_b128_sym_bits4_timestamp_hybrid";

/// Where a found model copy lives.
pub const SOURCE_SHARED: &str = "shared";
/// A copy in the app's own data tree.
pub const SOURCE_APP_DATA: &str = "app-data";
/// No usable copy.
pub const SOURCE_MISSING: &str = "missing";

/// User-visible names per model family. The host supplies its own copy.
#[derive(Debug, Clone, Copy)]
pub struct LocalAsrLabels<'a> {
    /// Current-generation model, any dated directory of it.
    pub current: &'a str,
    /// Retired intermediate generation (the old TDT directories).
    pub intermediate: &'a str,
    /// Retired previous generation, and the fallback for unknown directories.
    pub retired: &'a str,
}

impl Default for LocalAsrLabels<'static> {
    fn default() -> Self {
        Self {
            current: "Current model",
            intermediate: "Previous model",
            retired: "Retired model",
        }
    }
}

/// Status of one local ASR backend, in the shape a settings or onboarding UI reads.
#[derive(Debug, Clone, Serialize)]
pub struct LocalAsrModelStatus {
    /// Canonical backend id ([`canonical_local_asr_backend`]).
    pub backend: String,
    /// Family label for the resolved directory.
    pub label: String,
    /// A usable copy exists.
    pub installed: bool,
    /// [`SOURCE_SHARED`], [`SOURCE_APP_DATA`] or [`SOURCE_MISSING`].
    pub source: &'static str,
    /// The current model is selected but a replacement download is still pending.
    pub replacement_required: bool,
}

/// Canonical backend id for a stored alias: [`BACKEND_CURRENT`] stays, anything else is
/// [`BACKEND_RETIRED`].
pub fn canonical_local_asr_backend(alias: &str) -> &'static str {
    match alias.trim().to_ascii_lowercase().as_str() {
        BACKEND_CURRENT => BACKEND_CURRENT,
        _ => BACKEND_RETIRED,
    }
}

/// On-disk subdir for a backend alias on this platform.
pub fn local_asr_subdir(alias: &str) -> &'static str {
    let current = alias.trim().eq_ignore_ascii_case(BACKEND_CURRENT);
    #[cfg(target_os = "macos")]
    {
        if current {
            model_paths::MACOS_TDT_CURRENT_SUBDIR
        } else {
            RETIRED_MACOS_SUBDIR
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if current {
            model_paths::WINDOWS_TDT_CURRENT_SUBDIR
        } else {
            RETIRED_DESKTOP_SUBDIR
        }
    }
}

/// Family label for a model directory name. Any Ultra directory, a future dated one included,
/// is `labels.current`; the retired v3 directories are `labels.intermediate`; everything else
/// is `labels.retired`.
pub fn local_asr_label<'a>(subdir: &str, labels: &LocalAsrLabels<'a>) -> &'a str {
    if subdir == ultra_model::ULTRA_MACOS_ID
        || subdir == ultra_model::ULTRA_WINDOWS_ID
        || subdir.starts_with("parakeet_tdt_ultra")
        || subdir.starts_with("parakeet-tdt-ultra")
    {
        return labels.current;
    }
    match subdir {
        "parakeet-tdt-v3"
        | model_paths::WINDOWS_TDT_OLDEST_SUBDIR
        | model_paths::WINDOWS_TDT_OLDER_SUBDIR
        | model_paths::WINDOWS_TDT_PREVIOUS_SUBDIR => labels.intermediate,
        _ => labels.retired,
    }
}

/// Classifies a found model directory: under `shared_root` is [`SOURCE_SHARED`], elsewhere
/// [`SOURCE_APP_DATA`], none [`SOURCE_MISSING`].
pub fn classify_model_source(found: Option<&Path>, shared_root: &Path) -> &'static str {
    match found {
        Some(dir) if dir.starts_with(shared_root) => SOURCE_SHARED,
        Some(_) => SOURCE_APP_DATA,
        None => SOURCE_MISSING,
    }
}

/// Status of a backend alias against the live store. `replacement_pending` is the host's
/// "a replacement download is outstanding" fact, asked only for the current backend, so a host
/// whose check reads the disk pays for it once and never for the other backends.
pub fn local_asr_model_status(
    backend: &str,
    labels: &LocalAsrLabels<'_>,
    replacement_pending: impl FnOnce() -> bool,
) -> LocalAsrModelStatus {
    let subdir = local_asr_subdir(backend);
    let found = model_paths::resolve_model_dir(subdir);
    let canonical = canonical_local_asr_backend(backend);
    LocalAsrModelStatus {
        backend: canonical.to_string(),
        label: local_asr_label(subdir, labels).to_string(),
        installed: found.is_some(),
        source: classify_model_source(
            found.as_deref(),
            &crate::shared::store::paths::models_root(),
        ),
        replacement_required: canonical == BACKEND_CURRENT && replacement_pending(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_and_label() {
        assert_eq!(
            canonical_local_asr_backend(" Parakeet-TDT "),
            BACKEND_CURRENT
        );
        assert_eq!(canonical_local_asr_backend("x"), BACKEND_RETIRED);
        let l = LocalAsrLabels::default();
        assert_eq!(local_asr_label("parakeet_tdt_ultra_future", &l), l.current);
        assert_eq!(local_asr_label("parakeet-tdt-v3", &l), l.intermediate);
        assert_eq!(local_asr_label("other", &l), l.retired);
    }

    #[test]
    fn source_classification() {
        let root = Path::new("/r");
        assert_eq!(
            classify_model_source(Some(Path::new("/r/a")), root),
            SOURCE_SHARED
        );
        assert_eq!(
            classify_model_source(Some(Path::new("/x")), root),
            SOURCE_APP_DATA
        );
        assert_eq!(classify_model_source(None, root), SOURCE_MISSING);
    }
}
