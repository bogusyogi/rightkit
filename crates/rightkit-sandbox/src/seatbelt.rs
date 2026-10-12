//! macOS Seatbelt backend (ported from CodeRight `macos_seatbelt.rs`).
//!
//! Lessons carried over: a `(deny default)` profile MUST start with
//! `(import "bsd.sb")` or children SIGABRT before `main`; rules must name
//! realpath-resolved paths (`/tmp` -> `/private/tmp`). Plain children get the
//! profile in-process via `sandbox_init` in `pre_exec` (no argv leak); PTY
//! children use the `sandbox-exec -p` wrapper because `portable-pty` exposes
//! no `pre_exec`.

use crate::{NetworkPolicy, SandboxPolicy};
use std::ffi::{CString, OsString};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

unsafe extern "C" {
    fn sandbox_init(
        profile: *const libc::c_char,
        flags: u64,
        errorbuf: *mut *mut libc::c_char,
    ) -> i32;
    fn sandbox_free_error(errorbuf: *mut libc::c_char);
}

/// Resolve symlinks; for paths that do not exist yet, resolve the deepest existing ancestor.
pub fn resolve_for_profile(path: &Path) -> PathBuf {
    if let Ok(r) = std::fs::canonicalize(path) {
        return r;
    }
    let mut trailing = Vec::new();
    let mut cursor = path;
    while let Some(parent) = cursor.parent() {
        if let Some(name) = cursor.file_name() {
            trailing.push(name.to_owned());
        }
        if let Ok(mut out) = std::fs::canonicalize(parent) {
            for part in trailing.iter().rev() {
                out.push(part);
            }
            return out;
        }
        cursor = parent;
    }
    path.to_path_buf()
}

fn quote(path: &Path) -> String {
    let mut out = String::from("\"");
    for c in path.to_string_lossy().chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

const SYSTEM_READ: &[&str] = &[
    "/usr",
    "/System",
    "/Library",
    "/etc",
    "/private/etc",
    "/private/var/db",
    "/dev",
    "/bin",
    "/sbin",
    "/Applications",
];
const SYSTEM_EXEC: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/System",
    "/Library",
    "/Applications",
];

/// Inputs for the narrowly confined macOS browser-fixture profile.
///
/// `profile_dir` must be a disposable directory owned by the fixture run. The resulting profile
/// grants writes only beneath its canonical path.
#[derive(Clone, Debug)]
pub struct BrowserFixturePolicy {
    pub chrome_executable: PathBuf,
    pub profile_dir: PathBuf,
    pub proxy_addr: SocketAddr,
}

const BROWSER_FIXTURE_SYSTEM_READ: &[&str] = &[
    "/System/Library",
    "/usr/lib",
    "/usr/share",
    "/private/etc",
    "/private/var/db/dyld",
    "/dev",
];

fn canonical_existing(path: &Path, kind: &str) -> std::io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("browser fixture {kind} path must be absolute"),
        ));
    }
    let resolved = std::fs::canonicalize(path)?;
    let metadata = std::fs::metadata(&resolved)?;
    let valid = match kind {
        "Chrome executable" => metadata.is_file(),
        "profile directory" => metadata.is_dir(),
        _ => false,
    };
    if !valid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("browser fixture {kind} has the wrong file type"),
        ));
    }
    Ok(resolved)
}

fn quote_fixture_path(path: &Path) -> std::io::Result<String> {
    let value = path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "browser fixture paths must be valid UTF-8",
        )
    })?;
    if value.chars().any(char::is_control) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "browser fixture paths cannot contain control characters",
        ));
    }
    Ok(quote(path))
}

fn browser_app_bundle(executable: &Path) -> std::io::Result<PathBuf> {
    executable
        .ancestors()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".app"))
        })
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Chrome executable must be inside a .app bundle",
            )
        })
}

fn validate_proxy(addr: SocketAddr) -> std::io::Result<u16> {
    match addr {
        SocketAddr::V4(addr) if *addr.ip() == Ipv4Addr::LOCALHOST && addr.port() != 0 => {
            Ok(addr.port())
        }
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "browser fixture proxy must be 127.0.0.1 with a nonzero port",
        )),
    }
}

