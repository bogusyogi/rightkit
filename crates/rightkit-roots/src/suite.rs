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

impl Suite {
    /// The real per-user suite root, always [`Suite::per_user_default`]: no environment variable
    /// (`RIGHTKIT_SUITE_ROOT`, `HOME`, `LOCALAPPDATA` or any other) redirects it, in any build.
    /// There is no test mode (Adrian, 2026-11-03: "dev mode, is mine ... real data. snapshot and
    /// restore."); QA protects the real data with the `rightkit-qa` snapshot and restore.
    pub fn resolve() -> Result<Suite, SuiteError> {
        Self::per_user_default()
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
    /// * Windows: `<profile>\AppData\Local\Orthic Labs\shared`, `<profile>` from
    ///   `GetUserProfileDirectoryW` on the process token. `%USERPROFILE%` and `%LOCALAPPDATA%` are
    ///   ignored, so a launcher that sets them (a QA home) cannot move the real root.
    /// * Other Unix: not implemented; fails closed.
    pub fn per_user_default() -> Result<Suite, SuiteError> {
        let base = platform_default_base()?;
        Suite::at(absolute(base)?.join(SUITE_DIR_NAME).join(SHARED_DIR_NAME))
    }

    /// Kept for existing callers: the flag is ignored, because no build reads an override
    /// variable any more. An alias of [`Suite::per_user_default`].
    #[deprecated(since = "0.1.6", note = "use rightkit_roots::mode::{decide, roots}")]
    pub fn resolve_ignoring_env_overrides(ignore_env_overrides: bool) -> Result<Suite, SuiteError> {
        let _ = ignore_env_overrides;
        Self::per_user_default()
    }

    /// Whether the root was derived from the environment. Always `false`: no constructor reads one.
    pub fn env_derived(&self) -> bool {
        self.env_derived
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

    /// The directory that holds per-app roots: the sibling of `shared` when the suite root is the
    /// canonical `.../Orthic Labs/shared`, otherwise `<root>/apps`, so an isolated test or
    /// override root keeps every app root inside itself.
    pub(crate) fn app_parent(&self) -> PathBuf {
        let shared = self.root.file_name().is_some_and(|n| n == SHARED_DIR_NAME);
        let parent = self.root.parent();
        match parent {
            Some(p) if shared && p.file_name().is_some_and(|n| n == SUITE_DIR_NAME) => {
                p.to_path_buf()
            }
            _ => self.root.join("apps"),
        }
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

/// Windows: `<profile>\AppData\Local`, with `<profile>` from the process token
/// ([`account_profile_dir`]), never from `%USERPROFILE%` or `%LOCALAPPDATA%`.
///
/// `SHGetKnownFolderPath(FOLDERID_LocalAppData, token = NULL)` is not used: for the current user
/// it expands the known folder's `%USERPROFILE%\AppData\Local` default against the process
/// environment, so a launcher (`rightkit-qa` sets `USERPROFILE` for its child) redirects it.
/// (`rightkit_fs::app_dirs` does not call that API for Local AppData either: it reads the
/// `User Shell Folders` value and expands it against the account's environment block, so the
/// process environment does not move it. It differs from this base only for a relocated Local
/// AppData or profile; do not unify the two without migrating the installed suite folder.)
/// `LocalAppData` is a per-machine folder that Windows does not redirect to a server, so the
/// profile-relative path is the real one.
#[cfg(windows)]
fn platform_default_base() -> Result<PathBuf, SuiteError> {
    Ok(account_profile_dir()?.join("AppData").join("Local"))
}

/// Windows: the account's profile directory, from `GetUserProfileDirectoryW` on a
/// `TOKEN_QUERY` handle of the current process token.
///
/// Microsoft documents that function as returning "the path to the root directory of the specified
/// user's profile" for the user the token represents, and `OpenProcessToken` as a way to obtain
/// that token (`hToken` "must have TOKEN_QUERY access"):
/// <https://learn.microsoft.com/en-us/windows/win32/api/userenv/nf-userenv-getuserprofiledirectoryw>,
/// <https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-openprocesstoken>.
/// The lookup is keyed by the token's SID, so no environment variable takes part. The Microsoft
/// page for `SHGetKnownFolderPath` only promises token-based resolution when a token is passed and
/// does not say it stops expanding `%USERPROFILE%` from the environment, so that API is not relied on.
#[cfg(windows)]
pub(crate) fn account_profile_dir() -> Result<PathBuf, SuiteError> {
    use std::ffi::{c_void, OsString};
    use std::os::windows::ffi::OsStringExt;

    const TOKEN_QUERY: u32 = 0x0008;
    const ERROR_INSUFFICIENT_BUFFER: i32 = 122;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
    }
    #[link(name = "userenv")]
    unsafe extern "system" {
        fn GetUserProfileDirectoryW(token: *mut c_void, buf: *mut u16, size: *mut u32) -> i32;
    }

    struct Token(*mut c_void);
    impl Drop for Token {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful `OpenProcessToken` and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    let mut raw: *mut c_void = std::ptr::null_mut();
    // SAFETY: the pseudo-handle needs no close; `raw` is a valid out-parameter.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(SuiteError::NoRoot(format!(
            "the process token is unavailable: {}",
            io::Error::last_os_error()
        )));
    }
    let token = Token(raw);

