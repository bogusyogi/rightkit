//! The one resolver for a dictation app's per-user app-data root, shared by every crate that
//! reads or writes it (telemetry, settings, the engine runtime).
//!
//! The host names its data folder once at startup with [`configure_app_data_dir_name`]. The
//! production root is then the shared owner's (`rightkit_fs::app_dirs`) machine-local folder for
//! that flat name, with the dev namespace and the QA override variables switched off:
//!
//! | OS | production root |
//! |---|---|
//! | Windows | `<Local AppData known folder>\<name>` |
//! | macOS | `$HOME/Library/Application Support/<name>` |
//! | other Unix | `$XDG_DATA_HOME` (else `$HOME/.local/share`) `/<name>` |
//!
//! Two rules compile only under `cfg(feature = "test-mode")`: the direct-root
//! override variable `RIGHTKIT_APP_DATA_DIR` and the test-binary guard, which keeps any test
//! process off the real user folder. A release build reads neither.

#[cfg(feature = "test-mode")]
use std::path::PathBuf;

use rightkit_fs::app_dirs::{AppDirs, AppId, Options};
#[cfg(feature = "test-mode")]
use rightkit_fs::app_dirs::{AppDirsError, Os};

static APP_DATA_DIR_NAME: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// Folder name test and `test-mode` builds use when the host never configured one.
pub const TEST_APP_DATA_DIR_NAME: &str = "RightKitDictationTest";

/// Name the app's data folder (for example `MyAppNext`). Call once, before anything resolves a
/// path. Returns false when a different name was already configured (the first one stays).
pub fn configure_app_data_dir_name(name: &'static str) -> bool {
    APP_DATA_DIR_NAME.set(name).is_ok() || APP_DATA_DIR_NAME.get() == Some(&name)
}

/// The configured data folder name.
///
/// # Panics
///
/// In a release build, when [`configure_app_data_dir_name`] was never called: app data must
/// never land in a guessed folder.
pub fn app_data_dir_name() -> &'static str {
    if let Some(name) = APP_DATA_DIR_NAME.get() {
        return name;
    }
    #[cfg(feature = "test-mode")]
    {
        TEST_APP_DATA_DIR_NAME
    }
    #[cfg(not(feature = "test-mode"))]
    {
        panic!("app data folder name not configured: call app_paths::configure_app_data_dir_name at startup")
    }
}

fn app_id() -> AppId {
    AppId::new(app_data_dir_name()).without_org()
}

/// Test binaries never resolve the real per-user folder. Cargo puts every unit and
/// integration test binary in `<target>/<profile>/deps`; with no `RIGHTKIT_APP_DATA_DIR`
/// such a process gets a per-process temp root. Sidecars a test spawns inherit it
/// through the `RIGHTKIT_APP_DATA_DIR` the supervisor passes. On Windows a redirected
/// `LOCALAPPDATA` would not stop a Known Folder lookup, so the guard sits here.
///
/// Release builds have no guard: the function exists so callers compile unchanged, and returns `None`.
pub fn test_binary_data_root() -> Option<PathBuf> {
    #[cfg(feature = "test-mode")]
    {
        test_binary_data_root_impl()
    }
    #[cfg(not(feature = "test-mode"))]
    {
        None
    }
}

#[cfg(feature = "test-mode")]
fn test_binary_data_root_impl() -> Option<PathBuf> {
    static ROOT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        // The RightKit broker marks test jobs, and what they spawn, with RIGHTKIT_UNDER_TEST=1.
        let marked = std::env::var("RIGHTKIT_UNDER_TEST").as_deref() == Ok("1");
        let exe = std::env::current_exe().ok()?;
        (marked || exe.parent()?.file_name()? == "deps").then(|| {
            std::env::temp_dir()
                .join(format!(
                    "rightkit-dictation-test-data-{}",
                    std::process::id()
                ))
                .join(app_data_dir_name())
        })
    })
    .clone()
}

/// True when `app_data_root()` resolves via the explicit test override rather than the real
/// per-user OS data directory. Always false in a release build.
pub fn app_data_root_is_explicit_override() -> bool {
    #[cfg(feature = "test-mode")]
    {
        std::env::var_os("RIGHTKIT_APP_DATA_DIR")
            .filter(|path| !path.is_empty())
            .is_some()
    }
    #[cfg(not(feature = "test-mode"))]
    {
        false
    }
}

/// Test builds only: the release resolver asks the owner (account database / known folders) and
/// has no environment fallback.
///
/// `local` for the configured flat name from an explicit environment, release-shaped (no dev
/// namespace, no QA variables, no test root). On Windows the owner insists on `APPDATA` and
/// `USERPROFILE` as well, but only `LOCALAPPDATA` decides this folder, so a profile that lacks
/// the other two resolves from `LOCALAPPDATA` alone, as it always did.
#[cfg(feature = "test-mode")]
fn local_root_from(
    os: Os,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, AppDirsError> {
    let stand_in = |key: &str| {
        env(key).or_else(|| {
            (os == Os::Windows && matches!(key, "APPDATA" | "USERPROFILE"))
                .then(|| env("LOCALAPPDATA"))
                .flatten()
        })
    };
    AppDirs::resolve_with_legacy(&app_id(), &Options::new(os), &stand_in, &|_| false)
        .map(|dirs| dirs.local)
}

/// The installed-build location: the owner's machine-local folder for the configured name, never
/// the dev namespace and never a QA override. When the OS gives no usable base folder this
/// panics with the reason: app data must never land in the current directory.
///
/// Every telemetry line resolves its path through here. The owner caches the Windows
/// known-folder resolution per process (rightkit-fs 0.4.8, rightkit#1337); uncached, it cost
/// ~12 ms per call.
pub fn production_data_root() -> PathBuf {
    resolve_production_data_root()
}

fn resolve_production_data_root() -> PathBuf {
    let options = Options::current().dev(false).qa_overrides(false);
    let resolved = AppDirs::resolve_with_options(app_id(), &options).map(|dirs| dirs.local);
    #[cfg(feature = "test-mode")]
    let resolved =
        resolved.or_else(|_| local_root_from(Os::current(), &|key| std::env::var_os(key)));
    resolved.unwrap_or_else(|error| {
            panic!(
                "cannot resolve the per-user app data folder ({error}); refusing to use the current directory"
            )
        })
}

pub fn app_data_root() -> PathBuf {
    #[cfg(feature = "test-mode")]
    {
        if let Some(path) =
            std::env::var_os("RIGHTKIT_APP_DATA_DIR").filter(|path| !path.is_empty())
        {
            return PathBuf::from(path);
        }
        if let Some(root) = test_binary_data_root() {
            return root;
        }
    }
    production_data_root()
}
