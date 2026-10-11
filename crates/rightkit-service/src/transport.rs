//! Current-user-only local transport: Unix socket (macOS, other Unix) and named
//! pipe (Windows). One request is in flight per connection (lock-step), so a
//! connection never needs concurrent reads and writes on one handle.

use crate::suite::Suite;
use std::io::{self, Read, Write};
use std::sync::Arc;

/// Outcome of accepting one connection.
pub(crate) enum Accepted {
    /// The peer was authenticated as the current user.
    Conn(Conn),
    /// The peer failed OS-level authentication and was closed unread. The reason is the
    /// typed peer error, content-free.
    Rejected(String),
    /// The peer left before it could be identified (a probe, or the shutdown wake-up).
    /// Closed; not worth a rejection event.
    Vanished,
}

/// Default for [`HostOptions::max_pipe_instances`](crate::HostOptions::max_pipe_instances).
pub const DEFAULT_MAX_PIPE_INSTANCES: u32 = 16;

/// An accepted or connected duplex stream.
pub(crate) struct Conn {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    /// Non-blocking: has the peer closed its end? Safe to call from another thread.
    pub peer_closed: Arc<dyn Fn() -> bool + Send + Sync>,
    pub shutdown: Arc<dyn Fn() + Send + Sync>,
    /// Who the OS says is on the other end. Filled in for an accepted connection, where the
    /// peer check has just run; empty for a connection this process opened.
    pub peer: crate::peer::PeerIdentity,
}

fn fnv(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s
        .to_lowercase()
        .replace('\\', "/")
        .trim_end_matches('/')
        .bytes()
    {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Longest Unix socket path this crate will bind: `sockaddr_un.sun_path` is 104 bytes on macOS
/// and 108 on Linux, each including the trailing NUL, so 103 is the portable ceiling.
pub const MAX_UNIX_SOCKET_PATH: usize = 103;

/// In-suite socket paths longer than this move to the per-user temp dir. Kept below
/// [`MAX_UNIX_SOCKET_PATH`] on purpose: it is the historical threshold and changing it would move
/// the endpoint of services that bind today.
const IN_SUITE_PATH_LIMIT: usize = 100;

/// The endpoint for a service cannot be named within the OS socket path limit, even at the
/// short fallback location. Names the offending path, its length and the limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointError {
    pub path: String,
    pub len: usize,
    pub limit: usize,
}

impl std::fmt::Display for EndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "service socket path {:?} is {} bytes, over the {}-byte limit; set a shorter TMPDIR",
            self.path, self.len, self.limit
        )
    }
}
impl std::error::Error for EndpointError {}

/// 64-bit FNV-1a. Stable across Rust versions and processes (std's hashers are not), so a
/// client and a server derive the same fallback name.
fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// File name of the bounded fallback socket: `rk-<tag>-<fnv1a64(service)>.sock`, 33 bytes
/// whatever the service name length (the tag is 8 hex digits, the hash 16).
fn fallback_file_name(tag: &str, service: &str) -> String {
    format!("rk-{tag}-{:016x}.sock", fnv1a64(service))
}

/// The fallback name used before the bounded form existed: `rk-<tag>-<service>.sock`.
fn legacy_fallback_file_name(tag: &str, service: &str) -> String {
    format!("rk-{tag}-{service}.sock")
}

fn suite_tag(suite: &Suite) -> String {
    format!("{:08x}", fnv(&suite.root().to_string_lossy()))
}

/// Unix endpoint with the temp dir injected (the seam tests use). Returns the in-suite path when
/// it fits, else the bounded fallback under `temp`, else a typed error.
fn unix_endpoint_in(
    suite: &Suite,
    service: &str,
    temp: &std::path::Path,
) -> Result<String, EndpointError> {
    let p = suite.service_dir(service).join(format!("{service}.sock"));
    if p.as_os_str().len() <= IN_SUITE_PATH_LIMIT {
        return Ok(p.to_string_lossy().into_owned());
    }
    let f = temp
        .join(fallback_file_name(&suite_tag(suite), service))
        .to_string_lossy()
        .into_owned();
    if f.len() <= MAX_UNIX_SOCKET_PATH {
        Ok(f)
    } else {
        Err(EndpointError {
            len: f.len(),
            path: f,
            limit: MAX_UNIX_SOCKET_PATH,
        })
    }
}

