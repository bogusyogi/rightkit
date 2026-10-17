//! macOS Seatbelt backend (ported from CodeRight `macos_seatbelt.rs`).
//!
//! Lessons carried over: a `(deny default)` profile MUST start with
//! `(import "bsd.sb")` or children SIGABRT before `main`; rules must name
//! realpath-resolved paths (`/tmp` -> `/private/tmp`). Plain children get the
//! profile in-process via `sandbox_init` in `pre_exec` (no argv leak); PTY
//! children use the `sandbox-exec -p` wrapper because `portable-pty` exposes
//! no `pre_exec`.

use crate::deny_read::{glob_to_deny_regex, plan_deny_read_paths};
use crate::{ConfinementMode, DenyReadEntry, NetworkPolicy, SandboxPolicy};
use std::collections::HashSet;
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

/// CFBundleIdentifier from an XML `Contents/Info.plist`; `None` for binary/missing plists or
/// identifiers with characters outside `[A-Za-z0-9.-]`.
fn bundle_identifier(app_bundle: &Path) -> Option<String> {
    let text = std::fs::read_to_string(app_bundle.join("Contents/Info.plist")).ok()?;
    let rest = text.split("<key>CFBundleIdentifier</key>").nth(1)?;
    let id = rest
        .split("<string>")
        .nth(1)?
        .split("</string>")
        .next()?
        .trim();
    (!id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'))
    .then(|| id.to_string())
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
    let app_bundle_path = app_bundle.clone();
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
    // Scope the rendezvous service to the launched bundle's CFBundleIdentifier when its
    // Info.plist is readable XML; otherwise keep the three known names (residual risk: they also
    // match a real user Chrome's services, documented in owner-fixes/sandbox-macos-fixture.md).
    // The crashpad handshake prefix is not bundle-scoped and stays (same residual risk).
    let mut names: Vec<String> = match bundle_identifier(&app_bundle_path) {
        Some(id) => vec![format!("{id}.MachPortRendezvousServer")],
        None => vec![
            "com.google.Chrome.MachPortRendezvousServer".into(),
            "com.google.chrome.for.testing.MachPortRendezvousServer".into(),
        ],
    };
    names.push("org.chromium.crashpad.child_port_handshake".into());
    for name in &names {
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

/// Fixed locations a write-only profile always lets the child write: per-user and system temp,
/// and the device nodes ordinary tools open for writing.
const WRITE_ONLY_TEMP: &[&str] = &[
    "/private/tmp",
    "/private/var/folders",
    "/private/var/tmp",
    "/tmp",
];
const WRITE_ONLY_DEV: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/dtracehelper",
    "/dev/stdout",
    "/dev/stderr",
];

/// Build the write-confinement-only SBPL profile: default-allow, so reads and execution are
/// unrestricted and ordinary toolchains run, then every write denied except beneath
/// `write_paths` (realpath-resolved), the temporary locations and a few `/dev` nodes. With
/// [`NetworkPolicy::Deny`] outbound network is denied and local unix sockets stay allowed.
///
/// Pure string construction apart from path resolution. Nothing in it hides a file from reads;
/// add [`SandboxPolicy::deny_read()`] rules (see [`profile_for`]) for secrets.
pub fn write_only_profile(write_paths: &[PathBuf], network: NetworkPolicy) -> String {
    let mut p = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
    p.push_str("(allow file-write*\n");
    for fixed in WRITE_ONLY_TEMP {
        p.push_str(&format!("    (subpath {})\n", quote(Path::new(fixed))));
    }
    for dev in WRITE_ONLY_DEV {
        p.push_str(&format!("    (literal {})\n", quote(Path::new(dev))));
    }
    p.push_str("    (regex #\"^/dev/tty\")\n");
    for path in write_paths {
        p.push_str(&format!(
            "    (subpath {})\n",
            quote(&resolve_for_profile(path))
        ));
    }
    p.push_str(")\n");
    if network == NetworkPolicy::Deny {
        p.push_str("(deny network-outbound)\n(allow network-outbound (remote unix-socket))\n");
    }
    p
}

/// A glob with its literal leading directory realpath-resolved (`/tmp/x/*` -> `/private/tmp/x/*`),
/// so the regex names what Seatbelt evaluates.
fn resolve_glob_prefix(pattern: &str) -> String {
    let Some(first_wildcard) = pattern.find(['*', '?']) else {
        return resolve_for_profile(Path::new(pattern))
            .to_string_lossy()
            .into_owned();
    };
    let Some(cut) = pattern[..first_wildcard].rfind('/') else {
        return pattern.to_string();
    };
    let prefix = if cut == 0 { "/" } else { &pattern[..cut] };
    let resolved = resolve_for_profile(Path::new(prefix));
    let resolved = resolved.to_string_lossy();
    format!("{}/{}", resolved.trim_end_matches('/'), &pattern[cut + 1..])
}

/// `deny_read` rules: for every path both its configured spelling and its resolved one (and the
/// canonical target when it exists), for every glob a regex over the resolved prefix. Emitted
/// after all allows, so they win (the last matching rule decides in SBPL). Both read and write
/// are denied so a protected location cannot be overwritten either.
fn deny_read_rules(entries: &[DenyReadEntry]) -> std::io::Result<String> {
    let mut out = String::new();
    let mut seen_paths: HashSet<PathBuf> = HashSet::new();
    let mut seen_regexes: HashSet<String> = HashSet::new();
    let paths: Vec<PathBuf> = entries
        .iter()
        .filter_map(|entry| match entry {
            DenyReadEntry::Path(path) => Some(path.clone()),
            DenyReadEntry::Glob(_) => None,
        })
        .collect();
    for planned in plan_deny_read_paths(&paths)? {
        for candidate in [planned.clone(), resolve_for_profile(&planned)] {
            if seen_paths.insert(candidate.clone()) {
                out.push_str(&format!(
                    "(deny file-read* file-write* (subpath {}))\n",
                    quote(&candidate)
                ));
            }
        }
    }
    for entry in entries {
        if let DenyReadEntry::Glob(pattern) = entry {
            let resolved = resolve_glob_prefix(pattern);
            for spelling in [pattern.as_str(), resolved.as_str()] {
                let regex = glob_to_deny_regex(spelling);
                if seen_regexes.insert(regex.clone()) {
                    out.push_str(&format!(
                        "(deny file-read* file-write* (regex #\"{regex}\"))\n"
                    ));
                }
            }
        }
    }
    Ok(out)
}

/// Build the SBPL profile for `policy`, or say why it cannot be built (a `deny_read` entry that
/// resolves to a filesystem root, say). Deny-default ([`ConfinementMode::Strict`]) imports
/// `bsd.sb` first; [`ConfinementMode::WriteOnly`] is default-allow with writes confined.
pub fn try_profile_for(policy: &SandboxPolicy) -> std::io::Result<String> {
    let deny = deny_read_rules(&policy.deny_read)?;
    if policy.mode == ConfinementMode::WriteOnly {
        let mut p = write_only_profile(&policy.write_paths, policy.network);
        p.push_str(&deny);
        return Ok(p);
    }
    let mut p = String::from("(version 1)\n(import \"bsd.sb\")\n(deny default)\n(allow process-fork)\n(allow signal (target self))\n(allow sysctl-read)\n");
    for dir in SYSTEM_EXEC {
        p.push_str(&format!("(allow process-exec* (subpath \"{dir}\"))\n"));
    }
    if policy.exec_any {
        // Execute any program. The image still has to be readable: add read or exec paths.
        p.push_str("(allow process*)\n");
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
    p.push_str(&deny);
    Ok(p)
}

/// Build the SBPL profile for `policy`. A `deny_read` entry that cannot be planned makes the
/// result a profile that denies everything (the child cannot start), never a profile without the
/// deny; use [`try_profile_for`] to get the error instead.
pub fn profile_for(policy: &SandboxPolicy) -> String {
    try_profile_for(policy).unwrap_or_else(|_| String::from("(version 1)\n(deny default)\n"))
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
        Self::from_profile(try_profile_for(policy)?)
    }

    /// Prepare an SBPL profile the caller built (for example [`write_only_profile`]). The
    /// profile is applied as given: nothing is imported or added, so a deny-default profile must
    /// carry its own `(import "bsd.sb")`.
    pub fn from_profile(profile: String) -> std::io::Result<Self> {
        CString::new(profile)
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

/// `sandbox-exec -p <profile> program args...` wrapper (used for PTY children). A `deny_read`
/// entry that cannot be planned yields a deny-everything profile; see [`try_wrap_argv`].
pub fn wrap_argv(
    policy: &SandboxPolicy,
    program: &OsString,
    args: &[OsString],
) -> (OsString, Vec<OsString>) {
    wrap_profile(profile_for(policy), program, args)
}

/// As [`wrap_argv`], returning the error when the profile cannot be built.
pub fn try_wrap_argv(
    policy: &SandboxPolicy,
    program: &OsString,
    args: &[OsString],
) -> std::io::Result<(OsString, Vec<OsString>)> {
    Ok(wrap_profile(try_profile_for(policy)?, program, args))
}

fn wrap_profile(
    profile: String,
    program: &OsString,
    args: &[OsString],
) -> (OsString, Vec<OsString>) {
    let mut out = vec![
        OsString::from("-p"),
        OsString::from(profile),
        program.clone(),
    ];
    out.extend(args.iter().cloned());
    (OsString::from("/usr/bin/sandbox-exec"), out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rk-seatbelt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir).unwrap()
    }

    fn last_allow(profile: &str) -> usize {
        profile.rfind("(allow ").expect("an allow rule")
    }

    #[test]
    fn write_only_profile_is_default_allow_with_writes_confined() {
        let workspace = scratch("wo");
        let profile = write_only_profile(std::slice::from_ref(&workspace), NetworkPolicy::Allow);
        assert!(
            profile.starts_with("(version 1)\n(allow default)\n(deny file-write*)\n"),
            "{profile}"
        );
        assert!(!profile.contains("(deny default)"), "{profile}");
        assert!(!profile.contains("bsd.sb"), "{profile}");
        assert!(
            !profile.contains("(deny file-read"),
            "reads stay open:\n{profile}"
        );
        for fixed in [
            "/private/tmp",
            "/private/var/folders",
            "/private/var/tmp",
            "/tmp",
        ] {
            assert!(
                profile.contains(&format!("(subpath \"{fixed}\")")),
                "{fixed}\n{profile}"
            );
        }
        for dev in [
            "/dev/null",
            "/dev/zero",
            "/dev/dtracehelper",
            "/dev/stdout",
            "/dev/stderr",
        ] {
            assert!(
                profile.contains(&format!("(literal \"{dev}\")")),
                "{dev}\n{profile}"
            );
        }
        assert!(profile.contains("(regex #\"^/dev/tty\")"), "{profile}");
        assert!(
            profile.contains(&format!("(subpath {})", quote(&workspace))),
            "workspace is granted resolved:\n{profile}"
        );
        assert!(!profile.contains("network-outbound"), "{profile}");
    }

    #[test]
    fn write_only_profile_resolves_symlinked_write_paths() {
        // `/tmp` is a symlink to `/private/tmp`; Seatbelt matches the resolved path.
        let profile = write_only_profile(
            &[PathBuf::from("/tmp/rk-write-only-not-yet-created")],
            NetworkPolicy::Allow,
        );
        assert!(
            profile.contains("(subpath \"/private/tmp/rk-write-only-not-yet-created\")"),
            "{profile}"
        );
        assert!(
            !profile.contains("(subpath \"/tmp/rk-write-only-not-yet-created\")"),
            "{profile}"
        );
    }

    #[test]
    fn write_only_network_deny_keeps_unix_sockets() {
        let profile = write_only_profile(&[], NetworkPolicy::Deny);
        assert!(profile.contains("(deny network-outbound)\n"), "{profile}");
        assert!(
            profile.contains("(allow network-outbound (remote unix-socket))\n"),
            "{profile}"
        );
        assert!(!profile.contains("(allow network*)"), "{profile}");
    }

    #[test]
    fn a_hostile_write_path_cannot_break_out_of_its_rule() {
        let evil = PathBuf::from("/tmp/a\") (allow file-read* (subpath \"/");
        let profile = write_only_profile(std::slice::from_ref(&evil), NetworkPolicy::Allow);
        let quoted = quote(&resolve_for_profile(&evil));
        assert!(
            profile.contains(&format!("(subpath {quoted})")),
            "{profile}"
        );
        // Every quote inside the hostile text is escaped, so it stays inside its string.
        let inner = &quoted[1..quoted.len() - 1];
        let mut previous = ' ';
        for c in inner.chars() {
            if c == '"' {
                assert_eq!(previous, '\\', "unescaped quote in {quoted}");
            }
            previous = c;
        }
        assert!(inner.contains("\\\""), "{quoted}");
    }

    #[test]
    fn the_mode_selects_the_profile_and_deny_default_keeps_its_import_first() {
        let strict = profile_for(&SandboxPolicy::new());
        let import = strict.find("(import \"bsd.sb\")").expect("base import");
        let deny = strict.find("(deny default)").expect("deny default");
        assert!(import < deny, "{strict}");
        let write_only = profile_for(&SandboxPolicy::new().mode(ConfinementMode::WriteOnly));
        assert!(write_only.contains("(allow default)"), "{write_only}");
        assert!(!write_only.contains("(deny default)"), "{write_only}");
    }

    #[test]
    fn exec_any_adds_process_star_only_when_asked() {
        let plain = profile_for(&SandboxPolicy::new());
        assert!(!plain.contains("(allow process*)"), "{plain}");
        let any = profile_for(&SandboxPolicy::new().exec_any(true));
        assert!(any.contains("(allow process*)\n"), "{any}");
        // It grants execution, not reading: no read rule appears for it.
        assert_eq!(
            plain.matches("(allow file-read*").count(),
            any.matches("(allow file-read*").count()
        );
    }

    #[test]
    fn deny_read_rules_come_after_every_allow_so_they_win() {
        let dir = scratch("deny");
        let secret = dir.join(".ssh");
        std::fs::create_dir_all(&secret).unwrap();
        for policy in [
            SandboxPolicy::new().allow_write(&dir).deny_read(&secret),
            SandboxPolicy::new()
                .mode(ConfinementMode::WriteOnly)
                .allow_write(&dir)
                .deny_read(&secret),
        ] {
            let profile = profile_for(&policy);
            let deny = format!("(deny file-read* file-write* (subpath {}))", quote(&secret));
            let at = profile
                .find(&deny)
                .unwrap_or_else(|| panic!("{deny}\n{profile}"));
            assert!(
                at > last_allow(&profile),
                "deny must follow the last allow:\n{profile}"
            );
        }
    }

    #[test]
    fn deny_read_covers_both_spellings_of_a_symlinked_secret() {
        let dir = scratch("deny-link");
        let real = dir.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let profile = profile_for(&SandboxPolicy::new().deny_read(&link));
        assert!(
            profile.contains(&format!("(subpath {})", quote(&link))),
            "{profile}"
        );
        assert!(
            profile.contains(&format!("(subpath {})", quote(&real))),
            "{profile}"
        );
    }

    #[test]
    fn deny_read_globs_become_anchored_regexes_that_cover_children() {
        let dir = scratch("deny-glob");
        let pattern = format!("{}/keys/*.pem", dir.display());
        let profile = profile_for(&SandboxPolicy::new().deny_read_glob(&pattern));
        let rule = profile
            .lines()
            .find(|line| line.starts_with("(deny file-read* file-write* (regex"))
            .unwrap_or_else(|| panic!("{profile}"));
        assert!(rule.contains("#\"^"), "{rule}");
        assert!(rule.contains("[^/]*"), "{rule}");
        assert!(rule.ends_with("(/.*)?$\"))"), "{rule}");
    }

    #[test]
    fn a_glob_prefix_is_resolved_like_a_path() {
        assert_eq!(
            resolve_glob_prefix("/tmp/rk-glob-not-yet/*.pem"),
            "/private/tmp/rk-glob-not-yet/*.pem"
        );
        assert_eq!(resolve_glob_prefix("/*"), "/*");
        assert_eq!(
            resolve_glob_prefix("/tmp/rk-glob-not-yet/**/key"),
            "/private/tmp/rk-glob-not-yet/**/key"
        );
    }

    #[test]
    fn an_unplannable_deny_read_never_yields_a_profile_without_the_deny() {
        let relative = SandboxPolicy::new()
            .allow_write("/tmp")
            .deny_read("relative");
        assert!(try_profile_for(&relative).is_err());
        let poisoned = profile_for(&relative);
        assert!(
            !poisoned.contains("(allow "),
            "no allow may survive:\n{poisoned}"
        );
        assert!(Prepared::new(&relative).is_err());
        // A symlink that resolves to the filesystem root is refused too.
        let dir = scratch("deny-root-alias");
        let alias = dir.join("everything");
        std::os::unix::fs::symlink("/", &alias).unwrap();
        assert!(try_profile_for(&SandboxPolicy::new().deny_read(&alias)).is_err());
    }

    #[test]
    fn from_profile_applies_nothing_extra_and_rejects_nul() {
        assert!(Prepared::from_profile("(version 1)\n(allow default)\n".into()).is_ok());
        assert!(Prepared::from_profile("(version 1)\0".into()).is_err());
    }

    #[test]
    fn try_wrap_argv_matches_wrap_argv_for_a_valid_policy() {
        let policy = SandboxPolicy::new().mode(ConfinementMode::WriteOnly);
        let program = OsString::from("/bin/echo");
        let args = [OsString::from("hi")];
        let plain = wrap_argv(&policy, &program, &args);
        let tried = try_wrap_argv(&policy, &program, &args).unwrap();
        assert_eq!(plain, tried);
        assert_eq!(plain.0, OsString::from("/usr/bin/sandbox-exec"));
        assert_eq!(plain.1[0], OsString::from("-p"));
        let relative = SandboxPolicy::new().deny_read("relative");
        assert!(try_wrap_argv(&relative, &program, &args).is_err());
    }
}
