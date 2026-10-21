//! The one runtime decision between a release run and a test run (M21), and the only sanctioned
//! reader of a test or override selector (M23).
//!
//! Design rule: the strongest guarantee is that a release binary contains **no code that reads a
//! test or override selector**. So there are two builds of this module:
//!
//! * **Without the `test-mode` cargo feature** (the default, and the only thing a release build may
//!   use): [`decide`] returns [`Mode::Release`] and performs no environment or argument read at all.
//!   [`test_selector!`] expands to `None` and drops its name literal. The binary carries no selector
//!   name and no [`TEST_MODE_MARKER`]. [`roots`] resolves from the operating system's account
//!   database ([`Suite::per_user_default`]), never from the environment.
//! * **With `test-mode`** (QA and dev builds only): [`decide`] reads the activation selector. A run
//!   is a test run only when the activation selector AND a root that passes every check below are
//!   present; anything partial is a [`Refusal`] before any store, log, credential or service opens.
//!   The root must be absolute, an existing directory, not itself a symlink, disjoint from the real
//!   per-user suite root and the user's Documents/Pictures-style folders (equal, ancestor,
//!   descendant, or reached through a symlinked ancestor), outside cloud-sync folders, private to the
//!   current user on Unix, and carry the binding file (`.rightkit-app.json`, `{"app": "<id>"}`)
//!   that `rightkit-qa` writes when it creates an isolated home, naming this app.
//!
//! `@rightkit/release` (`release-switch-scan.mjs`) refuses a release build that enables
//! `test-mode`, and refuses a native binary that contains [`TEST_MODE_MARKER`] text or any selector
//! name. Selector names therefore live only behind the feature, and no message in this module that is
//! compiled into a release build names one.
//!
//! The decision is deliberately injectable ([`ModeInput`]) so tests can prove the "reads nothing"
//! and "refuses" properties without touching the real process environment.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::suite::{Suite, SuiteError, SUITE_DIR_NAME};

/// Whether this build compiled the test capability (cargo feature `test-mode`).
pub const TEST_CAPABILITY: bool = cfg!(feature = "test-mode");

/// Text of the marker a test-capable binary carries. Keep identical to `TEST_MODE_MARKER` in
/// `packages/release/release-switch-scan.mjs`.
#[cfg(feature = "test-mode")]
pub const TEST_MODE_MARKER_TEXT: &str = "RIGHTKIT-TEST-MODE-COMPILED";

/// Present in the binary only when `test-mode` is compiled; the release scan refuses any binary
/// that contains it. Secondary evidence: the authoritative gate is the resolved feature closure
/// (`cargo tree -e features`) checked by `@rightkit/release`. The bytes are inline in a named
/// section, exported, and `#[used]`, so a linker's section garbage collection keeps them.
#[cfg(feature = "test-mode")]
#[used]
#[no_mangle]
#[cfg_attr(target_os = "macos", link_section = "__DATA,__rkit_tm")]
#[cfg_attr(windows, link_section = ".rkittm")]
#[cfg_attr(not(any(target_os = "macos", windows)), link_section = ".rkit_tm")]
pub static TEST_MODE_MARKER: [u8; 27] = *b"RIGHTKIT-TEST-MODE-COMPILED";

/// Activation selector (class S1). Test-capable builds only.
#[cfg(feature = "test-mode")]
const SELECTOR_ACTIVATION: &str = "RIGHTKIT_UNDER_TEST";
/// Test root selector (class S4). Test-capable builds only.
#[cfg(feature = "test-mode")]
const SELECTOR_ROOT: &str = "RIGHTKIT_QA_DATA_DIR";
/// Binding file `rightkit-qa` writes inside every home it creates.
#[cfg(feature = "test-mode")]
const BINDING_FILE: &str = ".rightkit-app.json";

/// The app asking for a mode. `id` is the binding key (`"viewright"`); `dir_name` is the folder
/// under `Orthic Labs` that holds the app's own data (`"ViewRight"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppId {
    id: &'static str,
    dir_name: &'static str,
}

impl AppId {
    /// An app whose data folder carries the same name as its id.
    pub const fn new(id: &'static str) -> AppId {
        AppId { id, dir_name: id }
    }

    /// An app whose data folder differs from its id.
    pub const fn with_dir(id: &'static str, dir_name: &'static str) -> AppId {
        AppId { id, dir_name }
    }

    pub const fn id(&self) -> &'static str {
        self.id
    }

    pub const fn dir_name(&self) -> &'static str {
        self.dir_name
    }
}