/// The pre-upgrade fallback path, only when it would have fitted the socket limit (so an old
/// host could actually have bound it). `None` means there is nothing to stay compatible with.
fn legacy_unix_endpoint_in(suite: &Suite, service: &str, temp: &std::path::Path) -> Option<String> {
    let p = suite.service_dir(service).join(format!("{service}.sock"));
    if p.as_os_str().len() <= IN_SUITE_PATH_LIMIT {
        return None;
    }
    let f = temp
        .join(legacy_fallback_file_name(&suite_tag(suite), service))
        .to_string_lossy()
        .into_owned();
    (f.len() <= MAX_UNIX_SOCKET_PATH).then_some(f)
}

/// Endpoint for `service` on `suite`, or the typed reason none can be named. Windows: a pipe
/// name that carries the user and a hash of the suite root, so an isolated suite never collides
/// with the user's installed host (at most 96 characters for a 48-byte service name and a
/// 20-character user, far under the 256-character pipe name limit). Unix: a socket in the
/// private service dir, or, when that path would exceed the `sockaddr_un` limit, a bounded
/// `rk-<tag>-<hash>.sock` in the per-user temp dir.
///
/// This is the single place an endpoint is computed; the host binds it and publishes it in its
/// discovery record, and clients connect to the recorded endpoint.
pub fn try_endpoint_for(suite: &Suite, service: &str) -> Result<String, EndpointError> {
    if cfg!(windows) {
        let tag = suite_tag(suite);
        let user: String = std::env::var("USERNAME")
            .unwrap_or_default()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(20)
            .collect();
        Ok(format!(r"\\.\pipe\rightkit-{service}-{user}-{tag}"))
    } else {
        unix_endpoint_in(suite, service, &std::env::temp_dir())
    }
}

/// [`try_endpoint_for`] for callers that cannot handle the error: on failure it returns the path
/// that did not fit, so a bind attempt fails with the OS error rather than panicking here.
pub fn endpoint_for(suite: &Suite, service: &str) -> String {
    try_endpoint_for(suite, service).unwrap_or_else(|e| e.path)
}

/// Endpoints a client may find a live host on, in order: the current endpoint, then (Unix only)
/// the pre-upgrade long fallback name when it would have fitted. Compatibility decision: for one
/// release a new client still finds an old host on the old fallback name; that name is
/// connect-only and is never bound. Where the old name never fitted the limit no old host could
/// have bound it, so there is no second candidate.
pub fn endpoint_candidates(suite: &Suite, service: &str) -> Vec<String> {
    candidates_in(suite, service, &std::env::temp_dir())
}

fn candidates_in(suite: &Suite, service: &str, temp: &std::path::Path) -> Vec<String> {
    let mut v = Vec::new();
    if cfg!(windows) {
        if let Ok(e) = try_endpoint_for(suite, service) {
            v.push(e);
        }
        return v;
    }
    if let Ok(e) = unix_endpoint_in(suite, service, temp) {
        v.push(e);
    }
    if let Some(old) = legacy_unix_endpoint_in(suite, service, temp) {
        if !v.contains(&old) {
            v.push(old);
        }
    }
    v
}

/// The first of [`endpoint_candidates`] that answers a connect.
pub fn find_live_endpoint(suite: &Suite, service: &str) -> Option<String> {
    endpoint_candidates(suite, service)
        .into_iter()
        .find(|e| probe_endpoint(e))
}

