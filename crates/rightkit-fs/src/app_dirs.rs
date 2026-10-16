//! One resolver for an app's per-user folders (`data`, `local`, `cache`, `home`) with a dev
//! namespace and test/QA isolation.
//!
//! This replaces the hand-rolled `app_paths.rs` / `*_DATA_DIR` copies in each app. The rules:
//!
//! * **Per-OS bases.** Windows: `%APPDATA%` (data) and `%LOCALAPPDATA%` (local, cache), home from
//!   `%USERPROFILE%`. macOS: `~/Library/Application Support` (data and local) and
//!   `~/Library/Caches` (cache). Linux: `$XDG_DATA_HOME` (else `~/.local/share`) for data and
//!   local, `$XDG_CACHE_HOME` (else `~/.cache`) for cache. Every base must be absolute; an unset or
//!   relative base is an error, never the working directory.
//! * **App folder.** `<base>/<org>/<namespace>`, org defaulting to `Orthic Labs`
//!   ([`AppId::without_org`] keeps an existing flat `<base>/<name>` install). Windows cache is
//!   `<LOCALAPPDATA>/<org>/<namespace>/Cache` so it never shares a folder with state.
//! * **Dev namespace.** A dev build (`cfg!(debug_assertions)` of this crate's build, see
//!   [`Options::current`]) uses `<name>Dev` (for example `ViewRightDev`), so a dev run never touches
//!   the installed app's data.
//! * **Test root.** Under this crate's own `cfg(test)`, and from [`AppDirs::for_test`] in any build,
//!   every folder is under one per-process temp root and the OS is never asked.
//! * **QA override (feature `test-mode`).** Only a build that compiles `test-mode` reads any
//!   override variable (the same feature name as `rightkit-daemon`, so one switch isolates both).
//!   `RIGHTKIT_QA_DATA_DIR` becomes the data folder (`local`, `cache` and `home` live under it) and
//!   `RIGHTKIT_QA_HOME` names the home folder. An app may name its own historical variables with
//!   [`AppId::with_legacy_env`]. The override is **refused** (an error, not a fallback) when it is
//!   relative, contains `..`, or points at or inside a real per-user app folder of this app.
//!   A release build (without `test-mode`) contains none of these variable names and reads none.
//!
//! All the logic is in [`AppDirs::resolve_with`], a pure function over an injected environment.
//! Nothing here creates a directory except [`AppDirs::create_all`].
//!
//! Known limit: the real-folder check compares against bases taken from the same environment, so it
//! catches an override aimed at the real app folders of this process's environment, not at a real
//! folder the launcher hid by redirecting `APPDATA`/`HOME` (the QA workspace redirects them on
//! purpose). `rightkit_daemon::Suite::per_user_default` reads the account database for that; this
//! crate does not duplicate it.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// The organisation folder every Right Suite app nests under.
pub const DEFAULT_ORG: &str = "Orthic Labs";

/// The data-folder override variable, read only with feature `test-mode`.
#[cfg(feature = "test-mode")]
pub const QA_DATA_DIR_VAR: &str = "RIGHTKIT_QA_DATA_DIR";
/// The home-folder override variable, read only with feature `test-mode`.
#[cfg(feature = "test-mode")]
pub const QA_HOME_VAR: &str = "RIGHTKIT_QA_HOME";

/// Why folders could not be resolved.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AppDirsError {
    /// A base folder the OS mapping needs is unset or empty.
    #[error("cannot resolve the {what} folder: {why}")]
    NoBase { what: &'static str, why: String },
    /// A base or override is relative or contains `..`.
    #[error("{var} must be an absolute path without `..`: {}", .path.display())]
    NotAbsolute { var: String, path: PathBuf },
    /// An override points at (or inside) the real per-user folder; it is refused, not honoured.
    #[error("{var} points at the real per-user folder {}; refusing", .path.display())]
    RealFolder { var: String, path: PathBuf },
    #[error("{0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, AppDirsError>;

/// The operating system whose folder mapping is used. A parameter so each mapping is testable on
/// any host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
}

