//! OS-level peer authentication for the local transport.
//!
//! The directory mode (Unix) and the pipe DACL (Windows) already keep other
//! users out. This module is the second, independent check: after a connection
//! is accepted the kernel is asked who is on the other end, and anything that is
//! not the current user is dropped before a single byte is read.
//!
//! Fail closed: every function here returns `Ok` only when the peer was positively
//! identified as the current user. A platform that cannot answer returns
//! [`PeerError::Unsupported`]; it never falls back to "assume it is me".

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Who the kernel says is on the other end of a connection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct PeerIdentity {
    /// Unix: the peer's effective uid.
    pub uid: Option<u32>,
    /// Peer process id when the platform reports it (Linux, macOS, Windows; not the BSDs).
    pub pid: Option<u32>,
    /// Windows: the peer process token's user SID in `S-1-...` form.
    pub sid: Option<String>,
    /// macOS: the peer's audit token (`audit_token_t`, eight words: pid at index 5, pid version
    /// at index 7) as the kernel captured it when the connection was made (`LOCAL_PEERTOKEN`).
    /// The race-free handle for a code-signature check of the peer. `None` elsewhere, or when
    /// the kernel did not provide one.
    pub audit_token: Option<[u32; 8]>,
    /// The peer process's start marker as read when the connection was accepted (Unix: start
    /// time from the OS; Windows: creation time). Pins the pid to one process, so a pid reused
    /// by another process after the peer exited is told apart at the server-side executable
    /// check. `None` when it could not be read; that check then fails closed.
    pub started: Option<(u64, u64)>,
}

/// Why a peer was not accepted as the current user.
#[derive(Debug)]
#[non_exhaustive]
pub enum PeerError {
    /// This platform cannot authenticate the peer, so no peer is accepted.
    Unsupported(&'static str),
    /// The peer is a different user.
    Mismatch(PeerIdentity),
    /// The OS could not tell who the peer is (including a peer that already left).
    Query(io::Error),
    /// The peer process was found but the OS would not let this process read its identity. It
    /// carries what is known about the process so a host can say which program was refused
    /// (rightkit#1287). On Windows this is the usual answer for a client at a higher integrity
    /// level (an elevated process) than a medium-integrity server: the pid and image path are
    /// still readable ([`PeerFailureKind::Elevated`]) and the refusal is right, only the missing
    /// identity was the problem.
    Unreadable {
        /// The peer's process id, when the kernel reported one.
        pid: Option<u32>,
        /// The peer's executable path, when it could be read (Windows `QueryFullProcessImageNameW`
        /// works with `PROCESS_QUERY_LIMITED_INFORMATION` even for an elevated client).
        image: Option<PathBuf>,
        /// A stable kind a host can switch on instead of parsing text.
        kind: PeerFailureKind,
        /// The OS error.
        source: io::Error,
    },
}

/// Why a peer could not be read, as a stable kind (see [`PeerError::Unreadable`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PeerFailureKind {
    /// The OS refused access to the peer's token: the client runs at a higher integrity level
    /// than this host (typically elevated). Tell the user to run the client without
    /// administrator rights, or the host elevated too.
    Elevated,
    /// Any other failure to read the peer.
    Other,
}

impl PeerFailureKind {
    /// A stable lowercase name for logs and events.
    pub fn as_str(self) -> &'static str {
        match self {
            PeerFailureKind::Elevated => "elevated",
            PeerFailureKind::Other => "other",
        }
    }
}

/// A peer that could not be identified, as a plain value a [`ServiceEvent`](crate::ServiceEvent)
/// or a log line can carry (the OS error is flattened to text).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PeerRefusal {
    /// The peer's process id, when known.
    pub pid: Option<u32>,
    /// The peer's executable path, when it could be read.
    pub image: Option<PathBuf>,
    /// The stable kind.
    pub kind: PeerFailureKind,
    /// The OS error text.
    pub reason: String,
}

