//! Verifying the HOST from the client side.
//!
//! The server side of this crate has always checked its peer: the kernel says the client runs as
//! the same user. A client used to check only the mirror image of that, that whatever answered at
//! the recorded endpoint also runs as the same user. That says nothing about WHICH program it is,
//! so a program that managed to get the client to dial it (by redirecting the suite root, by
//! writing a `service.json`, or by binding the socket path first) received the client's `hello`
//! and every request after it, including any secret in them.
//!
//! [`HostExpectation`] closes that. The caller names the executable(s) the host may be;
//! [`Client::connect_expecting`](crate::Client::connect_expecting) asks the kernel who is on the
//! listening end of the connected socket and refuses anyone else before a single application
//! byte is written. See the README section "Verifying the host" for the threat model.

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The process the OS says is on the listening end of a connection this client opened, as it was
/// when every check passed. Returned by
/// [`Client::verified_host`](crate::Client::verified_host) and handed to a caller-supplied
/// verifier ([`HostExpectation::with_verifier`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HostProcess {
    /// The host's process id as the kernel recorded it for the socket. A pid names a process
    /// only while it runs: use it for an audit trail, not as a credential.
    pub pid: u32,
    /// Unix: the host's effective uid (always the client's own). Windows: `None`.
    pub uid: Option<u32>,
    /// Windows: the host process token's user SID. Unix: `None`.
    pub sid: Option<String>,
    /// The host's executable path as the kernel reports it for `pid` (macOS `proc_pidpath`,
    /// Linux `/proc/<pid>/exe`), canonicalised when it still exists on disk.
    pub executable: PathBuf,
    /// macOS: the host's audit token (`audit_token_t`, eight words: pid at index 5, pid version
    /// at index 7) from `LOCAL_PEERTOKEN`, when the kernel provides it. It is the race-free
    /// handle for code-signature checks (`SecCodeCopyGuestWithAttributes` with
    /// `kSecGuestAttributeAudit`), which a [`HostExpectation::with_verifier`] closure can use.
    /// `None` elsewhere.
    pub audit_token: Option<[u32; 8]>,
}

/// Why a host was refused. Always the host's identity, never a connection problem.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostIdentityReason {
    /// The expectation names no executable, so no host can match. Fails closed.
    NoExpectation,
    /// The host runs as another user than this client.
    UidMismatch { host: Option<u32>, client: u32 },
    /// The OS did not report the host's process id (a BSD, or a pid namespace that hides it).
    PidUnavailable,
    /// The kernel would not describe the host process (permission, or the call failed).
    Unreadable(String),
    /// The host's executable is not one of the expected executables.
    ExecutableMismatch,
    /// The process behind the socket exited or changed while it was being identified, so the pid
    /// could not be proven to name one process throughout.
    HostChanged(String),
    /// The executable matched but the caller's extra verification refused the host.
    Refused(String),
    /// Identifying the host is not implemented for this platform. Fails closed: it never means
    /// "assume it is the right one".
    Unverified {
        platform: &'static str,
        detail: &'static str,
    },
    /// The OS could not answer the peer-credential query.
    Query(String),
}

impl fmt::Display for HostIdentityReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostIdentityReason::NoExpectation => {
                write!(
                    f,
                    "no expected host executable was supplied, so no host can match"
                )
            }
            HostIdentityReason::UidMismatch { host, client } => match host {
                Some(host) => write!(
                    f,
                    "the host runs as uid {host}, this client as uid {client}"
                ),
                None => write!(
                    f,
                    "the host's uid is unknown, this client runs as uid {client}"
                ),
            },
            HostIdentityReason::PidUnavailable => {
                write!(f, "the OS did not report the host's process id")
            }
            HostIdentityReason::Unreadable(why) => {
                write!(f, "the OS would not describe the host process ({why})")
            }
            HostIdentityReason::ExecutableMismatch => {
                write!(
                    f,
                    "the host's executable is not one of the expected executables"
                )
            }
            HostIdentityReason::HostChanged(why) => {
                write!(
                    f,
                    "the host process changed while it was identified ({why})"
                )
            }
            HostIdentityReason::Refused(why) => {
                write!(f, "the caller's extra host verification refused it ({why})")
            }
            HostIdentityReason::Unverified { platform, detail } => {
                write!(
                    f,
                    "host verification is unavailable on {platform} ({detail})"
                )
            }
            HostIdentityReason::Query(why) => {
                write!(f, "the OS could not identify the peer ({why})")
            }
        }
    }
}

/// The process behind the endpoint is not the host the caller expected. No `hello` and no request
/// frame was written to it.
///
/// Distinct from a connection error: something answered, and the OS says it is the wrong program.
/// It is not retryable (reconnecting reaches the same program). Content-free: pids and paths
/// only, never request data.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HostIdentityError {
    /// The executables the caller said the host may be, as supplied (not canonicalised).
    pub expected: Vec<PathBuf>,
    /// The pid the kernel reported for the host, when it reported one.
    pub found_pid: Option<u32>,
    /// The host's executable, when it could be read before the refusal.
    pub found_executable: Option<PathBuf>,
    pub reason: HostIdentityReason,
}

