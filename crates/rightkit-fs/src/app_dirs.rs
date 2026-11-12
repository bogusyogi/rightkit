//! One resolver for an app's per-user folders (`data`, `local`, `cache`, `home`) with a dev
//! namespace.
//!
//! This replaces the hand-rolled `app_paths.rs` / `*_DATA_DIR` copies in each app. The rules:
//!
//! * **Per-OS bases.** (The mappings below describe [`AppDirs::resolve_with`] over a supplied
//!   environment; the process-reading resolvers feed it the account's folders instead of `HOME`,
//!   `XDG_*`, `APPDATA`, `LOCALAPPDATA` and `USERPROFILE`, and Linux has no XDG override there.) Windows: the Roaming AppData known folder (data) and the Local AppData known
//!   folder (local, cache), home from the Profile known folder. [`AppDirs::resolve`] asks
//!   `SHGetKnownFolderPath` for the Profile folder, and reads the Roaming and Local AppData folders
//!   from the `User Shell Folders` registry value expanded against the account's environment block
//!   (`ExpandEnvironmentStringsForUserW` with the process token); redirection of `%APPDATA%`,
//!   `%LOCALAPPDATA%` and `%USERPROFILE%` in the process environment is ignored, as the `dirs`
//!   crate does; a failed lookup is an error
//!   ([`AppDirsError::AccountHome`] or `NoBase`), never a fallback to the variable.
//!   [`AppDirs::resolve`] and [`AppDirs::resolve_with_options`] build the macOS and Linux bases from
//!   [`account_home`] (`getpwuid_r(geteuid())`), ignoring `HOME` and every `XDG_*` variable, so a
//!   redirected environment cannot move a release app's folders. [`AppDirs::resolve_with`] uses supplied environment for ordinary mappings;
//!   macOS: `~/Library/Application Support` (data and local) and
//!   `~/Library/Caches` (cache). Linux: `$XDG_DATA_HOME` (else `~/.local/share`) for data and
//!   local, `$XDG_CACHE_HOME` (else `~/.cache`) for cache. Every base must be absolute; an unset or
//!   relative base is an error, never the working directory.
//! * **Config folder.** [`AppDirs::config`] is where the platform keeps per-user settings, apart
//!   from data where it has a separate place: Linux `$XDG_CONFIG_HOME` (else `~/.config`); on
//!   Windows (Roaming AppData) and macOS (`~/Library/Application Support`) the platform has no
//!   separate base, so `config` is the same folder as `data`. A Linux process with no `HOME` and no
//!   `XDG_CONFIG_HOME` gets `config` next to `data` rather than a new failure. Android: the
//!   injected files folder.
//! * **Legacy names.** [`AppId::with_legacy_names`] lists earlier app names whose folders must not
//!   be abandoned. [`AppDirs::resolve`] (and [`AppDirs::resolve_with_legacy`], which takes the
//!   existence test as a parameter) picks, for each of `data`, `local` and `config` separately,
//!   the current name's folder if it exists, else the first legacy name's folder that exists, else
//!   the current name. `cache` always uses the current name (it is re-creatable). Nothing is moved
//!   or created. Injected folders ignore legacy names.
//! * **App folder.** `<base>/<org>/<namespace>`, org defaulting to `Orthic Labs`
//!   ([`AppId::without_org`] keeps an existing flat `<base>/<name>` install). Windows cache is
//!   `<LOCALAPPDATA>/<org>/<namespace>/Cache` so it never shares a folder with state.
//! * **User folders.** [`user_dirs`] answers the account's home, Desktop, Documents and Downloads.
//! * **Dev namespace.** Only an [`Options`] with `dev` set uses `<name>Dev` (for example
//!   `ViewRightDev`); no build sets it by default, so a dev run is not isolated from the installed app.
//! * **No test mode.** There is no test root and no QA override: every build resolves the real
//!   per-user folders. QA runs against the real home and uses `rightkit-qa` snapshot and restore.
//!
//! * **Android.** There is no environment-based resolution on Android: the app gets its folders
//!   from the `Context` (`getFilesDir()`, `getCacheDir()`) and passes them in with
//!   [`AppDirs::injected`]. [`AppDirs::resolve`] (and `resolve_with` for [`Os::Android`]) return
//!   [`AppDirsError::AndroidNeedsInjectedRoots`] instead of guessing from `HOME`. Those folders are
//!   already private to the package and already separate per `applicationId` (a dev build uses its
//!   own id), so no org or `Dev` namespace is added. The injected constructor is compiled on every
//!   target, so host tests cover it.
//!
//! Mappings are pure over an injected environment. The native resolvers use account and
//! known-folder locations, ignoring redirected HOME/XDG/APPDATA.
//! Nothing here creates a directory except [`AppDirs::create_all`].