impl Os {
    /// The OS this build targets (anything that is neither Windows nor macOS maps as Linux/XDG).
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }
}

/// Which app. Build with [`AppId::new`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct AppId {
    /// The installed folder name, for example `ViewRight`.
    pub name: &'static str,
    /// The organisation folder; `None` keeps a flat `<base>/<name>` layout.
    pub org: Option<&'static str>,
    /// Historical per-app data override variable (for example `HR_APP_DATA_DIR`), read only with
    /// feature `test-mode`, after `RIGHTKIT_QA_DATA_DIR`.
    pub legacy_data_env: Option<&'static str>,
    /// Historical per-app home override variable, read only with feature `test-mode`.
    pub legacy_home_env: Option<&'static str>,
}

impl AppId {
    /// `name` under [`DEFAULT_ORG`].
    pub const fn new(name: &'static str) -> AppId {
        AppId {
            name,
            org: Some(DEFAULT_ORG),
            legacy_data_env: None,
            legacy_home_env: None,
        }
    }

    /// A different organisation folder.
    pub const fn with_org(mut self, org: &'static str) -> AppId {
        self.org = Some(org);
        self
    }

    /// No organisation folder: `<base>/<name>`, for an app whose installed data predates the org
    /// layout.
    pub const fn without_org(mut self) -> AppId {
        self.org = None;
        self
    }

    /// Also honour these historical override variables (test-mode builds only).
    pub const fn with_legacy_env(
        mut self,
        data: Option<&'static str>,
        home: Option<&'static str>,
    ) -> AppId {
        self.legacy_data_env = data;
        self.legacy_home_env = home;
        self
    }

    /// The folder-name component: `name`, or `<name>Dev` for a dev build.
    pub fn namespace(&self, dev: bool) -> String {
        if dev {
            format!("{}Dev", self.name)
        } else {
            self.name.to_string()
        }
    }
}

/// The inputs of a resolution other than the app and the environment.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Options {
    pub os: Os,
    /// Use the `<name>Dev` namespace.
    pub dev: bool,
    /// Read the QA override variables. Has an effect only when feature `test-mode` is compiled.
    pub qa_overrides: bool,
    /// When set, every folder is under this root and the OS and environment are not consulted.
    pub test_root: Option<PathBuf>,
}

impl Options {
    /// A release-shaped resolution for `os`: not dev, no overrides, no test root.
    pub fn new(os: Os) -> Options {
        Options {
            os,
            dev: false,
            qa_overrides: false,
            test_root: None,
        }
    }

    /// This build: the target OS, dev when `debug_assertions` are on, overrides read only when
    /// feature `test-mode` is compiled, and the per-process test root under `cfg(test)`.
    pub fn current() -> Options {
        Options {
            os: Os::current(),
            dev: cfg!(debug_assertions),
            qa_overrides: cfg!(feature = "test-mode"),
            test_root: if cfg!(test) {
                Some(process_test_root())
            } else {
                None
            },
        }
    }

    pub fn dev(mut self, dev: bool) -> Options {
        self.dev = dev;
        self
    }

    pub fn qa_overrides(mut self, on: bool) -> Options {
        self.qa_overrides = on;
        self
    }

    pub fn test_root(mut self, root: Option<PathBuf>) -> Options {
        self.test_root = root;
        self
    }
}

/// The per-process temp root every test resolution uses: `<temp>/rightkit-test-<pid>`.
pub fn process_test_root() -> PathBuf {
    std::env::temp_dir().join(format!("rightkit-test-{}", std::process::id()))
}