/// Does something answer at `endpoint`? (Connect, then drop.)
pub fn probe_endpoint(endpoint: &str) -> bool {
    connect(endpoint).is_ok()
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    use crate::peer::{verify_peer_is_current_user, PeerError, PeerIdentity};

    extern "C" {
        fn recv(fd: i32, buf: *mut u8, len: usize, flags: i32) -> isize;
    }
    // MSG_PEEK | MSG_DONTWAIT: 0x2 | 0x80 on macOS, 0x2 | 0x40 on Linux.
    #[cfg(target_os = "macos")]
    const PEEK_NOWAIT: i32 = 0x2 | 0x80;
    #[cfg(not(target_os = "macos"))]
    const PEEK_NOWAIT: i32 = 0x2 | 0x40;

    pub fn current_uid() -> u32 {
        crate::peer::current_euid()
    }

    /// Authenticate `s` with `verify` before it is wrapped: nothing is read from a
    /// stream that fails. The stream is dropped (closed) on every non-`Conn` outcome.
    fn admit(
        s: UnixStream,
        verify: impl FnOnce(&UnixStream) -> Result<PeerIdentity, PeerError>,
    ) -> io::Result<Accepted> {
        match verify(&s) {
            Ok(peer) => wrap(s).map(|conn| Accepted::Conn(Conn { peer, ..conn })),
            Err(e) if e.peer_gone() => Ok(Accepted::Vanished),
            Err(e) => Ok(Accepted::Rejected(e.to_string())),
        }
    }

    fn wrap(s: UnixStream) -> io::Result<Conn> {
        let r = s.try_clone()?;
        let w = s.try_clone()?;
        let watch = s.try_clone()?;
        let sd = s;
        Ok(Conn {
            peer: crate::peer::PeerIdentity::default(),
            reader: Box::new(r),
            writer: Box::new(w),
            peer_closed: Arc::new(move || {
                let mut b = [0u8; 1];
                let n = unsafe { recv(watch.as_raw_fd(), b.as_mut_ptr(), 1, PEEK_NOWAIT) };
                n == 0
                    || (n < 0
                        && !matches!(
                            io::Error::last_os_error().kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ))
            }),
            shutdown: Arc::new(move || {
                let _ = sd.shutdown(std::net::Shutdown::Both);
            }),
        })
    }

    pub struct Listener {
        l: UnixListener,
        path: PathBuf,
    }

    impl Listener {
        /// `max_instances` is a Windows pipe-instance cap; a Unix socket has no such notion.
        pub fn bind(endpoint: &str, _max_instances: u32) -> io::Result<Listener> {
            let path = PathBuf::from(endpoint);
            if let Ok(md) = std::fs::symlink_metadata(&path) {
                // Only replace a dead socket that we own; never follow or unlink anything else.
                if md.uid() != current_uid() || !md.file_type().is_socket_like() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "endpoint path is occupied by something that is not our socket",
                    ));
                }
                if UnixStream::connect(&path).is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "a live host already serves this endpoint",
                    ));
                }
                let _ = std::fs::remove_file(&path);
            }
            let l = UnixListener::bind(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            Ok(Listener { l, path })
        }
        /// A peer that is not the current user is `Rejected`; the caller keeps accepting.
        pub fn accept(&self) -> io::Result<Accepted> {
            let (s, _) = self.l.accept()?;
            admit(s, verify_peer_is_current_user)
        }
        #[cfg(test)]
        pub fn accept_with(
            &self,
            verify: impl FnOnce(&UnixStream) -> Result<PeerIdentity, PeerError>,
        ) -> io::Result<Accepted> {
            let (s, _) = self.l.accept()?;
            admit(s, verify)
        }
        pub fn wake(&self) {
            let _ = UnixStream::connect(&self.path);
        }
        pub fn cleanup(&self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    trait SocketLike {
        fn is_socket_like(&self) -> bool;
    }
    impl SocketLike for std::fs::FileType {
        fn is_socket_like(&self) -> bool {
            use std::os::unix::fs::FileTypeExt;
            self.is_socket()
        }
    }

    /// Connect, then verify the server is the current user before any byte is exchanged
    /// (a squatter on the endpoint path would otherwise receive our `hello`).
    pub fn connect(endpoint: &str) -> io::Result<Conn> {
        let s = UnixStream::connect(endpoint)?;
        verify_peer_is_current_user(&s).map_err(io::Error::from)?;
        wrap(s)
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::peer::{
        owner_only_security_attributes, verify_pipe_peer_is_current_user,
        verify_pipe_server_is_current_user, OwnerOnlySecurityAttributes,
    };
    use std::os::windows::io::RawHandle;
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::sync::{Mutex, RwLock};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
        PIPE_ACCESS_DUPLEX,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PeekNamedPipe, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows::Win32::System::Threading::CreateEventW;
    use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

    pub fn current_uid() -> u32 {
        0
    }

    struct SendHandle(HANDLE);
    unsafe impl Send for SendHandle {}

    pub struct Listener {
        name: Vec<u16>,
        attrs: OwnerOnlySecurityAttributes,
        /// Cap on simultaneous instances of this pipe (live connections + the one waiting).
        max_instances: u32,
        first: AtomicBool,
        endpoint: String,
        /// The first instance, created at bind time (FILE_FLAG_FIRST_PIPE_INSTANCE) and
        /// kept open so the name is never unowned between bind and the first accept.
        pending: Mutex<Option<SendHandle>>,
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    const ERROR_BROKEN_PIPE: u32 = 109;
    const ERROR_IO_PENDING: u32 = 997;
    const ERROR_OPERATION_ABORTED: u32 = 995;
    const ERROR_NO_DATA: u32 = 232;
    const ERROR_PIPE_CONNECTED: u32 = 535;
    const FILE_FLAG_OVERLAPPED_BITS: u32 = 0x4000_0000;
    // CreateFile security quality-of-service flags (winbase.h).
    const SECURITY_SQOS_PRESENT_BITS: u32 = 0x0010_0000;
    const SECURITY_IDENTIFICATION_BITS: u32 = 0x0001_0000;

    fn code(e: &windows::core::Error) -> u32 {
        (e.code().0 as u32) & 0xFFFF
    }

    /// One pipe handle opened with FILE_FLAG_OVERLAPPED. Reads and writes each use
    /// their own OVERLAPPED + event, so a read blocked in one thread never blocks a
    /// write, a peer check, or `cancel` from another thread (a synchronous handle
    /// serialises all I/O on the file object). `cancel` issues CancelIoEx, which
    /// completes every pending operation with ERROR_OPERATION_ABORTED.
    struct PipeInner {
        /// `Some(handle)` until `cancel` closes it. Operations hold the read lock for
        /// their whole (possibly blocked) duration; `cancel` takes the write lock only
        /// after CancelIoEx has released them, so the handle is never closed under I/O.
        h: RwLock<Option<HANDLE>>,
        raw: AtomicIsize,
        cancelled: AtomicBool,
        closing: Mutex<()>,
    }
    unsafe impl Send for PipeInner {}
    unsafe impl Sync for PipeInner {}
    impl Drop for PipeInner {
        fn drop(&mut self) {
            if let Some(h) = self.h.get_mut().unwrap_or_else(|e| e.into_inner()).take() {
                unsafe {
                    let _ = CloseHandle(h);
                }
            }
        }
    }

    #[derive(Clone)]
    struct Pipe(Arc<PipeInner>);

    impl Pipe {
        fn new(h: HANDLE) -> Pipe {
            Pipe(Arc::new(PipeInner {
                h: RwLock::new(Some(h)),
                raw: AtomicIsize::new(h.0 as isize),
                cancelled: AtomicBool::new(false),
                closing: Mutex::new(()),
            }))
        }

        /// Interrupt every pending operation, then close the handle so the peer sees
        /// the disconnect now rather than when the last clone drops. Idempotent.
        fn cancel(&self) {
            let _one = self.0.closing.lock().unwrap_or_else(|e| e.into_inner());
            self.0.cancelled.store(true, Ordering::SeqCst);
            let raw = HANDLE(self.0.raw.load(Ordering::SeqCst) as *mut _);
            if self.0.h.read().map(|g| g.is_some()).unwrap_or(false) {
                // Safe: only `cancel` (serialised by `closing`) ever closes the handle.
                unsafe {
                    let _ = CancelIoEx(raw, None);
                }
            }
            let mut g = self.0.h.write().unwrap_or_else(|e| e.into_inner()); // waits for aborted ops to unwind
            if let Some(h) = g.take() {
                unsafe {
                    let _ = CloseHandle(h);
                }
            }
        }

        /// Run one overlapped operation to completion. `start` issues the call with
        /// the prepared OVERLAPPED; the wait happens here, outside any shared lock.
        fn run(
            &self,
            start: impl FnOnce(HANDLE, *mut OVERLAPPED) -> windows::core::Result<()>,
        ) -> io::Result<usize> {
            if self.0.cancelled.load(Ordering::SeqCst) {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "connection closed",
                ));
            }
            let guard = self.0.h.read().unwrap_or_else(|e| e.into_inner());
            let Some(handle) = *guard else {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "connection closed",
                ));
            };
            unsafe {
                let ev =
                    CreateEventW(None, true, false, PCWSTR::null()).map_err(io::Error::other)?;
                let mut ov = OVERLAPPED {
                    hEvent: ev,
                    ..Default::default()
                };
                let started = start(handle, &mut ov);
                // `cancel` may have run its CancelIoEx before this operation was issued;
                // re-check now that it is registered so it cannot wait forever.
                if self.0.cancelled.load(Ordering::SeqCst) {
                    let _ = CancelIoEx(handle, Some(&ov));
                }
                let outcome = match started {
                    Ok(()) => Ok(true),
                    Err(e) if code(&e) == ERROR_IO_PENDING => Ok(true),
                    // ConnectNamedPipe: the client connected before the call. Nothing to wait for.
                    Err(e) if code(&e) == ERROR_PIPE_CONNECTED => Ok(false),
                    Err(e) => Err(e),
                };
                let res = match outcome {
                    Ok(false) => Ok(0),
                    Ok(true) => {
                        let mut n = 0u32;
                        // bWait = true: blocks only this call; CancelIoEx releases it.
                        match GetOverlappedResult(handle, &ov, &mut n, true) {
                            Ok(()) => Ok(n as usize),
                            Err(e) => Err(e),
                        }
                    }
                    Err(e) => Err(e),
                };
                let _ = CloseHandle(ev);
                match res {
                    Ok(n) => Ok(n),
                    Err(e) => match code(&e) {
                        ERROR_BROKEN_PIPE | ERROR_NO_DATA => {
                            Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed"))
                        }
                        ERROR_OPERATION_ABORTED => Err(io::Error::new(
                            io::ErrorKind::ConnectionAborted,
                            "operation cancelled",
                        )),
                        _ => Err(io::Error::other(e)),
                    },
                }
            }
        }
    }

    struct Half(Pipe);
    impl Read for Half {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            match self
                .0
                .run(|h, ov| unsafe { ReadFile(h, Some(b), None, Some(ov)) })
            {
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(0), // peer closed: EOF
                r => r,
            }
        }
    }
    impl Write for Half {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0
                .run(|h, ov| unsafe { WriteFile(h, Some(b), None, Some(ov)) })
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(()) // writes complete when the pipe buffer accepts them
        }
    }

    fn wrap(pipe: Pipe) -> Conn {
        let (w, watch, sd) = (pipe.clone(), pipe.clone(), pipe.clone());
        Conn {
            peer: crate::peer::PeerIdentity::default(),
            reader: Box::new(Half(pipe)),
            writer: Box::new(Half(w)),
            peer_closed: Arc::new(move || {
                if watch.0.cancelled.load(Ordering::SeqCst) {
                    return true;
                }
                // PeekNamedPipe never blocks and is not serialised with a pending read.
                let g = watch.0.h.read().unwrap_or_else(|e| e.into_inner());
                match *g {
                    Some(h) => unsafe { PeekNamedPipe(h, None, 0, None, None, None).is_err() },
                    None => true,
                }
            }),
            // Real cancellation: pending reads/writes return now; the handle closes
            // when the last clone (reader, writer, this closure) drops.
            shutdown: Arc::new(move || sd.cancel()),
        }
    }

    impl Listener {
        /// `max_instances` is clamped to 1..=254 (255 means unlimited to Win32). Every
        /// instance of one pipe name must pass the same value.
        pub fn bind(endpoint: &str, max_instances: u32) -> io::Result<Listener> {
            let attrs = owner_only_security_attributes()?;
            let l = Listener {
                name: wide(endpoint),
                attrs,
                max_instances: max_instances.clamp(1, 254),
                first: AtomicBool::new(true),
                endpoint: endpoint.to_string(),
                pending: Mutex::new(None),
            };
            // Create the first instance now so a squatter fails the bind, not a later accept.
            // FILE_FLAG_FIRST_PIPE_INSTANCE on a name whose previous owner is still closing its
            // instances fails ERROR_ACCESS_DENIED (5): report that as the address being in use.
            let h = l.instance().map_err(|e| {
                if e.raw_os_error() == Some(5) {
                    io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!("pipe {endpoint} still held: {e}"),
                    )
                } else {
                    e
                }
            })?;
            *l.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(SendHandle(h));
            Ok(l)
        }

        fn instance(&self) -> io::Result<HANDLE> {
            let mut open = PIPE_ACCESS_DUPLEX;
            if self.first.swap(false, Ordering::SeqCst) {
                open |= FILE_FLAG_FIRST_PIPE_INSTANCE;
            }
            let h = unsafe {
                CreateNamedPipeW(
                    PCWSTR(self.name.as_ptr()),
                    open | FILE_FLAG_OVERLAPPED,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    self.max_instances,
                    64 * 1024,
                    64 * 1024,
                    0,
                    Some(self.attrs.raw()),
                )
            };
            if h.is_invalid() {
                return Err(io::Error::last_os_error());
            }
            Ok(h)
        }

        pub fn accept(&self) -> io::Result<Accepted> {
            let pending = self
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            let h = match pending {
                Some(p) => p.0,
                None => self.instance()?,
            };
            let pipe = Pipe::new(h); // owns (and on error closes) the instance
            let connected = pipe.run(|h, ov| unsafe { ConnectNamedPipe(h, Some(ov)) });
            connected.map_err(|e| io::Error::new(e.kind(), format!("accept: {e}")))?;
            // Authenticate before a single byte is read. A rejected instance is closed
            // (the peer sees a disconnect); the next accept creates a fresh instance.
            let raw = pipe.0.raw.load(Ordering::SeqCst) as RawHandle;
            match verify_pipe_peer_is_current_user(raw) {
                Ok(peer) => Ok(Accepted::Conn(Conn { peer, ..wrap(pipe) })),
                Err(e) => {
                    pipe.cancel();
                    if e.peer_gone() {
                        Ok(Accepted::Vanished)
                    } else {
                        Ok(Accepted::Rejected(e.to_string()))
                    }
                }
            }
        }
        pub fn wake(&self) {
            let _ = open_overlapped(&self.endpoint);
        }
        pub fn cleanup(&self) {}
    }

    /// Connect, then verify the server process runs as the current user before any byte is
    /// exchanged. The pipe is opened with SQOS + SECURITY_IDENTIFICATION, so the server can
    /// identify the caller but never impersonate it.
    pub fn connect(endpoint: &str) -> io::Result<Conn> {
        let pipe = open_overlapped(endpoint)?;
        let raw = pipe.0.raw.load(Ordering::SeqCst) as RawHandle;
        if let Err(e) = verify_pipe_server_is_current_user(raw) {
            pipe.cancel();
            return Err(e.into());
        }
        Ok(wrap(pipe))
    }

    fn open_overlapped(endpoint: &str) -> io::Result<Pipe> {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::IntoRawHandle;
        const ERROR_FILE_NOT_FOUND: i32 = 2;
        const ERROR_PIPE_BUSY: i32 = 231;
        let start = std::time::Instant::now();
        loop {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(FILE_FLAG_OVERLAPPED_BITS)
                .security_qos_flags(SECURITY_SQOS_PRESENT_BITS | SECURITY_IDENTIFICATION_BITS)
                .open(endpoint)
            {
                Ok(f) => return Ok(Pipe::new(HANDLE(f.into_raw_handle() as *mut _))),
                Err(e) => {
                    let limit = match e.raw_os_error() {
                        Some(ERROR_PIPE_BUSY) => std::time::Duration::from_secs(3),
                        Some(ERROR_FILE_NOT_FOUND) => std::time::Duration::from_secs(1),
                        _ => return Err(e),
                    };
                    if start.elapsed() >= limit {
                        return Err(e);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
            }
        }
    }
}

pub(crate) use imp::{connect, current_uid, Listener};

/// Is process `pid` alive? A dead pid marks a discovery record stale and a
/// takeover request abandoned. Hardened from HeardRight's version: pid 0 and
/// pids that do not fit `pid_t` are never alive (they would signal process
/// groups), and on Windows an exited process whose handle is still open is dead.
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    unsafe {
        kill(pid as i32, 0) == 0 || io::Error::last_os_error().raw_os_error() == Some(1)
        /* EPERM: exists */
    }
}
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const STILL_ACTIVE: u32 = 259;
    if pid == 0 {
        return false;
    }
    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => {
                let mut code = 0u32;
                let ok = GetExitCodeProcess(h, &mut code).is_ok();
                let _ = CloseHandle(h);
                ok && code == STILL_ACTIVE
            }
            Err(e) => e.code() == ERROR_ACCESS_DENIED.to_hresult(),
        }
    }
}