pub mod user_dirs;

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// The organisation folder every Right Suite app nests under.
pub const DEFAULT_ORG: &str = "Orthic Labs";

/// Why folders could not be resolved.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AppDirsError {
    /// A base folder the OS mapping needs is unset or empty.
    #[error("cannot resolve the {what} folder: {why}")]
    NoBase { what: &'static str, why: String },
    /// The operating system could not name the account's home or known folders. Never answered
    /// from `HOME`, `XDG_*`, `APPDATA`, `LOCALAPPDATA` or `USERPROFILE`: a redirected environment
    /// must not move an app's real folders.
    #[error("cannot resolve the account's folders from the operating system: {why}")]
    AccountHome { why: String },
    /// Android has no environment-based mapping; the folders come from the `Context`.
    #[error("Android has no environment-based folder resolution: pass the Context's filesDir and cacheDir to AppDirs::injected")]
    AndroidNeedsInjectedRoots,
    /// An injected root is unusable (relative, `..`, empty, or the two roots overlap).
    #[error("injected {what} folder is unusable: {why}")]
    BadInjectedRoot { what: &'static str, why: String },
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
    /// No environment mapping exists; folders are injected ([`AppDirs::injected`]).
    Android,
}

impl Os {
    /// The OS this build targets (anything that is neither Windows, Android nor macOS maps as
    /// Linux/XDG).
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "android") {
            Os::Android
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
    /// Earlier app names whose existing folders are still used, in order of preference (for
    /// example `&["LightCraft"]` for an app renamed to `Ember`). See [`AppId::with_legacy_names`].
    pub legacy_names: &'static [&'static str],
}

impl AppId {
    /// `name` under [`DEFAULT_ORG`].
    pub const fn new(name: &'static str) -> AppId {
        AppId {
            name,
            org: Some(DEFAULT_ORG),
            legacy_names: &[],
        }
    }

    /// Earlier names of this app, most preferred first. A folder under one of them that already
    /// exists is used instead of a missing folder under the current name, so a renamed app keeps
    /// its users' settings and data where they are. Applied to `data`, `local` and `config` (each
    /// on its own) by [`AppDirs::resolve`] and [`AppDirs::resolve_with_legacy`]; the dev namespace
    /// is applied to each legacy name too.
    pub const fn with_legacy_names(mut self, names: &'static [&'static str]) -> AppId {
        self.legacy_names = names;
        self
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
}

impl Options {
    /// A release-shaped resolution for `os`: not dev.
    pub fn new(os: Os) -> Options {
        Options { os, dev: false }
    }

    /// This build: the target OS, release-shaped (not dev).
    pub fn current() -> Options {
        Options {
            os: Os::current(),
            dev: false,
        }
    }

    pub fn dev(mut self, dev: bool) -> Options {
        self.dev = dev;
        self
    }
}

/// An app's resolved folders.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct AppDirs {
    /// Roaming data (databases, settings).
    pub data: PathBuf,
    /// Per-user configuration. A separate folder from `data` where the platform has one (Linux
    /// `$XDG_CONFIG_HOME`); the same folder as `data` on Windows and macOS, and on Android.
    pub config: PathBuf,
    /// Machine-local data (logs, WebView profile, large artifacts).
    pub local: PathBuf,
    /// Re-creatable cache.
    pub cache: PathBuf,
    /// The home folder to show or search (the user's real home).
    pub home: PathBuf,
}

impl AppDirs {
    /// Resolve for this build. The per-user base folders come from the operating system's account
    /// (see [`account_home`] and the module docs), never from `HOME`, `XDG_*`, `APPDATA`,
    /// `LOCALAPPDATA` or `USERPROFILE`.
    pub fn resolve(app: AppId) -> Result<AppDirs> {
        resolve_native(&app, &Options::current(), &account_home, &|p| p.is_dir())
    }