impl fmt::Display for HostIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the service host failed identity verification: {}",
            self.reason
        )?;
        if let Some(pid) = self.found_pid {
            write!(f, "; host pid {pid}")?;
        }
        if let Some(exe) = &self.found_executable {
            write!(f, "; host executable {}", exe.display())?;
        }
        if self.expected.is_empty() {
            write!(f, "; expected: none supplied")
        } else {
            write!(f, "; expected ")?;
            for (i, path) in self.expected.iter().enumerate() {
                if i > 0 {
                    write!(f, " or ")?;
                }
                write!(f, "{}", path.display())?;
            }
            Ok(())
        }
    }
}

impl std::error::Error for HostIdentityError {}

/// Internal: why opening a host-verified connection failed.
#[derive(Debug)]
pub(crate) enum HostConnectError {
    /// The connection itself failed, or the host left before it could be identified.
    Io(io::Error),
    /// The host answered and is not the expected one.
    Identity(HostIdentityError),
}

type Verifier = Arc<dyn Fn(&HostProcess) -> Result<(), String> + Send + Sync>;

/// Which program the client is willing to talk to, passed to
/// [`Client::connect_expecting`](crate::Client::connect_expecting).
///
/// The host must run as the client's own user (always) and its executable, read from the kernel
/// for the connected socket's process, must be one of the paths named here. Both sides of the
/// comparison are canonicalised when the check runs, so a symlink, a `..` or a case difference on
/// a case-insensitive volume does not matter; the same file reached by another name (a hard link)
/// is the same executable.
///
/// An expectation with no path matches nothing: it fails closed.
///
/// The install-relative constructors ([`sibling_executable`](Self::sibling_executable),
/// [`bundle_executable`](Self::bundle_executable),
/// [`relative_to_current_exe`](Self::relative_to_current_exe)) derive the host's path from where
/// THIS process's executable lives, so an engine shipped inside an app can name its app without a
/// hard-coded path. They inherit the trust of that location: see the README, "What this does not
/// do".
#[derive(Clone, Default)]
pub struct HostExpectation {
    executables: Vec<PathBuf>,
    verifier: Option<Verifier>,
}

impl fmt::Debug for HostExpectation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostExpectation")
            .field("executables", &self.executables)
            .field("verifier", &self.verifier.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what.to_string())
}

fn current_exe_canonical() -> io::Result<PathBuf> {
    std::fs::canonicalize(std::env::current_exe()?)
}

/// A bare file name: one normal component, no separators, no `..`.
fn is_single_component(name: &OsStr) -> bool {
    let path = Path::new(name);
    path.file_name() == Some(name) && path.components().count() == 1
}

/// `<ancestor of exe, levels_up levels above it>/<relative>`. `levels_up` 1 is the directory that
/// holds `exe`. `None` for 0 levels, a path with no such ancestor, or an absolute `relative`.
fn up_and_join(exe: &Path, levels_up: usize, relative: &Path) -> Option<PathBuf> {
    if levels_up == 0 || relative.is_absolute() {
        return None;
    }
    exe.ancestors()
        .nth(levels_up)
        .map(|base| base.join(relative))
}

/// `<enclosing>.app/Contents/MacOS/<file_name>` for the first `.app` directory above `exe`.
fn bundle_main(exe: &Path, file_name: &OsStr) -> Option<PathBuf> {
    exe.ancestors()
        .skip(1)
        .find(|dir| dir.extension() == Some(OsStr::new("app")))
        .map(|bundle| bundle.join("Contents").join("MacOS").join(file_name))
}

impl HostExpectation {
    /// The host is the program at `path`.
    pub fn executable(path: impl Into<PathBuf>) -> Self {
        Self {
            executables: vec![path.into()],
            verifier: None,
        }
    }

    /// The host is any one of `paths` (a release path and a QA path, say). Empty matches nothing.
    pub fn any_of<I, P>(paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        Self {
            executables: paths.into_iter().map(Into::into).collect(),
            verifier: None,
        }
    }

