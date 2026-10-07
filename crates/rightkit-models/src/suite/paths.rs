//! Shared Orthic Labs store locations.
//!
//! Store roots are shared by every Orthic Labs app on the machine:
//!
//! - Windows: `%LOCALAPPDATA%\Orthic Labs\shared\`
//! - macOS:   `~/Library/Application Support/Orthic Labs/shared/`
//! - other:   `~/.local/share/Orthic Labs/shared/`
//!
//! Layout:
//! - `<root>/models/<model-id>/<files>` plus `<model-id>/manifest.json`
//! - `<root>/models/refs/<app>.json` — per-app usage registry
//! - `<root>/tools/<tool>/<version>/<files>` plus `manifest.json`
//! - `<root>/tools/refs/<app>.json` — per-app tool usage registry
//!
//! `RIGHT_SUITE_DIR` overrides the root (dev/test). It is read on every call —
//! never cached — so tests and dev shells can point the store at a temp dir
//! without leaking state across the process.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Env var that overrides the suite store root. Read live (not OnceLock) so
/// tests can redirect the store without process-global ordering hazards.
pub const SUITE_ROOT_ENV: &str = "RIGHT_SUITE_DIR";
/// Orthic Labs shared-data override. RIGHT_SUITE_DIR remains a test/dev
/// override and takes precedence when set.
pub const ORTHIC_SHARED_DIR_ENV: &str = "ORTHIC_SHARED_DIR";
/// Last ScrapeRight release eligible for automatic legacy-root migration.
pub const LEGACY_COMPAT_MAX_APP_VERSION: &str = "0.1.12";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyCompatibility {
    pub max_app_version: String,
    pub release_window: u8,
}

pub fn legacy_compatibility() -> LegacyCompatibility {
    LegacyCompatibility {
        max_app_version: LEGACY_COMPAT_MAX_APP_VERSION.to_string(),
        release_window: 2,
    }
}

/// Whether `app_version` is still inside the two-release legacy window that
/// ends at ScrapeRight's [`LEGACY_COMPAT_MAX_APP_VERSION`]. The caller supplies
/// its own release version (the old crate read ScrapeRight's `package.json`).
pub fn legacy_locations_supported_for(app_version: &str) -> bool {
    version_within(app_version, LEGACY_COMPAT_MAX_APP_VERSION)
}

/// `app_version <= max_version`, comparing dotted numeric triples. Unparseable
/// input is outside the window (fail closed).
pub fn version_within(app_version: &str, max_version: &str) -> bool {
    fn parse(version: &str) -> Option<(u64, u64, u64)> {
        let mut parts = version.split('.').map(|part| part.parse().ok());
        Some((
            parts.next()??,
            parts.next()??,
            parts.next().unwrap_or(Some(0))?,
        ))
    }
    parse(app_version)
        .zip(parse(max_version))
        .map(|(current, max)| current <= max)
        .unwrap_or(false)
}

/// Suite-wide root directory shared by all Orthic Labs apps.
pub fn suite_root() -> PathBuf {
    if let Some(dir) = std::env::var_os(SUITE_ROOT_ENV) {
        let dir = PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return dir;
        }
    }
    if let Some(dir) = std::env::var_os(ORTHIC_SHARED_DIR_ENV) {
        let dir = PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return dir;
        }
    }
    default_suite_root()
}

/// Previous Right Suite registry root retained during compatibility window.
pub fn legacy_suite_root() -> PathBuf {
    if let Some(dir) = std::env::var_os(SUITE_ROOT_ENV) {
        let dir = PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return dir;
        }
    }
    default_legacy_suite_root()
}

fn default_legacy_suite_root() -> PathBuf {
    #[cfg(windows)]
    {
        dirs::data_local_dir()
            .unwrap_or_else(|| std::env::temp_dir().join("Orthic Labs"))
            .join("RightSuite")
    }
    #[cfg(not(windows))]
    {
        dirs::data_dir()
            .unwrap_or_else(|| std::env::temp_dir().join("Orthic Labs"))
            .join("RightSuite")
    }
}

fn default_suite_root() -> PathBuf {
    #[cfg(windows)]
    {
        dirs::data_local_dir()
            .unwrap_or_else(|| std::env::temp_dir().join("Orthic Labs"))
            .join("Orthic Labs")
            .join("shared")
    }
    #[cfg(target_os = "macos")]
    {
        dirs::data_dir()
            .unwrap_or_else(|| std::env::temp_dir().join("Orthic Labs"))
            .join("Orthic Labs")
            .join("shared")
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        dirs::data_dir()
            .unwrap_or_else(|| std::env::temp_dir().join("Orthic Labs"))
            .join("Orthic Labs")
            .join("shared")
    }
}

/// `<root>/models` — shared model store.
pub fn models_root() -> PathBuf {
    suite_root().join("models")
}

/// `<root>/models` under an explicit root (test/dev injection point).
pub fn models_root_at(root: &Path) -> PathBuf {
    root.join("models")
}

/// `<root>/models/refs` — per-app model reference registry.
pub fn model_refs_dir_at(models_root: &Path) -> PathBuf {
    models_root.join("refs")
}

/// `<root>/tools` — shared tools store (yt-dlp, deno, …).
pub fn tools_root() -> PathBuf {
    suite_root().join("tools")
}

/// `<root>/tools` under an explicit root.
pub fn tools_root_at(root: &Path) -> PathBuf {
    root.join("tools")
}

/// `<root>/tools/refs` — per-app tool reference registry.
pub fn tool_refs_dir_at(tools_root: &Path) -> PathBuf {
    tools_root.join("refs")
}

/// One model's directory inside a models root.
pub fn model_dir_at(models_root: &Path, model_id: &str) -> PathBuf {
    models_root.join(model_id)
}

/// One tool version's directory inside a tools root.
pub fn tool_dir_at(tools_root: &Path, tool: &str, version: &str) -> PathBuf {
    tools_root.join(tool).join(version)
}

/// Directory of shared runtime `kind` at `version` under the suite tools root
/// (`<root>/tools/<kind>/<version>`). `kind` and `version` must each be a
/// single safe path segment (model-id rules: ASCII letters, digits, `.`, `_`,
/// `-`; no separators, no `..`; `refs` is reserved), else `InvalidSpec`.
pub fn shared_runtime_dir(kind: &str, version: &str) -> crate::error::Result<PathBuf> {
    shared_runtime_dir_at(&tools_root(), kind, version)
}

/// As [`shared_runtime_dir`], under an explicit tools root.
pub fn shared_runtime_dir_at(
    tools_root: &Path,
    kind: &str,
    version: &str,
) -> crate::error::Result<PathBuf> {
    let (kind, version) = parse_shared_runtime(kind, version)?;
    Ok(tool_dir_at(tools_root, &kind, &version))
}

/// Validate a shared-runtime `(kind, version)` pair as safe path segments.
pub(crate) fn parse_shared_runtime(
    kind: &str,
    version: &str,
) -> crate::error::Result<(String, String)> {
    let kind = crate::spec::parse_shared_kind(kind)?;
    let version = crate::ids::ModelId::parse(version).map_err(|_| {
        crate::error::Error::new(
            crate::error::ErrorCode::InvalidSpec,
            format!("invalid shared-runtime version {version:?}"),
        )
    })?;
    Ok((kind, version.as_str().to_string()))
}

/// The manifest file inside a store entry dir.
pub fn manifest_path(entry_dir: &Path) -> PathBuf {
    entry_dir.join("manifest.json")
}
