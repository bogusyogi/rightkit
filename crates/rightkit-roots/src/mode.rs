//! The one runtime decision about where an app keeps its data (M21).
//!
//! There is no test mode. Adrian, 2026-11-03: "dev mode, is mine. im the fucking owner, my data. no
//! dummy data, no fucking made up shit. real data. snapshot and restore." Every build, release or
//! dev, resolves the real per-user roots from the operating system's account database
//! ([`Suite::per_user_default`]) and reads no environment variable, argument or marker to choose
//! them. QA runs on that real data and protects it with the snapshot and restore of `rightkit-qa`
//! (rightkit#1437), not with a throwaway root. The former `test-mode` feature, `Mode::Test`, the
//! override selectors and the `RIGHTKIT-TEST-MODE-COMPILED` marker are gone (rightkit-daemon 0.5.3).

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rightkit_fs::app_dirs::{AppDirs, AppDirsError, AppId as FolderId, Options};

use crate::suite::{Suite, SuiteError};

/// The app asking for a mode. `id` is the binding key (`"viewright"`); `dir_name` is the folder
/// that holds the app's own data (`"ViewRight"`): under `Orthic Labs` by default, or directly under
/// the platform's per-user local folder for an app whose installed data predates that layout
/// ([`AppId::without_org`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppId {
    id: &'static str,
    dir_name: &'static str,
    flat: bool,
    legacy_names: &'static [&'static str],
    dev_dir_name: Option<&'static str>,
}

impl AppId {
    /// An app whose data folder carries the same name as its id.
    pub const fn new(id: &'static str) -> AppId {
        AppId::with_dir(id, id)
    }

    /// An app whose data folder differs from its id.
    pub const fn with_dir(id: &'static str, dir_name: &'static str) -> AppId {
        AppId {
            id,
            dir_name,
            flat: false,
            legacy_names: &[],
            dev_dir_name: None,
        }
    }

    /// No organisation folder: the data folder is `<per-user local folder>/<dir_name>` (Windows
    /// Local AppData, `~/Library/Application Support`, `~/.local/share`), for an app whose
    /// installed data predates the `Orthic Labs` layout. The folder is the one
    /// `rightkit_fs::app_dirs::AppDirs::local` gives for a flat app, so installed users keep it.
    pub const fn without_org(mut self) -> AppId {
        self.flat = true;
        self
    }

    /// Earlier folder names whose existing folders are still used, most preferred first: when the
    /// current folder is missing and a legacy one exists, the legacy one is the app's folder.
    /// Nothing is moved or created.
    pub const fn with_legacy_names(mut self, names: &'static [&'static str]) -> AppId {
        self.legacy_names = names;
        self
    }

    /// The folder name of the dev or QA variant (for example `"ViewRightDev"`), used by
    /// [`AppId::dev_variant`]. The app chooses the variant at compile time (a feature); nothing is
    /// read from the environment.
    pub const fn with_dev_dir(mut self, dev_dir_name: &'static str) -> AppId {
        self.dev_dir_name = Some(dev_dir_name);
        self
    }

    /// This app's dev or QA variant: the same layout with the [`AppId::with_dev_dir`] folder and
    /// no legacy names. A compile error in a `const` when no dev folder was named.
    pub const fn dev_variant(mut self) -> AppId {
        match self.dev_dir_name {
            Some(dir) => {
                self.dir_name = dir;
                self.legacy_names = &[];
                self
            }
            None => panic!("AppId::dev_variant needs with_dev_dir"),
        }
    }

    /// Whether the data folder has no organisation folder.
    pub const fn is_flat(&self) -> bool {
        self.flat
    }

    pub const fn legacy_names(&self) -> &'static [&'static str] {
        self.legacy_names
    }

    pub const fn id(&self) -> &'static str {
        self.id
    }

    pub const fn dir_name(&self) -> &'static str {
        self.dir_name
    }
}

/// Why [`decide`] refused to run. Messages never name an environment variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The real per-user root could not be determined.
    UserRootUnknown,
    /// A filesystem error.
    Io(String),
}