    /// [`AppDirs::resolve`] with explicit [`Options`] instead of [`Options::current`]: `dev(false)`
    /// gives the release namespace from a dev build, and the namespace follows `opts.dev` alone.
    /// The base folders come from the operating system's account as in [`AppDirs::resolve`] (on
    /// Windows the known-folder API only, with no environment fallback), and legacy names apply
    /// when their folder already exists.
    /// On Windows the known-folder lookup is the build's own, so `opts.os` should name the build's
    /// OS for the real per-user folders.
    pub fn resolve_with_options(app: AppId, opts: &Options) -> Result<AppDirs> {
        resolve_native(&app, opts, &account_home, &|p| p.is_dir())
    }

    /// `env` answers variable lookups. Legacy folder discovery is disabled here; use
    /// [`AppDirs::resolve_with_legacy`] for that.
    pub fn resolve_with(
        app: &AppId,
        opts: &Options,
        env: &dyn Fn(&str) -> Option<OsString>,
    ) -> Result<AppDirs> {
        AppDirs::resolve_with_legacy(app, opts, env, &|_| false)
    }

    /// [`AppDirs::resolve_with`] that also applies [`AppId::legacy_names`]: `exists` answers
    /// whether a candidate folder is already there ([`AppDirs::resolve`] passes `Path::is_dir`).
    /// Pure when `exists` is.
    pub fn resolve_with_legacy(
        app: &AppId,
        opts: &Options,
        env: &dyn Fn(&str) -> Option<OsString>,
        exists: &dyn Fn(&Path) -> bool,
    ) -> Result<AppDirs> {
        let ns = app.namespace(opts.dev);
        if opts.os == Os::Android {
            return Err(AppDirsError::AndroidNeedsInjectedRoots);
        }
        real_dirs(app, opts, &ns, env, exists)
    }

    /// Folders handed in by the host app (Android: `Context.getFilesDir()` and
    /// `Context.getCacheDir()`), for targets with no environment mapping. `data` and `local` are
    /// `files_dir` (as on macOS, where the two are one folder), `cache` is `cache_dir`, and `home`
    /// is `files_dir` too: Android has no user home to show. Nothing is appended (no org, no `Dev`
    /// namespace): the OS already gives each package its own private folders.
    ///
    /// Both must be absolute and free of `..`, and neither may be the other or inside it (the OS
    /// may clear the cache folder, which must never be able to take state with it). Pure: touches
    /// no file and reads no environment, and it is compiled on every target.
    pub fn injected(
        files_dir: impl Into<PathBuf>,
        cache_dir: impl Into<PathBuf>,
    ) -> Result<AppDirs> {
        AppDirs::injected_for(Os::current(), files_dir.into(), cache_dir.into())
    }

    /// [`AppDirs::injected`] with the path syntax of `os` named, so each syntax is testable on any
    /// host.
    pub fn injected_for(os: Os, files_dir: PathBuf, cache_dir: PathBuf) -> Result<AppDirs> {
        let bad = |what: &'static str, why: &str| AppDirsError::BadInjectedRoot {
            what,
            why: why.to_string(),
        };
        for (what, path) in [("files", &files_dir), ("cache", &cache_dir)] {
            if path.as_os_str().is_empty() {
                return Err(bad(what, "empty"));
            }
            if !looks_absolute(os, path) {
                return Err(bad(what, "must be an absolute path without `..`"));
            }
        }
        let win = os == Os::Windows;
        if inside_or_equal(&files_dir, &cache_dir, win)
            || inside_or_equal(&cache_dir, &files_dir, win)
        {
            return Err(bad("cache", "must not equal or nest with the files folder"));
        }
        Ok(AppDirs {
            data: files_dir.clone(),
            config: files_dir.clone(),
            local: files_dir.clone(),
            cache: cache_dir,
            home: files_dir,
        })
    }

    /// Move the directory `from` to `to` (for example an old data folder to the current one) so
    /// that `to` is never visible half-migrated: one rename on the same volume, otherwise a copy to
    /// a hidden sibling of `to`, a rename of the sibling to `to`, then removal of `from`. `to` must
    /// be absent or an empty directory; a populated one is refused with
    /// [`FsError::AlreadyExists`](crate::FsError::AlreadyExists) and nothing is written. See
    /// [`crate::migrate`] for the full rules.
    pub fn migrate_dir(from: &Path, to: &Path) -> crate::error::Result<crate::migrate::Migrated> {
        crate::migrate::migrate_dir(from, to)
    }

    /// Create the folders (plain `create_dir_all`; use `ensure_private_dir` for private state).
    pub fn create_all(&self) -> io::Result<()> {
        for d in [
            &self.data,
            &self.config,
            &self.local,
            &self.cache,
            &self.home,
        ] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }
}

