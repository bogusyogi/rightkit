//! Where a document viewer keeps its own data: the `<vendor>/<app>` app root, resolved through
//! `rightkit-daemon` (`mode::roots`, [`AppRoot`]), plus the one-time move of an older install's
//! data into it.
//!
//! * **Root.** The app root under the platform data folder. Every store lives under it. A test run
//!   (decided by `rightkit_daemon::mode`) puts the same layout inside its validated test root, so a
//!   test run never reaches the real folders.
//! * **Migration.** Resolving the root moves an older install's folder in through
//!   `AppRoot::migrate_from` before any path is handed out, so no store can open ahead of it. A
//!   migration error is final for the session: the error is kept and no path is returned. There
//!   is no fallback to a fresh or in-memory store.
//!
//! The app describes itself with an [`AppPathsConfig`] and settles the root once with
//! [`AppPaths::resolve`], first thing at startup, before anything writes.

use std::path::{Path, PathBuf};

use rightkit_daemon::mode::{self, AppId};
use rightkit_daemon::{AppRoot, AppRootError, Suite, SuiteError};

/// How the app names itself to the path resolver.
#[derive(Debug, Clone, Copy)]
pub struct AppPathsConfig {
    /// Folder name under the vendor folder, and the runner's binding key.
    pub app: AppId,
    /// Name shown in error text.
    pub display_name: &'static str,
    /// Home-folder environment variable a QA runner may bind, besides the shared QA one.
    pub qa_home_env: Option<&'static str>,
}

/// Why the app root could not be settled. The text is shown to the user and written to stderr.
#[derive(Debug, Clone)]
pub struct StorageError {
    message: String,
}

impl StorageError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StorageError {}

fn root_error(name: &str, error: AppRootError) -> StorageError {
    StorageError::new(format!("{name}'s data folder is unavailable: {error}"))
}

fn suite_error(name: &str, error: SuiteError) -> StorageError {
    StorageError::new(format!("{name}'s data folder is unavailable: {error}"))
}

/// The settled root of this process.
pub struct AppPaths {
    /// Keeps a per-run temp suite alive for the process.
    _suite: Suite,
    root: AppRoot,
    /// The home folder of an isolated run, `None` when the user's real home applies.
    home: Option<PathBuf>,
    namespace: &'static str,
    test_run: bool,
}

impl AppPaths {
    /// Settle the app root: decide release or test once (exits on a refused test launch), resolve
    /// the roots, migrate an older install and resolve an isolated home for a test run.
    ///
    /// # Errors
    ///
    /// Returns the text to show the user when the root cannot be settled.
    pub fn resolve(config: &AppPathsConfig) -> Result<Self, StorageError> {
        let name = config.display_name;
        let mode = mode::init_or_exit(&config.app);
        let suite = mode::roots(&config.app, mode)
            .map_err(|e| suite_error(name, e))?
            .suite;
        let root = AppRoot::new(&suite, config.app.dir_name()).map_err(|e| root_error(name, e))?;
        let home = resolve_isolated_home(config, mode)?;
        Ok(Self {
            _suite: suite,
            root,
            home,
            namespace: config.app.dir_name(),
            test_run: mode.is_test(),
        })
    }

    /// A root under one per-run temp suite, for unit tests: no folder outside it is ever reached.
    ///
    /// # Errors
    ///
    /// Returns the text to show when the temp suite cannot be created.
    pub fn per_run_temp(config: &AppPathsConfig) -> Result<Self, StorageError> {
        let name = config.display_name;
        let suite = Suite::per_run_temp(config.app.id()).map_err(|e| suite_error(name, e))?;
        let root = AppRoot::new(&suite, config.app.dir_name()).map_err(|e| root_error(name, e))?;
        let home = Some(suite.root().join("home"));
        Ok(Self {
            _suite: suite,
            root,
            home,
            namespace: config.app.dir_name(),
            test_run: true,
        })
    }

    /// The app root, with an older install's data already moved in.
    pub fn root(&self) -> &AppRoot {
        &self.root
    }

    /// The app root's folder name.
    pub fn namespace(&self) -> &'static str {
        self.namespace
    }

    /// The folder holding the app root, so callers that join [`AppPaths::namespace`] land on the
    /// root itself.
    pub fn data_dir(&self) -> Option<PathBuf> {
        self.root.root().parent().map(Path::to_path_buf)
    }

    /// A file path inside the app root.
    ///
    /// # Errors
    ///
    /// Returns the root's error when the name is refused.
    pub fn path(&self, name: &str) -> Result<PathBuf, StorageError> {
        self.root
            .path(name)
            .map_err(|e| StorageError::new(format!("data path refused: {e}")))
    }

    /// True for a test run; a unit-test root is always a test run.
    pub fn is_test_run(&self) -> bool {
        self.test_run
    }

    /// The user's home folder, for the sidebar's Locations. Never the real home under test.
    pub fn home_dir(&self) -> Option<PathBuf> {
        match &self.home {
            Some(home) => Some(home.clone()),
            None => dirs::home_dir(),
        }
    }

    /// True when [`AppPaths::home_dir`] is not the user's real home.
    pub fn home_is_isolated(&self) -> bool {
        self.home.is_some()
    }
}

#[cfg(feature = "qa-home")]
fn resolve_isolated_home(
    config: &AppPathsConfig,
    mode: &mode::Mode,
) -> Result<Option<PathBuf>, StorageError> {
    if !mode.is_test() {
        return Ok(None);
    }
    use rightkit_fs::app_dirs::{AppDirs, AppId as FsAppId, Options, Os};
    let name = config.display_name;
    let app = FsAppId::new(config.app.dir_name().trim_end_matches("Dev"))
        .with_qa_app_id(config.app.id())
        .with_legacy_env(None, config.qa_home_env);
    let opts = Options::new(Os::current())
        .dev(config.app.dir_name().ends_with("Dev"))
        .qa_overrides(true);
    let dirs = AppDirs::resolve_with_options(app, &opts)
        .map_err(|e| StorageError::new(format!("{name}'s test home is refused: {e}")))?;
    if dirs.isolated {
        return Ok(Some(dirs.home));
    }
    Err(StorageError::new(format!(
        "{name}'s test run has no isolated home (no validated QA root)"
    )))
}

#[cfg(not(feature = "qa-home"))]
fn resolve_isolated_home(
    config: &AppPathsConfig,
    mode: &mode::Mode,
) -> Result<Option<PathBuf>, StorageError> {
    if !mode.is_test() {
        return Ok(None);
    }
    // No isolated home: a test run never falls back to a raw path or the real home.
    Err(StorageError::new(format!(
        "{}'s test run has no isolated home (build without the `qa-home` feature)",
        config.display_name
    )))
}
