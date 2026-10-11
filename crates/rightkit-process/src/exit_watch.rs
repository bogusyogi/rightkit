//! Event-driven observation of a process exit, so nothing in this crate sleeps in a loop to find
//! out whether a child is still running.
//!
//! An [`ExitWatch`] owns one kernel object that becomes readable when the process exits:
//!
//! * **macOS / iOS:** a kqueue with an `EVFILT_PROC` / `NOTE_EXIT` knote registered at spawn. The
//!   knote is bound to the process, not to the pid number, so a later reuse of the number can
//!   neither satisfy nor block it. The event is never drained: the kqueue stays readable, so every
//!   waiter (the owner, each `ProcessHandle` clone) wakes. Registering on a process that is
//!   already a zombie answers `ESRCH` (checked on Darwin 27: the receipt carries `EV_ERROR`,
//!   data `ESRCH`, and no exit event is queued); that is recorded as "already exited".
//! * **Linux / Android:** a pidfd (`pidfd_open`), also bound to the process. It is readable once
//!   the process has exited, and it allows `pidfd_send_signal` and an exact liveness proof
//!   ([`ExitWatch::exact_proof`]) that does not depend on the pid number at all.
//! * **Fallback** (kernel without pidfd, or a failed kqueue): one thread blocks in
//!   `waitid(P_PID, pid, WEXITED | WNOWAIT)`, which does not reap, and closes a pipe when it
//!   returns. This names the pid number for as long as it blocks. If other code collected the
//!   child and the kernel gave the number to a new child, the thread would wait on that child and
//!   report its exit; the identity proof that follows every wake-up (`UnixChild::prove`) still
//!   refuses to act on the replacement, so the cost is a wake-up that is late or spurious, never
//!   a signal or a reap of the wrong process.
//! * **Anything else:** no event source. Bounded waits return `ErrorKind::Unsupported`; an
//!   unbounded wait is a blocking reap, as in `std`.
//!
//! The exit is reported to waiters through `poll(2)`, so a wait costs one wake-up per exit (plus
//! signal interruptions), however long the child lives.

use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[derive(Debug)]
enum Source {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    Kqueue(OwnedFd),
    #[cfg(any(target_os = "linux", target_os = "android"))]
    Pidfd(OwnedFd),
    /// Closed by the watcher thread when `waitid` returns.
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))]
    Thread(OwnedFd),
    /// Exited (or not there) when the watch was opened.
    Gone,
    Unsupported,
}

#[derive(Debug)]
pub(crate) struct ExitWatch {
    source: Source,
    fired: AtomicBool,
    /// Test only: how many times a wait returned from the kernel.
    #[cfg(test)]
    wakeups: std::sync::atomic::AtomicUsize,
}