impl fmt::Display for PeerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeerError::Unsupported(why) => write!(f, "peer authentication unsupported: {why}"),
            PeerError::Mismatch(id) => {
                write!(f, "peer is not the current user")?;
                if let Some(uid) = id.uid {
                    write!(f, " (uid {uid})")?;
                }
                if let Some(pid) = id.pid {
                    write!(f, " (pid {pid})")?;
                }
                Ok(())
            }
            PeerError::Query(e) => write!(f, "could not identify peer: {e}"),
            PeerError::Unreadable {
                pid,
                image,
                kind,
                source,
            } => {
                write!(f, "could not identify peer")?;
                if let Some(pid) = pid {
                    write!(f, " (pid {pid})")?;
                }
                if let Some(image) = image {
                    write!(f, " ({})", image.display())?;
                }
                if *kind == PeerFailureKind::Elevated {
                    write!(
                        f,
                        " (the client runs at a higher integrity level, for example elevated)"
                    )?;
                }
                write!(f, ": {source}")
            }
        }
    }
}

impl std::error::Error for PeerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PeerError::Query(e) => Some(e),
            PeerError::Unreadable { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<PeerError> for io::Error {
    fn from(e: PeerError) -> Self {
        let kind = match &e {
            PeerError::Unsupported(_) => io::ErrorKind::Unsupported,
            PeerError::Mismatch(_) => io::ErrorKind::PermissionDenied,
            PeerError::Query(q) => q.kind(),
            PeerError::Unreadable { source, .. } => source.kind(),
        };
        io::Error::new(kind, e.to_string())
    }
}

impl PeerError {
    /// The peer's pid, when the error knows it.
    pub fn pid(&self) -> Option<u32> {
        match self {
            PeerError::Mismatch(id) => id.pid,
            PeerError::Unreadable { pid, .. } => *pid,
            _ => None,
        }
    }

    /// The peer's executable path, when the error carries it.
    pub fn image(&self) -> Option<&std::path::Path> {
        match self {
            PeerError::Unreadable { image, .. } => image.as_deref(),
            _ => None,
        }
    }

    /// The stable kind of an unreadable peer; `None` for the other errors.
    pub fn failure_kind(&self) -> Option<PeerFailureKind> {
        match self {
            PeerError::Unreadable { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    /// This error as a [`PeerRefusal`] when it is an unreadable peer (the server's rejection
    /// event carries it).
    pub(crate) fn refusal(&self) -> Option<PeerRefusal> {
        match self {
            PeerError::Unreadable {
                pid,
                image,
                kind,
                source,
            } => Some(PeerRefusal {
                pid: *pid,
                image: image.clone(),
                kind: *kind,
                reason: source.to_string(),
            }),
            _ => None,
        }
    }

    /// The peer disconnected before it could be identified (a probe, or `wake`).
    /// Not an attack; the connection is still dropped, but no rejection is logged.
    pub(crate) fn peer_gone(&self) -> bool {
        let e = match self {
            PeerError::Query(e) => e,
            PeerError::Unreadable { source, .. } => source,
            _ => return false,
        };
        if matches!(
            e.kind(),
            io::ErrorKind::NotConnected
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::UnexpectedEof
        ) {
            return true;
        }
        // ERROR_BROKEN_PIPE 109, ERROR_NO_DATA 232, ERROR_PIPE_NOT_CONNECTED 233.
        cfg!(windows) && matches!(e.raw_os_error(), Some(109 | 232 | 233))
    }
}

/// `GENERIC_ALL` as an SDDL access mask.
pub const ACCESS_GENERIC_ALL: u32 = 0x1000_0000;
/// The restricted mask an elevated service grants one interactive owner (rightkit#1042): read and
/// write data and attributes, `READ_CONTROL` and `SYNCHRONIZE`, and no delete, write-DAC or
/// execute (`0x12019b`, the mask Pulse's `winadmin` service uses).
pub const ACCESS_PIPE_OWNER_RW: u32 = 0x0012_019b;

/// One principal of a pipe DACL: a SID string and an access mask (rightkit#1042).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclEntry {
    /// The SID in `S-1-...` form, or a two-letter SDDL alias such as `SY`. Only ASCII letters,
    /// digits and `-` are accepted, so a value can never add an ACE or end the descriptor.
    pub sid: String,
    /// The access mask granted (see [`ACCESS_GENERIC_ALL`], [`ACCESS_PIPE_OWNER_RW`]).
    pub mask: u32,
}

impl AclEntry {
    pub fn new(sid: impl Into<String>, mask: u32) -> Self {
        Self {
            sid: sid.into(),
            mask,
        }
    }

    /// `GENERIC_ALL` for `sid`.
    pub fn full_control(sid: impl Into<String>) -> Self {
        Self::new(sid, ACCESS_GENERIC_ALL)
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    fn valid(&self) -> bool {
        !self.sid.is_empty()
            && self.sid.len() <= 184
            && self
                .sid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }
}

/// A mandatory integrity label for a pipe (rightkit#1123): the label is the level below which a
/// process cannot write to the object. A confined (AppContainer) child needs the label lowered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IntegrityLabel {
    Untrusted,
    Low,
    Medium,
    High,
}

impl IntegrityLabel {
    #[cfg_attr(not(windows), allow(dead_code))]
    fn sid(self) -> &'static str {
        match self {
            IntegrityLabel::Untrusted => "S-1-16-0",
            IntegrityLabel::Low => "S-1-16-4096",
            IntegrityLabel::Medium => "S-1-16-8192",
            IntegrityLabel::High => "S-1-16-12288",
        }
    }
}

/// The SDDL for `entries` (a protected DACL, one ACE each) and an optional label with the
/// no-write-up policy. `None` when `entries` is empty or an entry's SID is not plain.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn sddl_for_entries(
    entries: &[AclEntry],
    label: Option<IntegrityLabel>,
) -> Option<String> {
    if entries.is_empty() || !entries.iter().all(AclEntry::valid) {
        return None;
    }
    let mut out = String::from("D:P");
    for e in entries {
        out.push_str(&format!("(A;;0x{:x};;;{})", e.mask, e.sid));
    }
    if let Some(label) = label {
        out.push_str(&format!("S:(ML;;NW;;;{})", label.sid()));
    }
    Some(out)
}

/// `D:P(A;;GA;;;<sid>)`: protected DACL (no inheritance), one ACE granting
/// GENERIC_ALL to exactly this SID.
#[cfg(windows)]
pub(crate) fn sddl_for_sid(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})")
}