/// The OS base folders, each `None` when unset, empty or not absolute.
struct Bases {
    data: Option<PathBuf>,
    config: Option<PathBuf>,
    local: Option<PathBuf>,
    cache: Option<PathBuf>,
    home: Option<PathBuf>,
}

/// The environment [`AppDirs::resolve`] reads. The base-folder variables are never taken from the
/// process environment: macOS and Linux answer `HOME` from the account database
/// ([`account_home`]) and report `XDG_*` as unset; Windows answers `APPDATA`, `LOCALAPPDATA` and
/// `USERPROFILE` from the known-folder API only (no fallback to the variable when the call fails).
/// Every other name is the process environment.
fn native_env(
    key: &str,
    #[cfg_attr(not(any(unix, windows)), allow(unused_variables))] account: &dyn Fn() -> Result<
        PathBuf,
    >,
) -> Option<OsString> {
    match key {
        #[cfg(unix)]
        "HOME" => account().ok().map(PathBuf::into_os_string),
        #[cfg(unix)]
        "XDG_DATA_HOME" | "XDG_CACHE_HOME" | "XDG_CONFIG_HOME" => None,
        #[cfg(windows)]
        "APPDATA" => known::known_folder(&known::ROAMING_APP_DATA).map(PathBuf::into_os_string),
        #[cfg(windows)]
        "LOCALAPPDATA" => known::known_folder(&known::LOCAL_APP_DATA).map(PathBuf::into_os_string),
        #[cfg(windows)]
        "USERPROFILE" => account().ok().map(PathBuf::into_os_string),
        _ => std::env::var_os(key),
    }
}

/// [`AppDirs::resolve_with_legacy`] over [`native_env`], naming the account lookup as the cause
/// when a base is missing because the operating system could not answer. `account` is the home
/// lookup ([`account_home`]), a parameter so its failure is testable.
fn resolve_native(
    app: &AppId,
    opts: &Options,
    account: &dyn Fn() -> Result<PathBuf>,
    exists: &dyn Fn(&Path) -> bool,
) -> Result<AppDirs> {
    let env = |key: &str| native_env(key, account);
    AppDirs::resolve_with_legacy(app, opts, &env, exists).map_err(|err| {
        let AppDirsError::NoBase { what, .. } = &err else {
            return err;
        };
        #[cfg(not(windows))]
        let _ = what;
        if opts.os != Os::current() {
            return err;
        }
        #[cfg(unix)]
        {
            match account() {
                Err(account_err) => account_err,
                Ok(_) => err,
            }
        }
        #[cfg(windows)]
        {
            AppDirsError::AccountHome {
                why: format!(
                    "the known-folder lookup for the {what} folder failed or was not absolute"
                ),
            }
        }
        #[cfg(not(any(unix, windows)))]
        err
    })
}

/// The real home folder of the account this process runs as, from the operating system and never
/// from the environment.
///
/// * Unix: the `pw_dir` of `getpwuid_r(geteuid())`. `HOME` is ignored, so a launcher that
///   redirects it cannot move an app's folders.
/// * Windows: the Profile known folder (`SHGetKnownFolderPath`). `%USERPROFILE%` is ignored.
///
/// Fails with [`AppDirsError::AccountHome`] when the lookup fails, the account has no home, or the
/// answer is not an absolute path without `..`. There is no environment fallback.
pub fn account_home() -> Result<PathBuf> {
    #[cfg(unix)]
    {
        passwd_home()
    }
    #[cfg(windows)]
    {
        known::known_folder(&known::PROFILE)
            .filter(|p| looks_absolute(Os::Windows, p))
            .ok_or_else(|| AppDirsError::AccountHome {
                why: "the Profile known folder is unavailable or not absolute".to_string(),
            })
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(AppDirsError::AccountHome {
            why: "no account lookup on this platform".to_string(),
        })
    }
}