/// A raw duplex byte stream to an endpoint, for callers that speak their own
/// line protocol over the shared transport (HeardRight's v1 `EngineFrame`
/// dialect, which has no `hello`). Same current-user-only transport as
/// [`crate::Client`].
pub struct Stream {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    shutdown: Arc<dyn Fn() + Send + Sync>,
}

impl Stream {
    /// Abort the stream from any thread: a blocked read/write returns, and the
    /// host sees a disconnect (which cancels the request it is serving).
    pub fn abort_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
        self.shutdown.clone()
    }
}

/// Connect to `endpoint`. On Windows a busy pipe is retried up to 3 s and a
/// not-yet-created one up to 1 s (HeardRight's behaviour).
pub fn open_stream(endpoint: &str) -> io::Result<Stream> {
    let c = connect(endpoint)?;
    Ok(Stream {
        reader: c.reader,
        writer: c.writer,
        shutdown: c.shutdown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn unique(tag: &str) -> String {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        format!("{tag}-{}-{n}-{t:x}", std::process::id())
    }

    #[cfg(unix)]
    fn endpoint(dir: &tempfile::TempDir) -> String {
        dir.path().join("t.sock").to_string_lossy().into_owned()
    }
    #[cfg(windows)]
    fn endpoint(_: &NoDir) -> String {
        format!(r"\\.\pipe\rightkit-transport-test-{}", unique("p"))
    }
    #[cfg(unix)]
    fn tmp() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("rk").tempdir().unwrap()
    }
    /// Named pipes need no directory; this stands in for the Unix temporary directory.
    #[cfg(windows)]
    struct NoDir;
    #[cfg(windows)]
    fn tmp() -> NoDir {
        NoDir
    }

    /// A same-user client and host round-trip over the real transport, with the peer
    /// check running on both ends (accept and connect).
    #[test]
    fn same_user_round_trip_authenticates_both_ends() {
        let _ = unique("x");
        let dir = tmp();
        let ep = endpoint(&dir);
        let l = Listener::bind(&ep, 4).unwrap();
        let client_ep = ep.clone();
        let t = std::thread::spawn(move || {
            let mut c = connect(&client_ep).expect("same-user server must authenticate");
            c.writer.write_all(b"ping\n").unwrap();
            c.writer.flush().unwrap();
            let mut buf = [0u8; 5];
            c.reader.read_exact(&mut buf).unwrap();
            buf
        });
        let Accepted::Conn(mut conn) = l.accept().unwrap() else {
            panic!("same-user peer must be accepted");
        };
        let mut buf = [0u8; 5];
        conn.reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ping\n");
        conn.writer.write_all(b"pong\n").unwrap();
        conn.writer.flush().unwrap();
        assert_eq!(&t.join().unwrap(), b"pong\n");
        l.cleanup();
    }

    /// A peer the verifier refuses is closed unread and reported as Rejected.
    #[cfg(unix)]
    #[test]
    fn rejected_peer_is_closed_unread() {
        use crate::peer::{PeerError, PeerIdentity};
        let dir = tmp();
        let ep = endpoint(&dir);
        let l = Listener::bind(&ep, 4).unwrap();
        let client_ep = ep.clone();
        let t = std::thread::spawn(move || {
            let mut s = std::os::unix::net::UnixStream::connect(&client_ep).unwrap();
            let _ = s.write_all(b"{\"type\":\"hello\"}\n");
            let mut b = [0u8; 1];
            // The host closes without reading or answering: EOF (or a reset), never data.
            matches!(s.read(&mut b), Ok(0) | Err(_))
        });
        let foreign = PeerIdentity {
            uid: Some(crate::peer::current_euid().wrapping_add(1)),
            ..Default::default()
        };
        match l
            .accept_with(|_| Err(PeerError::Mismatch(foreign)))
            .unwrap()
        {
            Accepted::Rejected(reason) => assert!(reason.contains("not the current user")),
            _ => panic!("a foreign peer must be rejected"),
        }
        assert!(
            t.join().unwrap(),
            "rejected client must see a close, not a reply"
        );
        l.cleanup();
    }

    /// A platform that cannot authenticate the peer rejects instead of admitting it.
    #[cfg(unix)]
    #[test]
    fn unsupported_verifier_never_admits() {
        use crate::peer::PeerError;
        let dir = tmp();
        let ep = endpoint(&dir);
        let l = Listener::bind(&ep, 4).unwrap();
        let client_ep = ep.clone();
        let t = std::thread::spawn(move || std::os::unix::net::UnixStream::connect(client_ep));
        let a = l
            .accept_with(|_| Err(PeerError::Unsupported("test")))
            .unwrap();
        assert!(matches!(a, Accepted::Rejected(_)));
        let _ = t.join();
        l.cleanup();
    }

    #[cfg(windows)]
    #[test]
    fn public_pipe_verifiers_accept_a_same_user_pipe() {
        use crate::peer::{
            owner_only_security_attributes, verify_pipe_peer_is_current_user,
            verify_pipe_server_is_current_user,
        };
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::{AsRawHandle, RawHandle};
        use windows::core::PCWSTR;
        use windows::Win32::Security::SECURITY_ATTRIBUTES;
        use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
        use windows::Win32::System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
            PIPE_WAIT,
        };
        let name = format!(r"\\.\pipe\rightkit-verify-test-{}", unique("v"));
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let attrs = owner_only_security_attributes().unwrap();
        let server = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                4096,
                0,
                Some(attrs.as_ptr() as *const SECURITY_ATTRIBUTES),
            )
        };
        assert!(!server.is_invalid());
        let client_name = name.clone();
        let t = std::thread::spawn(move || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .security_qos_flags(0x0010_0000 | 0x0001_0000)
                .open(client_name)
                .unwrap()
        });
        // Blocking ConnectNamedPipe returns once the client has opened the pipe.
        unsafe { ConnectNamedPipe(server, None) }.ok();
        let client = t.join().unwrap();
        let id = verify_pipe_peer_is_current_user(server.0 as RawHandle).unwrap();
        assert_eq!(id.pid, Some(std::process::id()));
        assert_eq!(
            id.sid,
            Some(crate::peer::current_user_sid_string().unwrap())
        );
        let id = verify_pipe_server_is_current_user(client.as_raw_handle()).unwrap();
        assert_eq!(id.pid, Some(std::process::id()));
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(server) };
    }

    // ---- endpoint naming (gap ledger row 167) ----

    fn long_service() -> String {
        format!("{}-instance", "a".repeat(39))
    }

    #[test]
    fn fallback_file_name_is_pinned_and_bounded() {
        // FNV-1a 64 of the service name; changing this string breaks client/server agreement.
        assert_eq!(
            fallback_file_name("0badc0de", &long_service()),
            "rk-0badc0de-3a43f919c4e992b6.sock"
        );
        assert_eq!(
            fallback_file_name("0badc0de", "speech"),
            "rk-0badc0de-c1d6288e168cb0d7.sock"
        );
        assert_eq!(fallback_file_name("0badc0de", &long_service()).len(), 33);
        assert_eq!(fnv1a64(""), 0xcbf2_9ce4_8422_2325);
    }

    #[cfg(unix)]
    fn long_root() -> tempfile::TempDir {
        // About 60 bytes: long enough that the old `rk-<tag>-<service>.sock` (65 bytes for a
        // 48-byte name) never fitted, short enough for the bounded name (33 bytes).
        let d = tempfile::Builder::new()
            .prefix(&"r".repeat(49))
            .tempdir_in("/tmp")
            .unwrap();
        let n = d.path().as_os_str().len();
        assert!((50..=69).contains(&n), "root is {n} bytes");
        d
    }

    #[cfg(unix)]
    #[test]
    fn a_48_byte_service_name_binds_and_connects_under_a_long_temp_root() {
        let suite = Suite::per_run_temp("ep-long").unwrap();
        let service = long_service();
        assert_eq!(service.len(), 48);
        let root = long_root();
        let ep = unix_endpoint_in(&suite, &service, root.path()).unwrap();
        assert!(ep.starts_with(root.path().to_str().unwrap()));
        assert!(
            ep.len() <= MAX_UNIX_SOCKET_PATH,
            "{ep} is {} bytes",
            ep.len()
        );
        // The pre-fix name for the same inputs would not have fitted.
        assert_eq!(legacy_unix_endpoint_in(&suite, &service, root.path()), None);
        let l = Listener::bind(&ep, DEFAULT_MAX_PIPE_INSTANCES).unwrap();
        assert!(connect(&ep).is_ok());
        l.cleanup();
        // Both ends derive the same path from the same inputs.
        assert_eq!(
            candidates_in(&suite, &service, root.path()),
            vec![ep.clone()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_new_client_still_finds_an_old_host_on_the_old_fallback_name() {
        let suite = Suite::per_run_temp("ep-old").unwrap();
        // A 20-byte name with a short temp root: the old name fitted, so an old host may hold it.
        let service = "legacy-fallback-name";
        let root = tempfile::Builder::new()
            .prefix("rk")
            .tempdir_in("/tmp")
            .unwrap();
        // Only reachable when the in-suite path is too long, as under a long $TMPDIR.
        if suite
            .service_dir(service)
            .join(format!("{service}.sock"))
            .as_os_str()
            .len()
            <= IN_SUITE_PATH_LIMIT
        {
            return;
        }
        let new = unix_endpoint_in(&suite, service, root.path()).unwrap();
        let old = legacy_unix_endpoint_in(&suite, service, root.path()).expect("old name fitted");
        assert_ne!(new, old);
        assert_eq!(
            candidates_in(&suite, service, root.path()),
            vec![new.clone(), old.clone()]
        );
        // An old host holds only the old name.
        let _old_host = std::os::unix::net::UnixListener::bind(&old).unwrap();
        let live = candidates_in(&suite, service, root.path())
            .into_iter()
            .find(|e| probe_endpoint(e));
        assert_eq!(live.as_deref(), Some(old.as_str()));
        // A new host on the new name wins over the old one (it is listed first).
        let l = Listener::bind(&new, DEFAULT_MAX_PIPE_INSTANCES).unwrap();
        let live = candidates_in(&suite, service, root.path())
            .into_iter()
            .find(|e| probe_endpoint(e));
        assert_eq!(live.as_deref(), Some(new.as_str()));
        l.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn an_unfittable_temp_root_is_a_typed_error_naming_path_length_and_limit() {
        let suite = Suite::per_run_temp("ep-absurd").unwrap();
        let service = long_service();
        let temp = std::path::PathBuf::from(format!("/{}", "t".repeat(90)));
        let e = unix_endpoint_in(&suite, &service, &temp).unwrap_err();
        assert_eq!(e.limit, MAX_UNIX_SOCKET_PATH);
        assert_eq!(e.len, e.path.len());
        assert!(e.len > e.limit);
        assert!(e.path.starts_with(temp.to_str().unwrap()));
        let msg = e.to_string();
        assert!(msg.contains(&e.path) && msg.contains(&e.len.to_string()) && msg.contains("103"));
        assert!(legacy_unix_endpoint_in(&suite, &service, &temp).is_none());
        assert!(candidates_in(&suite, &service, &temp).is_empty());
    }
}
