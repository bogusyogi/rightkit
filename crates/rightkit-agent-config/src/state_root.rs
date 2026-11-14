//! The the host app state root.
//!
//! This is the rightkit-roots `AppRoot`: `<platform data dir>/Orthic Labs/the host app`
//! (`%LOCALAPPDATA%\Orthic Labs\the host app` on Windows,
//! `~/Library/Application Support/Orthic Labs/the host app` on macOS and iOS,
//! `~/.local/share/Orthic Labs/the host app` on Linux). Every the host app process resolves it through
//! [`state_root`]: the CLI, the daemon (config, hooks, agents, `daemon.port`, journals), the
//! desktop shell (through the daemon client), the updater and the support bundle. Nothing else
//! computes it.
//!
//! Override: none. The root comes from the account database and the known-folder table.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use rightkit_roots::{Suite, SuiteError};

/// Default folder name under `Orthic Labs` until the host calls [`set_app_name`].
pub const APP_NAME: &str = "Agent";

static HOST_APP_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Set the host's state folder name under `Orthic Labs` (for example `MyApp`). Call once at
/// startup before [`state_root`]; returns `false` if a name was already set.
pub fn set_app_name(name: &str) -> bool {
    HOST_APP_NAME.set(name.to_string()).is_ok()
}

/// The state folder name in effect.
pub fn app_name() -> &'static str {
    HOST_APP_NAME.get().map(String::as_str).unwrap_or(APP_NAME)
}

/// Workspace/user state folder name (`<workspace>/<name>/config.toml`, ...) until the host calls
/// [`set_state_dir_name`].
pub const DEFAULT_STATE_DIR_NAME: &str = ".rightkit";

/// Environment-variable prefix (without the trailing `_`) until the host calls [`set_env_prefix`].
pub const DEFAULT_ENV_PREFIX: &str = "RIGHTKIT";

static HOST_STATE_DIR_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static HOST_ENV_PREFIX: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Set the host's workspace state folder name, with its leading dot (for example `.myapp`).
/// Call once at startup; returns `false` if a name was already set.
pub fn set_state_dir_name(name: &str) -> bool {
    HOST_STATE_DIR_NAME.set(name.to_string()).is_ok()
}

/// The workspace state folder name in effect.
pub fn state_dir_name() -> &'static str {
    HOST_STATE_DIR_NAME
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_STATE_DIR_NAME)
}

/// Set the host's environment-variable prefix, upper case, no trailing underscore (for example
/// `MYAPP` reads `MYAPP_MODEL`). Call once at startup; returns `false` if one was already set.
pub fn set_env_prefix(prefix: &str) -> bool {
    HOST_ENV_PREFIX.set(prefix.to_string()).is_ok()
}

/// The environment-variable prefix in effect.
pub fn env_prefix() -> &'static str {
    HOST_ENV_PREFIX
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_ENV_PREFIX)
}

/// `<prefix>_<suffix>`, the host's environment variable name for `suffix` (for example `MODEL`).
pub fn env_name(suffix: &str) -> String {
    format!("{}_{suffix}", env_prefix())
}

type Resolved = Option<PathBuf>;

fn cache() -> &'static Mutex<HashMap<PathBuf, Resolved>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Resolved>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The suite this process works in.
fn process_suite() -> Result<Suite, SuiteError> {
    #[cfg(target_os = "ios")]
    {
        // rightkit-roots has no per-user default for iOS; the app sandbox's Application Support
        // directory is the platform data dir, so the same `Orthic Labs/the host app` layout applies.
        let home = dirs::home_dir()
            .ok_or_else(|| SuiteError::NoRoot("the app sandbox has no home directory".into()))?;
        Suite::at(
            home.join("Library")
                .join("Application Support")
                .join("Orthic Labs")
                .join("shared"),
        )
    }
    #[cfg(not(target_os = "ios"))]
    {
        // Reads no environment here: the root comes from the account database / known-folder table.
        Suite::resolve()
    }
}

fn open(suite: &Suite) -> Resolved {
    let app = match suite.app_root(app_name()) {
        Ok(app) => app,
        Err(error) => {
            eprintln!("rightkit-agent: the state directory is unavailable: {error}");
            return None;
        }
    };
    Some(app.root().to_path_buf())
}

/// The host app state directory (`<platform data dir>/Orthic Labs/<app name>`). `None` when the
/// platform data dir cannot be determined (the reason is printed once to stderr).
pub fn state_root() -> Option<PathBuf> {
    let suite = match process_suite() {
        Ok(suite) => suite,
        Err(error) => {
            eprintln!("rightkit-agent: the state directory is unavailable: {error}");
            return None;
        }
    };
    let key = suite.root().to_path_buf();
    let mut cache = cache().lock().ok()?;
    cache.entry(key).or_insert_with(|| open(&suite)).clone()
}

/// Whether this process's state root comes from the environment (an explicit suite root) instead
/// of the per-user default.
pub fn state_root_is_isolated() -> bool {
    process_suite().is_ok_and(|suite| suite.env_derived())
}

/// The operating system home directory of the current user, with no environment override. For
/// user-owned files that live outside the state root (for example `~/.config/host app/mcp.json`
/// and other tools' dot directories). Never join the host app state onto it; use [`state_root`].
pub fn os_home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}