#[cfg(unix)]
fn passwd_home() -> Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let fail = |why: &str| AppDirsError::AccountHome {
        why: why.to_string(),
    };
    let mut size = 16 * 1024;
    loop {
        let mut buffer = vec![0u8; size];
        let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut found = std::ptr::null_mut();
        // SAFETY: `record` and `buffer` are writable and stay alive until `pw_dir` is copied.
        let status = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                record.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        };
        if status == libc::ERANGE && size < 1024 * 1024 {
            size *= 2;
            continue;
        }
        if status != 0 {
            return Err(fail("getpwuid_r failed"));
        }
        if found.is_null() {
            return Err(fail("the account has no passwd entry"));
        }
        // SAFETY: a successful lookup initialized `record`; `pw_dir` points into `buffer`.
        let raw = unsafe { record.assume_init().pw_dir };
        if raw.is_null() {
            return Err(fail("the passwd entry has no home directory"));
        }
        // SAFETY: `pw_dir` is a NUL-terminated string inside `buffer`, still alive.
        let bytes = unsafe { std::ffi::CStr::from_ptr(raw) }.to_bytes();
        let home = PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
        if !looks_absolute(Os::current(), &home) {
            return Err(fail("the passwd home is empty, relative or contains `..`"));
        }
        return Ok(home);
    }
}

/// A per-process memo of operating-system folder answers, keyed by what the answer depends on.
///
/// Only successes are kept: a failed lookup (for example while the environment block changes) is
/// retried on the next call, never remembered. The lock is held while a missing entry is resolved,
/// so concurrent first callers run the lookup once and the rest wait for that answer.
#[cfg_attr(not(windows), allow(dead_code))]
struct FolderCache<K> {
    entries: std::sync::Mutex<Vec<(K, PathBuf)>>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl<K: PartialEq> FolderCache<K> {
    const fn new() -> FolderCache<K> {
        FolderCache {
            entries: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The cached answer for `key`, else `resolve()`'s, which is cached when it is `Some`.
    fn get_or_resolve(&self, key: K, resolve: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, path)) = entries.iter().find(|(k, _)| *k == key) {
            return Some(path.clone());
        }
        let path = resolve()?;
        entries.push((key, path.clone()));
        Some(path)
    }
}

#[cfg(windows)]
mod known {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;

    #[repr(C)]
    #[derive(PartialEq, Eq)]
    pub(super) struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    impl Guid {
        /// All 128 bits, the cache key.
        fn key(&self) -> u128 {
            let mut bytes = [0u8; 16];
            bytes[..4].copy_from_slice(&self.data1.to_le_bytes());
            bytes[4..6].copy_from_slice(&self.data2.to_le_bytes());
            bytes[6..8].copy_from_slice(&self.data3.to_le_bytes());
            bytes[8..].copy_from_slice(&self.data4);
            u128::from_le_bytes(bytes)
        }
    }

