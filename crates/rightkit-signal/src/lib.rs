//! Cross-process wake-up signals between a native companion and a host app (rightkit#867).
//!
//! A menu-bar helper, a notch, a Swift companion or a second process of the same product needs to
//! tell the host "something happened" without a socket: the data travels by file or queue and the
//! signal only wakes the reader. This crate is that signal, with one API on both desktop systems:
//!
//! * macOS: Darwin notifications (`notify_post`, `notify_register_file_descriptor`, libSystem). They
//!   carry no payload and are system wide.
//! * Windows: named auto-reset events in the session namespace (`Local\<name>`). They carry no
//!   payload either.
//! * Elsewhere: every call answers [`SignalError::Unsupported`].
//!
//! The pieces:
//!
//! * [`post`] wakes every [`Watcher`] of a name, in any process.
//! * [`Watcher`] observes one or more names on its own thread and calls a handler with the name's
//!   index. Several posts that arrive before the handler runs are one call: a signal means "look",
//!   never "this many times". A post while no watcher exists is lost; the reader catches up by
//!   reading the data it was told about when it starts.
//! * [`Lease`] repeats a post on an interval while a probe says the app is visible (or alive), and
//!   [`LeaseMonitor`] is the reading side: it is live while the last post is younger than a time to
//!   live, so a lease lapses when the poster dies. The notch's "the hub is visible" heartbeat is
//!   exactly this.
//!
//! Names are 1 to 200 characters of ASCII letters, digits, `.`, `_` and `-`, for example
//! `dev.orthic.pulse.hub.show.overview`; reverse-DNS names keep products apart. The payload-free
//! rule is deliberate: an app that needs to say which of several things happened uses one name per
//! thing (the Pulse hub uses one per section) or a file beside the signal.
//!
//! macOS caveat: a Darwin notification name is global to the user's session, so a name must belong
//! to the product. Windows: `Local\` scopes the event to the logon session, as the Pulse notch does.

use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Why a signal call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalError {
    /// The name is empty, too long or has a character outside the allowed set.
    InvalidName(String),
    /// A call needs at least one name, or more names than the platform can wait on.
    InvalidNames(String),
    /// The operating system refused the call; the text names the call and its code.
    Os(String),
    /// This platform has no signal transport.
    Unsupported,
}

impl fmt::Display for SignalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SignalError::InvalidName(name) => write!(
                f,
                "signal name {name:?} must be 1 to {MAX_NAME_LEN} ASCII letters, digits, '.', '_' or '-'"
            ),
            SignalError::InvalidNames(why) => f.write_str(why),
            SignalError::Os(why) => f.write_str(why),
            SignalError::Unsupported => f.write_str("signals are not available on this platform"),
        }
    }
}

impl std::error::Error for SignalError {}

/// Longest signal name.
pub const MAX_NAME_LEN: usize = 200;

/// Most names one [`Watcher`] observes. Windows waits on at most 64 handles and one is the stop
/// event; the same limit holds everywhere so a watcher moves between platforms unchanged.
pub const MAX_WATCHED: usize = 63;

/// Check a signal name.
pub fn validate_name(name: &str) -> Result<(), SignalError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(SignalError::InvalidName(name.to_owned()))
    }
}

/// Wake every [`Watcher`] of `name`, in this or any other process of the user's session.
pub fn post(name: &str) -> Result<(), SignalError> {
    validate_name(name)?;
    platform::post(name)
}