    /// Also accept the program at `path`.
    pub fn or_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executables.push(path.into());
        self
    }

    /// The host is the executable `file_name` in the same directory as THIS process's executable
    /// (symlinks resolved first). Fails when the running executable cannot be located or
    /// `file_name` is not a bare file name.
    pub fn sibling_executable(file_name: impl AsRef<OsStr>) -> io::Result<Self> {
        let name = file_name.as_ref();
        if !is_single_component(name) {
            return Err(invalid("the host file name must be a bare file name"));
        }
        Self::relative_to_current_exe(1, name)
    }

    /// The host is the main executable `file_name` of the macOS app bundle that contains THIS
    /// process's executable: `<App>.app/Contents/MacOS/<file_name>`. For an engine or command
    /// line tool shipped inside the app, whether it sits in `Contents/Resources/bin` or in
    /// `Contents/MacOS` itself. Fails when the running executable is not inside an `.app`
    /// directory or `file_name` is not a bare file name.
    pub fn bundle_executable(file_name: impl AsRef<OsStr>) -> io::Result<Self> {
        let name = file_name.as_ref();
        if !is_single_component(name) {
            return Err(invalid("the host file name must be a bare file name"));
        }
        let exe = current_exe_canonical()?;
        let path = bundle_main(&exe, name).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "the running executable is not inside an .app bundle",
            )
        })?;
        Ok(Self::executable(path))
    }

    /// The host is `relative` under the directory `levels_up` levels above THIS process's
    /// executable (symlinks resolved first; 1 is the directory that holds it). For install
    /// layouts that are neither a sibling nor a macOS bundle, for example
    /// `relative_to_current_exe(2, "bin/app")` for `<root>/libexec/engine` and `<root>/bin/app`.
    pub fn relative_to_current_exe(
        levels_up: usize,
        relative: impl AsRef<Path>,
    ) -> io::Result<Self> {
        let exe = current_exe_canonical()?;
        let path = up_and_join(&exe, levels_up, relative.as_ref()).ok_or_else(|| {
            invalid("no such directory above the running executable, or the path is absolute")
        })?;
        Ok(Self::executable(path))
    }

    /// Add a check that runs AFTER the executable matched, with the host the OS identified. A
    /// returned `Err(reason)` refuses the host as [`HostIdentityReason::Refused`]. The check runs
    /// before the first application byte is written.
    ///
    /// This is the hook for a stronger identity than a path. This crate links no code-signing API,
    /// so the macOS team-identifier check is the caller's: link Security.framework in the app,
    /// build a `SecCode` for the host from [`HostProcess::audit_token`] with
    /// `SecCodeCopyGuestWithAttributes` and `kSecGuestAttributeAudit` (never from the pid alone,
    /// which can be reused), read `kSecCodeInfoTeamIdentifier` with `SecCodeCopySigningInformation`
    /// and compare it with your own (`SecCodeCopySelf`). On Windows the same hook carries an
    /// Authenticode (`WinVerifyTrust`) check of [`HostProcess::executable`].
    pub fn with_verifier(
        mut self,
        check: impl Fn(&HostProcess) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        self.verifier = Some(Arc::new(check));
        self
    }

    /// The executables this expectation names, as supplied.
    pub fn executables(&self) -> &[PathBuf] {
        &self.executables
    }
}

#[cfg(unix)]
pub(crate) use unix_impl::verify_unix_host;