// ---------------------------------------------------------------------------
// Unix
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    extern "C" {
        fn geteuid() -> u32;
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    extern "C" {
        fn getpeereid(fd: i32, euid: *mut u32, egid: *mut u32) -> i32;
    }

    // Linux/Android generic ABI: SOL_SOCKET = 1, SO_PEERCRED = 17. The mips, powerpc
    // and sparc families number these differently and are deliberately left Unsupported
    // rather than guessed.
    #[cfg(all(
        any(target_os = "linux", target_os = "android"),
        not(any(
            target_arch = "mips",
            target_arch = "mips32r6",
            target_arch = "mips64",
            target_arch = "mips64r6",
            target_arch = "powerpc",
            target_arch = "powerpc64",
            target_arch = "sparc",
            target_arch = "sparc64"
        ))
    ))]
    mod linux {
        pub const SOL_SOCKET: i32 = 1;
        pub const SO_PEERCRED: i32 = 17;
        #[repr(C)]
        #[derive(Default)]
        pub struct Ucred {
            pub pid: i32,
            pub uid: u32,
            pub gid: u32,
        }
        extern "C" {
            pub fn getsockopt(
                fd: i32,
                level: i32,
                name: i32,
                val: *mut core::ffi::c_void,
                len: *mut u32,
            ) -> i32;
        }
    }

    pub(crate) fn current_euid() -> u32 {
        unsafe { geteuid() }
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    fn peer_identity(s: &UnixStream) -> Result<PeerIdentity, PeerError> {
        let (mut uid, mut gid) = (u32::MAX, u32::MAX);
        // SAFETY: valid fd borrowed from `s`; out-pointers are valid locals.
        let rc = unsafe { getpeereid(s.as_raw_fd(), &mut uid, &mut gid) };
        if rc != 0 {
            return Err(PeerError::Query(io::Error::last_os_error()));
        }
        Ok(PeerIdentity {
            uid: Some(uid),
            pid: peer_pid(s),
            sid: None,
            audit_token: crate::host_identity::peer_audit_token(s),
            started: peer_pid(s).and_then(crate::host_identity::process_start_marker),
        })
    }

    /// The peer's process id from the `LOCAL_PEERPID` socket option. `None` when the option
    /// fails: the peer is still authenticated by its uid, the pid is only extra detail.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    fn peer_pid(s: &UnixStream) -> Option<u32> {
        const SOL_LOCAL: i32 = 0;
        const LOCAL_PEERPID: i32 = 0x002;
        extern "C" {
            fn getsockopt(
                fd: i32,
                level: i32,
                name: i32,
                val: *mut core::ffi::c_void,
                len: *mut u32,
            ) -> i32;
        }
        let mut pid: i32 = 0;
        let mut len = std::mem::size_of::<i32>() as u32;
        // SAFETY: valid fd borrowed from `s`; `pid` and `len` are valid locals of the size
        // the option writes (`pid_t`, `socklen_t`).
        let rc = unsafe {
            getsockopt(
                s.as_raw_fd(),
                SOL_LOCAL,
                LOCAL_PEERPID,
                (&mut pid as *mut i32).cast(),
                &mut len,
            )
        };
        if rc != 0 {
            return None;
        }
        u32::try_from(pid).ok().filter(|p| *p != 0)
    }

    /// The BSDs have no peer-pid socket option of this shape.
    #[cfg(any(
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    fn peer_pid(_s: &UnixStream) -> Option<u32> {
        None
    }

    #[cfg(all(
        any(target_os = "linux", target_os = "android"),
        not(any(
            target_arch = "mips",
            target_arch = "mips32r6",
            target_arch = "mips64",
            target_arch = "mips64r6",
            target_arch = "powerpc",
            target_arch = "powerpc64",
            target_arch = "sparc",
            target_arch = "sparc64"
        ))
    ))]
    fn peer_identity(s: &UnixStream) -> Result<PeerIdentity, PeerError> {
        let mut cred = linux::Ucred::default();
        let mut len = std::mem::size_of::<linux::Ucred>() as u32;
        // SAFETY: valid fd; `cred` and `len` describe a writable ucred.
        let rc = unsafe {
            linux::getsockopt(
                s.as_raw_fd(),
                linux::SOL_SOCKET,
                linux::SO_PEERCRED,
                &mut cred as *mut linux::Ucred as *mut core::ffi::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(PeerError::Query(io::Error::last_os_error()));
        }
        if (len as usize) < std::mem::size_of::<linux::Ucred>() {
            return Err(PeerError::Query(io::Error::new(
                io::ErrorKind::InvalidData,
                "short SO_PEERCRED reply",
            )));
        }
        Ok(PeerIdentity {
            uid: Some(cred.uid),
            pid: u32::try_from(cred.pid).ok().filter(|p| *p != 0),
            sid: None,
            audit_token: None,
            started: u32::try_from(cred.pid)
                .ok()
                .filter(|p| *p != 0)
                .and_then(crate::host_identity::process_start_marker),
        })
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        all(
            any(target_os = "linux", target_os = "android"),
            not(any(
                target_arch = "mips",
                target_arch = "mips32r6",
                target_arch = "mips64",
                target_arch = "mips64r6",
                target_arch = "powerpc",
                target_arch = "powerpc64",
                target_arch = "sparc",
                target_arch = "sparc64"
            ))
        )
    )))]
    fn peer_identity(_s: &UnixStream) -> Result<PeerIdentity, PeerError> {
        Err(PeerError::Unsupported(
            "no peer-credential query is implemented for this Unix platform",
        ))
    }

    /// The pure decision: the peer is accepted only when its uid is known and equals `me`.
    pub(crate) fn accept_uid(id: PeerIdentity, me: u32) -> Result<PeerIdentity, PeerError> {
        if id.uid == Some(me) {
            Ok(id)
        } else {
            Err(PeerError::Mismatch(id))
        }
    }

    /// Verify the peer of a connected Unix stream is the current user.
    ///
    /// macOS and the BSDs use `getpeereid`; Linux and Android use `SO_PEERCRED`. The
    /// peer's effective uid must equal this process's `geteuid`. Any other Unix
    /// returns [`PeerError::Unsupported`]: the answer is never `Ok` on a platform that
    /// cannot check.
    ///
    /// The credentials are those the kernel captured when the connection was made, so a
    /// peer that connects and then exits is still identified correctly.
    pub fn verify_peer_is_current_user(stream: &UnixStream) -> Result<PeerIdentity, PeerError> {
        accept_uid(peer_identity(stream)?, current_euid())
    }
}

