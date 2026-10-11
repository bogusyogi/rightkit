use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub const SUITE_DIR_NAME: &str = "Orthic Labs";
/// The shared-data subdirectory under [`SUITE_DIR_NAME`]. HeardRight, ScrapeRight and
/// CutRight already root their shared model store and speech discovery here
/// (`.../Orthic Labs/shared`), so the one convention adopts it.
pub const SHARED_DIR_NAME: &str = "shared";
/// Overrides the platform default (tests, isolated dev profiles). Must be absolute.
/// `RIGHT_SUITE_DIR` and `ORTHIC_SHARED_DIR` are honoured as legacy aliases.
pub const ENV_SUITE_ROOT: &str = "RIGHTKIT_SUITE_ROOT";

#[derive(Debug)]
pub enum SuiteError {
    /// No absolute app-data root could be determined. Never resolved to the cwd.
    NoRoot(String),
    NotAbsolute(PathBuf),
    /// A directory that must be private to the current user is not.
    Insecure(String),
    Io(io::Error),
}

impl fmt::Display for SuiteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuiteError::NoRoot(s) => write!(
                f,
                "no app-data root: {s} (refusing to fall back to the working directory)"
            ),
            SuiteError::NotAbsolute(p) => write!(f, "suite root must be absolute: {}", p.display()),
            SuiteError::Insecure(s) => write!(f, "insecure service directory: {s}"),
            SuiteError::Io(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for SuiteError {}
impl From<io::Error> for SuiteError {
    fn from(e: io::Error) -> Self {
        SuiteError::Io(e)
    }
}

struct TempGuard(PathBuf);
impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The per-user suite directory that holds `runtime/<service>/service.json`
/// and other shared state. Always absolute.
#[derive(Clone)]
pub struct Suite {
    root: PathBuf,
    _temp: Option<Arc<TempGuard>>,
    /// The root was derived from the environment ([`Suite::resolve`]): an override variable, or
    /// `HOME`/`XDG_DATA_HOME`/`LOCALAPPDATA`. Whoever launches a process controls those.
    env_derived: bool,
}

impl fmt::Debug for Suite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Suite({})", self.root.display())
    }
}

fn absolute(p: PathBuf) -> Result<PathBuf, SuiteError> {
    if !p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(SuiteError::NotAbsolute(p));
    }
    Ok(p)
}

fn env_nonempty(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.trim().is_empty())
}

impl Suite {
    /// `RIGHTKIT_SUITE_ROOT` if set, else the platform per-user default
    /// (`~/Library/Application Support/Orthic Labs/shared`, `%LOCALAPPDATA%\Orthic Labs\shared`).
    /// Errors, never guesses, when the environment does not name an absolute root.
    ///
    /// Everything here comes from the environment, which is chosen by whoever launches the
    /// process: `RIGHTKIT_SUITE_ROOT`, `RIGHT_SUITE_DIR` and `ORTHIC_SHARED_DIR` redirect the root
    /// anywhere, and `HOME` (macOS, Linux), `XDG_DATA_HOME` (Linux) and `LOCALAPPDATA` (Windows)
    /// redirect the default. That is right for tests and isolated dev profiles and wrong for a
    /// release build that talks to its installed app: use [`Suite::per_user_default`] or
    /// [`Suite::resolve_ignoring_env_overrides`] there. The returned suite reports
    /// [`Suite::env_derived`] `true`.
    pub fn resolve() -> Result<Suite, SuiteError> {
        let mut suite = Self::resolve_from_env()?;
        suite.env_derived = true;
        Ok(suite)
    }

    /// The per-user default suite root from the operating system's account database, never from
    /// the environment: no `RIGHTKIT_SUITE_ROOT`/`RIGHT_SUITE_DIR`/`ORTHIC_SHARED_DIR`, no `HOME`,
    /// no `XDG_DATA_HOME`, no `LOCALAPPDATA`. This is what a release build of an app or of its
    /// command-line engine should use, on the host side as well as the client side, so that a
    /// launcher cannot point either at a different directory.
    ///
    /// * macOS: `<home>/Library/Application Support/Orthic Labs/shared`, `<home>` from
    ///   `getpwuid_r(geteuid())`.
    /// * Linux: `<home>/.local/share/Orthic Labs/shared`. `XDG_DATA_HOME` is an environment
    ///   variable and is ignored, so a user who relocated it must host with this function too, or
    ///   host and hardened client disagree on the root.
    /// * Windows: not implemented; fails closed. The Windows implementation must call
    ///   `SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None)` (then `CoTaskMemFree`
    ///   the result), which reads the profile's registry, not `%LOCALAPPDATA%`.
    /// * Other Unix: not implemented; fails closed.
    pub fn per_user_default() -> Result<Suite, SuiteError> {
        let base = platform_default_base()?;
        Suite::at(absolute(base)?.join(SUITE_DIR_NAME).join(SHARED_DIR_NAME))
    }