impl ExitWatch {
    /// Opens the watch for `pid`. `own_child` allows the thread fallback, which only makes
    /// sense for a child of this process (`waitid` on any other process answers `ECHILD`).
    pub(crate) fn open(pid: u32, own_child: bool) -> Self {
        let (source, fired) = open_source(pid, own_child);
        Self {
            source,
            fired: AtomicBool::new(fired),
            #[cfg(test)]
            wakeups: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// A watch with no event source, for tests.
    #[cfg(test)]
    pub(crate) fn unsupported() -> Self {
        Self {
            source: Source::Unsupported,
            fired: AtomicBool::new(false),
            wakeups: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    #[cfg(test)]
    pub(crate) fn wakeups(&self) -> usize {
        self.wakeups.load(Ordering::SeqCst)
    }

    pub(crate) fn is_supported(&self) -> bool {
        !matches!(self.source, Source::Unsupported)
    }

    /// True when this watch can say, without reference to the pid number, whether the
    /// original process still exists (Linux pidfd).
    pub(crate) fn proves_exactly(&self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        if matches!(self.source, Source::Pidfd(_)) {
            return true;
        }
        false
    }

    /// `Some(Ok(true))`: the original process still exists (running, or exited and unreaped).
    /// `Some(Ok(false))`: it is gone. `None`: this watch cannot tell.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn exact_proof(&self) -> Option<io::Result<bool>> {
        match self.send_signal(0)? {
            Ok(()) => Some(Ok(true)),
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Some(Ok(false)),
            Err(error) => Some(Err(error)),
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    pub(crate) fn exact_proof(&self) -> Option<io::Result<bool>> {
        None
    }

    /// Sends `signal` to the original process through its pidfd: no pid number is involved, so
    /// no recycled number can receive it. `None` when this watch has no pidfd.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn send_signal(&self, signal: i32) -> Option<io::Result<()>> {
        let Source::Pidfd(fd) = &self.source else {
            return None;
        };
        loop {
            // SAFETY: pidfd_send_signal(pidfd, sig, NULL info, 0 flags) takes no pointers here.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd() as libc::c_long,
                    signal as libc::c_long,
                    0 as libc::c_long,
                    0 as libc::c_long,
                )
            };
            if result == 0 {
                return Some(Ok(()));
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                return Some(Err(error));
            }
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    pub(crate) fn send_signal(&self, _signal: i32) -> Option<io::Result<()>> {
        None
    }

    fn raw_fd(&self) -> Option<RawFd> {
        match &self.source {
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            Source::Kqueue(fd) => Some(fd.as_raw_fd()),
            #[cfg(any(target_os = "linux", target_os = "android"))]
            Source::Pidfd(fd) => Some(fd.as_raw_fd()),
            #[cfg(any(
                target_os = "linux",
                target_os = "android",
                target_os = "macos",
                target_os = "ios"
            ))]
            Source::Thread(fd) => Some(fd.as_raw_fd()),
            Source::Gone | Source::Unsupported => None,
        }
    }

    /// Blocks until the process has exited (`Ok(true)`) or `timeout` has passed (`Ok(false)`);
    /// `None` waits without limit. Holds no lock and uses no timer: the thread sleeps in
    /// `poll(2)` until the kernel reports the exit. `Err(Unsupported)` when there is no event
    /// source.
    pub(crate) fn wait(&self, timeout: Option<Duration>) -> io::Result<bool> {
        if self.fired.load(Ordering::Acquire) {
            return Ok(true);
        }
        let fd = match self.raw_fd() {
            Some(fd) => fd,
            None if matches!(self.source, Source::Gone) => return Ok(true),
            None => return Err(io::Error::from(io::ErrorKind::Unsupported)),
        };
        let deadline = timeout.and_then(|limit| Instant::now().checked_add(limit));
        loop {
            let mut entry = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd, as counted.
            let ready = unsafe { libc::poll(&mut entry, 1, poll_millis(deadline)) };
            #[cfg(test)]
            self.wakeups.fetch_add(1, Ordering::SeqCst);
            if ready > 0 {
                self.fired.store(true, Ordering::Release);
                return Ok(true);
            }
            if ready == 0 {
                return Ok(false); // poll's own timeout ran out
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                return Err(error);
            }
            if let Some(deadline) = deadline {
                if Instant::now() >= deadline {
                    return Ok(false);
                }
            }
        }
    }

    /// Blocks until every watch has fired (`Ok(true)`) or `timeout` has passed (`Ok(false)`).
    /// One `poll(2)` over all the descriptors; no timer.
    pub(crate) fn wait_all(watches: &[ExitWatch], timeout: Duration) -> io::Result<bool> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            let mut entries = Vec::with_capacity(watches.len());
            let mut pending = Vec::with_capacity(watches.len());
            for (index, watch) in watches.iter().enumerate() {
                if watch.fired.load(Ordering::Acquire) {
                    continue;
                }
                match watch.raw_fd() {
                    Some(fd) => {
                        entries.push(libc::pollfd {
                            fd,
                            events: libc::POLLIN,
                            revents: 0,
                        });
                        pending.push(index);
                    }
                    None if matches!(watch.source, Source::Gone) => {}
                    None => return Err(io::Error::from(io::ErrorKind::Unsupported)),
                }
            }
            if entries.is_empty() {
                return Ok(true);
            }
            // SAFETY: `entries` holds `len` valid pollfd records.
            let ready = unsafe {
                libc::poll(
                    entries.as_mut_ptr(),
                    entries.len() as libc::nfds_t,
                    poll_millis(deadline),
                )
            };
            if ready > 0 {
                for (entry, index) in entries.iter().zip(&pending) {
                    if entry.revents != 0 {
                        watches[*index].fired.store(true, Ordering::Release);
                    }
                }
            } else if ready == 0 {
                return Ok(false); // poll's own timeout ran out
            } else {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EINTR) {
                    return Err(error);
                }
                if deadline.is_some_and(|at| Instant::now() >= at) {
                    return Ok(false);
                }
            }
        }
    }
}