    /// `FOLDERID_RoamingAppData`.
    pub(super) static ROAMING_APP_DATA: Guid = Guid {
        data1: 0x3EB6_85DB,
        data2: 0x65F9,
        data3: 0x4CF6,
        data4: [0xA0, 0x3A, 0xE3, 0xEF, 0x65, 0x72, 0x9F, 0x3D],
    };
    /// `FOLDERID_LocalAppData`.
    pub(super) static LOCAL_APP_DATA: Guid = Guid {
        data1: 0xF1B3_2785,
        data2: 0x6FBA,
        data3: 0x4FCF,
        data4: [0x9D, 0x55, 0x7B, 0x8E, 0x7F, 0x15, 0x70, 0x91],
    };
    /// `FOLDERID_Desktop`.
    pub(super) static DESKTOP: Guid = Guid {
        data1: 0xB4BF_CC3A,
        data2: 0xDB2C,
        data3: 0x424C,
        data4: [0xB0, 0x29, 0x7F, 0xE9, 0x9A, 0x87, 0xC6, 0x41],
    };
    /// `FOLDERID_Documents`.
    pub(super) static DOCUMENTS: Guid = Guid {
        data1: 0xFDD3_9AD0,
        data2: 0x238F,
        data3: 0x46AF,
        data4: [0xAD, 0xB4, 0x6C, 0x85, 0x48, 0x03, 0x69, 0xC7],
    };
    /// `FOLDERID_Downloads`.
    pub(super) static DOWNLOADS: Guid = Guid {
        data1: 0x374D_E290,
        data2: 0x123F,
        data3: 0x4565,
        data4: [0x91, 0x64, 0x39, 0xC4, 0x92, 0x5E, 0x46, 0x7B],
    };
    /// `FOLDERID_Profile`.
    pub(super) static PROFILE: Guid = Guid {
        data1: 0x5E6C_858F,
        data2: 0x0E22,
        data3: 0x4760,
        data4: [0x9A, 0xFE, 0xEA, 0x33, 0x17, 0xB6, 0x71, 0x73],
    };

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHGetKnownFolderPath(
            rfid: *const Guid,
            flags: u32,
            token: *mut c_void,
            path: *mut *mut u16,
        ) -> i32;
    }

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(pv: *mut c_void);
    }

    /// `KF_FLAG_DONT_VERIFY`: do not require the folder to exist (the Profile folder is returned
    /// even when a later subfolder is missing; creation is the caller's choice).
    const KF_FLAG_DONT_VERIFY: u32 = 0x4000;

    /// The known folder `id` of the current user, or `None` when the call fails or returns an
    /// empty string. The returned buffer is always freed with `CoTaskMemFree`, also on failure
    /// (the API may set it even then).
    fn api_folder(id: &Guid) -> Option<PathBuf> {
        let mut raw: *mut u16 = std::ptr::null_mut();
        // SAFETY: `id` is a valid GUID, a null token means the current user, and `raw` is a valid
        // out pointer. On return `raw` is null or a NUL-terminated UTF-16 string we own.
        let hr = unsafe {
            SHGetKnownFolderPath(id, KF_FLAG_DONT_VERIFY, std::ptr::null_mut(), &mut raw)
        };
        if raw.is_null() {
            return None;
        }
        let path = if hr >= 0 {
            let mut len = 0usize;
            // SAFETY: `raw` is NUL-terminated, so every offset up to the terminator is readable.
            while unsafe { *raw.add(len) } != 0 {
                len += 1;
            }
            // SAFETY: `len` elements were just read from `raw`.
            let wide = unsafe { std::slice::from_raw_parts(raw, len) };
            Some(PathBuf::from(std::ffi::OsString::from_wide(wide)))
        } else {
            None
        };
        // SAFETY: `raw` came from SHGetKnownFolderPath and is freed exactly once.
        unsafe { CoTaskMemFree(raw.cast()) };
        path.filter(|p| !p.as_os_str().is_empty())
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
        fn RegOpenCurrentUser(sam: u32, key: *mut *mut c_void) -> i32;
        fn RegCloseKey(key: *mut c_void) -> i32;
        fn RegGetValueW(
            key: *mut c_void,
            subkey: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut c_void,
            len: *mut u32,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    #[link(name = "userenv")]
    unsafe extern "system" {
        fn ExpandEnvironmentStringsForUserW(
            token: *mut c_void,
            src: *const u16,
            dest: *mut u16,
            size: u32,
        ) -> i32;
    }

    const TOKEN_DUPLICATE: u32 = 0x2;
    const TOKEN_IMPERSONATE: u32 = 0x4;
    const TOKEN_QUERY: u32 = 0x8;
    const KEY_READ: u32 = 0x2_0019;
    const RRF_RT_REG_SZ: u32 = 0x2;
    const RRF_RT_REG_EXPAND_SZ: u32 = 0x4;
    const RRF_NOEXPAND: u32 = 0x1000_0000;
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_MORE_DATA: i32 = 234;
    const ERROR_INSUFFICIENT_BUFFER: i32 = 122;
    const USER_SHELL_FOLDERS: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders";

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        text.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn from_wide_nul(buffer: &[u16]) -> std::ffi::OsString {
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        std::ffi::OsString::from_wide(&buffer[..len])
    }

    /// Expand `%NAME%` references in `template` against the *account's* environment block
    /// (`ExpandEnvironmentStringsForUserW` with this process's token), never the process
    /// environment. `None` when the token or the expansion fails, or when a reference is left
    /// unexpanded.
    pub(super) fn expand_template(template: &std::ffi::OsStr) -> Option<PathBuf> {
        let mut token: *mut c_void = std::ptr::null_mut();
        // SAFETY: the pseudo handle is valid, `token` is a valid out pointer.
        let ok = unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE,
                &mut token,
            )
        };
        if ok == 0 || token.is_null() {
            return None;
        }
        let src = wide(template);
        let mut size = 1024usize;
        let result = loop {
            let mut dest = vec![0u16; size];
            // SAFETY: `src` is NUL-terminated; `dest` holds `size` writable u16 elements.
            let ok = unsafe {
                ExpandEnvironmentStringsForUserW(
                    token,
                    src.as_ptr(),
                    dest.as_mut_ptr(),
                    size as u32,
                )
            };
            if ok != 0 {
                break Some(from_wide_nul(&dest));
            }
            let code = std::io::Error::last_os_error().raw_os_error();
            if code == Some(ERROR_INSUFFICIENT_BUFFER) && size < 64 * 1024 {
                size *= 2;
                continue;
            }
            break None;
        };
        // SAFETY: `token` was opened above and is closed once.
        unsafe { CloseHandle(token) };
        let path = PathBuf::from(result?);
        let text = path.to_string_lossy();
        (!text.contains('%') && super::looks_absolute(super::Os::Windows, &path)).then_some(path)
    }

    /// The raw (unexpanded) `User Shell Folders` template of the current user for `name`, read
    /// from the user hive that `RegOpenCurrentUser` resolves from the token. `Ok(None)` when the
    /// value is absent; `Err` for any other failure.
    fn user_shell_folder_template(name: &str) -> Result<Option<std::ffi::OsString>, ()> {
        let mut key: *mut c_void = std::ptr::null_mut();
        // SAFETY: `key` is a valid out pointer.
        if unsafe { RegOpenCurrentUser(KEY_READ, &mut key) } != 0 {
            return Err(());
        }
        let subkey = wide(std::ffi::OsStr::new(USER_SHELL_FOLDERS));
        let value = wide(std::ffi::OsStr::new(name));
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
        let mut bytes = 1024u32;
        let result = loop {
            let mut data = vec![0u16; (bytes as usize).div_ceil(2)];
            let mut len = bytes;
            // SAFETY: `key` is open; the name buffers are NUL-terminated; `data` holds `len`
            // writable bytes.
            let status = unsafe {
                RegGetValueW(
                    key,
                    subkey.as_ptr(),
                    value.as_ptr(),
                    flags,
                    std::ptr::null_mut(),
                    data.as_mut_ptr().cast(),
                    &mut len,
                )
            };
            match status {
                0 => break Ok(Some(from_wide_nul(&data))),
                ERROR_FILE_NOT_FOUND => break Ok(None),
                ERROR_MORE_DATA if len > bytes && len <= 64 * 1024 => bytes = len,
                _ => break Err(()),
            }
        };
        // SAFETY: `key` was opened above and is closed once.
        unsafe { RegCloseKey(key) };
        result
    }

    /// The known folder `id` of the current account, independent of the process environment.
    ///
    /// `SHGetKnownFolderPath` cannot be used for Roaming and Local AppData: the registry holds
    /// them as `REG_EXPAND_SZ` templates (`%USERPROFILE%\AppData\Roaming`, or a redirection such
    /// as `\\server\share\%USERNAME%\AppData\Roaming`) and the API expands them against the
    /// *process* environment (measured: a redirected `USERPROFILE` moves the result). Microsoft
    /// documents `KF_FLAG_DONT_UNEXPAND` for `SHSetKnownFolderPath` only, so the raw template
    /// cannot be had from the getter. Those two folders are therefore read as the raw
    /// `User Shell Folders` value of the current user hive and expanded with
    /// `ExpandEnvironmentStringsForUserW` over this process's token, so every variable comes from
    /// the account's own environment block. An absent value means the Windows default under the
    /// Profile folder. Any failure is `None`: there is no fallback to the process environment.
    /// The Profile folder comes from the account token and is returned by the API directly.
    ///
    /// The answer is memoized per process, keyed by the folder id: it depends only on the process
    /// token and the user hive (never on the process environment), which are fixed for the life
    /// of the process. The uncached lookup costs about 12 ms (`ExpandEnvironmentStringsForUserW`
    /// alone is about 9 ms), so call sites on a hot path rely on this. Failures are not cached.
    pub(super) fn known_folder(id: &Guid) -> Option<PathBuf> {
        static CACHE: super::FolderCache<u128> = super::FolderCache::new();
        CACHE.get_or_resolve(id.key(), || resolve_known_folder(id))
    }

    /// [`known_folder`] without the memo.
    fn resolve_known_folder(id: &Guid) -> Option<PathBuf> {
        let (value, default) = if *id == ROAMING_APP_DATA {
            ("AppData", r"AppData\Roaming")
        } else if *id == LOCAL_APP_DATA {
            ("Local AppData", r"AppData\Local")
        } else {
            return api_folder(id);
        };
        match user_shell_folder_template(value).ok()? {
            Some(template) if !template.is_empty() => expand_template(&template),
            Some(_) => None,
            None => Some(api_folder(&PROFILE)?.join(default)),
        }
    }
}

