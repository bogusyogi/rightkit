//! A wake-up hint for waiters. It is never a verdict.
//!
//! Every wait in this crate is a loop of non-blocking looks (`try_wait`, or the non-reaping
//! `waitid(WNOHANG | WNOWAIT)` probe), each taken under the child's lock and released at once.
//! Between two looks the thread sleeps. An [`ExitWatch`] only decides **how** it sleeps:
//!
//! * **With an event source** it sleeps in `poll(2)` on a descriptor the kernel makes readable
//!   when the process exits, so a long-lived child costs one wake-up per second at most
//!   instead of one per interval. The sleep never outlasts the caller's limit, and never
//!   outlasts [`EVENT_SLICE`] either, so a missing or early event cannot stall a waiter: the
//!   next look finds out for itself.
//!   * macOS / iOS: a kqueue with an `EVFILT_PROC` / `NOTE_EXIT` knote registered at spawn.
//!   * Linux / Android: a pidfd (`pidfd_open`).
//! * **Without one** (no such kernel facility, a process out of descriptors, or a process that
//!   was already gone when the watch was opened) it sleeps [`POLL_INTERVAL`], the interval the
//!   published 0.3.3 used, and the caller looks again. Never an unbounded block, never a
//!   busy loop.
//!
//! What the event does **not** mean: the kernel posts `NOTE_EXIT` before the process becomes
//! waitable, and a registration against a process that is already exiting answers `ESRCH`
//! although `wait` would still block. So the event is only ever a reason to look sooner. No
//! reap, no signal and no decision is made from it. Once it has fired a waiter that still finds
//! the child unwaitable sleeps [`POLL_INTERVAL`] between looks.
//!
//! The watch is bound to the process, not to its pid number, and is closed with the last
//! clone of the child.