/// Build strict deny-default SBPL for a browser fixture lane.
///
/// Network access is limited to TCP at `proxy_addr`; the only listener allowance is a loopback
/// TCP debugging socket. Reads and execution stay within the Chrome bundle and fixed runtime
/// locations, while writes stay within the canonical fixture profile directory.
pub fn fixture_profile_for(policy: &BrowserFixturePolicy) -> std::io::Result<String> {
    let executable = canonical_existing(&policy.chrome_executable, "Chrome executable")?;
    let profile = canonical_existing(&policy.profile_dir, "profile directory")?;
    let app_bundle = browser_app_bundle(&executable)?;
    if profile.starts_with(&app_bundle) || app_bundle.starts_with(&profile) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "browser fixture profile must not overlap the Chrome app bundle",
        ));
    }
    let proxy_port = validate_proxy(policy.proxy_addr)?;
    let executable = quote_fixture_path(&executable)?;
    let app_bundle = quote_fixture_path(&app_bundle)?;
    let profile = quote_fixture_path(&profile)?;

    // Do not import bsd.sb here: it carries grants outside this fixture policy, including
    // filesystem writes and IPC allowances which deny-default does not revoke.
    let mut p = String::from(
        "(version 1)\n(deny default)\n(allow process-fork)\n(allow signal (target self))\n(allow sysctl-read)\n",
    );
    p.push_str(&format!(
        "(allow process-exec* (subpath {app_bundle}))\n(allow file-read* (subpath {app_bundle}))\n(allow file-read* (subpath {profile}))\n(allow file-write* (subpath {profile}))\n"
    ));
    p.push_str(&format!("(allow process-exec (literal {executable}))\n"));
    for dir in BROWSER_FIXTURE_SYSTEM_READ {
        p.push_str(&format!("(allow file-read* (subpath {dir:?}))\n"));
    }
    p.push_str(
        "(allow file-read-metadata)\n(allow file-read* (literal \"/\"))\n\
         (deny network-outbound)\n\
         (allow network-outbound (require-all (remote ip \"127.0.0.1:",
    );
    p.push_str(&proxy_port.to_string());
    p.push_str("\") (socket-type SOCK_STREAM)))\n");
    p.push_str(
        "(deny network-inbound)\n\
         (deny network-bind)\n\
         (allow network-bind (require-all (local ip \"127.0.0.1:*\") (socket-type SOCK_STREAM)))\n\
         (allow network-inbound (require-all (local ip \"127.0.0.1:*\") (remote ip \"127.0.0.1:*\") (socket-type SOCK_STREAM)))\n",
    );
    Ok(p)
}

/// Build the deny-default SBPL profile for `policy`.
pub fn profile_for(policy: &SandboxPolicy) -> String {
    let mut p = String::from("(version 1)\n(import \"bsd.sb\")\n(deny default)\n(allow process-fork)\n(allow signal (target self))\n(allow sysctl-read)\n");
    for dir in SYSTEM_EXEC {
        p.push_str(&format!("(allow process-exec* (subpath \"{dir}\"))\n"));
    }
    for e in &policy.exec_paths {
        let r = quote(&resolve_for_profile(e));
        p.push_str(&format!(
            "(allow process-exec* (subpath {r}))\n(allow file-read* (subpath {r}))\n"
        ));
    }
    match policy.network {
        NetworkPolicy::Allow => p.push_str("(allow network*)\n"),
        NetworkPolicy::Deny => {
            p.push_str("(deny network-outbound)\n(allow network-outbound (remote unix-socket))\n")
        }
    }
    for dir in SYSTEM_READ {
        p.push_str(&format!("(allow file-read* (subpath \"{dir}\"))\n"));
    }
    // Root directory entries and metadata are needed for path walking.
    p.push_str("(allow file-read-metadata)\n(allow file-read* (literal \"/\"))\n");
    for r in &policy.read_paths {
        p.push_str(&format!(
            "(allow file-read* (subpath {}))\n",
            quote(&resolve_for_profile(r))
        ));
    }
    for w in &policy.write_paths {
        let r = quote(&resolve_for_profile(w));
        p.push_str(&format!("(allow file-read* file-write* (subpath {r}))\n"));
    }
    p.push_str("(allow file-read* file-write* file-ioctl (regex #\"^/dev/(null|zero|tty|ttys[0-9]+|dtracehelper|urandom|random)$\"))\n");
    p
}

/// Prepared in the parent; call [`Prepared::apply`] from `pre_exec`.
pub struct Prepared(CString);

impl Prepared {
    pub fn new(policy: &SandboxPolicy) -> std::io::Result<Self> {
        CString::new(profile_for(policy))
            .map(Self)
            .map_err(|_| std::io::Error::other("seatbelt profile contained a NUL byte"))
    }

    /// Prepare strict browser-fixture confinement in the parent, before child `pre_exec`.
    pub fn for_browser_fixture(policy: &BrowserFixturePolicy) -> std::io::Result<Self> {
        CString::new(fixture_profile_for(policy)?)
            .map(Self)
            .map_err(|_| std::io::Error::other("seatbelt profile contained a NUL byte"))
    }

    /// Apply to the current process. Intended only for a forked child before exec.
    pub fn apply(&self) -> std::io::Result<()> {
        let mut err: *mut libc::c_char = std::ptr::null_mut();
        // SAFETY: valid NUL-terminated profile; err is a valid out-pointer freed below.
        let status = unsafe { sandbox_init(self.0.as_ptr(), 0, &mut err) };
        if status == 0 {
            return Ok(());
        }
        let detail = if err.is_null() {
            "unknown error".to_string()
        } else {
            // SAFETY: sandbox_init populated a NUL-terminated string, freed exactly once.
            let t = unsafe { std::ffi::CStr::from_ptr(err) }
                .to_string_lossy()
                .into_owned();
            unsafe { sandbox_free_error(err) };
            t
        };
        Err(std::io::Error::other(format!("sandbox_init: {detail}")))
    }
}

/// `sandbox-exec -p <profile> program args...` wrapper (used for PTY children).
pub fn wrap_argv(
    policy: &SandboxPolicy,
    program: &OsString,
    args: &[OsString],
) -> (OsString, Vec<OsString>) {
    let mut out = vec![
        OsString::from("-p"),
        OsString::from(profile_for(policy)),
        program.clone(),
    ];
    out.extend(args.iter().cloned());
    (OsString::from("/usr/bin/sandbox-exec"), out)
}