/// Milliseconds until `deadline`, rounded up; `-1` (no limit) when there is none.
fn poll_millis(deadline: Option<Instant>) -> libc::c_int {
    let Some(deadline) = deadline else {
        return -1;
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    let millis = remaining.as_millis() + u128::from(remaining.subsec_nanos() % 1_000_000 != 0);
    millis.min(libc::c_int::MAX as u128) as libc::c_int
}

fn set_cloexec(fd: RawFd) {
    // SAFETY: fcntl on a descriptor this module owns.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn open_source(pid: u32, own_child: bool) -> (Source, bool) {
    match kqueue_source(pid) {
        Ok(Some(fd)) => (Source::Kqueue(fd), false),
        Ok(None) => (Source::Gone, true),
        Err(_) if own_child => thread_source(pid),
        Err(_) => (Source::Unsupported, false),
    }
}

/// `Ok(None)` when the process had already exited (`ESRCH` in the receipt).
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn kqueue_source(pid: u32) -> io::Result<Option<OwnedFd>> {
    // SAFETY: kqueue takes no arguments.
    let raw = unsafe { libc::kqueue() };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a fresh descriptor owned by nothing else.
    let kq = unsafe { OwnedFd::from_raw_fd(raw) };
    set_cloexec(raw);
    let change = libc::kevent {
        ident: pid as usize,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_RECEIPT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: an all-zero kevent is a valid value for every field.
    let mut receipt: libc::kevent = unsafe { std::mem::zeroed() };
    let no_wait = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: one change and one receipt, as counted; the timeout is a valid timespec.
    let count = unsafe { libc::kevent(raw, &change, 1, &mut receipt, 1, &no_wait) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    if count != 1 {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    if (receipt.flags & libc::EV_ERROR) != 0 && receipt.data != 0 {
        if receipt.data == libc::ESRCH as isize {
            return Ok(None);
        }
        return Err(io::Error::from_raw_os_error(receipt.data as i32));
    }
    Ok(Some(kq))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_source(pid: u32, own_child: bool) -> (Source, bool) {
    // SAFETY: pidfd_open(pid, 0) takes no pointers. The descriptor is close-on-exec.
    let raw =
        unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::c_long, 0 as libc::c_long) };
    if raw >= 0 {
        // SAFETY: a fresh descriptor owned by nothing else.
        return (
            Source::Pidfd(unsafe { OwnedFd::from_raw_fd(raw as RawFd) }),
            false,
        );
    }
    if io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        return (Source::Gone, true);
    }
    if own_child {
        // Kernel without pidfd (ENOSYS), or one that a sandbox filters (EPERM): thread fallback.
        return thread_source(pid);
    }
    (Source::Unsupported, false)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
fn open_source(_pid: u32, _own_child: bool) -> (Source, bool) {
    (Source::Unsupported, false)
}

/// The fallback: a thread blocks in `waitid(WNOWAIT)` (which does not reap) and closes the write
/// end of a pipe when it returns, which makes the read end readable (`POLLHUP`).
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
fn thread_source(pid: u32) -> (Source, bool) {
    let mut ends: [RawFd; 2] = [-1; 2];
    // SAFETY: pipe writes two descriptors into the array.
    if unsafe { libc::pipe(ends.as_mut_ptr()) } != 0 {
        return (Source::Unsupported, false);
    }
    set_cloexec(ends[0]);
    set_cloexec(ends[1]);
    // SAFETY: two fresh descriptors, each owned by nothing else.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
    let spawned = std::thread::Builder::new()
        .name("rightkit-exit-watch".into())
        .spawn(move || {
            loop {
                // SAFETY: an all-zero siginfo_t is valid, and waitid fills it.
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                // SAFETY: valid out pointer; WNOWAIT leaves the zombie in place.
                let result = unsafe {
                    libc::waitid(
                        libc::P_PID,
                        pid as libc::id_t,
                        &mut info,
                        libc::WEXITED | libc::WNOWAIT,
                    )
                };
                if result == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    break;
                }
            }
            drop(write);
        });
    match spawned {
        Ok(_) => (Source::Thread(read), false),
        Err(_) => (Source::Unsupported, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn child(script: &str) -> std::process::Child {
        Command::new("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn")
    }

    #[test]
    fn a_running_child_times_out_once_and_then_fires_on_exit() {
        let mut process = child("exec sleep 0.4");
        let watch = ExitWatch::open(process.id(), true);
        assert!(watch.is_supported());
        assert!(!watch.wait(Some(Duration::from_millis(50))).expect("wait"));
        assert!(watch.wait(Some(Duration::from_secs(10))).expect("wait"));
        // Fired for good: answers at once, with no further system call.
        let before = watch.wakeups();
        assert!(watch.wait(None).expect("wait"));
        assert_eq!(watch.wakeups(), before);
        process.wait().expect("reap");
    }

    #[test]
    fn a_watch_opened_after_the_exit_reports_it() {
        let mut process = child("exit 0");
        // The child is a zombie by now; it is still unreaped, so its number is still ours.
        std::thread::sleep(Duration::from_millis(300));
        let watch = ExitWatch::open(process.id(), true);
        assert!(watch.wait(Some(Duration::from_secs(5))).expect("wait"));
        process.wait().expect("reap");
    }

    #[test]
    fn a_watch_survives_the_reap_and_still_reports_the_exit_to_a_late_waiter() {
        let mut process = child("exit 0");
        let watch = ExitWatch::open(process.id(), true);
        process.wait().expect("reap");
        assert!(watch.wait(Some(Duration::from_secs(5))).expect("wait"));
    }

    #[test]
    fn wait_all_waits_for_every_watch() {
        let mut slow = child("exec sleep 0.3");
        let mut fast = child("exit 0");
        let watches = [
            ExitWatch::open(slow.id(), true),
            ExitWatch::open(fast.id(), true),
        ];
        assert!(!ExitWatch::wait_all(&watches, Duration::from_millis(20)).expect("wait"));
        assert!(ExitWatch::wait_all(&watches, Duration::from_secs(10)).expect("wait"));
        slow.wait().expect("reap");
        fast.wait().expect("reap");
    }

    #[test]
    fn an_unsupported_watch_refuses_bounded_waits() {
        let watch = ExitWatch::unsupported();
        let error = watch.wait(Some(Duration::from_millis(1))).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn a_pidfd_proves_the_original_process_and_signals_it_without_the_number() {
        let mut process = child("exec sleep 600");
        let watch = ExitWatch::open(process.id(), true);
        if !watch.proves_exactly() {
            return; // kernel without pidfd: the thread fallback is in use
        }
        assert!(matches!(watch.exact_proof(), Some(Ok(true))));
        watch
            .send_signal(libc::SIGKILL)
            .expect("pidfd")
            .expect("signal");
        // Exited but unreaped: still the same process.
        assert!(watch.wait(Some(Duration::from_secs(10))).expect("wait"));
        assert!(matches!(watch.exact_proof(), Some(Ok(true))));
        process.wait().expect("reap");
        assert!(matches!(watch.exact_proof(), Some(Ok(false))));
    }
}
