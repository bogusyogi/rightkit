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

/// Endpoint for `service` on `suite`. Windows: a pipe name that carries the
/// user and a hash of the suite root, so an isolated suite never collides with
/// the user's installed host. Unix: a socket in the private service dir, or in
/// the per-user temp dir when the path would exceed the `sockaddr_un` limit.
pub fn endpoint_for(suite: &Suite, service: &str) -> String {
    let tag = format!("{:08x}", fnv(&suite.root().to_string_lossy()));
    if cfg!(windows) {
        let user: String = std::env::var("USERNAME")
            .unwrap_or_default()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(20)
            .collect();
        format!(r"\\.\pipe\rightkit-{service}-{user}-{tag}")
    } else {
        let p = suite.service_dir(service).join(format!("{service}.sock"));
        if p.as_os_str().len() <= 100 {
            p.to_string_lossy().into_owned()
        } else {
            std::env::temp_dir()
                .join(format!("rk-{tag}-{service}.sock"))
                .to_string_lossy()
                .into_owned()
        }
    }
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
            Ok(_) => wrap(s).map(Accepted::Conn),
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
                Ok(_) => Ok(Accepted::Conn(wrap(pipe))),
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
    fn endpoint(_: &()) -> String {
        format!(r"\\.\pipe\rightkit-transport-test-{}", unique("p"))
    }
    #[cfg(unix)]
    fn tmp() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("rk").tempdir().unwrap()
    }
    #[cfg(windows)]
    fn tmp() {}

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
}
