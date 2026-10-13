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
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

unsafe extern "C" {
    fn sandbox_init(
        profile: *const libc::c_char,
        flags: u64,
        errorbuf: *mut *mut libc::c_char,
    ) -> i32;
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

/// Canonical per-user temporary directory (`confstr(_CS_DARWIN_USER_TEMP_DIR)`).
fn user_temp_dir() -> Option<PathBuf> {
    let mut buf = [0 as libc::c_char; 1024];
    // SAFETY: buf is valid for buf.len() bytes; confstr NUL-terminates within that length.
    let len = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr(), buf.len()) };
    if len == 0 || len > buf.len() {
        return None;
    }
    // SAFETY: confstr wrote a NUL-terminated string within buf.
    let raw = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_str()
        .ok()?;
    std::fs::canonicalize(raw).ok()
}

fn regex_escape(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        if !c.is_ascii_alphanumeric() && c != '/' && c != '_' && c != '-' {
            out.push('\\');
        }
        out.push(c);
    }
    out
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
/// Network authority is limited to `proxy_addr`: validated IPv4 loopback at one fixed TCP port.
/// SBPL spells that host `localhost` (loopback, both families), while a later outbound AF_INET6 denial keeps the
/// allowance IPv4-only. The only listener allowance is a loopback TCP debugging socket. Reads
/// and execution stay within the Chrome bundle and fixed runtime locations, while writes stay
/// within the canonical fixture profile directory.
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
         (allow network-outbound (require-all (remote ip \"localhost:",
    );
    p.push_str(&proxy_port.to_string());
    // `localhost` also matches ::1, and a system-socket deny does not gate connect(2);
    // this later outbound deny is what blocks same-port IPv6 (validated with sandbox-exec).
    p.push_str(
        "\") (socket-type SOCK_STREAM)))\n(deny network-outbound (socket-domain AF_INET6))\n",
    );
    p.push_str(
        "(deny network-inbound)\n\
         (deny network-bind)\n\
         (allow network-bind (require-all (local ip \"localhost:*\") (socket-type SOCK_STREAM)))\n\
         (allow network-inbound (local ip \"localhost:*\"))\n",
    );
    // Chrome browser process on macOS, validated with sandbox-exec against Chrome 155: removing
    // any rule below made Chrome fail to report its DevTools endpoint (abort, SIGSEGV in
    // IONotificationPortGetRunLoopSource, or no ProcessSingleton). Helpers run with
    // `--no-sandbox` because a nested sandbox_init is refused here; this profile confines the tree.
    p.push_str(
        "(allow mach-lookup (global-name \"com.apple.coreservices.launchservicesd\"))\n\
         (allow mach-lookup (global-name \"com.apple.bsd.dirhelper\"))\n\
         (allow iokit-open (iokit-registry-entry-class \"RootDomainUserClient\"))\n",
    );
    // The mach port rendezvous service is named after the bundle id: `com.google.Chrome` for
    // Chrome, `com.google.chrome.for.testing` for Chrome for Testing (the CI browser). The
    // crashpad handler handshake also needs a service. Validated with sandbox-exec against
    // Chrome for Testing: without these, children abort ("No rendezvous client") or crashpad
    // fails its check-in.
    for name in [
        "com.google.Chrome.MachPortRendezvousServer",
        "com.google.chrome.for.testing.MachPortRendezvousServer",
        "org.chromium.crashpad.child_port_handshake",
    ] {
        p.push_str(&format!(
            "(allow mach-register (global-name-prefix \"{name}\"))\n\
             (allow mach-lookup (global-name-prefix \"{name}\"))\n"
        ));
    }
    // Crashpad's handler runs close_stdio, which reopens /dev/null for writing; /dev is
    // otherwise read-only here.
    p.push_str("(allow file-write* (literal \"/dev/null\"))\n");
    // ProcessSingleton binds `SingletonSocket` (and creates `SingletonCookie`) in a mkdtemp
    // directory (`<bundle id>.<6 alnum>`) under the per-user temporary directory (confstr,
    // not $TMPDIR). The bundle id is `com.google.Chrome` (Chrome), `org.chromium.Chromium`, or
    // `com.google.chrome.for.testing` (Chrome for Testing). Grant only that directory shape and
    // its two files, never the whole Chrome-named tree.
    // U: a real user Chrome's singleton dirs share this shape; a per-session prefix is not
    // possible because Chrome picks the name (see owner-fixes/sandbox-macos-fixture.md).
    if let Some(temp) = user_temp_dir() {
        let temp = regex_escape(&temp.to_string_lossy());
        // SBPL regex has no `{n}` repetition (validated with sandbox-exec): spell out six.
        let dir = format!(
            "(com\\.google\\.Chrome|org\\.chromium\\.Chromium|com\\.google\\.chrome\\.for\\.testing)\\.{}",
            "[A-Za-z0-9]".repeat(6)
        );
        p.push_str(&format!(
            "(allow file-read* file-write* (regex #\"^{temp}/{dir}(/Singleton(Socket|Cookie))?$\"))\n\
             (allow network-bind (local unix-socket (regex #\"^{temp}/{dir}/SingletonSocket$\")))\n"
        ));
    }
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
pub struct Prepared {
    profile: CString,
    diagnostic: bool,
    /// CI-only duplicate of parent stderr. CLOEXEC keeps it available to `pre_exec` while the
    /// empty fixture inheritance sweep runs, then closes it on Chrome exec.
    diagnostic_stderr: Option<OwnedFd>,
}

impl Prepared {
    pub fn new(policy: &SandboxPolicy) -> std::io::Result<Self> {
        CString::new(profile_for(policy))
            .map(|profile| Self {
                profile,
                diagnostic: false,
                diagnostic_stderr: None,
            })
            .map_err(|_| std::io::Error::other("seatbelt profile contained a NUL byte"))
    }

    /// Prepare strict browser-fixture confinement in the parent, before child `pre_exec`.
    pub fn for_browser_fixture(policy: &BrowserFixturePolicy) -> std::io::Result<Self> {
        let diagnostic = std::env::var("GITHUB_ACTIONS").is_ok_and(|value| value == "true");
        let diagnostic_stderr = if diagnostic {
            // SAFETY: fcntl duplicates the valid parent stderr descriptor. OwnedFd takes sole
            // ownership of its new descriptor and closes it after spawn returns.
            let fd = unsafe { libc::fcntl(libc::STDERR_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: fd is a fresh duplicate owned by no Rust value.
            Some(unsafe { OwnedFd::from_raw_fd(fd) })
        } else {
            None
        };
        CString::new(fixture_profile_for(policy)?)
            .map(|profile| Self {
                profile,
                diagnostic,
                diagnostic_stderr,
            })
            .map_err(|_| std::io::Error::other("seatbelt profile contained a NUL byte"))
    }

    /// Writes bounded CI diagnostics from the post-fork child without allocation or locking.
    fn write_diagnostic(&self, bytes: &[u8]) {
        if self.diagnostic {
            if let Some(stderr) = &self.diagnostic_stderr {
                // SAFETY: stderr is a parent-prepared, CLOEXEC descriptor held by Prepared.
                // The write is best-effort diagnostic output only.
                let _ = unsafe {
                    libc::write(
                        stderr.as_raw_fd(),
                        bytes.as_ptr().cast::<libc::c_void>(),
                        bytes.len(),
                    )
                };
            }
        }
    }

    /// Apply to the current process. Intended only for a forked child before exec.
    pub fn apply(&self) -> std::io::Result<()> {
        self.write_diagnostic(b"rightkit-fixture-seatbelt:sandbox_init:start\n");
        let mut err: *mut libc::c_char = std::ptr::null_mut();
        // SAFETY: valid NUL-terminated profile and out-pointer prepared in the parent.
        let status = unsafe { sandbox_init(self.profile.as_ptr(), 0, &mut err) };
        if status == 0 {
            self.write_diagnostic(b"rightkit-fixture-seatbelt:sandbox_init:success\n");
            return Ok(());
        }
        self.write_diagnostic(b"rightkit-fixture-seatbelt:sandbox_init:failure:");
        if self.diagnostic && !err.is_null() {
            let mut len = 0usize;
            // SAFETY: sandbox_init returned this NUL-terminated buffer. A 1 KiB cap prevents
            // unbounded child work before exec; it is not freed because the child now exits.
            unsafe {
                while len < 1024 && *err.add(len) != 0 {
                    len += 1;
                }
                self.write_diagnostic(std::slice::from_raw_parts(err.cast::<u8>(), len));
            }
        }
        self.write_diagnostic(b"\n");
        // Child pre_exec must not allocate or format errors. The parent receives this fixed
        // errno while CI stderr retains sandbox_init's bounded diagnostic detail.
        Err(std::io::Error::from_raw_os_error(libc::EINVAL))
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