/// An app's resolved folders.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct AppDirs {
    /// Roaming data (databases, settings).
    pub data: PathBuf,
    /// Machine-local data (logs, WebView profile, large artifacts).
    pub local: PathBuf,
    /// Re-creatable cache.
    pub cache: PathBuf,
    /// The home folder to show or search (the user's real home unless isolated).
    pub home: PathBuf,
    /// True when any folder came from the test root or a QA override, so nothing is the real
    /// per-user folder.
    pub isolated: bool,
}

impl AppDirs {
    /// Resolve for this build and this process's environment.
    pub fn resolve(app: AppId) -> Result<AppDirs> {
        AppDirs::resolve_with(&app, &Options::current(), &|k| std::env::var_os(k))
    }

    /// Per-process temp folders in any build (consumer tests): never asks the OS.
    pub fn for_test(app: AppId) -> Result<AppDirs> {
        let opts = Options::current().test_root(Some(process_test_root()));
        AppDirs::resolve_with(&app, &opts, &|_| None)
    }

    /// The pure resolver: `env` answers every variable lookup.
    pub fn resolve_with(
        app: &AppId,
        opts: &Options,
        env: &dyn Fn(&str) -> Option<OsString>,
    ) -> Result<AppDirs> {
        let ns = app.namespace(opts.dev);
        if let Some(root) = &opts.test_root {
            check_absolute(opts.os, "test root", root)?;
            let r = root.join(&ns);
            return Ok(AppDirs {
                data: r.join("data"),
                local: r.join("local"),
                cache: r.join("cache"),
                home: r.join("home"),
                isolated: true,
            });
        }

        let bases = Bases::read(opts.os, env);
        let (qa_data, qa_home) = if opts.qa_overrides {
            read_qa(app, opts, env, &bases)?
        } else {
            (None, None)
        };

        if let Some(root) = qa_data {
            let home = qa_home.unwrap_or_else(|| root.join("home"));
            return Ok(AppDirs {
                local: root.join("local"),
                cache: root.join("cache"),
                data: root,
                home,
                isolated: true,
            });
        }

        let dirs = real_dirs(app, opts.os, &ns, env)?;
        Ok(match qa_home {
            Some(home) => AppDirs {
                home,
                isolated: true,
                ..dirs
            },
            None => dirs,
        })
    }

    /// Create the four folders (plain `create_dir_all`; use `ensure_private_dir` for private state).
    pub fn create_all(&self) -> io::Result<()> {
        for d in [&self.data, &self.local, &self.cache, &self.home] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }
}

/// The OS base folders, each `None` when unset, empty or not absolute.
struct Bases {
    data: Option<PathBuf>,
    local: Option<PathBuf>,
    cache: Option<PathBuf>,
    home: Option<PathBuf>,
}