    /// One switch for hosts and clients: with `ignore_env_overrides` the root is
    /// [`Suite::per_user_default`], otherwise [`Suite::resolve`]. A release build passes `true`,
    /// a QA build `false`; wire the flag to a cargo feature or `cfg!(debug_assertions)` in the
    /// app, not to the environment.
    pub fn resolve_ignoring_env_overrides(ignore_env_overrides: bool) -> Result<Suite, SuiteError> {
        if ignore_env_overrides {
            Self::per_user_default()
        } else {
            Self::resolve()
        }
    }

    /// Whether the root was derived from the environment ([`Suite::resolve`]). A suite from
    /// [`Suite::at`], [`Suite::per_user_default`] or [`Suite::per_run_temp`] is not.
    pub fn env_derived(&self) -> bool {
        self.env_derived
    }

    fn resolve_from_env() -> Result<Suite, SuiteError> {
        // Legacy aliases used by existing apps' isolated test runs (HeardRight e2e_engine).
        for key in [ENV_SUITE_ROOT, "RIGHT_SUITE_DIR", "ORTHIC_SHARED_DIR"] {
            if let Some(v) = env_nonempty(key) {
                return Suite::at(v);
            }
        }
        let base = if cfg!(windows) {
            env_nonempty("LOCALAPPDATA")
                .map(PathBuf::from)
                .ok_or_else(|| SuiteError::NoRoot("LOCALAPPDATA is not set".into()))?
        } else if cfg!(target_os = "macos") {
            env_nonempty("HOME")
                .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
                .ok_or_else(|| SuiteError::NoRoot("HOME is not set".into()))?
        } else {
            env_nonempty("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    env_nonempty("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
                })
                .ok_or_else(|| SuiteError::NoRoot("neither XDG_DATA_HOME nor HOME is set".into()))?
        };
        Suite::at(absolute(base)?.join(SUITE_DIR_NAME).join(SHARED_DIR_NAME))
    }

    /// An explicit absolute root (isolated dev profiles, tests).
    pub fn at(root: impl Into<PathBuf>) -> Result<Suite, SuiteError> {
        Ok(Suite {
            root: absolute(root.into())?,
            _temp: None,
            env_derived: false,
        })
    }

    /// A fresh private directory under the OS temp dir, deleted when the last
    /// clone is dropped. The explicit alternative to a missing app-data root.
    pub fn per_run_temp(label: &str) -> Result<Suite, SuiteError> {
        static N: AtomicU64 = AtomicU64::new(0);
        let label: String = label
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .take(24)
            .collect();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = absolute(std::env::temp_dir())?.join(format!(
            "rk-{label}-{}-{}-{nanos:x}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        make_private_dir(&dir)?;
        Ok(Suite {
            root: dir.clone(),
            _temp: Some(Arc::new(TempGuard(dir))),
            env_derived: false,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<suite>/runtime/<service>`.
    pub fn service_dir(&self, service: &str) -> PathBuf {
        self.root.join("runtime").join(service)
    }

    /// A private (owner-only) directory under `<suite>/runtime/<service>`, e.g. `shots`.
    pub fn private_service_subdir(&self, service: &str, name: &str) -> Result<PathBuf, SuiteError> {
        let d = self.service_dir(service).join(name);
        make_private_dir(&d)?;
        Ok(d)
    }

    /// `<suite>/<area>`: shared app data that must not be relative to the cwd
    /// (screenshots, evidence, models). Created private to the user.
    pub fn data_dir(&self, area: &str) -> Result<PathBuf, SuiteError> {
        let d = self.root.join(area);
        make_private_dir(&d)?;
        Ok(d)
    }
}

/// The base directory (before `Orthic Labs/shared`) of the per-user default, from the account
/// database and never from the environment.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn platform_default_base() -> Result<PathBuf, SuiteError> {
    let home = passwd_home_dir()?;
    Ok(if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support")
    } else {
        home.join(".local").join("share")
    })
}

/// Windows: NOT IMPLEMENTED, fails closed. See [`Suite::per_user_default`] for the call the Windows
/// implementation must make (`SHGetKnownFolderPath` for `FOLDERID_LocalAppData`; the `windows`
/// crate needs `Win32_UI_Shell` and `Win32_System_Com` for it).
#[cfg(windows)]
fn platform_default_base() -> Result<PathBuf, SuiteError> {
    Err(SuiteError::NoRoot(
        "Unverified: the environment-independent per-user root is not implemented on Windows yet"
            .into(),
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn platform_default_base() -> Result<PathBuf, SuiteError> {
    Err(SuiteError::NoRoot(
        "no environment-independent per-user root is implemented for this platform".into(),
    ))
}

/// The current user's home directory from `getpwuid_r(geteuid())`, ignoring `HOME`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn passwd_home_dir() -> Result<PathBuf, SuiteError> {
    use std::ffi::{c_char, c_int, CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;

    /// `struct passwd` in the layout of each OS (checked against `<pwd.h>`; on macOS also on
    /// Darwin 27 by calling `getpwuid_r` and reading `pw_dir`). Only `pw_dir` is read.
    #[repr(C)]
    #[allow(dead_code)]
    struct Passwd {
        pw_name: *mut c_char,
        pw_passwd: *mut c_char,
        pw_uid: u32,
        pw_gid: u32,
        #[cfg(target_os = "macos")]
        pw_change: i64,
        #[cfg(target_os = "macos")]
        pw_class: *mut c_char,
        pw_gecos: *mut c_char,
        pw_dir: *mut c_char,
        pw_shell: *mut c_char,
        #[cfg(target_os = "macos")]
        pw_expire: i64,
    }

    extern "C" {
        fn getpwuid_r(
            uid: u32,
            pwd: *mut Passwd,
            buf: *mut c_char,
            buflen: usize,
            result: *mut *mut Passwd,
        ) -> c_int;
    }

    /// ERANGE is 34 on macOS and Linux.
    const ERANGE: c_int = 34;
    let uid = crate::transport::current_uid();
    let mut buf = vec![0u8; 1024];
    loop {
        let mut pwd = std::mem::MaybeUninit::<Passwd>::zeroed();
        let mut result: *mut Passwd = std::ptr::null_mut();
        // SAFETY: `pwd` and `buf` are writable for the sizes passed; on success `result` points
        // at `pwd` and its strings point into `buf`, both alive until the end of this iteration.
        let rc = unsafe {
            getpwuid_r(
                uid,
                pwd.as_mut_ptr(),
                buf.as_mut_ptr().cast::<c_char>(),
                buf.len(),
                &mut result,
            )
        };
        if rc == ERANGE && buf.len() < (1 << 20) {
            buf = vec![0u8; buf.len() * 4];
            continue;
        }
        if rc != 0 {
            return Err(SuiteError::NoRoot(format!(
                "getpwuid_r failed: {}",
                io::Error::from_raw_os_error(rc)
            )));
        }
        if result.is_null() {
            return Err(SuiteError::NoRoot(
                "the account database has no entry for the current user".into(),
            ));
        }
        // SAFETY: `result` is non-null and points at the `Passwd` that `pwd` holds.
        let dir = unsafe { (*result).pw_dir };
        if dir.is_null() {
            return Err(SuiteError::NoRoot(
                "the account database has no home directory for the current user".into(),
            ));
        }
        // SAFETY: `pw_dir` is a NUL-terminated string inside `buf`.
        let bytes = unsafe { CStr::from_ptr(dir) }.to_bytes();
        return Ok(PathBuf::from(OsStr::from_bytes(bytes)));
    }
}

/// The longest service name `host` accepts, in bytes.
pub const MAX_SERVICE_NAME_LEN: usize = 48;

/// Whether `s` is a service name `host` accepts: non-empty, at most [`MAX_SERVICE_NAME_LEN`] bytes,
/// and only ASCII lowercase letters, digits and `-`. This is the single rule `host` enforces.
pub fn valid_service_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_SERVICE_NAME_LEN
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Create `dir` (and parents) and make the leaf owner-only (0700 on Unix;
/// Windows ACLs inherit from the per-user profile).
pub(crate) fn make_private_dir(dir: &Path) -> Result<(), SuiteError> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let md = std::fs::metadata(dir)?;
        if md.uid() != crate::transport::current_uid() {
            return Err(SuiteError::Insecure(format!(
                "{} is not owned by the current user",
                dir.display()
            )));
        }
        if md.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_resolve_is_marked_environment_derived() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!Suite::at(dir.path()).unwrap().env_derived());
        assert!(!Suite::per_run_temp("env-flag").unwrap().env_derived());
        // `resolve` fails only where the environment names no root at all; then there is no
        // suite to inspect.
        if let Ok(suite) = Suite::resolve() {
            assert!(suite.env_derived());
            assert!(suite.clone().env_derived(), "the mark survives a clone");
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_per_user_default_comes_from_the_account_database_under_orthic_labs_shared() {
        let suite = Suite::per_user_default().unwrap();
        assert!(!suite.env_derived());
        assert!(suite.root().is_absolute());
        assert!(
            suite
                .root()
                .ends_with(Path::new(SUITE_DIR_NAME).join(SHARED_DIR_NAME)),
            "{:?}",
            suite.root()
        );
        let home = passwd_home_dir().unwrap();
        assert!(home.is_absolute());
        assert!(suite.root().starts_with(&home));
        // The switch picks the same suite when told to ignore the environment.
        assert_eq!(
            Suite::resolve_ignoring_env_overrides(true).unwrap().root(),
            suite.root()
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_per_user_default_fails_closed_until_windows_implements_it() {
        assert!(matches!(
            Suite::per_user_default(),
            Err(SuiteError::NoRoot(_))
        ));
    }
}