/// Why a test run was refused. Each variant names its rule through [`Refusal::rule`]. Messages
/// never name an environment variable: this enum is compiled into release builds too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The activation selector is set but no root is given.
    ActivationWithoutRoot,
    /// A root is given but the activation selector is not set.
    RootWithoutActivation,
    RootNotAbsolute,
    /// Missing, or not a directory.
    RootNotDirectory,
    /// The root itself is a symlink.
    RootHasSymlink,
    /// Equal to, an ancestor of, a descendant of, or resolving into a real user location.
    RootOverlapsUserData,
    /// Inside a cloud-synchronised folder (name heuristic, see [`Refusal::rule`]).
    RootOnRemovableOrCloud,
    /// Not private to the current user (Unix: not owned by the user, or group/other bits set).
    RootNotPrivate,
    /// No readable binding file: not a home a runner created.
    RootNotCreatedByRunner,
    /// The binding names a different app.
    BindingAppMismatch,
    /// The real per-user root could not be determined, so disjointness cannot be proven.
    UserRootUnknown,
    /// A filesystem error while verifying or resetting.
    Io(String),
}

impl Refusal {
    /// A stable kebab-case rule id (the vocabulary `rightkit-qa`'s guard shares).
    pub fn rule(&self) -> &'static str {
        match self {
            Refusal::ActivationWithoutRoot => "activation-without-root",
            Refusal::RootWithoutActivation => "root-without-activation",
            Refusal::RootNotAbsolute => "root-not-absolute",
            Refusal::RootNotDirectory => "root-not-directory",
            Refusal::RootHasSymlink => "root-has-symlink",
            Refusal::RootOverlapsUserData => "root-overlaps-user-data",
            Refusal::RootOnRemovableOrCloud => "root-on-removable-or-cloud",
            Refusal::RootNotPrivate => "root-not-private",
            Refusal::RootNotCreatedByRunner => "root-not-created-by-runner",
            Refusal::BindingAppMismatch => "binding-app-mismatch",
            Refusal::UserRootUnknown => "user-root-unknown",
            Refusal::Io(_) => "io",
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Io(e) => write!(f, "test mode refused ({}): {e}", self.rule()),
            _ => write!(f, "test mode refused ({})", self.rule()),
        }
    }
}

impl std::error::Error for Refusal {}

impl From<io::Error> for Refusal {
    fn from(e: io::Error) -> Self {
        Refusal::Io(e.to_string())
    }
}

/// A verified test root: canonical, symlink-free at the leaf, disjoint from real user data,
/// private, and bound to one app by the runner. Constructible only by [`decide`] in a build that
/// compiled `test-mode`.
#[derive(Debug, Clone)]
pub struct TestRoot {
    path: PathBuf,
    suite: Suite,
    #[cfg_attr(not(feature = "test-mode"), allow(dead_code))]
    app_id: &'static str,
}

impl TestRoot {
    /// The canonical root directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The suite root for this run: `<root>/shared`.
    pub fn suite(&self) -> Suite {
        self.suite.clone()
    }

    /// A unit-test stand-in for what [`decide`] builds after validation.
    #[cfg(test)]
    pub(crate) fn for_test(path: PathBuf, suite: Suite) -> TestRoot {
        TestRoot {
            path,
            suite,
            app_id: "test",
        }
    }

    /// The app's own data directory inside the root: `<root>/<dir_name>`.
    pub fn app_dir(&self, app: &AppId) -> PathBuf {
        self.path.join(app.dir_name)
    }
}

/// How a run was decided. `Release` is the only value a build without `test-mode` can produce.
#[derive(Debug, Clone)]
pub enum Mode {
    Release,
    Test(TestRoot),
}

impl Mode {
    pub fn is_test(&self) -> bool {
        matches!(self, Mode::Test(_))
    }

    /// The test root, when this is a test run.
    pub fn test_root(&self) -> Option<&TestRoot> {
        match self {
            Mode::Test(root) => Some(root),
            Mode::Release => None,
        }
    }

    /// Read an override selector. Only [`test_selector!`] calls this, and only in a build that
    /// compiled `test-mode`; always `None` for [`Mode::Release`].
    #[cfg(feature = "test-mode")]
    #[doc(hidden)]
    pub fn selector_env(&self, name: &str) -> Option<String> {
        match self {
            Mode::Test(_) => std::env::var(name).ok().filter(|v| !v.trim().is_empty()),
            Mode::Release => None,
        }
    }
}