use std::{
    io,
    os::fd::RawFd,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// The pause between two looks when no event can wake a waiter: the interval the published
/// 0.3.3 used for `wait_timeout`.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// The longest one sleep on the event lasts. The waiter looks again after it, whatever the
/// event did, so the event is a hint that can only save wake-ups, never lose an exit.
pub(crate) const EVENT_SLICE: Duration = Duration::from_secs(1);

#[derive(Debug)]
enum Source {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    Kqueue(OwnedFd),
    #[cfg(any(target_os = "linux", target_os = "android"))]
    Pidfd(OwnedFd),
    /// No event source: waiters poll.
    None,
}

#[derive(Debug)]
pub(crate) struct ExitWatch {
    source: Source,
    /// The event has been seen. It stays set: the process is exiting or gone, so the event can
    /// say nothing more, and further sleeps are plain intervals.
    fired: AtomicBool,
}

impl ExitWatch {
    /// Opens the watch for `pid`, a child of this process that has not been reaped. Any failure
    /// (no facility, no descriptor, the process already exiting) yields a watch with no event
    /// source, which waiters poll.
    pub(crate) fn open(pid: u32) -> Self {
        Self {
            source: open_source(pid),
            fired: AtomicBool::new(false),
        }
    }

    /// A watch with no event source, for tests.
    #[cfg(test)]
    pub(crate) fn unsupported() -> Self {
        Self {
            source: Source::None,
            fired: AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    pub(crate) fn has_event_source(&self) -> bool {
        self.raw_fd().is_some()
    }

    fn raw_fd(&self) -> Option<RawFd> {
        match &self.source {
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            Source::Kqueue(fd) => Some(fd.as_raw_fd()),
            #[cfg(any(target_os = "linux", target_os = "android"))]
            Source::Pidfd(fd) => Some(fd.as_raw_fd()),
            Source::None => None,
        }
    }

    /// Sleeps for at most `limit` and returns. With an unfired event source it sleeps on the
    /// event (at most [`EVENT_SLICE`]); otherwise for [`POLL_INTERVAL`] (or `limit`, if
    /// shorter). It reports nothing: the caller looks again.
    pub(crate) fn pause(&self, limit: Duration) {
        if limit.is_zero() {
            return;
        }
        if !self.fired.load(Ordering::Acquire) {
            if let Some(fd) = self.raw_fd() {
                match wait_readable(fd, limit.min(EVENT_SLICE)) {
                    Ok(true) => {
                        self.fired.store(true, Ordering::Release);
                        return;
                    }
                    // Slept the whole slice, or a signal ended the sleep early.
                    Ok(false) => return,
                    // The descriptor is unusable: fall through to the plain interval.
                    Err(_) => {}
                }
            }
        }
        thread::sleep(limit.min(POLL_INTERVAL));
    }

    /// Calls `look` until it reports something (`Ok(Some)`) or fails, or until `deadline`
    /// passes (`Ok(None)`); `None` waits without limit. `look` is called first, then after
    /// every [`pause`](Self::pause), and once more after the last pause, so a result that
    /// arrives at the deadline is returned. The pause is never longer than the time left, so
    /// the call returns within one scheduler wake-up of the deadline whatever the event did.
    pub(crate) fn wait_for<T>(
        &self,
        deadline: Option<Instant>,
        mut look: impl FnMut() -> io::Result<Option<T>>,
    ) -> io::Result<Option<T>> {
        loop {
            if let Some(found) = look()? {
                return Ok(Some(found));
            }
            let limit = match deadline {
                Some(at) => at.saturating_duration_since(Instant::now()),
                None => EVENT_SLICE,
            };
            if limit.is_zero() {
                return Ok(None);
            }
            self.pause(limit);
        }
    }
}

/// Sleeps in `poll(2)` until `fd` is readable (`Ok(true)`), `limit` has passed or a signal
/// interrupted the sleep (`Ok(false)`).
fn wait_readable(fd: RawFd, limit: Duration) -> io::Result<bool> {
    // Round up so a sub-millisecond remainder does not turn into a zero-length poll.
    let millis = limit.as_millis() + u128::from(!limit.subsec_nanos().is_multiple_of(1_000_000));
    let millis = millis.min(libc::c_int::MAX as u128) as libc::c_int;
    let mut entry = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd, as counted.
    let ready = unsafe { libc::poll(&mut entry, 1, millis) };
    if ready > 0 {
        return Ok(true);
    }
    if ready == 0 {
        return Ok(false);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EINTR) {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
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
fn open_source(pid: u32) -> Source {
    // An `ESRCH` receipt means the process is already exiting or gone. That says nothing about
    // whether `wait` would block (XNU hides a process from registration before it is waitable),
    // so it is not an exit: the watch simply has no event, and waiters poll.
    kqueue_source(pid).map_or(Source::None, Source::Kqueue)
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn kqueue_source(pid: u32) -> io::Result<OwnedFd> {
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
        return Err(io::Error::from_raw_os_error(receipt.data as i32));
    }
    Ok(kq)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_source(pid: u32) -> Source {
    // SAFETY: pidfd_open(pid, 0) takes no pointers. The descriptor is close-on-exec.
    let raw =
        unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::c_long, 0 as libc::c_long) };
    if raw >= 0 {
        // SAFETY: a fresh descriptor owned by nothing else.
        return Source::Pidfd(unsafe { OwnedFd::from_raw_fd(raw as RawFd) });
    }
    // Kernel without pidfd (ENOSYS), one a sandbox filters (EPERM), no descriptor left, or the
    // process already gone: no event, waiters poll.
    Source::None
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
fn open_source(_pid: u32) -> Source {
    Source::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::sync::atomic::AtomicUsize;

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
    fn a_pause_on_a_running_child_ends_at_its_limit_not_after_it() {
        let mut process = child("exec sleep 600");
        let watch = ExitWatch::open(process.id());
        let started = Instant::now();
        watch.pause(Duration::from_millis(150));
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
        if watch.has_event_source() {
            assert!(elapsed >= Duration::from_millis(100), "{elapsed:?}");
        }
        process.kill().expect("kill");
        process.wait().expect("reap");
    }

    #[test]
    fn an_exit_wakes_a_long_pause_early() {
        let mut process = child("exec sleep 0.3");
        let watch = ExitWatch::open(process.id());
        let started = Instant::now();
        watch.pause(Duration::from_secs(30));
        assert!(started.elapsed() < Duration::from_secs(10));
        process.wait().expect("reap");
    }

    #[test]
    fn a_watch_opened_after_the_exit_still_pauses_for_a_bounded_time() {
        let mut process = child("exit 0");
        // A zombie by now. macOS registers nothing for it; Linux gives a pidfd that is already
        // readable. Neither may make a pause block.
        std::thread::sleep(Duration::from_millis(300));
        let watch = ExitWatch::open(process.id());
        let started = Instant::now();
        watch.pause(Duration::from_secs(30));
        watch.pause(Duration::from_secs(30));
        assert!(started.elapsed() < Duration::from_secs(5));
        process.wait().expect("reap");
    }

    /// The fallback: no event source. A pause is the polling interval, not the limit.
    #[test]
    fn a_watch_without_an_event_source_pauses_one_interval_not_the_limit() {
        let watch = ExitWatch::unsupported();
        assert!(!watch.has_event_source());
        let started = Instant::now();
        watch.pause(Duration::from_secs(60));
        assert!(started.elapsed() < Duration::from_secs(2));
        let started = Instant::now();
        watch.pause(Duration::ZERO);
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    /// The fallback loop keeps the caller's deadline and neither spins nor sleeps it away.
    #[test]
    fn the_fallback_loop_honours_the_deadline_and_does_not_spin() {
        let watch = ExitWatch::unsupported();
        let looks = AtomicUsize::new(0);
        let started = Instant::now();
        let found: Option<()> = watch
            .wait_for(Some(started + Duration::from_millis(200)), || {
                looks.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            })
            .expect("wait_for");
        let elapsed = started.elapsed();
        assert!(found.is_none());
        assert!(
            elapsed >= Duration::from_millis(200) && elapsed < Duration::from_secs(5),
            "{elapsed:?}"
        );
        // One look per 5 ms is about 40 in 200 ms. A spin would be thousands; fewer than three
        // would mean the loop slept the deadline away in one go.
        let looks = looks.load(Ordering::SeqCst);
        assert!((3..=60).contains(&looks), "{looks} looks");
    }

    #[test]
    fn a_deadline_already_passed_looks_once_and_returns() {
        let watch = ExitWatch::unsupported();
        let looks = AtomicUsize::new(0);
        let found: Option<()> = watch
            .wait_for(Some(Instant::now()), || {
                looks.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            })
            .expect("wait_for");
        assert!(found.is_none());
        assert_eq!(looks.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_unbounded_wait_returns_as_soon_as_a_look_reports() {
        let watch = ExitWatch::unsupported();
        let looks = AtomicUsize::new(0);
        let started = Instant::now();
        let found = watch
            .wait_for(None, || {
                Ok((looks.fetch_add(1, Ordering::SeqCst) == 2).then_some("done"))
            })
            .expect("wait_for");
        assert_eq!(found, Some("done"));
        assert_eq!(looks.load(Ordering::SeqCst), 3);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_failing_look_ends_the_wait_with_its_error() {
        let watch = ExitWatch::unsupported();
        let error = watch
            .wait_for(None, || -> io::Result<Option<()>> {
                Err(io::Error::from_raw_os_error(libc::ECHILD))
            })
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
    }

    /// With the real source: the loop finds the exit, early, and the child stays reapable.
    #[test]
    fn a_wait_on_a_real_child_returns_its_status_when_it_exits() {
        let mut process = child("exec sleep 0.3");
        let watch = ExitWatch::open(process.id());
        let started = Instant::now();
        let status = watch
            .wait_for(Some(started + Duration::from_secs(30)), || {
                process.try_wait()
            })
            .expect("wait_for")
            .expect("the child exits within 30 s");
        assert!(status.success());
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