#[cfg(unix)]
mod unix_impl {
    use super::{
        HostConnectError, HostExpectation, HostIdentityError, HostIdentityReason, HostProcess,
    };
    use crate::peer::{current_euid, verify_peer_is_current_user, PeerError};
    use std::io;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};

    /// `(device, inode)`: names one file whatever path it is reached by.
    pub type FileId = (u64, u64);

    pub fn file_id(path: &Path) -> Option<FileId> {
        let md = std::fs::metadata(path).ok()?;
        Some((md.dev(), md.ino()))
    }

    /// A running process's executable as the kernel reports it.
    pub struct ExeInfo {
        pub path: PathBuf,
        /// The file the process is actually running, when the kernel lets us stat it (Linux:
        /// through `/proc/<pid>/exe`, exact even if the file was replaced; macOS: by path).
        pub id: Option<FileId>,
    }

    /// What must be unchanged across the executable-path read to prove the pid named one process
    /// throughout: its start time, and (macOS) its audit token, which carries a pid version.
    #[derive(PartialEq, Eq)]
    struct Witness {
        start: (u64, u64),
        token: Option<[u32; 8]>,
    }

    enum ObserveError {
        Gone,
        Unreadable(String),
    }

    impl From<ObserveError> for HostIdentityReason {
        fn from(e: ObserveError) -> Self {
            match e {
                ObserveError::Gone => HostIdentityReason::HostChanged(
                    "it exited before it could be identified".to_string(),
                ),
                ObserveError::Unreadable(why) => HostIdentityReason::Unreadable(why),
            }
        }
    }

    /// macOS: the errno that says "no such process" (ESRCH, 3), or a stale zero, is `Ok(None)`;
    /// anything else is an error. Read right after a `proc_*` call that returned 0.
    #[cfg(target_os = "macos")]
    fn missing_or_err<T>() -> io::Result<Option<T>> {
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            Some(3) | Some(0) | None => Ok(None),
            _ => Err(e),
        }
    }

    fn witness(stream: &UnixStream, pid: u32) -> Result<Witness, ObserveError> {
        let token = os::audit_token(stream);
        let start = match os::start_marker(pid) {
            Ok(Some(start)) => start,
            Ok(None) => return Err(ObserveError::Gone),
            Err(e) => return Err(ObserveError::Unreadable(format!("process start time: {e}"))),
        };
        Ok(Witness { start, token })
    }

    /// Is the running executable (`found`, canonical, with its file identity `found_id`) the one
    /// `expected` names? Equal after canonicalisation, or the very same file (device and inode),
    /// which covers a case-insensitive volume (macOS `realpath` does not normalise case, so
    /// `/applications/x` and `/Applications/x` differ as strings) and a hard link. An expected
    /// path that does not exist matches nothing.
    pub fn same_executable(expected: &Path, found: &Path, found_id: Option<FileId>) -> bool {
        let Ok(canonical) = std::fs::canonicalize(expected) else {
            return false;
        };
        if canonical == found {
            return true;
        }
        match (found_id, file_id(&canonical)) {
            (Some(running), Some(named)) => running == named,
            _ => false,
        }
    }

    /// Ask the kernel who is on the listening end of `stream` and decide whether it is the host
    /// `expect` names. Reads and writes nothing on the stream.
    ///
    /// Order, each step failing closed:
    /// 1. the peer's uid equals ours (`getpeereid` / `SO_PEERCRED`, the existing same-user check);
    /// 2. the pid the kernel reports for the socket (`LOCAL_PEERPID` / `SO_PEERCRED`);
    /// 3. the pid's start time (and, on macOS, the audit token), then its executable path
    ///    (`proc_pidpath` / `/proc/<pid>/exe`), then the start time and token again, then the pid
    ///    from the socket again: all must agree, or the pid cannot be proven to name one process;
    /// 4. the executable is one of the expected executables;
    /// 5. the caller's extra verifier, if any.
    pub(crate) fn verify_unix_host(
        stream: &UnixStream,
        expect: &HostExpectation,
    ) -> Result<HostProcess, HostConnectError> {
        let refuse = |pid: Option<u32>, exe: Option<PathBuf>, reason: HostIdentityReason| {
            HostConnectError::Identity(HostIdentityError {
                expected: expect.executables.clone(),
                found_pid: pid,
                found_executable: exe,
                reason,
            })
        };
        if expect.executables.is_empty() {
            return Err(refuse(None, None, HostIdentityReason::NoExpectation));
        }
        if !os::SUPPORTED {
            return Err(refuse(
                None,
                None,
                HostIdentityReason::Unverified {
                    platform: std::env::consts::OS,
                    detail: "reading a process's executable path is not implemented for this Unix",
                },
            ));
        }

        let first = match verify_peer_is_current_user(stream) {
            Ok(id) => id,
            Err(PeerError::Mismatch(id)) => {
                return Err(refuse(
                    id.pid,
                    None,
                    HostIdentityReason::UidMismatch {
                        host: id.uid,
                        client: current_euid(),
                    },
                ))
            }
            Err(PeerError::Unsupported(detail)) => {
                return Err(refuse(
                    None,
                    None,
                    HostIdentityReason::Unverified {
                        platform: std::env::consts::OS,
                        detail,
                    },
                ))
            }
            Err(e) => {
                // The host hung up before it could be identified: a connection problem.
                if e.peer_gone() {
                    return Err(HostConnectError::Io(io::Error::from(e)));
                }
                return Err(refuse(None, None, HostIdentityReason::Query(e.to_string())));
            }
        };
        let Some(pid) = first.pid else {
            return Err(refuse(None, None, HostIdentityReason::PidUnavailable));
        };

        let observe = |e: ObserveError| refuse(Some(pid), None, e.into());
        let changed = |why: &str| {
            refuse(
                Some(pid),
                None,
                HostIdentityReason::HostChanged(why.to_string()),
            )
        };

        let before = witness(stream, pid).map_err(&observe)?;
        let exe = match os::executable_of(pid) {
            Ok(Some(exe)) => exe,
            Ok(None) => return Err(observe(ObserveError::Gone)),
            Err(e) => {
                return Err(observe(ObserveError::Unreadable(format!(
                    "executable path: {e}"
                ))))
            }
        };
        let after = witness(stream, pid).map_err(&observe)?;
        if before != after {
            return Err(changed(
                "its identity differed before and after its executable path was read",
            ));
        }
        match verify_peer_is_current_user(stream) {
            Ok(again) if again.pid == Some(pid) => {}
            _ => return Err(changed("the socket no longer reports the same process id")),
        }

        let ExeInfo { path, id } = exe;
        let found = std::fs::canonicalize(&path).unwrap_or(path);
        if !expect
            .executables
            .iter()
            .any(|expected| same_executable(expected, &found, id))
        {
            return Err(refuse(
                Some(pid),
                Some(found),
                HostIdentityReason::ExecutableMismatch,
            ));
        }

        let host = HostProcess {
            pid,
            uid: Some(current_euid()),
            sid: None,
            executable: found,
            audit_token: after.token,
        };
        if let Some(verifier) = &expect.verifier {
            if let Err(why) = verifier(&host) {
                return Err(refuse(
                    Some(pid),
                    Some(host.executable.clone()),
                    HostIdentityReason::Refused(why),
                ));
            }
        }
        Ok(host)
    }

    #[cfg(target_os = "macos")]
    mod os {
        use super::{file_id, missing_or_err, ExeInfo};
        use std::ffi::{c_void, OsString};
        use std::io;
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::net::UnixStream;
        use std::path::PathBuf;

        pub const SUPPORTED: bool = true;

        /// `struct proc_bsdinfo` from `<sys/proc_info.h>`. Checked on macOS (Darwin 27): 136 bytes,
        /// `pbi_pid` at offset 12, `pbi_start_tvsec` at offset 120.
        #[repr(C)]
        #[allow(dead_code)]
        struct ProcBsdInfo {
            pbi_flags: u32,
            pbi_status: u32,
            pbi_xstatus: u32,
            pbi_pid: u32,
            pbi_ppid: u32,
            pbi_uid: u32,
            pbi_gid: u32,
            pbi_ruid: u32,
            pbi_rgid: u32,
            pbi_svuid: u32,
            pbi_svgid: u32,
            rfu_1: u32,
            pbi_comm: [u8; 16],
            pbi_name: [u8; 32],
            pbi_nfiles: u32,
            pbi_pgid: u32,
            pbi_pjobc: u32,
            e_tdev: u32,
            e_tpgid: u32,
            pbi_nice: i32,
            pbi_start_tvsec: u64,
            pbi_start_tvusec: u64,
        }
        const _: () = assert!(std::mem::size_of::<ProcBsdInfo>() == 136);

        const PROC_PIDTBSDINFO: i32 = 3;
        const PROC_PIDPATHINFO_MAXSIZE: usize = 4 * 1024;
        const SOL_LOCAL: i32 = 0;
        const LOCAL_PEERTOKEN: i32 = 0x006;

        extern "C" {
            fn proc_pidinfo(
                pid: i32,
                flavor: i32,
                arg: u64,
                buffer: *mut c_void,
                buffersize: i32,
            ) -> i32;
            fn proc_pidpath(pid: i32, buffer: *mut c_void, buffersize: u32) -> i32;
            fn getsockopt(fd: i32, level: i32, name: i32, val: *mut c_void, len: *mut u32) -> i32;
        }

        fn to_pid(pid: u32) -> io::Result<i32> {
            i32::try_from(pid)
                .ok()
                .filter(|p| *p > 0)
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))
        }

        /// The process's start time (seconds, microseconds) from `proc_pidinfo`. `Ok(None)` when
        /// there is no such process.
        pub fn start_marker(pid: u32) -> io::Result<Option<(u64, u64)>> {
            let raw_pid = to_pid(pid)?;
            let mut info = std::mem::MaybeUninit::<ProcBsdInfo>::zeroed();
            let size = std::mem::size_of::<ProcBsdInfo>() as i32;
            // SAFETY: `info` is a writable buffer of exactly `size` bytes; the call fills it.
            let written = unsafe {
                proc_pidinfo(
                    raw_pid,
                    PROC_PIDTBSDINFO,
                    0,
                    info.as_mut_ptr().cast::<c_void>(),
                    size,
                )
            };
            if written != size {
                return missing_or_err();
            }
            // SAFETY: the kernel wrote the whole struct (`written == size`), and every field is
            // a plain integer or byte array, so any bit pattern is valid.
            let info = unsafe { info.assume_init() };
            Ok((info.pbi_pid == pid).then_some((info.pbi_start_tvsec, info.pbi_start_tvusec)))
        }

        /// The process's executable path from `proc_pidpath`. `Ok(None)` when there is no such
        /// process.
        pub fn executable_of(pid: u32) -> io::Result<Option<ExeInfo>> {
            let raw_pid = to_pid(pid)?;
            let mut buf = vec![0u8; PROC_PIDPATHINFO_MAXSIZE];
            // SAFETY: `buf` is a writable buffer of the length passed.
            let n = unsafe {
                proc_pidpath(raw_pid, buf.as_mut_ptr().cast::<c_void>(), buf.len() as u32)
            };
            if n <= 0 {
                return missing_or_err();
            }
            buf.truncate(n as usize);
            if let Some(nul) = buf.iter().position(|b| *b == 0) {
                buf.truncate(nul);
            }
            let path = PathBuf::from(OsString::from_vec(buf));
            let id = file_id(&path);
            Ok(Some(ExeInfo { path, id }))
        }

        /// The peer's audit token (`LOCAL_PEERTOKEN`), when the kernel provides it.
        pub fn audit_token(stream: &UnixStream) -> Option<[u32; 8]> {
            let mut token = [0u32; 8];
            let mut len = std::mem::size_of_val(&token) as u32;
            // SAFETY: valid fd borrowed from `stream`; `token` and `len` describe a writable
            // 32-byte `audit_token_t`.
            let rc = unsafe {
                getsockopt(
                    stream.as_raw_fd(),
                    SOL_LOCAL,
                    LOCAL_PEERTOKEN,
                    token.as_mut_ptr().cast::<c_void>(),
                    &mut len,
                )
            };
            (rc == 0 && len as usize == std::mem::size_of_val(&token)).then_some(token)
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    mod os {
        use super::{is_no_such_process, ExeInfo};
        use std::io;
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::net::UnixStream;

        pub const SUPPORTED: bool = true;

        /// The process's start time in clock ticks since boot (field 22 of `/proc/<pid>/stat`).
        /// `Ok(None)` when there is no such process.
        pub fn start_marker(pid: u32) -> io::Result<Option<(u64, u64)>> {
            let text = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Ok(text) => text,
                Err(e) if is_no_such_process(&e) => return Ok(None),
                Err(e) => return Err(e),
            };
            let malformed = || io::Error::new(io::ErrorKind::InvalidData, "unreadable /proc stat");
            // `pid (comm) state ...`: comm may hold spaces and parentheses, so split after the
            // last ')'. The state is the first field after it, starttime the 20th.
            let rest = text.rsplit_once(')').ok_or_else(malformed)?.1;
            let ticks = rest
                .split_whitespace()
                .nth(19)
                .and_then(|field| field.parse::<u64>().ok())
                .ok_or_else(malformed)?;
            Ok(Some((ticks, 0)))
        }

        /// The process's executable from `/proc/<pid>/exe`; its identity is the file the process
        /// is really running (the magic link is followed, so a replaced or deleted binary is
        /// still named exactly). `Ok(None)` when there is no such process.
        pub fn executable_of(pid: u32) -> io::Result<Option<ExeInfo>> {
            let link = format!("/proc/{pid}/exe");
            let path = match std::fs::read_link(&link) {
                Ok(path) => path,
                Err(e) if is_no_such_process(&e) => return Ok(None),
                Err(e) => return Err(e),
            };
            let id = std::fs::metadata(&link).ok().map(|md| (md.dev(), md.ino()));
            Ok(Some(ExeInfo { path, id }))
        }

        /// Linux has no audit token; the start time is the witness.
        pub fn audit_token(_stream: &UnixStream) -> Option<[u32; 8]> {
            None
        }
    }

    /// Any other Unix: the executable of a process cannot be read here, so no host is verified.
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
    mod os {
        use super::ExeInfo;
        use std::io;
        use std::os::unix::net::UnixStream;

        pub const SUPPORTED: bool = false;

        pub fn start_marker(_pid: u32) -> io::Result<Option<(u64, u64)>> {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
        pub fn executable_of(_pid: u32) -> io::Result<Option<ExeInfo>> {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
        pub fn audit_token(_stream: &UnixStream) -> Option<[u32; 8]> {
            None
        }
    }

    /// Linux: is this the error of a process that does not exist (`ENOENT` on the `/proc` entry,
    /// or `ESRCH` when it died mid-read)?
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn is_no_such_process(e: &io::Error) -> bool {
        e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(3)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::net::UnixListener;

        fn canonical_current_exe() -> PathBuf {
            std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap()
        }

        #[test]
        fn the_kernel_describes_this_process_and_a_missing_pid_is_gone_not_an_error() {
            let me = std::process::id();
            let exe = os::executable_of(me).unwrap().expect("this process exists");
            assert_eq!(
                std::fs::canonicalize(&exe.path).unwrap(),
                canonical_current_exe()
            );
            let start = os::start_marker(me).unwrap().expect("this process exists");
            assert!(start.0 > 0);
            assert_eq!(os::start_marker(me).unwrap(), Some(start));

            let nobody = i32::MAX as u32;
            assert!(os::start_marker(nobody).unwrap().is_none());
            assert!(os::executable_of(nobody).unwrap().is_none());
        }

        #[test]
        fn a_listener_in_this_process_is_identified_and_matched_by_its_executable() {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("h.sock");
            let _listener = UnixListener::bind(&sock).unwrap();
            let stream = UnixStream::connect(&sock).unwrap();

            let me = canonical_current_exe();
            let host = verify_unix_host(&stream, &HostExpectation::executable(&me))
                .unwrap_or_else(|e| panic!("this binary is the host: {e:?}"));
            assert_eq!(host.pid, std::process::id());
            assert_eq!(host.executable, me);
            assert_eq!(host.uid, Some(current_euid()));

            // Another existing file is not the host: refused with what was found.
            let other = dir.path().join("other");
            std::fs::write(&other, b"x").unwrap();
            let Err(HostConnectError::Identity(e)) =
                verify_unix_host(&stream, &HostExpectation::executable(&other))
            else {
                panic!("a different executable must be refused");
            };
            assert_eq!(e.reason, HostIdentityReason::ExecutableMismatch);
            assert_eq!(e.found_pid, Some(std::process::id()));
            assert_eq!(e.found_executable, Some(me.clone()));
            assert_eq!(e.expected, vec![other.clone()]);

            // One matching path among several is enough; a missing one matches nothing.
            let any = HostExpectation::any_of([other, dir.path().join("missing"), me]);
            assert!(verify_unix_host(&stream, &any).is_ok());
        }

        #[test]
        fn no_expectation_and_a_refusing_verifier_both_fail_closed() {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("h.sock");
            let _listener = UnixListener::bind(&sock).unwrap();
            let stream = UnixStream::connect(&sock).unwrap();

            let Err(HostConnectError::Identity(e)) =
                verify_unix_host(&stream, &HostExpectation::any_of(Vec::<PathBuf>::new()))
            else {
                panic!("an empty expectation must refuse");
            };
            assert_eq!(e.reason, HostIdentityReason::NoExpectation);

            let expect = HostExpectation::executable(canonical_current_exe())
                .with_verifier(|host| Err(format!("pid {} is not signed by us", host.pid)));
            let Err(HostConnectError::Identity(e)) = verify_unix_host(&stream, &expect) else {
                panic!("a refusing verifier must refuse");
            };
            assert!(
                matches!(e.reason, HostIdentityReason::Refused(ref why) if why.contains("not signed"))
            );
            assert_eq!(e.found_executable, Some(canonical_current_exe()));
        }

        #[test]
        fn a_symlink_a_hard_link_and_an_unknown_path_are_judged_by_the_file_not_the_spelling() {
            let dir = tempfile::tempdir().unwrap();
            let real = dir.path().join("real");
            std::fs::write(&real, b"binary").unwrap();
            let found = std::fs::canonicalize(&real).unwrap();
            let id = file_id(&found);
            assert!(id.is_some());

            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(same_executable(&link, &found, id));

            let hard = dir.path().join("hard");
            std::fs::hard_link(&real, &hard).unwrap();
            assert!(same_executable(&hard, &found, id), "same file, other name");

            // Same bytes, different file: not the running executable.
            let copy = dir.path().join("copy");
            std::fs::copy(&real, &copy).unwrap();
            assert!(!same_executable(&copy, &found, id));

            assert!(!same_executable(&dir.path().join("missing"), &found, id));
            // Without a file identity only the canonical path can match.
            assert!(same_executable(&link, &found, None));
            assert!(!same_executable(&hard, &found, None));
        }

        /// macOS volumes are case-insensitive by default and `realpath` keeps the spelling it was
        /// given, so the same file must still match by identity. Skipped on a case-sensitive
        /// volume, where the upper-case spelling does not name the file at all.
        #[cfg(target_os = "macos")]
        #[test]
        fn a_case_variant_spelling_of_the_expected_path_names_the_same_file() {
            let dir = tempfile::tempdir().unwrap();
            let real = dir.path().join("hostbin");
            std::fs::write(&real, b"binary").unwrap();
            let found = std::fs::canonicalize(&real).unwrap();
            let id = file_id(&found);
            let upper = dir.path().join("HOSTBIN");
            if std::fs::metadata(&upper).is_err() {
                return;
            }
            assert!(same_executable(&upper, &found, id));
        }
    }
}

/// Windows: verifying the host is NOT IMPLEMENTED. This stub fails closed with
/// [`HostIdentityReason::Unverified`] so a Windows client that asks for a verified host never
/// silently gets an unverified one. It runs after the existing same-user check
/// (`verify_pipe_server_is_current_user`) and before any byte is written to the pipe.
///
/// What the Windows implementation must do, in order, on `pipe` (the client end opened by
/// `CreateFile` with `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`):
///
/// 1. `GetNamedPipeServerProcessId(pipe)` for the server's pid (already done by
///    `verify_pipe_server_is_current_user`, which also compares the process token's user SID with
///    ours; reuse its [`PeerIdentity`](crate::PeerIdentity)).
/// 2. `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, FALSE, pid)` and KEEP the
///    handle: it pins the process object, so the pid cannot be recycled while it is held.
/// 3. `GetProcessTimes(handle, ..)` for the creation time (witness one).
/// 4. `QueryFullProcessImageNameW(handle, 0 /* PROCESS_NAME_WIN32 */, buf, &mut len)`, growing
///    `buf` past `MAX_PATH` up to 32767 UTF-16 units; this is the executable path.
/// 5. Canonicalise the found path and every expected path the same way, with `CreateFileW`
///    (`FILE_FLAG_BACKUP_SEMANTICS`, no access, full sharing) then `GetFinalPathNameByHandleW`
///    (`VOLUME_NAME_DOS`, strip the `\\?\` prefix), and compare case-insensitively (NTFS is
///    case-insensitive and case-preserving); or compare file identity with
///    `GetFileInformationByHandle` (`dwVolumeSerialNumber` + `nFileIndexHigh/Low`), which is the
///    analogue of the Unix device and inode comparison.
/// 6. Re-prove the pid: `GetProcessTimes` again must equal witness one, `WaitForSingleObject(handle,
///    0)` must return `WAIT_TIMEOUT` (still running), and `GetNamedPipeServerProcessId(pipe)` must
///    still return the same pid.
/// 7. Optionally, behind [`HostExpectation::with_verifier`]: `WinVerifyTrust`
///    (`WINTRUST_ACTION_GENERIC_VERIFY_V2`, `WINTRUST_FILE_INFO` of the found path) and compare the
///    signer (`WTHelperProvDataFromStateData`, `WTHelperGetProvSignerFromChain`) with the
///    client's own. Fill [`HostProcess::sid`] from the process token (`OpenProcessToken`,
///    `GetTokenInformation(TokenUser)`) as `verify_pipe_server_is_current_user` does.
///
/// Cargo features the `windows` dependency will need beyond today's: `Win32_Security_WinTrust`
/// (only for step 7). `QueryFullProcessImageNameW`, `GetProcessTimes` and `OpenProcess` are in
/// `Win32_System_Threading`; `CreateFileW` and `GetFinalPathNameByHandleW` in
/// `Win32_Storage_FileSystem`; both are already enabled.
///
/// The returned error carries `expected` so the typed refusal reads the same on every platform.
#[cfg(windows)]
pub(crate) fn verify_pipe_host(
    _pipe: std::os::windows::io::RawHandle,
    expect: &HostExpectation,
) -> Result<HostProcess, HostConnectError> {
    Err(HostConnectError::Identity(HostIdentityError {
        expected: expect.executables.clone(),
        found_pid: None,
        found_executable: None,
        reason: HostIdentityReason::Unverified {
            platform: "windows",
            detail: "not implemented yet; see the Windows contract in the docs of host_identity::verify_pipe_host",
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_file_name_is_one_component() {
        assert!(is_single_component(OsStr::new("mailright")));
        assert!(is_single_component(OsStr::new("mailright.exe")));
        assert!(!is_single_component(OsStr::new("")));
        assert!(!is_single_component(OsStr::new("..")));
        assert!(!is_single_component(OsStr::new("a/b")));
        assert!(!is_single_component(OsStr::new("/abs")));
    }

    #[test]
    fn install_relative_paths_are_derived_from_where_the_engine_lives() {
        let engine = Path::new("/Applications/MailRight.app/Contents/Resources/bin/engine");
        assert_eq!(
            bundle_main(engine, OsStr::new("mailright")),
            Some(PathBuf::from(
                "/Applications/MailRight.app/Contents/MacOS/mailright"
            ))
        );
        assert_eq!(
            up_and_join(engine, 1, Path::new("app")),
            Some(PathBuf::from(
                "/Applications/MailRight.app/Contents/Resources/bin/app"
            ))
        );
        assert_eq!(
            up_and_join(engine, 3, Path::new("MacOS/mailright")),
            Some(PathBuf::from(
                "/Applications/MailRight.app/Contents/MacOS/mailright"
            ))
        );
        // Not in a bundle; level 0 is the executable itself; absolute relative paths are refused.
        assert_eq!(
            bundle_main(Path::new("/opt/app/bin/engine"), OsStr::new("x")),
            None
        );
        assert_eq!(up_and_join(engine, 0, Path::new("x")), None);
        assert_eq!(up_and_join(engine, 1, Path::new("/etc/passwd")), None);
        assert_eq!(up_and_join(Path::new("engine"), 5, Path::new("x")), None);
    }

    #[test]
    fn helpers_resolve_against_the_running_test_binary() {
        let exe = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        let sibling = HostExpectation::sibling_executable("host-binary").unwrap();
        assert_eq!(
            sibling.executables(),
            [exe.parent().unwrap().join("host-binary")]
        );
        assert!(HostExpectation::sibling_executable("a/b").is_err());
        assert!(HostExpectation::sibling_executable("..").is_err());
        assert!(HostExpectation::bundle_executable("x/y").is_err());
        let up2 = HostExpectation::relative_to_current_exe(2, "bin/host").unwrap();
        assert_eq!(
            up2.executables(),
            [exe.parent().unwrap().parent().unwrap().join("bin/host")]
        );
        assert!(HostExpectation::relative_to_current_exe(0, "x").is_err());
    }

    #[test]
    fn the_error_says_what_was_expected_and_what_was_found() {
        let e = HostIdentityError {
            expected: vec![PathBuf::from("/a/app"), PathBuf::from("/b/app")],
            found_pid: Some(42),
            found_executable: Some(PathBuf::from("/tmp/evil")),
            reason: HostIdentityReason::ExecutableMismatch,
        };
        let text = e.to_string();
        assert!(
            text.contains("pid 42") && text.contains("/tmp/evil"),
            "{text}"
        );
        assert!(text.contains("/a/app or /b/app"), "{text}");
        let none = HostIdentityError {
            expected: Vec::new(),
            found_pid: None,
            found_executable: None,
            reason: HostIdentityReason::NoExpectation,
        };
        assert!(none.to_string().contains("none supplied"));
    }

    #[test]
    fn a_refusal_collapses_to_a_non_retryable_wire_error() {
        let e = HostIdentityError {
            expected: vec![PathBuf::from("/a/app")],
            found_pid: Some(7),
            found_executable: None,
            reason: HostIdentityReason::PidUnavailable,
        };
        let wire: crate::ServiceError = e.clone().into();
        assert_eq!(wire.code, crate::ERROR_HOST_IDENTITY);
        assert!(!wire.retryable);
        assert!(wire.message.contains("pid 7"));
        let c: crate::ConnectError = e.into();
        assert_eq!(c.into_service_error(), wire);
    }
}
