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
//! In a `test-mode` build `RIGHTKIT_SUITE_ROOT`, `RIGHT_SUITE_DIR` or `ORTHIC_SHARED_DIR`
//! overrides the root (dev/test), resolved through `rightkit_daemon::Suite::resolve`. It is read
//! on every call — never cached. A build without `test-mode` reads none of them.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Legacy alias that overrides the suite store root in `test-mode` builds only. Read live (not
/// OnceLock) so tests can redirect the store without process-global ordering hazards.
pub const SUITE_ROOT_ENV: &str = "RIGHT_SUITE_DIR";
/// Orthic Labs shared-data override (`test-mode` builds only). `RIGHTKIT_SUITE_ROOT` and
/// `RIGHT_SUITE_DIR` take precedence when set.
pub const ORTHIC_SHARED_DIR_ENV: &str = "ORTHIC_SHARED_DIR";
/// The preferred override, honoured in `test-mode` builds only (`rightkit_daemon::ENV_SUITE_ROOT`).
pub const RIGHTKIT_SUITE_ROOT_ENV: &str = rightkit_daemon::ENV_SUITE_ROOT;
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
/// Resolved by [`rightkit_daemon::Suite::resolve`], the one rule every Right Suite crate uses:
///
/// - **With the `test-mode` feature** the override variables `RIGHTKIT_SUITE_ROOT`,
///   `RIGHT_SUITE_DIR` and `ORTHIC_SHARED_DIR` are honoured, in that order. A value that is empty
///   after trimming counts as unset. A relative override, or no override at all with the real
///   `HOME`, fails closed (panics) instead of resolving to the real per-user folder.
/// - **Without it** (every release build) no override variable is read: the root is the operating
///   system's per-user default from the account database.
pub fn suite_root() -> PathBuf {
    // An app-injected files directory (Android) is the root outright: there is no per-user
    // account folder to resolve, and nothing else may redirect it.
    if let Some(files_dir) = crate::android::android_files_dir() {
        return files_dir;
    }
    let resolved = rightkit_daemon::Suite::resolve();
    // `TEST_CAPABILITY` is the daemon's own feature, so it is also true when an app enabled
    // `rightkit-daemon/test-mode` directly instead of this crate's `test-mode`.
    if !rightkit_daemon::mode::TEST_CAPABILITY {
        return match resolved {
            Ok(suite) => suite.root().to_path_buf(),
            Err(_) => default_suite_root(),
        };
    }
    // A test-capable build never reaches the real per-user folder: an unresolvable root, or one
    // that equals the account's real default (no usable override, real HOME), is refused loudly
    // rather than written to. Resolved fresh on every call, never cached.
    let root = match resolved {
        Ok(suite) => suite.root().to_path_buf(),
        Err(err) => panic!("rightkit-models: cannot resolve the suite root in test-mode: {err}"),
    };
    if let Ok(real) = rightkit_daemon::Suite::per_user_default() {
        if real.root() == root {
            panic!(
                "rightkit-models: refusing the real per-user suite root {} in a test-mode build; set RIGHTKIT_SUITE_ROOT (or redirect HOME) to an isolated directory",
                root.display()
            );
        }
    }
    root
}

/// Previous Right Suite registry root retained during compatibility window.
///
/// When [`suite_root`] was redirected (a test-mode override), this is that same root, so isolated
/// runs never see the real legacy folder. Otherwise it is `RightSuite` beside the `Orthic Labs`
/// folder, as before.
pub fn legacy_suite_root() -> PathBuf {
    let root = suite_root();
    // No legacy tree exists inside an injected app directory: stay in it, like a redirected root.
    if crate::android::android_files_dir().is_some() {
        return root;
    }
    match rightkit_daemon::Suite::per_user_default() {
        Ok(default) if default.root() != root => root,
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
/// In a `test-mode` build whose suite root was redirected by an override, this is
/// `<suite root>/app-data`, so nothing that derives an app folder from it can reach the real
/// per-user folder from an isolated run (device ids, sibling-app discovery). Otherwise it is the
/// platform directory, as before.
pub fn app_data_base() -> Option<PathBuf> {
    if crate::android::android_files_dir().is_some() {
        return Some(suite_root().join("app-data"));
    }
    let redirected = rightkit_daemon::Suite::per_user_default()
        .map(|default| default.root() != suite_root())
        .unwrap_or(false);
    if redirected {
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