    let mut buf = vec![0u16; 260];
    for _ in 0..2 {
        let mut size = buf.len() as u32;
        // SAFETY: `buf` is writable for `size` UTF-16 units and `token` is a live query handle.
        let ok = unsafe { GetUserProfileDirectoryW(token.0, buf.as_mut_ptr(), &mut size) };
        if ok != 0 {
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let dir = PathBuf::from(OsString::from_wide(&buf[..len]));
            return if dir.is_absolute() {
                Ok(dir)
            } else {
                Err(SuiteError::NoRoot(
                    "the account's profile directory is not absolute".into(),
                ))
            };
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_INSUFFICIENT_BUFFER) && size as usize > buf.len() {
            // `size` now holds the required length in UTF-16 units, terminator included.
            buf = vec![0u16; size as usize];
            continue;
        }
        return Err(SuiteError::NoRoot(format!(
            "the account's profile directory is unavailable: {error}"
        )));
    }
    Err(SuiteError::NoRoot(
        "the account's profile directory changed size during the lookup".into(),
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn platform_default_base() -> Result<PathBuf, SuiteError> {
    Err(SuiteError::NoRoot(
        "no environment-independent per-user root is implemented for this platform".into(),
    ))
}

/// The effective uid of this process (`geteuid`). Moved from `rightkit-daemon`'s transport, where
/// it was the same call.
#[cfg(unix)]
fn current_uid() -> u32 {
    extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: `geteuid` takes no arguments, cannot fail and touches no memory of ours.
    unsafe { geteuid() }
}

/// The current user's home directory from `getpwuid_r(geteuid())`, ignoring `HOME`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn passwd_home_dir() -> Result<PathBuf, SuiteError> {
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
    let uid = current_uid();
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

/// Create `dir` (and parents), make the leaf owner-only and prove it, through the
/// `rightkit-fs` private primitives (one-way roots -> fs dependency). On Windows this is the
/// corrected existing-object repair: a pinned no-reparse handle, current-token ownership, a
/// protected owner-only DACL, then a reverify; a foreign-owned or too-permissive directory is
/// [`SuiteError::Insecure`], never silently inherited from whatever profile or explicit root
/// happened to contain it. Unix behaviour is unchanged in effect: owner-only creation, the leaf
/// narrowed, and a foreign owner refused as `Insecure`.
pub fn make_private_dir(dir: &Path) -> Result<(), SuiteError> {
    #[cfg(unix)]
    {
        // Preserve the `Insecure` classification for a foreign-owned existing leaf: the fs
        // repair would otherwise surface the chmod refusal as plain I/O before the verify
        // below could name the owner problem.
        use std::os::unix::fs::MetadataExt;
        if let Ok(md) = std::fs::symlink_metadata(dir) {
            if md.uid() != current_uid() {
                return Err(SuiteError::Insecure(format!(
                    "{} is not owned by the current user",
                    dir.display()
                )));
            }
        }
    }
    rightkit_fs::private::ensure_private_dir(dir).map_err(suite_fs_error)?;
    rightkit_fs::private::verify_owner_only(dir).map_err(suite_fs_error)
}

/// Privacy failures from the fs layer are `Insecure`; plain I/O failures stay `Io`.
fn suite_fs_error(error: rightkit_fs::FsError) -> SuiteError {
    match error {
        rightkit_fs::FsError::Io { source, .. } => SuiteError::Io(source),
        other => SuiteError::Insecure(other.to_string()),
    }
}
