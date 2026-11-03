//! Per-user, per-repository receipts root shared by receipt writers and readers.
//!
//! Receipts live in one per-user state directory keyed by repository identity,
//! never beneath whatever directory a process happened to start in.
//!
//! The directory names and the override environment variable belong to the
//! consuming application. It declares them once at start-up with
//! [`set_state_identity`]; without that call the neutral [`StateIdentity::default`]
//! (`RIGHTKIT_STATE_ROOT`, `RightKit`) applies.

use crate::canonical::canonical_digest;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Default environment override honoured by every receipt writer.
pub const STATE_ROOT_ENV: &str = "RIGHTKIT_STATE_ROOT";

/// Application-owned names that place the per-user state directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateIdentity {
    /// Environment variable that overrides the state root when set and non-empty.
    pub env_var: String,
    /// Vendor directory on macOS and Windows (for example `Example Co`).
    pub vendor_dir: String,
    /// Application directory under the vendor directory on macOS and Windows.
    pub app_dir: String,
    /// Directory under `$XDG_STATE_HOME` (or `~/.local/state`) on other platforms.
    pub xdg_dir: String,
}

impl Default for StateIdentity {
    fn default() -> Self {
        Self {
            env_var: STATE_ROOT_ENV.to_string(),
            vendor_dir: "RightKit".to_string(),
            app_dir: "RightKit".to_string(),
            xdg_dir: "rightkit".to_string(),
        }
    }
}

static IDENTITY: OnceLock<StateIdentity> = OnceLock::new();

/// Declare the application's state identity. The first call wins; it returns
/// `false` when an identity was already set.
pub fn set_state_identity(identity: StateIdentity) -> bool {
    IDENTITY.set(identity).is_ok()
}

/// The state identity in effect for this process.
pub fn state_identity() -> &'static StateIdentity {
    IDENTITY.get_or_init(StateIdentity::default)
}

/// The environment variable that overrides the state root for this process.
pub fn state_root_env() -> &'static str {
    &state_identity().env_var
}

/// The per-user receipts directory for this platform, before repository keying.
pub fn user_state_receipts_root(
    macos: bool,
    windows: bool,
    home: Option<PathBuf>,
    user_profile: Option<PathBuf>,
    local_app_data: Option<PathBuf>,
    xdg_state_home: Option<PathBuf>,
) -> Option<PathBuf> {
    let identity = state_identity();
    let non_empty = |value: Option<PathBuf>| value.filter(|path| !path.as_os_str().is_empty());
    if macos {
        return Some(
            non_empty(home)?
                .join("Library")
                .join("Application Support")
                .join(&identity.vendor_dir)
                .join(&identity.app_dir)
                .join("state")
                .join("receipts"),
        );
    }
    if windows {
        let base = non_empty(local_app_data).or_else(|| {
            non_empty(user_profile).map(|profile| profile.join("AppData").join("Local"))
        })?;
        return Some(
            base.join(&identity.vendor_dir)
                .join(&identity.app_dir)
                .join("state")
                .join("receipts"),
        );
    }
    let base = non_empty(xdg_state_home)
        .or_else(|| non_empty(home).map(|home| home.join(".local").join("state")))?;
    Some(base.join(&identity.xdg_dir).join("receipts"))
}

/// Receipts root for a repository identity (its cwd) under the per-user state
/// directory for the running platform.
pub fn per_user_repository_receipts_root(identity: &str) -> Option<PathBuf> {
    let digest = canonical_digest(&identity.to_string()).ok()?;
    let key = digest.trim_start_matches("sha256:");
    let key = key.get(..16).unwrap_or(key);
    let base = user_state_receipts_root(
        cfg!(target_os = "macos"),
        cfg!(windows),
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var_os("USERPROFILE").map(PathBuf::from),
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
    )?;
    Some(base.join(key))
}

/// Receipts root for `cwd`: `$<state root env>/receipts` when set, otherwise
/// the per-user, per-repository directory.
pub fn receipts_root_for_cwd(cwd: &Path) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os(state_root_env()).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(root).join("receipts"));
    }
    per_user_repository_receipts_root(&cwd.to_string_lossy())
}