#[cfg(unix)]
pub(crate) use unix::current_euid;
#[cfg(unix)]
pub use unix::verify_peer_is_current_user;

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use super::*;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::RawHandle;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, LocalFree, ERROR_ACCESS_DENIED, HANDLE, HLOCAL};
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
        TOKEN_QUERY, TOKEN_USER,
    };
    use windows::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// Win32 error as an `io::Error` that keeps the code, so `peer_gone` can see it.
    fn win_err(e: windows::core::Error) -> io::Error {
        io::Error::from_raw_os_error((e.code().0 as u32 & 0xFFFF) as i32)
    }

    struct Owned(HANDLE);
    impl Drop for Owned {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    fn sid_to_string(sid: PSID) -> io::Result<String> {
        unsafe {
            let mut p = PWSTR::null();
            ConvertSidToStringSidW(sid, &mut p).map_err(win_err)?;
            let s = p.to_string().map_err(io::Error::other);
            let _ = LocalFree(Some(HLOCAL(p.0 as *mut _)));
            s
        }
    }

    /// The user SID string of an access token opened with TOKEN_QUERY.
    fn token_user_sid(tok: HANDLE) -> io::Result<String> {
        unsafe {
            let mut need = 0u32;
            let _ = GetTokenInformation(tok, TokenUser, None, 0, &mut need);
            if need == 0 {
                return Err(io::Error::other("token user size query returned 0"));
            }
            // u64 storage keeps the TOKEN_USER header pointer-aligned.
            let mut buf = vec![0u64; (need as usize).div_ceil(8)];
            GetTokenInformation(
                tok,
                TokenUser,
                Some(buf.as_mut_ptr() as *mut _),
                (buf.len() * 8) as u32,
                &mut need,
            )
            .map_err(win_err)?;
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            sid_to_string(user.User.Sid)
        }
    }

    /// The SID string of the user running this process.
    pub(crate) fn current_user_sid_string() -> io::Result<String> {
        unsafe {
            let mut tok = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok).map_err(win_err)?;
            let tok = Owned(tok);
            token_user_sid(tok.0)
        }
    }

    /// The executable path of an open process, `None` when it cannot be read.
    fn process_image(proc: HANDLE) -> Option<PathBuf> {
        let mut buf = vec![0u16; 32768];
        let mut len = buf.len() as u32;
        // SAFETY: `buf` holds `len` UTF-16 units; the handle is open for the call.
        unsafe {
            QueryFullProcessImageNameW(proc, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
        }
        .ok()?;
        (len > 0).then(|| PathBuf::from(std::ffi::OsString::from_wide(&buf[..len as usize])))
    }

    /// `ERROR_ACCESS_DENIED` as the raw OS code `win_err` keeps (the low 16 bits).
    fn is_access_denied(e: &io::Error) -> bool {
        e.raw_os_error() == Some(ERROR_ACCESS_DENIED.0 as i32)
    }

    /// The SID string of the user running process `pid`. Fails closed: a process that
    /// cannot be opened or queried yields an error, never a guess. A process that opens but
    /// whose token is refused (a client at a higher integrity level) is
    /// [`PeerError::Unreadable`] with its pid and image path (rightkit#1287).
    fn process_user_sid_string(pid: u32) -> Result<String, PeerError> {
        unsafe {
            let proc = Owned(
                OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                    .map_err(|e| unreadable(pid, None, win_err(e)))?,
            );
            let mut tok = HANDLE::default();
            if let Err(e) = OpenProcessToken(proc.0, TOKEN_QUERY, &mut tok) {
                return Err(unreadable(pid, process_image(proc.0), win_err(e)));
            }
            let tok = Owned(tok);
            token_user_sid(tok.0).map_err(|e| unreadable(pid, process_image(proc.0), e))
        }
    }

    fn unreadable(pid: u32, image: Option<PathBuf>, source: io::Error) -> PeerError {
        let kind = if is_access_denied(&source) {
            PeerFailureKind::Elevated
        } else {
            PeerFailureKind::Other
        };
        PeerError::Unreadable {
            pid: Some(pid),
            image,
            kind,
            source,
        }
    }

    /// Compare a peer process's user SID with `expected` (a canonical `S-1-...` string).
    fn verify_pid_against(pid: u32, expected: &str) -> Result<PeerIdentity, PeerError> {
        if pid == 0 {
            return Err(PeerError::Query(io::Error::other("peer process id is 0")));
        }
        let theirs = process_user_sid_string(pid)?;
        let id = PeerIdentity {
            uid: None,
            pid: Some(pid),
            sid: Some(theirs.clone()),
            audit_token: None,
            started: crate::host_identity::process_start_marker(pid),
        };
        // SID strings are canonical (`S-1-5-21-...`), so equality of the strings is
        // equality of the SIDs.
        if theirs.eq_ignore_ascii_case(expected) {
            Ok(id)
        } else {
            Err(PeerError::Mismatch(id))
        }
    }

    /// Compare a peer process's user SID with ours.
    fn verify_pid(pid: u32) -> Result<PeerIdentity, PeerError> {
        let me = current_user_sid_string().map_err(PeerError::Query)?;
        verify_pid_against(pid, &me)
    }

    /// Server side: verify that the client connected to `handle` (a server pipe instance
    /// on which `ConnectNamedPipe` has completed) runs as the current user.
    ///
    /// `GetNamedPipeClientProcessId`, then the process token's user SID against ours. Call
    /// it before reading any request byte. Fails closed: a client that cannot be opened or
    /// queried is rejected. The pid is the one the kernel recorded at connect time; the
    /// pipe DACL (see [`owner_only_security_attributes`]) is the primary boundary and this
    /// is the independent second check.
    ///
    /// `handle` must be a valid, connected named-pipe server handle for the duration of
    /// the call.
    pub fn verify_pipe_peer_is_current_user(handle: RawHandle) -> Result<PeerIdentity, PeerError> {
        let mut pid = 0u32;
        // SAFETY: the caller guarantees a valid pipe handle; `pid` is a valid out-pointer.
        unsafe { GetNamedPipeClientProcessId(HANDLE(handle), &mut pid) }
            .map_err(|e| PeerError::Query(win_err(e)))?;
        verify_pid(pid)
    }

    /// Server side, for a service that does not run as the user it serves (an elevated service
    /// serving one interactive owner, rightkit#1042): like [`verify_pipe_peer_is_current_user`],
    /// but the client's process-token user must equal `sid` (an `S-1-...` string), not the
    /// current user's SID, which for a SYSTEM service would refuse the owner. Same fail-closed
    /// rules: a client that cannot be opened or queried is rejected.
    ///
    /// `handle` must be a valid, connected named-pipe server handle for the duration of the
    /// call.
    pub fn verify_pipe_peer_is_sid(
        handle: RawHandle,
        sid: &str,
    ) -> Result<PeerIdentity, PeerError> {
        if sid.is_empty() {
            return Err(PeerError::Query(io::Error::other(
                "no SID to compare the peer with",
            )));
        }
        let mut pid = 0u32;
        // SAFETY: the caller guarantees a valid pipe handle; `pid` is a valid out-pointer.
        unsafe { GetNamedPipeClientProcessId(HANDLE(handle), &mut pid) }
            .map_err(|e| PeerError::Query(win_err(e)))?;
        verify_pid_against(pid, sid)
    }

    /// Client side: verify that the server behind `handle` (a pipe opened with
    /// `CreateFile`) runs as the current user, via `GetNamedPipeServerProcessId`. Catches
    /// a different user squatting on the pipe name. Open the pipe with
    /// `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION` as well, so the server can
    /// identify but never impersonate the caller.
    pub fn verify_pipe_server_is_current_user(
        handle: RawHandle,
    ) -> Result<PeerIdentity, PeerError> {
        let mut pid = 0u32;
        // SAFETY: as above, for a client pipe handle.
        unsafe { GetNamedPipeServerProcessId(HANDLE(handle), &mut pid) }
            .map_err(|e| PeerError::Query(win_err(e)))?;
        verify_pid(pid)
    }

    /// A security descriptor granting `GENERIC_ALL` to the current user and nobody else,
    /// plus a `SECURITY_ATTRIBUTES` that points at it.
    ///
    /// SDDL: `D:P(A;;GA;;;<current user SID>)`, built from the process token's real SID
    /// (never a literal such as `OW` or `BA`). The DACL is protected (`P`), so it inherits
    /// nothing, and `bInheritHandle` is false.
    ///
    /// # Lifetime
    ///
    /// The descriptor is `LocalAlloc`ed and freed when this guard drops. The pointer from
    /// [`as_ptr`](Self::as_ptr) is valid exactly as long as the guard is alive (the
    /// attributes live on the heap, so moving the guard does not invalidate it). Windows
    /// copies the descriptor into the kernel object inside `CreateNamedPipeW`,
    /// `CreateMutexW`, `CreateEventW` and similar calls, so a single create call needs the
    /// guard only until it returns. Keep the guard alive across every create call that
    /// uses it (for example each new pipe instance).
    pub struct OwnerOnlySecurityAttributes {
        attrs: Box<SECURITY_ATTRIBUTES>,
        sd: PSECURITY_DESCRIPTOR,
        sddl: String,
    }

    // SAFETY: the descriptor and attributes are written once at construction and only
    // read afterwards.
    unsafe impl Send for OwnerOnlySecurityAttributes {}
    unsafe impl Sync for OwnerOnlySecurityAttributes {}

    impl OwnerOnlySecurityAttributes {
        /// Pointer to the `SECURITY_ATTRIBUTES`, for the `lpSecurityAttributes` argument of
        /// `CreateNamedPipeW`, `CreateMutexW`, `CreateEventW`, ... Cast it to the
        /// `*const SECURITY_ATTRIBUTES` of whichever Windows binding crate you use. Valid
        /// while `self` is alive; null is never returned.
        pub fn as_ptr(&self) -> *const core::ffi::c_void {
            &*self.attrs as *const SECURITY_ATTRIBUTES as *const core::ffi::c_void
        }

        /// The SDDL string the descriptor was built from.
        pub fn sddl(&self) -> &str {
            &self.sddl
        }

        /// The `SECURITY_ATTRIBUTES` itself, for `CreateNamedPipeW(.., Some(attrs.as_raw()))`
        /// with the `windows` crate this crate uses. Valid while `self` is alive.
        pub fn as_raw(&self) -> &SECURITY_ATTRIBUTES {
            &self.attrs
        }

        pub(crate) fn raw(&self) -> &SECURITY_ATTRIBUTES {
            &self.attrs
        }
    }

    impl Drop for OwnerOnlySecurityAttributes {
        fn drop(&mut self) {
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.sd.0)));
            }
        }
    }

    /// Build owner-only [`OwnerOnlySecurityAttributes`] for the current user. Usable for
    /// named pipes, mutexes and events.
    pub fn owner_only_security_attributes() -> io::Result<OwnerOnlySecurityAttributes> {
        let sddl = sddl_for_sid(&current_user_sid_string()?);
        attributes_from_sddl(sddl)
    }

    /// Owner-only attributes for the current user plus `extra` principals and an optional
    /// integrity `label` (rightkit#1123): the descriptor for a pipe a confined child must write,
    /// `D:P(A;;GA;;;<user>)(A;;GA;;;<container>)S:(ML;;NW;;;S-1-16-0)`. The current user always
    /// keeps full control; every extra SID is checked to be plain (letters, digits, `-`).
    pub fn owner_only_security_attributes_with(
        extra: &[AclEntry],
        label: Option<IntegrityLabel>,
    ) -> io::Result<OwnerOnlySecurityAttributes> {
        let mut entries = vec![AclEntry::full_control(current_user_sid_string()?)];
        entries.extend(extra.iter().cloned());
        pipe_security_attributes(&entries, label)
    }

    /// Attributes for exactly the principals in `entries` (rightkit#1042), for example SYSTEM
    /// with full control and one interactive owner with [`ACCESS_PIPE_OWNER_RW`]:
    /// `D:P(A;;0x10000000;;;SY)(A;;0x12019b;;;<owner>)`. The DACL is protected, so it inherits
    /// nothing; nobody not listed has access. An empty list or an entry whose SID is not plain
    /// is `InvalidInput`. Hand [`as_raw`](OwnerOnlySecurityAttributes::as_raw) to
    /// `CreateNamedPipeW`, and check each client with [`verify_pipe_peer_is_sid`].
    pub fn pipe_security_attributes(
        entries: &[AclEntry],
        label: Option<IntegrityLabel>,
    ) -> io::Result<OwnerOnlySecurityAttributes> {
        let sddl = sddl_for_entries(entries, label).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "a pipe DACL needs at least one entry and plain SID strings",
            )
        })?;
        attributes_from_sddl(sddl)
    }

    fn attributes_from_sddl(sddl: String) -> io::Result<OwnerOnlySecurityAttributes> {
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                windows::core::PCWSTR(wide.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
            .map_err(win_err)?;
        }
        let attrs = Box::new(SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        });
        Ok(OwnerOnlySecurityAttributes { attrs, sd, sddl })
    }
}

#[cfg(windows)]
pub use win::{
    owner_only_security_attributes, owner_only_security_attributes_with, pipe_security_attributes,
    verify_pipe_peer_is_current_user, verify_pipe_peer_is_sid, verify_pipe_server_is_current_user,
    OwnerOnlySecurityAttributes,
};

/// The SID string (`S-1-...`) of the user running this process, read from the process
/// token. For callers building their own owner-only DACLs or comparing a peer's
/// [`PeerIdentity::sid`] against the current user outside a pipe check.
#[cfg(windows)]
pub fn current_user_sid() -> io::Result<String> {
    win::current_user_sid_string()
}