fn nonempty(env: &dyn Fn(&str) -> Option<OsString>, key: &str) -> Option<PathBuf> {
    env(key)
        .filter(|v| !v.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
}

fn absolute_env(os: Os, env: &dyn Fn(&str) -> Option<OsString>, key: &str) -> Option<PathBuf> {
    nonempty(env, key).filter(|p| looks_absolute(os, p))
}

impl Bases {
    fn read(os: Os, env: &dyn Fn(&str) -> Option<OsString>) -> Bases {
        match os {
            Os::Windows => Bases {
                data: absolute_env(os, env, "APPDATA"),
                config: absolute_env(os, env, "APPDATA"),
                local: absolute_env(os, env, "LOCALAPPDATA"),
                cache: absolute_env(os, env, "LOCALAPPDATA"),
                home: absolute_env(os, env, "USERPROFILE"),
            },
            Os::MacOs => {
                let home = absolute_env(os, env, "HOME");
                let support = home.as_ref().map(|h| h.join("Library/Application Support"));
                Bases {
                    data: support.clone(),
                    config: support.clone(),
                    local: support,
                    cache: home.as_ref().map(|h| h.join("Library/Caches")),
                    home,
                }
            }
            Os::Android => Bases {
                data: None,
                config: None,
                local: None,
                cache: None,
                home: None,
            },
            Os::Linux => {
                let home = absolute_env(os, env, "HOME");
                let data = absolute_env(os, env, "XDG_DATA_HOME")
                    .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
                let cache = absolute_env(os, env, "XDG_CACHE_HOME")
                    .or_else(|| home.as_ref().map(|h| h.join(".cache")));
                let config = absolute_env(os, env, "XDG_CONFIG_HOME")
                    .or_else(|| home.as_ref().map(|h| h.join(".config")))
                    .or_else(|| data.clone());
                Bases {
                    local: data.clone(),
                    config,
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
            "data" | "config" => "APPDATA is not set to an absolute path",
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

/// `<base>/<org>/<ns>` for the current name, unless that folder is missing and a legacy name's
/// folder (same base, org and dev rule) exists: then the first such legacy folder.
fn pick_app_dir(
    base: &Path,
    app: &AppId,
    opts: &Options,
    ns: &str,
    exists: &dyn Fn(&Path) -> bool,
) -> PathBuf {
    let current = app_dir(base, app, ns);
    if exists(&current) {
        return current;
    }
    for legacy in app.legacy_names {
        let name = if opts.dev {
            format!("{legacy}Dev")
        } else {
            (*legacy).to_string()
        };
        let candidate = app_dir(base, app, &name);
        if exists(&candidate) {
            return candidate;
        }
    }
    current
}

fn real_dirs(
    app: &AppId,
    opts: &Options,
    ns: &str,
    env: &dyn Fn(&str) -> Option<OsString>,
    exists: &dyn Fn(&Path) -> bool,
) -> Result<AppDirs> {
    let os = opts.os;
    let b = Bases::read(os, env);
    let data = b.data.ok_or_else(|| missing(os, "data"))?;
    let config = b.config.ok_or_else(|| missing(os, "config"))?;
    let local = b.local.ok_or_else(|| missing(os, "local"))?;
    let cache = b.cache.ok_or_else(|| missing(os, "cache"))?;
    let home = b.home.ok_or_else(|| missing(os, "home"))?;
    let cache = if os == Os::Windows {
        app_dir(&cache, app, ns).join("Cache")
    } else {
        app_dir(&cache, app, ns)
    };
    Ok(AppDirs {
        data: pick_app_dir(&data, app, opts, ns, exists),
        config: pick_app_dir(&config, app, opts, ns, exists),
        local: pick_app_dir(&local, app, opts, ns, exists),
        cache,
        home,
    })
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
    rooted
        && !components(&s, os == Os::Windows, false)
            .iter()
            .any(|c| c == "..")
}

fn components(s: &str, windows: bool, lower: bool) -> Vec<String> {
    s.split(|c: char| c == '/' || (windows && c == '\\'))
        .filter(|c| !c.is_empty())
        .map(|c| {
            if lower {
                c.to_lowercase()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// Whether `path` is `base` or below it (component-wise; case-insensitive on Windows).
fn inside_or_equal(path: &Path, base: &Path, windows: bool) -> bool {
    // On a Windows host `Path::join` writes `\`, so a path built for another OS (tests) mixes
    // separators; treat `\` as one there too.
    let seps = windows || cfg!(windows);
    let p = components(&path.to_string_lossy(), seps, windows);
    let b = components(&base.to_string_lossy(), seps, windows);
    p.len() >= b.len() && p[..b.len()] == b[..]
}
