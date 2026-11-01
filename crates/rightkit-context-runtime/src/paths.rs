//! Stable roots for the Membrane runtime, resolved by `rightkit_fs::AppDirs`.
//!
//! The installed state root predates the host app and does not move, so existing
//! installed data carries over. `RIGHTKIT_CONTEXT_STATE_ROOT` and `RIGHTKIT_CONTEXT_CACHE_ROOT`
//! pin a scratch tree for tests and operators.

use std::path::PathBuf;

use rightkit_fs::app_dirs::{AppDirs, AppId, Options};

/// Name of the product as it appears under the user's data tree.
pub const PRODUCT_DIR_NAME: &str = "Membrane";

/// Resolve `Membrane` folders from the process environment: release namespace
/// (never `MembraneDev`), no QA override, no test root.
fn resolve(app: AppId) -> Option<AppDirs> {
    let options = Options::current()
        .dev(false)
        .qa_overrides(false)
        .test_root(None);
    AppDirs::resolve_with(&app, &options, &|key| std::env::var_os(key)).ok()
}

fn override_dir(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// Stable cache root: regenerable caches, `AppDirs::cache`: `~/Library/Caches/Membrane` on the
/// Mac, `%LOCALAPPDATA%\Membrane\Cache` on Windows (never the state folder).
pub fn cache_root() -> PathBuf {
    if let Some(dir) = override_dir(crate::qa_env::cache_root()) {
        return dir;
    }
    resolve(AppId::new(PRODUCT_DIR_NAME).without_org())
        .map(|dirs| dirs.cache)
        .unwrap_or_else(|| PathBuf::from(PRODUCT_DIR_NAME))
}

/// Installed state root: `<data base>/Orthic Labs/Membrane/state`, with
/// `%LOCALAPPDATA%` as the Windows base and `~/Library/Application Support` as
/// the Mac base. Holds the databases, `run/`, `resident.json` and `models/`.
pub fn state_root() -> PathBuf {
    if let Some(dir) = override_dir(crate::qa_env::state_root()) {
        return dir;
    }
    resolve(AppId::new(PRODUCT_DIR_NAME))
        .map(|dirs| dirs.local)
        .unwrap_or_else(|| PathBuf::from(PRODUCT_DIR_NAME))
        .join("state")
}

/// `state/resident.json`: `{pid, endpoint, generation, started, ts}`.
pub fn resident_json_path() -> PathBuf {
    state_root().join("resident.json")
}