fn nonempty(env: &dyn Fn(&str) -> Option<OsString>, key: &str) -> Option<PathBuf> {
    env(key)
        .filter(|v| !v.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
}

fn absolute_env(
    os: Os,
    env: &dyn Fn(&str) -> Option<OsString>,
    key: &str,
) -> Option<PathBuf> {
    nonempty(env, key).filter(|p| looks_absolute(os, p))
}

impl Bases {
    fn read(os: Os, env: &dyn Fn(&str) -> Option<OsString>) -> Bases {
        match os {
            Os::Windows => Bases {
                data: absolute_env(os, env, "APPDATA"),
                local: absolute_env(os, env, "LOCALAPPDATA"),
                cache: absolute_env(os, env, "LOCALAPPDATA"),
                home: absolute_env(os, env, "USERPROFILE"),
            },
            Os::MacOs => {
                let home = absolute_env(os, env, "HOME");
                let support = home.as_ref().map(|h| h.join("Library/Application Support"));
                Bases {
                    data: support.clone(),
                    local: support,
                    cache: home.as_ref().map(|h| h.join("Library/Caches")),
                    home,
                }
            }
            Os::Linux => {
                let home = absolute_env(os, env, "HOME");
                let data = absolute_env(os, env, "XDG_DATA_HOME")
                    .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
                let cache = absolute_env(os, env, "XDG_CACHE_HOME")
                    .or_else(|| home.as_ref().map(|h| h.join(".cache")));
                Bases {
                    local: data.clone(),
                    data,
                    cache,
                    home,
                }
            }
        }
    }
}

fn app_dir(base: &Path, app: &AppId, ns: &str) -> PathBuf {
    match app.org {
        Some(org) => base.join(org).join(ns),
        None => base.join(ns),
    }
}

fn missing(os: Os, what: &'static str) -> AppDirsError {
    let why = match os {
        Os::Windows => match what {
            "data" => "APPDATA is not set to an absolute path",
            "home" => "USERPROFILE is not set to an absolute path",
            _ => "LOCALAPPDATA is not set to an absolute path",
        },
        _ => "HOME is not set to an absolute path",
    };
    AppDirsError::NoBase {
        what,
        why: why.to_string(),
    }
}

fn real_dirs(
    app: &AppId,
    os: Os,
    ns: &str,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<AppDirs> {
    let b = Bases::read(os, env);
    let data = b.data.ok_or_else(|| missing(os, "data"))?;
    let local = b.local.ok_or_else(|| missing(os, "local"))?;
    let cache = b.cache.ok_or_else(|| missing(os, "cache"))?;
    let home = b.home.ok_or_else(|| missing(os, "home"))?;
    let cache = if os == Os::Windows {
        app_dir(&cache, app, ns).join("Cache")
    } else {
        app_dir(&cache, app, ns)
    };
    Ok(AppDirs {
        data: app_dir(&data, app, ns),
        local: app_dir(&local, app, ns),
        cache,
        home,
        isolated: false,
    })
}

/// The QA data and home overrides, validated. Compiled to `(None, None)` without `test-mode`, so
/// the variable names are absent from a release binary.
#[cfg(not(feature = "test-mode"))]
fn read_qa(
    _app: &AppId,
    _opts: &Options,
    _env: &dyn Fn(&str) -> Option<OsString>,
    _bases: &Bases,
) -> Result<(Option<PathBuf>, Option<PathBuf>)> {
    Ok((None, None))
}

#[cfg(feature = "test-mode")]
fn read_qa(
    app: &AppId,
    opts: &Options,
    env: &dyn Fn(&str) -> Option<OsString>,
    bases: &Bases,
) -> Result<(Option<PathBuf>, Option<PathBuf>)> {
    let pick = |names: [Option<&'static str>; 2]| -> Option<(&'static str, PathBuf)> {
        names
            .into_iter()
            .flatten()
            .find_map(|n| nonempty(env, n).map(|p| (n, p)))
    };
    let data = pick([Some(QA_DATA_DIR_VAR), app.legacy_data_env]);
    let home = pick([Some(QA_HOME_VAR), app.legacy_home_env]);
    if data.is_none() && home.is_none() {
        return Ok((None, None));
    }
    // The real folders of this app, in both namespaces and with or without the org folder, and the
    // real home. An override at or inside the first group, or equal to the home, is refused.
    let mut inside: Vec<PathBuf> = Vec::new();
    for base in [&bases.data, &bases.local, &bases.cache].into_iter().flatten() {
        for ns in [app.name.to_string(), format!("{}Dev", app.name)] {
            for org in [app.org, None] {
                let probe = AppId { org, ..app.clone() };
                inside.push(app_dir(base, &probe, &ns));
            }
        }
    }
    let win = opts.os == Os::Windows;
    let check = |var: &str, path: PathBuf, home_var: bool| -> Result<PathBuf> {
        check_absolute(opts.os, var, &path)?;
        let hit = inside.iter().any(|real| inside_or_equal(&path, real, win))
            || (home_var
                && bases
                    .home
                    .as_ref()
                    .is_some_and(|h| inside_or_equal(&path, h, win) && inside_or_equal(h, &path, win)));
        if hit {
            return Err(AppDirsError::RealFolder {
                var: var.to_string(),
                path,
            });
        }
        Ok(path)
    };
    let data = data.map(|(v, p)| check(v, p, false)).transpose()?;
    let home = home.map(|(v, p)| check(v, p, true)).transpose()?;
    Ok((data, home))
}

fn check_absolute(os: Os, var: &str, path: &Path) -> Result<()> {
    if looks_absolute(os, path) {
        Ok(())
    } else {
        Err(AppDirsError::NotAbsolute {
            var: var.to_string(),
            path: path.to_path_buf(),
        })
    }
}

/// Absolute for `os` and free of `..`, decided from the text so every OS mapping is testable on
/// any host.
fn looks_absolute(os: Os, path: &Path) -> bool {
    let s = path.to_string_lossy();
    let rooted = match os {
        Os::Windows => {
            let b = s.as_bytes();
            s.starts_with("\\\\")
                || (b.len() >= 3
                    && b[0].is_ascii_alphabetic()
                    && b[1] == b':'
                    && (b[2] == b'\\' || b[2] == b'/'))
        }
        _ => s.starts_with('/'),
    };
    rooted && !components(&s, os == Os::Windows, false).iter().any(|c| c == "..")
}

fn components(s: &str, windows: bool, lower: bool) -> Vec<String> {
    s.split(|c: char| c == '/' || (windows && c == '\\'))
        .filter(|c| !c.is_empty())
        .map(|c| if lower { c.to_lowercase() } else { c.to_string() })
        .collect()
}

/// Whether `path` is `base` or below it (component-wise; case-insensitive on Windows).
#[cfg_attr(not(feature = "test-mode"), allow(dead_code))]
fn inside_or_equal(path: &Path, base: &Path, windows: bool) -> bool {
    let p = components(&path.to_string_lossy(), windows, windows);
    let b = components(&base.to_string_lossy(), windows, windows);
    p.len() >= b.len() && p[..b.len()] == b[..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| {
            pairs
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| OsString::from(v))
        }
    }

    const WIN: &[(&str, &str)] = &[
        ("APPDATA", r"C:\Users\a\AppData\Roaming"),
        ("LOCALAPPDATA", r"C:\Users\a\AppData\Local"),
        ("USERPROFILE", r"C:\Users\a"),
    ];

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// Path equality that ignores the host's separator, so every OS mapping is checked on any host.
    fn same(actual: &Path, expected: &str) {
        assert_eq!(
            components(&actual.to_string_lossy(), true, false),
            components(expected, true, false),
            "{} vs {expected}",
            actual.display()
        );
    }

    #[test]
    fn windows_mapping() {
        let d = AppDirs::resolve_with(
            &AppId::new("ViewRight"),
            &Options::new(Os::Windows),
            &env_of(WIN),
        )
        .unwrap();
        same(&d.data, r"C:\Users\a\AppData\Roaming\Orthic Labs\ViewRight");
        same(&d.local, r"C:\Users\a\AppData\Local\Orthic Labs\ViewRight");
        same(&d.cache, r"C:\Users\a\AppData\Local\Orthic Labs\ViewRight\Cache");
        same(&d.home, r"C:\Users\a");
        assert!(!d.isolated);
    }

    #[test]
    fn macos_mapping() {
        let d = AppDirs::resolve_with(
            &AppId::new("MailRight"),
            &Options::new(Os::MacOs),
            &env_of(&[("HOME", "/Users/a")]),
        )
        .unwrap();
        same(&d.data, "/Users/a/Library/Application Support/Orthic Labs/MailRight");
        assert_eq!(d.local, d.data);
        same(&d.cache, "/Users/a/Library/Caches/Orthic Labs/MailRight");
        same(&d.home, "/Users/a");
    }

    #[test]
    fn linux_mapping_uses_xdg_when_set_and_falls_back_to_home() {
        let app = AppId::new("CutRight");
        let o = Options::new(Os::Linux);
        let d = AppDirs::resolve_with(&app, &o, &env_of(&[("HOME", "/home/a")])).unwrap();
        same(&d.data, "/home/a/.local/share/Orthic Labs/CutRight");
        assert_eq!(d.local, d.data);
        same(&d.cache, "/home/a/.cache/Orthic Labs/CutRight");
        let d = AppDirs::resolve_with(
            &app,
            &o,
            &env_of(&[
                ("HOME", "/home/a"),
                ("XDG_DATA_HOME", "/x/data"),
                ("XDG_CACHE_HOME", "/x/cache"),
            ]),
        )
        .unwrap();
        same(&d.data, "/x/data/Orthic Labs/CutRight");
        same(&d.cache, "/x/cache/Orthic Labs/CutRight");
        // A relative XDG value is ignored, not trusted.
        let d = AppDirs::resolve_with(
            &app,
            &o,
            &env_of(&[("HOME", "/home/a"), ("XDG_DATA_HOME", "rel")]),
        )
        .unwrap();
        same(&d.data, "/home/a/.local/share/Orthic Labs/CutRight");
    }

    #[test]
    fn no_org_keeps_a_flat_layout() {
        let d = AppDirs::resolve_with(
            &AppId::new("ViewRight").without_org(),
            &Options::new(Os::Linux),
            &env_of(&[("HOME", "/home/a")]),
        )
        .unwrap();
        same(&d.data, "/home/a/.local/share/ViewRight");
    }

    #[test]
    fn dev_build_uses_the_dev_namespace() {
        let d = AppDirs::resolve_with(
            &AppId::new("ViewRight"),
            &Options::new(Os::Linux).dev(true),
            &env_of(&[("HOME", "/home/a")]),
        )
        .unwrap();
        same(&d.data, "/home/a/.local/share/Orthic Labs/ViewRightDev");
        same(&d.cache, "/home/a/.cache/Orthic Labs/ViewRightDev");
        assert_eq!(AppId::new("X").namespace(false), "X");
        assert_eq!(AppId::new("X").namespace(true), "XDev");
    }

    #[test]
    fn missing_or_relative_base_is_an_error_never_the_cwd() {
        let app = AppId::new("A");
        let none = env_of(&[]);
        for os in [Os::Windows, Os::MacOs, Os::Linux] {
            assert!(matches!(
                AppDirs::resolve_with(&app, &Options::new(os), &none),
                Err(AppDirsError::NoBase { .. })
            ));
        }
        let rel = env_of(&[("APPDATA", "rel"), ("LOCALAPPDATA", "rel"), ("USERPROFILE", "rel")]);
        assert!(AppDirs::resolve_with(&app, &Options::new(Os::Windows), &rel).is_err());
        let blank = env_of(&[("HOME", "   ")]);
        assert!(AppDirs::resolve_with(&app, &Options::new(Os::Linux), &blank).is_err());
    }

    #[test]
    fn test_root_is_used_without_consulting_the_environment() {
        let root = process_test_root();
        let o = Options::new(Os::current()).test_root(Some(root.clone()));
        // An empty environment would error on the real mapping; the test root needs none.
        let d = AppDirs::resolve_with(&AppId::new("ViewRight"), &o, &env_of(&[])).unwrap();
        let r = root.join("ViewRight");
        assert_eq!(d.data, r.join("data"));
        assert_eq!(d.local, r.join("local"));
        assert_eq!(d.cache, r.join("cache"));
        assert_eq!(d.home, r.join("home"));
        assert!(d.isolated);
        for dir in [&d.data, &d.local, &d.cache, &d.home] {
            assert!(dir.starts_with(std::env::temp_dir()));
        }
        // The dev suffix applies inside the test root too.
        let d = AppDirs::resolve_with(&AppId::new("ViewRight"), &o.clone().dev(true), &env_of(&[]))
            .unwrap();
        assert_eq!(d.data, root.join("ViewRightDev").join("data"));
        // A relative test root is refused.
        let bad = Options::new(Os::Linux).test_root(Some(p("rel")));
        assert!(AppDirs::resolve_with(&AppId::new("A"), &bad, &env_of(&[])).is_err());
    }

    #[test]
    fn resolve_under_this_crates_own_tests_stays_in_the_temp_root() {
        let d = AppDirs::resolve(AppId::new("ViewRight")).unwrap();
        let root = process_test_root();
        for dir in [&d.data, &d.local, &d.cache, &d.home] {
            assert!(dir.starts_with(&root), "{} outside {}", dir.display(), root.display());
        }
        let t = AppDirs::for_test(AppId::new("ViewRight")).unwrap();
        assert!(t.data.starts_with(&root));
    }

    #[test]
    fn create_all_makes_the_folders() {
        let d = AppDirs::for_test(AppId::new("AppDirsCreateAll")).unwrap();
        d.create_all().unwrap();
        assert!(d.data.is_dir() && d.local.is_dir() && d.cache.is_dir() && d.home.is_dir());
        let _ = std::fs::remove_dir_all(process_test_root().join("AppDirsCreateAll"));
    }

    /// Without `test-mode` nothing reads an override variable, even when asked to.
    #[cfg(not(feature = "test-mode"))]
    #[test]
    fn overrides_are_ignored_without_test_mode() {
        let env = env_of(&[
            ("HOME", "/home/a"),
            ("RIGHTKIT_QA_DATA_DIR", "/tmp/qa"),
            ("RIGHTKIT_QA_HOME", "/tmp/qahome"),
        ]);
        let d = AppDirs::resolve_with(
            &AppId::new("ViewRight"),
            &Options::new(Os::Linux).qa_overrides(true),
            &env,
        )
        .unwrap();
        same(&d.data, "/home/a/.local/share/Orthic Labs/ViewRight");
        assert!(!d.isolated);
    }

    #[cfg(feature = "test-mode")]
    mod qa {
        use super::*;

        fn qa(os: Os) -> Options {
            Options::new(os).qa_overrides(true)
        }

        #[test]
        fn data_override_roots_every_folder_under_it() {
            let env = env_of(&[("HOME", "/home/a"), ("RIGHTKIT_QA_DATA_DIR", "/tmp/qa")]);
            let d = AppDirs::resolve_with(&AppId::new("ViewRight"), &qa(Os::Linux), &env).unwrap();
            assert_eq!(d.data, p("/tmp/qa"));
            same(&d.local, "/tmp/qa/local");
            same(&d.cache, "/tmp/qa/cache");
            same(&d.home, "/tmp/qa/home");
            assert!(d.isolated);
        }

        #[test]
        fn named_qa_home_wins_over_the_default_inside_the_data_dir() {
            let env = env_of(&[
                ("RIGHTKIT_QA_DATA_DIR", "/tmp/qa"),
                ("RIGHTKIT_QA_HOME", "/tmp/qahome"),
            ]);
            // No real base is needed when everything is overridden.
            let d = AppDirs::resolve_with(&AppId::new("A"), &qa(Os::Linux), &env).unwrap();
            assert_eq!(d.home, p("/tmp/qahome"));
        }

        #[test]
        fn home_alone_keeps_real_data_but_isolates_home() {
            let env = env_of(&[("HOME", "/home/a"), ("RIGHTKIT_QA_HOME", "/tmp/qahome")]);
            let d = AppDirs::resolve_with(&AppId::new("A"), &qa(Os::Linux), &env).unwrap();
            assert_eq!(d.home, p("/tmp/qahome"));
            same(&d.data, "/home/a/.local/share/Orthic Labs/A");
            assert!(d.isolated);
        }

        #[test]
        fn legacy_variables_apply_after_the_shared_one() {
            let app = AppId::new("HeardRight")
                .with_legacy_env(Some("HR_APP_DATA_DIR"), Some("HR_HOME"));
            let env = env_of(&[("HR_APP_DATA_DIR", "/tmp/hr"), ("HR_HOME", "/tmp/hrhome")]);
            let d = AppDirs::resolve_with(&app, &qa(Os::Linux), &env).unwrap();
            assert_eq!((d.data, d.home), (p("/tmp/hr"), p("/tmp/hrhome")));
            let env = env_of(&[
                ("HR_APP_DATA_DIR", "/tmp/hr"),
                ("RIGHTKIT_QA_DATA_DIR", "/tmp/shared"),
            ]);
            let d = AppDirs::resolve_with(&app, &qa(Os::Linux), &env).unwrap();
            assert_eq!(d.data, p("/tmp/shared"));
        }

        #[test]
        fn relative_override_is_refused() {
            for bad in ["rel/dir", "/tmp/../etc", ""] {
                let env = env_of(&[("HOME", "/home/a"), ("RIGHTKIT_QA_DATA_DIR", bad)]);
                let r = AppDirs::resolve_with(&AppId::new("A"), &qa(Os::Linux), &env);
                if bad.is_empty() {
                    // An empty variable counts as unset: the real mapping applies.
                    assert!(!r.unwrap().isolated);
                } else {
                    assert!(matches!(r, Err(AppDirsError::NotAbsolute { .. })), "{bad}");
                }
            }
        }

        #[test]
        fn override_at_or_inside_the_real_app_folder_is_refused() {
            let app = AppId::new("ViewRight");
            for dir in [
                "/home/a/.local/share/Orthic Labs/ViewRight",
                "/home/a/.local/share/Orthic Labs/ViewRight/qa",
                "/home/a/.local/share/Orthic Labs/ViewRightDev",
                "/home/a/.local/share/ViewRight",
                "/home/a/.cache/Orthic Labs/ViewRight",
            ] {
                let env = env_of(&[("HOME", "/home/a"), ("RIGHTKIT_QA_DATA_DIR", dir)]);
                assert!(
                    matches!(
                        AppDirs::resolve_with(&app, &qa(Os::Linux), &env),
                        Err(AppDirsError::RealFolder { .. })
                    ),
                    "{dir}"
                );
            }
            // A sibling that merely shares a prefix is not the real folder.
            let env = env_of(&[
                ("HOME", "/home/a"),
                ("RIGHTKIT_QA_DATA_DIR", "/home/a/.local/share/Orthic Labs/ViewRightQA"),
            ]);
            assert!(AppDirs::resolve_with(&app, &qa(Os::Linux), &env).is_ok());
        }

        #[test]
        fn windows_refusal_is_case_insensitive_and_covers_home() {
            let mut pairs = WIN.to_vec();
            pairs.push(("RIGHTKIT_QA_DATA_DIR", r"c:\users\A\appdata\roaming\orthic labs\viewright"));
            let r = AppDirs::resolve_with(&AppId::new("ViewRight"), &qa(Os::Windows), &env_of(&pairs));
            assert!(matches!(r, Err(AppDirsError::RealFolder { .. })));

            let mut pairs = WIN.to_vec();
            pairs.push(("RIGHTKIT_QA_HOME", r"C:\USERS\a"));
            let r = AppDirs::resolve_with(&AppId::new("ViewRight"), &qa(Os::Windows), &env_of(&pairs));
            assert!(matches!(r, Err(AppDirsError::RealFolder { .. })));

            // A QA workspace that redirects APPDATA to its own data dir is not "real".
            let env = env_of(&[
                ("APPDATA", r"C:\qa\data"),
                ("LOCALAPPDATA", r"C:\qa\data"),
                ("USERPROFILE", r"C:\qa\home"),
                ("RIGHTKIT_QA_DATA_DIR", r"C:\qa\data"),
            ]);
            let d = AppDirs::resolve_with(&AppId::new("ViewRight"), &qa(Os::Windows), &env).unwrap();
            assert_eq!(d.data, p(r"C:\qa\data"));
        }
    }
}
