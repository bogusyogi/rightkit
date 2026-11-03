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
//! Android uses none of the above: the app injects `Context.getFilesDir()` once
//! (`crate::init_android_files_dir`) and that directory is the root (`<filesDir>/models`).
//! Unverified on a device.
//!
//! The root is `rightkit_daemon::Suite::resolve`, the per-user default, in every build. No
//! environment variable overrides it, and it is read on every call, never cached.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
///
/// Resolved by [`rightkit_daemon::Suite::resolve`], which is always the operating system's
/// per-user default from the account database. No environment variable overrides it.
///
pub fn suite_root() -> PathBuf {
    match checked_suite_root() {
        Ok(root) => root,
        Err(refusal) => panic!("{refusal}"),
    }
}

/// Whether `models_root` is the suite's own models root ([`models_root`]).
///
/// Comparing a path writes nothing, so a root that cannot be resolved answers `false` here
/// instead of panicking: nothing can be the suite's models root when the suite has none. Store
/// code uses this to decide whether the legacy registry beside the suite root is also consulted.
pub fn is_suite_models_root(models_root: &Path) -> bool {
    checked_suite_root()
        .map(|root| models_root_at(&root) == models_root)
        .unwrap_or(false)
}

/// Whether `root` is the suite root itself ([`suite_root`]); same non-panicking contract as
/// [`is_suite_models_root`]. Lease code uses this to decide whether the legacy root's lease is
/// taken alongside an explicit root.
pub fn is_suite_root(root: &Path) -> bool {
    checked_suite_root()
        .map(|suite| suite == root)
        .unwrap_or(false)
}

/// [`suite_root`] with the refusal as the error, in the words [`suite_root`] panics with.
fn checked_suite_root() -> Result<PathBuf, String> {
    // An app-injected files directory (Android) is the root outright: there is no per-user
    // account folder to resolve, and nothing else may redirect it.
    if let Some(files_dir) = crate::android::android_files_dir() {
        return Ok(files_dir);
    }
    // `Suite::resolve` is the per-user default in every build. Resolved fresh on every call.
    Ok(match rightkit_daemon::Suite::resolve() {
        Ok(suite) => suite.root().to_path_buf(),
        Err(_) => default_suite_root(),
    })
}

/// Previous Right Suite registry root retained during compatibility window: `RightSuite` beside
/// the `Orthic Labs` folder.
pub fn legacy_suite_root() -> PathBuf {
    let root = suite_root();
    // No legacy tree exists inside an injected app directory: stay in it, like a redirected root.
    if crate::android::android_files_dir().is_some() {
        return root;
    }
    match rightkit_daemon::Suite::per_user_default() {
        Ok(default) => default
            .root()
            .parent()
            .and_then(Path::parent)
            .map(|base| base.join("RightSuite"))
            .unwrap_or_else(default_legacy_suite_root),
        Err(_) => default_legacy_suite_root(),
    }
}

/// The platform per-user data directory that holds each app's own folder
/// (`<data dir>/<app>/...`; Windows `%LOCALAPPDATA%`, else the roaming/application-support dir),
/// or `None` when the platform has none.
///
/// The platform per-user data directory. Only an app-injected Android files directory replaces it
/// (`<files dir>/app-data`).
pub fn app_data_base() -> Option<PathBuf> {
    if crate::android::android_files_dir().is_some() {
        return Some(suite_root().join("app-data"));
    }
    #[cfg(windows)]
    {
        dirs::data_local_dir()
    }
    #[cfg(not(windows))]
    {
        dirs::data_dir()
    }
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

/// `<root>/models` under an explicit root (an app's own directory, or a sandbox).
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

/// Names under `<tools>/<kind>/` that belong to the fixed in-place runtime path, never to a version.
pub const SHARED_RUNTIME_CURRENT: &str = "current";
pub const SHARED_RUNTIME_PREVIOUS: &str = "current.previous";
pub const SHARED_RUNTIME_STAGING: &str = "current.staging";

/// True for the fixed in-place runtime names (`current`, `current.previous`, `current.staging`).
pub fn is_shared_runtime_fixed_name(name: &str) -> bool {
    matches!(
        name,
        SHARED_RUNTIME_CURRENT | SHARED_RUNTIME_PREVIOUS | SHARED_RUNTIME_STAGING
    )
}

/// The ONE path a shared runtime is loaded from: `<tools root>/<kind>/current`. A runtime library
/// (for example the ONNX Runtime dylib) must be loaded from here, never from
/// `<kind>/<version>`: macOS and Windows tie privacy grants to the path the code runs from, and a
/// per-version folder is a new path on every update. Use [`crate::Store::activate_shared_runtime`]
/// to install a version here in place.
pub fn shared_runtime_current_dir_at(
    tools_root: &Path,
    kind: &str,
) -> crate::error::Result<PathBuf> {
    let kind = crate::spec::parse_shared_kind(kind)?;
    Ok(tools_root.join(kind).join(SHARED_RUNTIME_CURRENT))
}

/// As [`shared_runtime_current_dir_at`], under the suite tools root.
pub fn shared_runtime_current_dir(kind: &str) -> crate::error::Result<PathBuf> {
    shared_runtime_current_dir_at(&tools_root(), kind)
}

/// Validate a shared-runtime `(kind, version)` pair as safe path segments.
pub(crate) fn parse_shared_runtime(
    kind: &str,
    version: &str,
) -> crate::error::Result<(String, String)> {
    let kind = crate::spec::parse_shared_kind(kind)?;
    if is_shared_runtime_fixed_name(version) {
        return Err(crate::error::Error::new(
            crate::error::ErrorCode::InvalidSpec,
            format!("shared-runtime version {version:?} is reserved for the fixed in-place path"),
        ));
    }
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