/// The only sanctioned reader of an override selector: `test_selector!(mode, "NAME")` returns the
/// variable's value for a test run and `None` otherwise. In a build without `test-mode` it expands
/// to `None` and the name literal does not appear in the binary.
#[cfg(feature = "test-mode")]
#[macro_export]
macro_rules! test_selector {
    ($mode:expr, $name:literal) => {
        $crate::mode::Mode::selector_env(&$mode, $name)
    };
}

/// See the `test-mode` form: this one drops the name.
#[cfg(not(feature = "test-mode"))]
#[macro_export]
macro_rules! test_selector {
    ($mode:expr, $name:literal) => {{
        let _ = &$mode;
        ::core::option::Option::<::std::string::String>::None
    }};
}

/// Where [`decide`] reads from. The default implementation of the provided methods is the real
/// operating system. A test supplies a fake, including one that panics on any read.
pub trait ModeInput {
    /// An environment variable. Called only by a build that compiled `test-mode`.
    fn env(&self, name: &str) -> Option<String>;

    /// The real per-user suite root, from the account database.
    fn per_user_default(&self) -> Result<Suite, SuiteError> {
        Suite::per_user_default()
    }

    /// Locations a test root may never overlap, given the real suite root
    /// (`<base>/Orthic Labs/shared`): the whole `Orthic Labs` folder and the user's content
    /// folders under the home that `<base>` belongs to.
    fn protected_paths(&self, real: &Suite) -> Vec<PathBuf> {
        default_protected_paths(real)
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

/// Protected paths derived from the real suite root: the whole `Orthic Labs` folder and the
/// user's content folders under the home.
///
/// Fails closed with an empty list (which [`decide`] turns into [`Refusal::UserRootUnknown`])
/// when, on Windows in a `test-mode` build, the real root lies under a `USERPROFILE` or
/// `LOCALAPPDATA` that differs from the account's own value: such a root was shaped by the
/// environment (a QA home), and the folders derived from it would protect the wrong place.
pub fn default_protected_paths(real: &Suite) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if real_root_is_env_shaped(real.root()) {
        return out;
    }
    let Some(orthic) = real.root().parent() else {
        return out;
    };
    out.push(orthic.to_path_buf());
    // Only trust the shape when the parent is `Orthic Labs`: `<home>` is then three ancestors up
    // (`<home>/Library/Application Support`, `<home>/.local/share`, `<home>\AppData\Local`).
    if orthic.file_name().and_then(|n| n.to_str()) == Some(SUITE_DIR_NAME) {
        if let Some(home) = orthic.ancestors().nth(3) {
            for dir in user_content_dirs() {
                out.push(home.join(dir));
            }
        }
    }
    out
}

/// Windows, `test-mode` build: whether `real` lies under an environment profile value that
/// differs from the account's. An unreadable account profile counts as env-shaped. Elsewhere
/// (and in a build without `test-mode`, which reads no environment) always `false`.
fn real_root_is_env_shaped(real: &Path) -> bool {
    #[cfg(all(windows, feature = "test-mode"))]
    {
        let Ok(profile) = crate::suite::account_profile_dir() else {
            return true;
        };
        let env = |key: &str| {
            std::env::var_os(key)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let local = profile.join("AppData").join("Local");
        root_under_differing_env(
            real,
            &[
                (env("USERPROFILE"), profile),
                (env("LOCALAPPDATA"), local),
            ],
        )
    }
    #[cfg(not(all(windows, feature = "test-mode")))]
    {
        let _ = real;
        false
    }
}

/// True when `root` lies under (or equals) an `env` value of a pair whose `env` differs from its
/// `account` value. Paths compare by whole components, ignoring a verbatim prefix and, on
/// Windows, case and separator style.
#[cfg_attr(not(all(windows, feature = "test-mode")), allow(dead_code))]
pub(crate) fn root_under_differing_env(root: &Path, pairs: &[(Option<PathBuf>, PathBuf)]) -> bool {
    fn norm(p: &Path) -> Vec<String> {
        let text = p.to_string_lossy();
        let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
        text.split(['/', '\\'])
            .filter(|c| !c.is_empty())
            .map(|c| {
                if cfg!(windows) {
                    c.to_ascii_lowercase()
                } else {
                    c.to_string()
                }
            })
            .collect()
    }
    let root = norm(root);
    pairs.iter().any(|(env, account)| {
        env.as_deref().is_some_and(|env| {
            let env = norm(env);
            !env.is_empty() && env != norm(account) && root.starts_with(&env)
        })
    })
}

fn user_content_dirs() -> [&'static str; 7] {
    [
        "Documents",
        "Pictures",
        "Desktop",
        "Downloads",
        "Movies",
        "Music",
        "Videos",
    ]
}

/// Resolved roots a store must come through.
#[derive(Debug, Clone)]
pub struct AppRoots {
    /// The suite root (`<Orthic Labs>/shared` in release, `<test root>/shared` in a test run).
    pub suite: Suite,
    /// The app's own data directory.
    pub app_dir: PathBuf,
}

/// Decide once, before any store, log, credential or service starts.
///
/// Without `test-mode` this returns [`Mode::Release`] and reads nothing from `input`.
pub fn decide(app: &AppId, input: &dyn ModeInput) -> Result<Mode, Refusal> {
    #[cfg(feature = "test-mode")]
    {
        capable::decide(app, input)
    }
    #[cfg(not(feature = "test-mode"))]
    {
        let _ = (app, input);
        Ok(Mode::Release)
    }
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

/// The roots every store must come through, against the real process.
pub fn roots(app: &AppId, mode: &Mode) -> Result<AppRoots, SuiteError> {
    roots_with(app, mode, &ProcessInput)
}

/// [`roots`] with an injectable source for the real per-user root.
pub fn roots_with(app: &AppId, mode: &Mode, input: &dyn ModeInput) -> Result<AppRoots, SuiteError> {
    match mode {
        Mode::Release => {
            let suite = input.per_user_default()?;
            let orthic = suite
                .root()
                .parent()
                .ok_or_else(|| SuiteError::NoRoot("the per-user suite root has no parent".into()))?
                .to_path_buf();
            Ok(AppRoots {
                suite,
                app_dir: orthic.join(app.dir_name),
            })
        }
        Mode::Test(root) => Ok(AppRoots {
            suite: root.suite(),
            app_dir: root.app_dir(app),
        }),
    }
}

/// The test-capable half. Compiled only with `test-mode`.
#[cfg(feature = "test-mode")]
mod capable {
    use super::*;
    use std::fs;

    fn truthy(v: &str) -> bool {
        let t = v.trim();
        !(t.is_empty() || t == "0" || t.eq_ignore_ascii_case("false"))
    }

    pub(super) fn decide(app: &AppId, input: &dyn ModeInput) -> Result<Mode, Refusal> {
        let activated = input
            .env(SELECTOR_ACTIVATION)
            .filter(|v| truthy(v))
            .is_some();
        let root = input.env(SELECTOR_ROOT).filter(|v| !v.trim().is_empty());
        match (activated, root) {
            (false, None) => Ok(Mode::Release),
            (false, Some(_)) => Err(Refusal::RootWithoutActivation),
            (true, None) => Err(Refusal::ActivationWithoutRoot),
            (true, Some(root)) => verify(app, input, &root).map(Mode::Test),
        }
    }

    fn map_refusal(refusal: rightkit_fs::test_mode::Refusal) -> Refusal {
        use rightkit_fs::test_mode::Refusal as FsRefusal;
        match refusal {
            FsRefusal::ActivationWithoutRoot => Refusal::ActivationWithoutRoot,
            FsRefusal::RootWithoutActivation => Refusal::RootWithoutActivation,
            FsRefusal::RootNotAbsolute => Refusal::RootNotAbsolute,
            FsRefusal::RootNotDirectory => Refusal::RootNotDirectory,
            FsRefusal::RootHasSymlink => Refusal::RootHasSymlink,
            FsRefusal::RootOverlapsUserData => Refusal::RootOverlapsUserData,
            FsRefusal::RootOnRemovableOrCloud => Refusal::RootOnRemovableOrCloud,
            FsRefusal::RootNotPrivate => Refusal::RootNotPrivate,
            FsRefusal::RootNotCreatedByRunner => Refusal::RootNotCreatedByRunner,
            FsRefusal::BindingAppMismatch => Refusal::BindingAppMismatch,
            FsRefusal::UserRootUnknown => Refusal::UserRootUnknown,
            FsRefusal::Io(e) => Refusal::Io(e),
            other => Refusal::Io(other.rule().to_string()),
        }
    }

    fn check_binding(app: &AppId, canon: &Path) -> Result<(), Refusal> {
        let bytes =
            fs::read(canon.join(BINDING_FILE)).map_err(|_| Refusal::RootNotCreatedByRunner)?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| Refusal::RootNotCreatedByRunner)?;
        match value.get("app").and_then(|v| v.as_str()) {
            None => Err(Refusal::RootNotCreatedByRunner),
            Some(bound) if bound == app.id => Ok(()),
            Some(_) => Err(Refusal::BindingAppMismatch),
        }
    }

    fn verify(app: &AppId, input: &dyn ModeInput, raw: &str) -> Result<TestRoot, Refusal> {
        let real = input
            .per_user_default()
            .map_err(|_| Refusal::UserRootUnknown)?;
        let mut protected = input.protected_paths(&real);
        if protected.is_empty() {
            // Nothing to prove disjointness against; the real root alone is not enough.
            return Err(Refusal::UserRootUnknown);
        }
        protected.push(real.root().to_path_buf());
        let validated =
            rightkit_fs::test_mode::validate_root(app.id, Path::new(raw.trim()), &protected)
                .map_err(map_refusal)?;
        let suite_path = validated.child(Path::new("shared")).map_err(map_refusal)?;
        let suite = Suite::at(suite_path).map_err(|e| Refusal::Io(e.to_string()))?;
        Ok(TestRoot {
            path: validated.path().to_path_buf(),
            suite,
            app_id: app.id,
        })
    }

    impl TestRoot {
        /// Remove everything inside the root except the runner's dot files (binding and markers),
        /// after re-verifying the binding. Never follows a symlink out of the root, and nothing
        /// reachable from this type is a real user location.
        pub fn reset(&self) -> Result<(), Refusal> {
            let app = AppId {
                id: self.app_id,
                dir_name: self.app_id,
            };
            check_binding(&app, &self.path)?;
            for entry in fs::read_dir(&self.path)? {
                let entry = entry?;
                if entry.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                let ty = entry.file_type()?;
                if ty.is_dir() && !ty.is_symlink() {
                    fs::remove_dir_all(entry.path())?;
                } else {
                    fs::remove_file(entry.path())?;
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(env: Option<&str>, account: &str) -> (Option<PathBuf>, PathBuf) {
        (env.map(PathBuf::from), PathBuf::from(account))
    }

    #[test]
    fn a_root_under_a_differing_env_profile_is_env_shaped() {
        let root = Path::new("/qa/home/AppData/Local/Orthic Labs/shared");
        // The env value differs from the account and the root lies under it: fail closed.
        assert!(root_under_differing_env(
            root,
            &[pair(Some("/qa/home"), "/users/real")]
        ));
        assert!(root_under_differing_env(
            root,
            &[pair(None, "/users/real"), pair(Some("/qa/home/AppData/Local"), "/x")]
        ));
        // The env value equals the account value, is unset, or the root is not under it.
        assert!(!root_under_differing_env(
            root,
            &[pair(Some("/qa/home"), "/qa/home")]
        ));
        assert!(!root_under_differing_env(root, &[pair(None, "/qa/home")]));
        assert!(!root_under_differing_env(
            root,
            &[pair(Some("/elsewhere"), "/qa/home")]
        ));
        // Whole components only: `/qa/hom` is not an ancestor of `/qa/home/...`.
        assert!(!root_under_differing_env(
            root,
            &[pair(Some("/qa/hom"), "/users/real")]
        ));
    }

    #[cfg(windows)]
    #[test]
    fn env_comparison_ignores_case_separators_and_the_verbatim_prefix_on_windows() {
        let root = Path::new(r"\\?\C:\Users\QA\AppData\Local\Orthic Labs\shared");
        assert!(root_under_differing_env(
            root,
            &[pair(Some("c:/users/qa"), r"C:\Users\Real")]
        ));
        assert!(!root_under_differing_env(
            root,
            &[pair(Some(r"C:\USERS\QA"), r"c:\users\qa")]
        ));
    }

    #[test]
    fn the_default_protected_paths_keep_the_orthic_folder_and_content_dirs() {
        let real = Suite::at(Path::new(if cfg!(windows) {
            r"C:\Users\acct\AppData\Local\Orthic Labs\shared"
        } else {
            "/users/acct/AppData/Local/Orthic Labs/shared"
        }))
        .unwrap();
        let paths = default_protected_paths(&real);
        assert!(paths.iter().any(|p| p.ends_with("Orthic Labs")));
        assert!(paths.iter().any(|p| p.ends_with("Documents")));
    }
}
