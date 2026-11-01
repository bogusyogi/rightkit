//! Host services for RightKit companion crates (chat bridge, AI accounts, agent gateway, approvals,
//! web session). Those crates were moved out of one app and still need a few answers only the app
//! can give: where its state folder is, which folder is the account's home, and what its UI last
//! published (its settings and state document). The app installs an [`AppHost`] once at start with
//! [`install`]; until it does, every crate sees [`NeutralHost`], which keeps no state folder and
//! reports empty settings.
//!
//! ```
//! use rightkit_app_host::{host, AppHost, install};
//! use std::path::PathBuf;
//!
//! struct MyApp;
//! impl AppHost for MyApp {
//!     fn state_root(&self) -> Option<PathBuf> {
//!         Some(std::env::temp_dir().join("my-app-state"))
//!     }
//!     fn product_name(&self) -> &str {
//!         "MyApp"
//!     }
//! }
//! install(Box::new(MyApp));
//! assert_eq!(host().product_name(), "MyApp");
//! ```

use serde_json::Value;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;

/// The default name of the account's secondary-profiles folder (`<home>/ClaudeProfiles/<name>`),
/// used by [`AppHost::account_home`] and [`AppHost::env_var`] unless the host overrides them.
pub const DEFAULT_PROFILES_DIR: &str = "ClaudeProfiles";

/// What the app tells the companion crates. Every method has a neutral default.
pub trait AppHost: Send + Sync {
    /// The app's own per-user state folder, or `None` when it keeps none (the default).
    fn state_root(&self) -> Option<PathBuf> {
        None
    }

    /// The product name shown in messages (default `RightKit`).
    fn product_name(&self) -> &str {
        "RightKit"
    }

    /// The account's own home folder: `HOME` (`USERPROFILE` on Windows) unless it lies inside the
    /// account's secondary-profiles folder, where the account's real home wins.
    fn account_home(&self) -> PathBuf {
        rightkit_fs::profile_home::profile_aware_home(DEFAULT_PROFILES_DIR)
    }

    /// An environment variable as per-app folder resolution should read it (profile-aware).
    fn env_var(&self, key: &str) -> Option<OsString> {
        rightkit_fs::profile_home::profile_aware_env_var(key, DEFAULT_PROFILES_DIR)
    }

    /// The UI's published settings object (`Value::Null` when there is none, the default). Every
    /// switch a companion crate reads from it defaults on when absent.
    fn settings(&self) -> Value {
        Value::Null
    }

    /// The UI's whole published state document, when the app has one and it is the app's own.
    fn published_state(&self) -> Option<Value> {
        None
    }
}

/// The host used before [`install`]: no state folder, empty settings, profile-aware home.
#[derive(Debug, Default, Clone, Copy)]
pub struct NeutralHost;

impl AppHost for NeutralHost {}

static HOST: OnceLock<Box<dyn AppHost>> = OnceLock::new();

/// Install the app's host. Only the first call takes effect; returns `false` otherwise (also when
/// a companion crate already read the host, which fixes [`NeutralHost`]).
pub fn install(host: Box<dyn AppHost>) -> bool {
    HOST.set(host).is_ok()
}

/// The installed host, or [`NeutralHost`] when the app installed none.
pub fn host() -> &'static dyn AppHost {
    HOST.get_or_init(|| Box::new(NeutralHost)).as_ref()
}

/// [`AppHost::state_root`] of the installed host.
pub fn state_root() -> Option<PathBuf> {
    host().state_root()
}

/// [`AppHost::state_root`], or a `NotFound` error naming what is missing.
pub fn state_directory() -> std::io::Result<PathBuf> {
    state_root().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "local metadata directory unavailable",
        )
    })
}

/// [`AppHost::account_home`] of the installed host.
pub fn account_home() -> PathBuf {
    host().account_home()
}

/// [`AppHost::env_var`] of the installed host.
pub fn env_var(key: &str) -> Option<OsString> {
    host().env_var(key)
}

/// [`AppHost::settings`] of the installed host.
pub fn settings() -> Value {
    host().settings()
}

/// [`AppHost::published_state`] of the installed host.
pub fn published_state() -> Option<Value> {
    host().published_state()
}

/// A boolean switch from [`settings`], `default` when absent or not a boolean.
pub fn setting_bool(key: &str, default: bool) -> bool {
    settings()
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}