impl Refusal {
    /// A stable kebab-case rule id.
    pub fn rule(&self) -> &'static str {
        match self {
            Refusal::UserRootUnknown => "user-root-unknown",
            Refusal::Io(_) => "io",
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Io(e) => write!(f, "run refused ({}): {e}", self.rule()),
            _ => write!(f, "run refused ({})", self.rule()),
        }
    }
}

impl std::error::Error for Refusal {}

impl From<io::Error> for Refusal {
    fn from(e: io::Error) -> Self {
        Refusal::Io(e.to_string())
    }
}

/// How a run was decided. The only value is `Release`: the real per-user roots.
#[derive(Debug, Clone)]
pub enum Mode {
    Release,
}

impl Mode {
    /// Always `false`: no build has a test mode.
    pub fn is_test(&self) -> bool {
        false
    }
}

/// Where [`decide`] reads from. The default implementation of the provided methods is the real
/// operating system. A test supplies a fake, including one that panics on any read.
pub trait ModeInput {
    /// An environment variable. [`decide`] and [`roots`] never call it.
    fn env(&self, name: &str) -> Option<String>;

    /// The real per-user suite root, from the account database.
    fn per_user_default(&self) -> Result<Suite, SuiteError> {
        Suite::per_user_default()
    }

    /// The per-user folders of a flat app ([`AppId::without_org`]), from the account database and
    /// never the environment. A test supplies the base folders of any OS.
    fn app_dirs(&self, app: &FolderId) -> Result<AppDirs, AppDirsError> {
        AppDirs::resolve_with_options(app.clone(), &Options::current().dev(false))
    }

    /// Whether a candidate legacy folder exists.
    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }
}

/// The real process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessInput;

impl ModeInput for ProcessInput {
    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// Resolved roots a store must come through.
#[derive(Debug, Clone)]
pub struct AppRoots {
    /// The suite root (`<Orthic Labs>/shared`).
    pub suite: Suite,
    /// The app's own data directory.
    pub app_dir: PathBuf,
}

/// Decide once, before any store, log, credential or service starts. Returns [`Mode::Release`]
/// and reads nothing from `input`.
pub fn decide(app: &AppId, input: &dyn ModeInput) -> Result<Mode, Refusal> {
    let _ = (app, input);
    Ok(Mode::Release)
}

/// [`decide`] against the real process, once per process. Prints one line to stderr and exits with
/// status 2 on a [`Refusal`]; hosts call it first in `main`.
pub fn init_or_exit(app: &AppId) -> &'static Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    if let Some(mode) = MODE.get() {
        return mode;
    }
    match decide(app, &ProcessInput) {
        Ok(mode) => MODE.get_or_init(|| mode),
        Err(refusal) => {
            eprintln!("{refusal}");
            std::process::exit(2);
        }
    }
}

/// The roots every store must come through, against the real process. Wrap `app_dir` with
/// [`AppRoot::for_app`](crate::AppRoot::for_app).
pub fn roots(app: &AppId, mode: &Mode) -> Result<AppRoots, SuiteError> {
    roots_with(app, mode, &ProcessInput)
}

/// [`roots`] with an injectable source for the real per-user root.
pub fn roots_with(app: &AppId, mode: &Mode, input: &dyn ModeInput) -> Result<AppRoots, SuiteError> {
    match mode {
        Mode::Release => {
            let suite = input.per_user_default()?;
            let app_dir = if app.flat {
                let folder = FolderId::new(app.dir_name)
                    .without_org()
                    .with_legacy_names(app.legacy_names);
                input
                    .app_dirs(&folder)
                    .map_err(|e| SuiteError::NoRoot(e.to_string()))?
                    .local
            } else {
                let orthic = suite.root().parent().ok_or_else(|| {
                    SuiteError::NoRoot("the per-user suite root has no parent".into())
                })?;
                let current = orthic.join(app.dir_name);
                if input.is_dir(&current) {
                    current
                } else {
                    app.legacy_names
                        .iter()
                        .map(|legacy| orthic.join(legacy))
                        .find(|candidate| input.is_dir(candidate))
                        .unwrap_or(current)
                }
            };
            Ok(AppRoots { suite, app_dir })
        }
    }
}

#[cfg(test)]
mod tests;