/// Observes names on its own thread until it is stopped or dropped.
pub struct Watcher {
    stop: platform::StopHandle,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Observe `names` and call `on_signal(index, name)` for each wake-up, where `index` is the
    /// position of the name in `names`. The handler runs on the watcher's thread, so it should
    /// return quickly (queue work, emit an event) rather than block.
    ///
    /// Every name is registered before this returns, so a post after `start` is never missed.
    pub fn start(
        names: &[&str],
        on_signal: impl Fn(usize, &str) + Send + 'static,
    ) -> Result<Self, SignalError> {
        if names.is_empty() {
            return Err(SignalError::InvalidNames(
                "a watcher needs at least one name".into(),
            ));
        }
        if names.len() > MAX_WATCHED {
            return Err(SignalError::InvalidNames(format!(
                "a watcher observes at most {MAX_WATCHED} names"
            )));
        }
        for name in names {
            validate_name(name)?;
        }
        let owned: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
        let (stop, thread) = platform::watch(owned, Box::new(on_signal))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    /// Stop observing and wait for the thread to finish. Dropping does the same.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.signal();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Posts `name` every `interval` while `probe` returns true, until stopped or dropped. The first
/// post happens at once, so a reader that just started sees the lease without waiting a period.
pub struct Lease {
    shared: Arc<LeaseShared>,
    thread: Option<JoinHandle<()>>,
}

struct LeaseShared {
    stopped: Mutex<bool>,
    wake: Condvar,
}

impl Lease {
    /// Start a lease on `name`. `probe` runs on the lease's thread once per `interval` (for the
    /// Pulse hub: "is the main window visible"); a probe must not block. `interval` must be at
    /// least 10 ms.
    pub fn start(
        name: &str,
        interval: Duration,
        probe: impl Fn() -> bool + Send + 'static,
    ) -> Result<Self, SignalError> {
        validate_name(name)?;
        if interval < Duration::from_millis(10) {
            return Err(SignalError::InvalidNames(
                "a lease interval is at least 10 ms".into(),
            ));
        }
        if !platform::SUPPORTED {
            return Err(SignalError::Unsupported);
        }
        let shared = Arc::new(LeaseShared {
            stopped: Mutex::new(false),
            wake: Condvar::new(),
        });
        let (name, worker) = (name.to_owned(), Arc::clone(&shared));
        let thread = std::thread::Builder::new()
            .name("rightkit-signal-lease".into())
            .spawn(move || {
                let mut stopped = worker.stopped.lock().unwrap_or_else(|e| e.into_inner());
                while !*stopped {
                    if probe() {
                        // A failed post is retried on the next period; nothing to report.
                        let _ = platform::post(&name);
                    }
                    stopped = match worker.wake.wait_timeout(stopped, interval) {
                        Ok((guard, _)) => guard,
                        Err(poisoned) => poisoned.into_inner().0,
                    };
                }
            })
            .map_err(|e| SignalError::Os(format!("spawn lease thread: {e}")))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// Stop posting and wait for the thread. Dropping does the same.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        *self
            .shared
            .stopped
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The reading side of a [`Lease`]: live while the last post is younger than the time to live.
pub struct LeaseMonitor {
    last: Arc<Mutex<Option<Instant>>>,
    _watcher: Watcher,
}

impl LeaseMonitor {
    /// Observe the lease named `name`.
    pub fn start(name: &str) -> Result<Self, SignalError> {
        let last = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&last);
        let watcher = Watcher::start(&[name], move |_, _| {
            *seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        })?;
        Ok(Self {
            last,
            _watcher: watcher,
        })
    }

    /// When the last post arrived, `None` before the first.
    pub fn last_seen(&self) -> Option<Instant> {
        *self.last.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the lease holder posted within `ttl`. Pick `ttl` at several times the lease
    /// interval (the notch uses 6 s over a 0.5 s lease) so one late post does not drop it.
    pub fn is_live(&self, ttl: Duration) -> bool {
        self.last_seen().is_some_and(|at| at.elapsed() <= ttl)
    }
}

type Handler = Box<dyn Fn(usize, &str) + Send + 'static>;

#[cfg(target_os = "macos")]
mod platform {
    use super::{Handler, SignalError};
    use std::ffi::{c_char, c_int, CString};
    use std::os::unix::io::RawFd;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread::JoinHandle;

    pub const SUPPORTED: bool = true;

    const NOTIFY_STATUS_OK: u32 = 0;
    const POLLIN: i16 = 0x0001;
    const POLLERR: i16 = 0x0008;
    const POLLHUP: i16 = 0x0010;
    const POLLNVAL: i16 = 0x0020;
    const F_SETFL: c_int = 4;
    const O_NONBLOCK: c_int = 0x0004;
    /// How often a watcher looks at its stop flag when nothing is posted.
    const STOP_POLL_MS: c_int = 200;

    #[repr(C)]
    struct PollFd {
        fd: c_int,
        events: i16,
        revents: i16,
    }

    extern "C" {
        fn notify_post(name: *const c_char) -> u32;
        fn notify_register_file_descriptor(
            name: *const c_char,
            notify_fd: *mut c_int,
            flags: c_int,
            out_token: *mut c_int,
        ) -> u32;
        fn notify_cancel(token: c_int) -> u32;
        fn poll(fds: *mut PollFd, nfds: u32, timeout: c_int) -> c_int;
        fn read(fd: c_int, buf: *mut std::ffi::c_void, count: usize) -> isize;
        fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    }

    pub fn post(name: &str) -> Result<(), SignalError> {
        let name = CString::new(name).map_err(|_| SignalError::InvalidName(name.to_owned()))?;
        // SAFETY: `name` is a NUL-terminated C string that lives for the call.
        let status = unsafe { notify_post(name.as_ptr()) };
        if status == NOTIFY_STATUS_OK {
            Ok(())
        } else {
            Err(SignalError::Os(format!(
                "notify_post failed with status {status}"
            )))
        }
    }

    /// Stops a watcher: the thread looks at the flag between polls.
    pub struct StopHandle(Arc<AtomicBool>);

    impl StopHandle {
        pub fn signal(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    struct Registration {
        token: c_int,
        fd: RawFd,
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            // SAFETY: the token came from a successful registration and is cancelled once; the
            // cancel also closes the descriptor.
            unsafe {
                notify_cancel(self.token);
            }
        }
    }

    pub fn watch(
        names: Vec<String>,
        on_signal: Handler,
    ) -> Result<(StopHandle, JoinHandle<()>), SignalError> {
        let mut registrations: Vec<Registration> = Vec::with_capacity(names.len());
        for name in &names {
            let c_name =
                CString::new(name.as_str()).map_err(|_| SignalError::InvalidName(name.clone()))?;
            let (mut fd, mut token): (c_int, c_int) = (-1, 0);
            // SAFETY: `c_name` is NUL-terminated; both out pointers are valid for the call.
            let status =
                unsafe { notify_register_file_descriptor(c_name.as_ptr(), &mut fd, 0, &mut token) };
            if status != NOTIFY_STATUS_OK || fd < 0 {
                // Earlier registrations are cancelled by their `Drop`.
                return Err(SignalError::Os(format!(
                    "notify_register_file_descriptor({name}) failed with status {status}"
                )));
            }
            let registration = Registration { token, fd };
            // SAFETY: `fd` is the descriptor the registration just returned; draining must never
            // block the watcher thread.
            unsafe {
                fcntl(fd, F_SETFL, O_NONBLOCK);
            }
            registrations.push(registration);
        }
        let flag = Arc::new(AtomicBool::new(false));
        let stop = StopHandle(Arc::clone(&flag));
        let thread = std::thread::Builder::new()
            .name("rightkit-signal-watch".into())
            .spawn(move || {
                let mut live: Vec<bool> = vec![true; registrations.len()];
                while !flag.load(Ordering::SeqCst) {
                    let mut fds: Vec<PollFd> = Vec::new();
                    let mut which: Vec<usize> = Vec::new();
                    for (i, registration) in registrations.iter().enumerate() {
                        if live[i] {
                            fds.push(PollFd {
                                fd: registration.fd,
                                events: POLLIN,
                                revents: 0,
                            });
                            which.push(i);
                        }
                    }
                    if fds.is_empty() {
                        // Every descriptor died: nothing can arrive any more.
                        return;
                    }
                    // SAFETY: `fds` is a valid array of `fds.len()` entries for the call.
                    let ready = unsafe { poll(fds.as_mut_ptr(), fds.len() as u32, STOP_POLL_MS) };
                    if ready <= 0 {
                        continue;
                    }
                    for (slot, entry) in fds.iter().enumerate() {
                        let i = which[slot];
                        if entry.revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
                            // Never spin on a dead descriptor.
                            live[i] = false;
                            continue;
                        }
                        if entry.revents & POLLIN == 0 {
                            continue;
                        }
                        // Empty the pipe: each post writes a 4-byte token, and posts that arrived
                        // together are one wake-up.
                        let mut sink = [0u8; 64];
                        loop {
                            // SAFETY: `sink` is a valid writable buffer of its length.
                            let n = unsafe { read(entry.fd, sink.as_mut_ptr().cast(), sink.len()) };
                            if n == 0 {
                                live[i] = false;
                                break;
                            }
                            if n < 0 {
                                break;
                            }
                        }
                        if live[i] {
                            on_signal(i, &names[i]);
                        }
                    }
                }
            })
            .map_err(|e| SignalError::Os(format!("spawn watcher thread: {e}")))?;
        Ok((stop, thread))
    }
}

#[cfg(windows)]
mod platform {
    use super::{Handler, SignalError};
    use std::ffi::c_void;
    use std::thread::JoinHandle;

    pub const SUPPORTED: bool = true;

    type Handle = *mut c_void;

    const WAIT_OBJECT_0: u32 = 0;
    const INFINITE: u32 = 0xFFFF_FFFF;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateEventW(
            attributes: *mut c_void,
            manual_reset: i32,
            initial_state: i32,
            name: *const u16,
        ) -> Handle;
        fn SetEvent(event: Handle) -> i32;
        fn ResetEvent(event: Handle) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn WaitForMultipleObjects(
            count: u32,
            handles: *const Handle,
            wait_all: i32,
            milliseconds: u32,
        ) -> u32;
        fn GetLastError() -> u32;
    }

    fn event_name(name: &str) -> Vec<u16> {
        format!("Local\\{name}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }

    /// An event handle that one thread owns. The raw pointer is only moved, never shared.
    struct Owned(usize);

    impl Owned {
        fn handle(&self) -> Handle {
            self.0 as Handle
        }
    }

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: the handle came from `CreateEventW` and is closed exactly once.
            unsafe {
                CloseHandle(self.handle());
            }
        }
    }

    /// An auto-reset event named `Local\<name>`: created when absent, opened when it exists.
    fn named_event(name: &str) -> Result<Owned, SignalError> {
        let wide = event_name(name);
        // SAFETY: `wide` is NUL-terminated and lives for the call; null attributes mean defaults.
        let handle = unsafe { CreateEventW(std::ptr::null_mut(), 0, 0, wide.as_ptr()) };
        if handle.is_null() {
            // SAFETY: reads the calling thread's last-error value.
            let code = unsafe { GetLastError() };
            return Err(SignalError::Os(format!(
                "CreateEventW({name}) failed with error {code}"
            )));
        }
        Ok(Owned(handle as usize))
    }

    pub fn post(name: &str) -> Result<(), SignalError> {
        let event = named_event(name)?;
        // SAFETY: `event` holds a live event handle. A waiting watcher keeps the event alive
        // after this handle closes; with no watcher the signal is lost, by contract.
        let ok = unsafe { SetEvent(event.handle()) };
        if ok != 0 {
            Ok(())
        } else {
            // SAFETY: reads the calling thread's last-error value.
            let code = unsafe { GetLastError() };
            Err(SignalError::Os(format!(
                "SetEvent({name}) failed with error {code}"
            )))
        }
    }

    /// Stops a watcher by setting its private manual-reset event.
    pub struct StopHandle(Owned);

    impl StopHandle {
        pub fn signal(&self) {
            // SAFETY: the stop event is a live handle owned by this value.
            unsafe {
                SetEvent(self.0.handle());
            }
        }
    }

    pub fn watch(
        names: Vec<String>,
        on_signal: Handler,
    ) -> Result<(StopHandle, JoinHandle<()>), SignalError> {
        let mut events: Vec<Owned> = Vec::with_capacity(names.len());
        for name in &names {
            events.push(named_event(name)?);
        }
        // SAFETY: an unnamed manual-reset event, initially not signalled.
        let stop_event = unsafe { CreateEventW(std::ptr::null_mut(), 1, 0, std::ptr::null()) };
        if stop_event.is_null() {
            // SAFETY: reads the calling thread's last-error value.
            let code = unsafe { GetLastError() };
            return Err(SignalError::Os(format!(
                "CreateEventW(stop) failed with error {code}"
            )));
        }
        // The `StopHandle` owns the stop event and closes it when the `Watcher` drops, which is
        // after the thread was joined; the thread only waits on the raw handle value.
        let stop_raw = stop_event as usize;
        let stop = StopHandle(Owned(stop_raw));
        let thread = std::thread::Builder::new()
            .name("rightkit-signal-watch".into())
            .spawn(move || {
                let mut handles: Vec<Handle> = events.iter().map(Owned::handle).collect();
                handles.push(stop_raw as Handle);
                let stop_index = events.len() as u32;
                loop {
                    // SAFETY: `handles` holds `len` live handles that this thread owns.
                    let woke = unsafe {
                        WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE)
                    };
                    if woke == WAIT_OBJECT_0 + stop_index {
                        break;
                    }
                    if woke < stop_index {
                        let i = (woke - WAIT_OBJECT_0) as usize;
                        // A manual-reset event another party created stays signalled: reset it
                        // so the wait does not spin. An auto-reset one is already clear.
                        // SAFETY: a live handle owned by this thread.
                        unsafe {
                            ResetEvent(handles[i]);
                        }
                        on_signal(i, &names[i]);
                    } else {
                        // WAIT_FAILED or an abandoned handle: nothing more can arrive.
                        break;
                    }
                }
                drop(events);
            })
            .map_err(|e| SignalError::Os(format!("spawn watcher thread: {e}")))?;
        Ok((stop, thread))
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::{Handler, SignalError};
    use std::thread::JoinHandle;

    pub const SUPPORTED: bool = false;

    pub fn post(_name: &str) -> Result<(), SignalError> {
        Err(SignalError::Unsupported)
    }

    pub struct StopHandle;

    impl StopHandle {
        pub fn signal(&self) {}
    }

    pub fn watch(
        _names: Vec<String>,
        _on_signal: Handler,
    ) -> Result<(StopHandle, JoinHandle<()>), SignalError> {
        Err(SignalError::Unsupported)
    }
}
