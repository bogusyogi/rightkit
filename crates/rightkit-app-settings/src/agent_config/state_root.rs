//! The the host app state root.
//!
//! This is the rightkit-daemon `AppRoot`: `<platform data dir>/Orthic Labs/the host app`
//! (`%LOCALAPPDATA%\Orthic Labs\the host app` on Windows,
//! `~/Library/Application Support/Orthic Labs/the host app` on macOS and iOS,
//! `~/.local/share/Orthic Labs/the host app` on Linux). Every the host app process resolves it through
//! [`state_root`]: the CLI, the daemon (config, hooks, agents, `daemon.port`, journals), the
//! desktop shell (through the daemon client), the updater and the support bundle. Nothing else
//! computes it.
//!
//! Override: none in a release build. A build with the `test-mode` feature (QA and tests only)
//! honours `RIGHTKIT_SUITE_ROOT` (the app root is then `<that>/apps/the host app`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use rightkit_daemon::{Suite, SuiteError};

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

type Resolved = Option<PathBuf>;

fn cache() -> &'static Mutex<HashMap<PathBuf, Resolved>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Resolved>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The suite this process works in.
fn process_suite() -> Result<Suite, SuiteError> {
    #[cfg(target_os = "ios")]
    {
        // rightkit-daemon has no per-user default for iOS; the app sandbox's Application Support
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
    #[cfg(all(not(target_os = "ios"), test))]
    {
        if std::env::var_os(rightkit_daemon::ENV_SUITE_ROOT).is_some_and(|value| !value.is_empty())
        {
            Suite::resolve()
        } else {
            Suite::at(
                std::env::temp_dir()
                    .join(format!("rightkit-agent-test-state-{}", std::process::id()))
                    .join("Orthic Labs")
                    .join("shared"),
            )
        }
    }
    #[cfg(all(not(target_os = "ios"), not(test), feature = "agent_config-test-mode"))]
    {
        // Test and QA builds: an explicit `RIGHTKIT_SUITE_ROOT` wins. Without one, fail safe to a
        // per-process temp suite, never the user's real per-user root (a test that forgets the
        // variable must not read or write real data).
        if std::env::var_os(rightkit_daemon::ENV_SUITE_ROOT).is_some_and(|value| !value.is_empty())
        {
            Suite::resolve()
        } else {
            Suite::at(
                std::env::temp_dir()
                    .join(format!("rightkit-agent-test-state-{}", std::process::id()))
                    .join("Orthic Labs")
                    .join("shared"),
            )
        }
    }
    #[cfg(all(
        not(target_os = "ios"),
        not(test),
        not(feature = "agent_config-test-mode")
    ))]
    {
        // A release build (no `test-mode`) reads no environment here: the root comes from the
        // account database / known-folder table.
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

/// [`state_root`] for the startup path of a process that cannot run without durable state: when the
/// platform data dir does not resolve it prints the reason and exits with status 2. There is no
/// fallback location (clean port: a missing state root fails, it never redirects state into the
/// working directory or the temp dir).
pub fn state_root_required() -> PathBuf {
    state_root().unwrap_or_else(|| {
        eprintln!("cannot continue without a state directory; refusing to fall back");
        std::process::exit(2)
    })
}

/// Whether this process runs against an isolated root (`test-mode` build with
/// `RIGHTKIT_SUITE_ROOT` set). Always `false` in a release build. Stores that live outside the
/// state root in production (the database next to the installed executable) use this to stay
/// inside the isolated root during QA.
pub fn state_root_is_isolated() -> bool {
    process_suite().is_ok_and(|suite| suite.env_derived())
}

/// The operating system home directory of the current user, with no environment override. For
/// user-owned files that live outside the state root (for example `~/.config/host app/mcp.json`
/// and other tools' dot directories). Never join the host app state onto it; use [`state_root`].
pub fn os_home_dir() -> Option<PathBuf> {
    // Test and QA builds honour the harness's isolated home (`HOME`, then `USERPROFILE`):
    // `dirs::home_dir` reads the Windows profile folder from the OS and ignores both, which let
    // e2e daemons write `~/.config/host app/mcp.json` into the real user's home. A release
    // build reads no environment here.
    #[cfg(all(not(target_os = "ios"), any(test, feature = "agent_config-test-mode")))]
    for name in ["HOME", "USERPROFILE"] {
        if let Some(home) = std::env::var_os(name).filter(|value| !value.is_empty()) {
            return Some(PathBuf::from(home));
        }
    }
    dirs::home_dir()
}

/// [`os_home_dir`] for a startup path that needs it: prints the reason and exits with status 2
/// when the home directory does not resolve, with no working-directory fallback.
pub fn os_home_dir_required() -> PathBuf {
    os_home_dir().unwrap_or_else(|| {
        eprintln!("cannot continue without a home directory; refusing to fall back");
        std::process::exit(2)
    })
}
