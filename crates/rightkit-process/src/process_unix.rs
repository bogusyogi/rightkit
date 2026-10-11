use std::{io, os::fd::RawFd};

#[cfg(target_os = "macos")]
use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    time::Duration,
};

pub(crate) struct Inheritance {
    allowed: Vec<RawFd>,
    upper_bound: RawFd,
}

impl Inheritance {
    pub(crate) fn new(mut allowed: Vec<RawFd>) -> io::Result<Self> {
        if allowed.iter().any(|fd| *fd < 0) {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        allowed.sort_unstable();
        allowed.dedup();
        for &fd in &allowed {
            if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self {
            allowed,
            upper_bound: fd_upper_bound()?,
        })
    }

    // Called after fork: no allocation, directory traversal, locks or destructors.
    pub(crate) fn apply(&self) -> io::Result<()> {
        let mut first = 3u32;
        for &fd in self.allowed.iter().filter(|fd| **fd >= 3) {
            self.exclude(first, fd as u32 - 1)?;
            first = fd as u32 + 1;
        }
        self.exclude(first, u32::MAX)?;
        for &fd in self.allowed.iter().filter(|fd| **fd >= 3) {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    fn exclude(&self, first: u32, last: u32) -> io::Result<()> {
        if first > last {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        {
            // Keep std's CLOEXEC exec-error pipe alive until exec: closing it
            // in pre_exec would make failed exec look like successful spawn.
            // CLOSE_RANGE_CLOEXEC excludes every unwanted fd atomically at exec.
            const CLOSE_RANGE_CLOEXEC: libc::c_uint = 4;
            if unsafe { libc::syscall(libc::SYS_close_range, first, last, CLOSE_RANGE_CLOEXEC) }
                == 0
            {
                return Ok(());
            }
        }
        let end = last.min(self.upper_bound.saturating_sub(1) as u32);
        for fd in first..=end {
            let fd = fd as RawFd;
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EBADF) {
                    return Err(error);
                }
            } else if flags & libc::FD_CLOEXEC == 0 {
                // All preexisting CLOEXEC fds (including std's error pipe)
                // already disappear at exec; only inheritable fds need close.
                // Even if close is interrupted on a host that leaves fd open,
                // CLOEXEC guarantees it cannot escape into the executed child.
                if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
                    return Err(io::Error::last_os_error());
                }
                unsafe {
                    libc::close(fd);
                }
            }
        }
        Ok(())
    }
}

/// One past the highest descriptor that may be open in this process. Includes
/// descriptors above a lowered soft limit, so every inherited fd can be closed.
pub(crate) fn fd_upper_bound() -> io::Result<RawFd> {
    let maximum = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
    if maximum < 0 {
        return Err(io::Error::last_os_error());
    }
    let highest = ["/proc/self/fd", "/dev/fd"]
        .into_iter()
        .find_map(|path| {
            std::fs::read_dir(path).ok().map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter_map(|entry| entry.file_name().to_str()?.parse::<RawFd>().ok())
                    .max()
                    .unwrap_or(2)
            })
        })
        .unwrap_or(2);
    let upper_bound = maximum.min(i64::from(i32::MAX) as libc::c_long) as RawFd;
    Ok(upper_bound.max(highest.saturating_add(1)))
}

/// Parent-death tie to the supervising process, enabled by `kill_on_parent_exit(true)`.
///
/// Three parts, because the child-side hook runs between `fork` and `exec` and must stay
/// async-signal-safe:
///
/// * [`ParentDeathTie::prepare`] runs on the spawning thread, before the process is spawned.
///   It may allocate. On macOS it forks the helper here, from the parent, so the helper is
///   never created by a second `fork` inside the new process.
/// * [`EngineTie::apply_in_child`] runs in the new process between `fork` and `exec`.
/// * [`ParentDeathTie::settle`] runs on the spawning thread once spawning is over.
pub(crate) struct ParentDeathTie {
    supervisor: libc::pid_t,
    #[cfg(target_os = "macos")]
    helper: Option<HelperLink>,
    #[cfg(target_os = "macos")]
    ack_timeout: Duration,
}

/// What the new process needs, copied into the `pre_exec` closure.
#[derive(Clone, Copy)]
pub(crate) struct EngineTie {
    supervisor: libc::pid_t,
    #[cfg(target_os = "macos")]
    hello_write: RawFd,
    #[cfg(target_os = "macos")]
    ack_read: RawFd,
    #[cfg(target_os = "macos")]
    ack_timeout: Duration,
}

/// Where no helper process exists, so code shared by every Unix host names one type. It has no
/// values.
#[cfg(not(target_os = "macos"))]
#[derive(Debug)]
pub(crate) enum Helper {}

#[cfg(not(target_os = "macos"))]
impl Helper {
    pub(crate) fn pid(&self) -> libc::pid_t {
        match *self {}
    }

    pub(crate) fn has_gone(&self) -> bool {
        match *self {}
    }

    pub(crate) fn retire(self) {
        match self {}
    }
}

#[cfg(not(target_os = "macos"))]
impl ParentDeathTie {
    pub(crate) fn prepare(_session: bool) -> io::Result<Self> {
        Ok(Self {
            // SAFETY: getpid has no preconditions.
            supervisor: unsafe { libc::getpid() },
        })
    }

    pub(crate) fn engine_side(&self) -> EngineTie {
        EngineTie {
            supervisor: self.supervisor,
        }
    }

    /// No helper process exists on this platform.
    pub(crate) fn settle(self, _spawned: bool) -> Option<Helper> {
        None
    }
}

/// Linux: `PR_SET_PDEATHSIG(SIGTERM)` on the direct child. The kernel fires it
/// when the forking thread exits, and it targets only that child.
///
/// This is a weaker guarantee than the macOS helper:
/// - it signals the direct child only, never the rest of its process group, so
///   grandchildren the child started survive unless the child cleans them up;
/// - it is a catchable `SIGTERM`, so a child that ignores or handles it can keep
///   running, and there is no `SIGKILL` escalation;
/// - the kernel keys it to the forking *thread*, so a supervisor that spawns from
///   a short-lived thread terminates its children when that thread ends.
#[cfg(target_os = "linux")]
impl EngineTie {
    pub(crate) fn apply_in_child(&self) -> io::Result<()> {
        // SAFETY: PR_SET_PDEATHSIG takes a signal number and no pointers.
        if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // The supervisor may have died between fork and prctl. The death signal
        // would then never arrive, so fail the spawn rather than run untied.
        // SAFETY: getppid has no preconditions.
        if unsafe { libc::getppid() } != self.supervisor {
            return Err(io::Error::from_raw_os_error(libc::ESRCH));
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl EngineTie {
    /// Other Unix hosts have no parent-death primitive here, so the spawn fails.
    pub(crate) fn apply_in_child(&self) -> io::Result<()> {
        let _ = self.supervisor;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

// ---------------------------------------------------------------------------------------
// macOS helper
// ---------------------------------------------------------------------------------------
//
// A helper process, forked by the supervisor itself, watches the supervisor with
// `EVFILT_PROC`/`NOTE_EXIT`. When the supervisor dies it sends `SIGTERM` to the engine's
// process group, waits `PARENT_EXIT_GRACE`, then sends `SIGKILL` to the group.
//
// Sequence:
//  1. `prepare` (supervisor, spawning thread): create three pipes, `hello` (engine -> helper),
//     `ack` (helper -> engine) and `tether` (supervisor -> helper), then `fork` the helper.
//     The helper lists its descriptor table with the kernel and closes every descriptor but
//     its three pipe ends. If it cannot prove it closed them all it fails the spawn. Then it
//     blocks until the engine's pid arrives on `hello`.
//  2. std spawns the engine (its own fork/exec). In the engine, after `setpgid(0, 0)`,
//     `EngineTie::apply_in_child` writes the engine pid to `hello` and waits for `ack`. The
//     engine therefore runs no program code until the helper has armed.
//  3. The helper joins the engine's process group (`setpgid(0, engine)`), creates its kqueue
//     (kqueue descriptors do not survive `fork(2)`), registers the supervisor watch and a read
//     watch on `tether`, and writes `0` (armed) or an `errno` to `ack`. It then waits.
//  4. `settle` (supervisor): on success the `Helper` (pid and the `tether` write end) goes to
//     the `UnixChild`. On failure the helper is retired at once.
//
// Why the helper joins the group: a process-group id is not recycled while any process is
// in the group. The helper stays in the group for as long as it lives, so the later
// `kill(-engine, ..)` can only name this group.
//
// Why the fork is in the supervisor: a `fork` inside the new process re-runs the embedding's
// `pthread_atfork` handlers there. Handlers written for "child normally execs at once" (lock
// in prepare, unlock only in the parent handler) leave a lock held in the first forked
// process, and a second fork would block forever, before any timeout began.
//
// How long the tie lasts: the helper does not exit when the engine does. It lives as long
// as its owner `UnixChild` (the `OwnedChild` and its handle clones), so a supervisor that
// dies while an `OwnedChild` still exists also takes down descendants that outlived the
// engine. Dropping the last owner retires the helper; nothing is protected after that.
//
// Why the helper is never signalled by pid: it is a child of the supervisor, and a process in
// the embedding (`waitpid(-1)`) may reap it behind our back; its pid can then be recycled for an
// unrelated process, and a later `kill(pid, ..)` would hit that process. So the supervisor
// tells the helper to stand down over the `tether` pipe (one byte, `RELEASE`), and the helper
// exits by itself. The only calls that name the pid are `waitpid(pid, WNOHANG)`, which can only
// ever collect a zombie child, and on `ECHILD` the pid is forgotten.
//
// The tether must not be confused with the supervisor's death, which closes it too. Only the
// explicit byte is a release. End of file on the tether means the supervisor is gone or the
// channel is broken: the helper stops watching the tether and carries on waiting for the
// supervisor's `NOTE_EXIT`, which is the real trigger and is never lost.
//
// Not supported on macOS: `UnixContainment::Session`. `setsid` puts the engine in a new
// session, and a process may join a group only inside its own session, so the helper could
// not join and the group id would not be pinned. The spawn fails with `Unsupported`.

/// The byte the supervisor writes on the tether to tell the helper to stand down.
#[cfg(target_os = "macos")]
const RELEASE: u8 = b'R';

/// `F_SETNOSIGPIPE` from `<sys/fcntl.h>`: a write to a pipe with no reader returns `EPIPE`
/// instead of raising `SIGPIPE`, so releasing a helper that already exited cannot kill an
/// embedding that left `SIGPIPE` at its default action.
#[cfg(target_os = "macos")]
const F_SETNOSIGPIPE: libc::c_int = 73;

/// How long `Helper::retire` waits for the helper to exit and be collected before it hands the
/// collection to a background thread. The helper exits as soon as it reads the release byte.
#[cfg(target_os = "macos")]
const RETIRE_WAIT: Duration = Duration::from_secs(2);

/// How long that background thread keeps trying before it gives up and leaves a zombie.
#[cfg(target_os = "macos")]
const RETIRE_BACKGROUND_WAIT: Duration = Duration::from_secs(30);

/// The `errno` the helper reports when it cannot prove it closed every inherited descriptor.
/// The spawn fails with it (or with `EPIPE` when the engine writes after the helper exited).
#[cfg(target_os = "macos")]
pub(crate) const LISTING_FAILED: i32 = libc::EIO;

/// The supervisor's side of an armed parent-death helper, owned by the `UnixChild`.
///
/// The helper's pid is never signalled; see "Why the helper is never signalled by pid" above.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub(crate) struct Helper {
    pid: libc::pid_t,
    /// Write end of the private release channel. `None` once released.
    tether: Option<OwnedFd>,
}

#[cfg(target_os = "macos")]
impl Helper {
    pub(crate) fn pid(&self) -> libc::pid_t {
        self.pid
    }

    /// True once the helper is no longer this process's unreaped child: collected here, or
    /// collected by another reaper in this process (`ECHILD`). From then on its pid may belong
    /// to anyone, so the caller must forget it.
    pub(crate) fn has_gone(&self) -> bool {
        child_gone(self.pid)
    }

    /// Tells the helper to stand down: one byte on the tether, then the tether closes. Nothing
    /// is signalled. Safe when the helper already exited: the write fails with `EPIPE`.
    fn release(&mut self) {
        let Some(tether) = self.tether.take() else {
            return;
        };
        let fd = tether.as_raw_fd();
        // SAFETY: `fd` is the open write end that `tether` owns; the byte is a static value.
        unsafe {
            libc::fcntl(fd, F_SETNOSIGPIPE, 1);
            loop {
                let written = libc::write(fd, (&RELEASE as *const u8).cast(), 1);
                if written >= 0 || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    break;
                }
            }
        }
    }

    /// Releases the helper and collects its zombie. The helper exits at once on the release
    /// byte, so the collection normally finishes within a few milliseconds. If it has not
    /// after `RETIRE_WAIT`, a background thread keeps trying for a while longer.
    pub(crate) fn retire(mut self) {
        self.release();
        let pid = self.pid;
        if reap_within(pid, RETIRE_WAIT, Duration::from_millis(1)) {
            return;
        }
        let _ = std::thread::Builder::new()
            .name("rightkit-helper-reap".into())
            .spawn(move || {
                reap_within(pid, RETIRE_BACKGROUND_WAIT, Duration::from_millis(20));
            });
    }
}

/// `waitpid(pid, WNOHANG)`: true when `pid` was collected here, or is not this process's child
/// at all (`ECHILD`: another reaper took it). It only ever collects a zombie child, and it
/// never blocks.
#[cfg(target_os = "macos")]
fn child_gone(pid: libc::pid_t) -> bool {
    loop {
        let mut status = 0;
        // SAFETY: waitpid with WNOHANG and a valid out pointer.
        let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if reaped == pid {
            return true;
        }
        if reaped == 0 {
            return false;
        }
        match io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::ECHILD) => return true,
            _ => return false,
        }
    }
}

/// Polls `child_gone` every `step` for at most `limit`.
#[cfg(target_os = "macos")]
fn reap_within(pid: libc::pid_t, limit: Duration, step: Duration) -> bool {
    let started = std::time::Instant::now();
    loop {
        if child_gone(pid) {
            return true;
        }
        if started.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(step);
    }
}

#[cfg(target_os = "macos")]
struct HelperLink {
    pid: libc::pid_t,
    hello_write: RawFd,
    ack_read: RawFd,
    tether_write: RawFd,
}

#[cfg(target_os = "macos")]
impl HelperLink {
    /// Closes the two ends that only the spawn needed and keeps the tether.
    fn into_helper(self) -> Helper {
        close_all(&[self.hello_write, self.ack_read]);
        Helper {
            pid: self.pid,
            // SAFETY: `tether_write` was created by `prepare`, is open, and is owned by nothing
            // else once the link is consumed.
            tether: Some(unsafe { OwnedFd::from_raw_fd(self.tether_write) }),
        }
    }
}

/// How long the engine waits for the helper to confirm its watch. Closing descriptors,
/// creating the kqueue and registering take well under a second.
#[cfg(target_os = "macos")]
pub(crate) const HELPER_ACK_TIMEOUT: Duration = Duration::from_secs(5);

/// Grace between the parent-death `SIGTERM` and `SIGKILL` on macOS.
#[cfg(target_os = "macos")]
pub(crate) const PARENT_EXIT_GRACE: Duration = Duration::from_secs(2);

/// Test-only helper failures.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    /// Report this `errno` instead of registering.
    #[cfg(test)]
    Errno(i32),
    /// Never report; wait to be released.
    #[cfg(test)]
    Hang,
    /// Exit without reporting.
    #[cfg(test)]
    Die,
    /// The kernel will not list the descriptor table.
    #[cfg(test)]
    ListingFails,
}

#[cfg(target_os = "macos")]
struct Options {
    supervisor: libc::pid_t,
    ack_timeout: Duration,
    fault: Fault,
    /// Test only: runs after the pipes exist and immediately before the helper is forked, like
    /// another thread's `dup2` landing in that window.
    #[cfg(test)]
    before_fork: Option<Box<dyn FnOnce()>>,
}

#[cfg(target_os = "macos")]
impl ParentDeathTie {
    pub(crate) fn prepare(session: bool) -> io::Result<Self> {
        Self::prepare_with(
            session,
            Options {
                // SAFETY: getpid has no preconditions.
                supervisor: unsafe { libc::getpid() },
                ack_timeout: HELPER_ACK_TIMEOUT,
                fault: Fault::None,
                #[cfg(test)]
                before_fork: None,
            },
        )
    }

    fn prepare_with(session: bool, options: Options) -> io::Result<Self> {
        if session {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "kill_on_parent_exit cannot combine with UnixContainment::Session on macOS: \
                 the helper cannot join a process group in another session",
            ));
        }
        let Options {
            supervisor,
            ack_timeout,
            fault,
            #[cfg(test)]
            before_fork,
        } = options;
        let (hello_read, hello_write) = close_on_exec_pipe()?;
        let (ack_read, ack_write) = match close_on_exec_pipe() {
            Ok(ends) => ends,
            Err(error) => {
                close_all(&[hello_read, hello_write]);
                return Err(error);
            }
        };
        let (tether_read, tether_write) = match close_on_exec_pipe() {
            Ok(ends) => ends,
            Err(error) => {
                close_all(&[hello_read, hello_write, ack_read, ack_write]);
                return Err(error);
            }
        };
        #[cfg(test)]
        {
            if let Some(hook) = before_fork {
                hook();
            }
        }
        // SAFETY: the child branch below runs `reaper`, which uses only syscalls, stack
        // values and `_exit`, and never returns.
        let helper = unsafe { libc::fork() };
        if helper < 0 {
            let error = io::Error::last_os_error();
            close_all(&[
                hello_read,
                hello_write,
                ack_read,
                ack_write,
                tether_read,
                tether_write,
            ]);
            return Err(error);
        }
        if helper == 0 {
            reaper(Plan {
                hello_read,
                ack_write,
                tether_read,
                supervisor,
                fault,
            })
        }
        // The supervisor keeps the engine's two ends and the tether's write end only.
        close_all(&[hello_read, ack_write, tether_read]);
        Ok(Self {
            supervisor,
            helper: Some(HelperLink {
                pid: helper,
                hello_write,
                ack_read,
                tether_write,
            }),
            ack_timeout,
        })
    }

    pub(crate) fn engine_side(&self) -> EngineTie {
        let link = self.helper.as_ref().expect("tie is not settled");
        EngineTie {
            supervisor: self.supervisor,
            hello_write: link.hello_write,
            ack_read: link.ack_read,
            ack_timeout: self.ack_timeout,
        }
    }

    /// Ends the supervisor's part. Returns the armed helper when the spawn succeeded (the
    /// caller now owns it and must drop or retire it), and retires it otherwise.
    pub(crate) fn settle(mut self, spawned: bool) -> Option<Helper> {
        let helper = self.helper.take()?.into_helper();
        if spawned {
            Some(helper)
        } else {
            helper.retire();
            None
        }
    }

    #[cfg(test)]
    fn helper_pid(&self) -> libc::pid_t {
        self.helper.as_ref().expect("tie is not settled").pid
    }
}

#[cfg(target_os = "macos")]
impl Drop for ParentDeathTie {
    /// Covers a spawn that never reached `settle` (a panic): no helper may be left running.
    fn drop(&mut self) {
        if let Some(link) = self.helper.take() {
            link.into_helper().retire();
        }
    }
}

#[cfg(target_os = "macos")]
fn close_all(fds: &[RawFd]) {
    for &fd in fds {
        // SAFETY: descriptors owned by the caller.
        unsafe { libc::close(fd) };
    }
}

/// A pipe whose ends are at least 3 (so std's stdio `dup2` cannot overwrite them) and
/// close-on-exec from creation (`F_DUPFD_CLOEXEC`).
#[cfg(target_os = "macos")]
fn close_on_exec_pipe() -> io::Result<(RawFd, RawFd)> {
    let mut ends: [RawFd; 2] = [-1; 2];
    // SAFETY: pipe writes two descriptors into the array.
    if unsafe { libc::pipe(ends.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut moved: [RawFd; 2] = [-1; 2];
    for index in 0..2 {
        // SAFETY: `ends[index]` was just created and is owned here.
        let duplicate = unsafe { libc::fcntl(ends[index], libc::F_DUPFD_CLOEXEC, 3) };
        if duplicate < 0 {
            let error = io::Error::last_os_error();
            close_all(&ends);
            close_all(&moved[..index]);
            return Err(error);
        }
        moved[index] = duplicate;
    }
    close_all(&ends);
    Ok((moved[0], moved[1]))
}

#[cfg(target_os = "macos")]
impl EngineTie {
    /// Runs in the new process between `fork` and `exec`: hand the helper the engine pid,
    /// then wait for its verdict. Async-signal-safe calls only (`getpid`, `sigaction`,
    /// `write`, `poll`, `read`, `clock_gettime`, `getppid`).
    pub(crate) fn apply_in_child(&self) -> io::Result<()> {
        // SAFETY: getpid has no preconditions. The engine pid equals this process's pid
        // after exec, and it is the process group id.
        let engine = unsafe { libc::getpid() };
        send_engine_pid(self.hello_write, engine)?;
        await_ack(self.ack_read, self.ack_timeout)?;
        // The watch is armed. If the supervisor was already gone when it was armed, the pid
        // may have been reused, which would arm the wrong process: a dead supervisor has
        // reparented this process.
        // SAFETY: getppid has no preconditions.
        if unsafe { libc::getppid() } == self.supervisor {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(libc::ESRCH))
        }
    }
}

/// Writes the engine pid with `SIGPIPE` ignored for the duration: a helper that already
/// died must fail the spawn with `EPIPE`, not kill the engine before it can report.
#[cfg(target_os = "macos")]
fn send_engine_pid(fd: RawFd, engine: libc::pid_t) -> io::Result<()> {
    let bytes = engine.to_ne_bytes();
    // SAFETY: sigaction with valid structs; write of four bytes from a four-byte array.
    let (written, error) = unsafe {
        let mut ignore: libc::sigaction = std::mem::zeroed();
        ignore.sa_sigaction = libc::SIG_IGN;
        let mut previous: libc::sigaction = std::mem::zeroed();
        libc::sigaction(libc::SIGPIPE, &ignore, &mut previous);
        let written = loop {
            let count = libc::write(fd, bytes.as_ptr().cast(), bytes.len());
            if count >= 0 || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                break count;
            }
        };
        let error = io::Error::last_os_error();
        libc::sigaction(libc::SIGPIPE, &previous, std::ptr::null_mut());
        (written, error)
    };
    if written == bytes.len() as isize {
        Ok(())
    } else if written < 0 {
        Err(error)
    } else {
        Err(io::Error::from_raw_os_error(libc::ECONNRESET))
    }
}

/// Waits for the helper's four-byte result. Async-signal-safe: `poll`, `read`
/// and `clock_gettime` only.
#[cfg(target_os = "macos")]
fn await_ack(fd: RawFd, timeout: Duration) -> io::Result<()> {
    let now = || -> Duration {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: ts is a valid out pointer.
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
    };
    let deadline = now() + timeout;
    loop {
        let remaining = deadline.saturating_sub(now());
        if remaining.is_zero() {
            return Err(io::Error::from_raw_os_error(libc::ETIMEDOUT));
        }
        let mut pending = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = remaining.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut pending, 1, millis) };
        if ready < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(io::Error::last_os_error());
        }
        if ready == 0 {
            continue;
        }
        let mut code = [0u8; 4];
        // SAFETY: reads at most four bytes into a four-byte buffer.
        let count = unsafe { libc::read(fd, code.as_mut_ptr().cast(), code.len()) };
        if count < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(io::Error::last_os_error());
        }
        if count as usize != code.len() {
            // End of file or a short result: the helper died before it armed.
            return Err(io::Error::from_raw_os_error(libc::ECONNRESET));
        }
        return match i32::from_ne_bytes(code) {
            0 => Ok(()),
            errno => Err(io::Error::from_raw_os_error(errno)),
        };
    }
}

#[cfg(target_os = "macos")]
fn exit_watch(pid: libc::pid_t) -> libc::kevent {
    libc::kevent {
        ident: pid as usize,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_RECEIPT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

/// A change record for the read watch on the tether (`flags` is `EV_ADD | EV_RECEIPT` to
/// register it and `EV_DELETE` to drop it).
#[cfg(target_os = "macos")]
fn tether_watch(fd: RawFd, flags: u16) -> libc::kevent {
    libc::kevent {
        ident: fd as usize,
        filter: libc::EVFILT_READ,
        flags,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

/// Registers `NOTE_EXIT` for the supervisor. Returns true when it had already exited
/// (`ESRCH` in the receipt); any other registration error is returned.
#[cfg(target_os = "macos")]
fn register_supervisor_watch(kq: RawFd, supervisor: libc::pid_t) -> io::Result<bool> {
    let change = exit_watch(supervisor);
    let mut receipt = empty_kevent();
    // SAFETY: one change and one receipt, as counted.
    let count = unsafe { libc::kevent(kq, &change, 1, &mut receipt, 1, std::ptr::null()) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    if count != 1 {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    if (receipt.flags & libc::EV_ERROR) == 0 || receipt.data == 0 {
        return Ok(false);
    }
    if receipt.data == libc::ESRCH as isize {
        return Ok(true);
    }
    Err(io::Error::from_raw_os_error(receipt.data as i32))
}

/// Registers the read watch on the tether. Any registration error is returned.
#[cfg(target_os = "macos")]
fn register_tether_watch(kq: RawFd, tether: RawFd) -> io::Result<()> {
    let change = tether_watch(tether, libc::EV_ADD | libc::EV_RECEIPT);
    let mut receipt = empty_kevent();
    // SAFETY: one change and one receipt, as counted.
    let count = unsafe { libc::kevent(kq, &change, 1, &mut receipt, 1, std::ptr::null()) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    if count != 1 {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    if (receipt.flags & libc::EV_ERROR) != 0 && receipt.data != 0 {
        return Err(io::Error::from_raw_os_error(receipt.data as i32));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn empty_kevent() -> libc::kevent {
    // SAFETY: an all-zero kevent is a valid value for every field.
    unsafe { std::mem::zeroed() }
}

/// Reports the helper's result to the engine: `0` for armed, else an `errno`.
#[cfg(target_os = "macos")]
unsafe fn report(ack: RawFd, errno: i32) {
    let code = errno.to_ne_bytes();
    // A short or failed write reads as end of file on the engine side, which
    // fails the spawn, so the result needs no retry.
    libc::write(ack, code.as_ptr().cast(), code.len());
}

/// Reads exactly `buffer.len()` bytes; false on end of file or error.
#[cfg(target_os = "macos")]
unsafe fn read_all(fd: RawFd, buffer: &mut [u8]) -> bool {
    let mut filled = 0;
    while filled < buffer.len() {
        let count = libc::read(
            fd,
            buffer[filled..].as_mut_ptr().cast(),
            buffer.len() - filled,
        );
        if count > 0 {
            filled += count as usize;
        } else if count == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return false;
        }
    }
    true
}

/// Closes every descriptor except those in `keep`, in the helper right after `fork`, and says
/// whether it can prove it closed them all.
///
/// Why it cannot be a bounded walk: the helper never execs, so close-on-exec releases
/// nothing here, and any descriptor it keeps (a pipe writer another thread duplicated high,
/// std's exec-error pipe) holds that pipe open for as long as the helper lives. A bound
/// computed before the fork is stale by the time the helper runs, so no bound is used.
///
/// What it calls, and why each is fit for a forked child of a multithreaded process:
/// * `proc_pidinfo(getpid(), PROC_PIDLISTFDS, ..)`: libproc's thin wrapper over the
///   `__proc_info` system call. It reads the kernel's table for this process into the caller's
///   buffer (here on the stack), takes no lock in user space and does not allocate. It is not
///   on POSIX's async-signal-safe list; the claim rests on libproc's source, which is a bare
///   syscall wrapper, and the tests below run it in a forked child.
/// * `close`, `getpid`: system calls on the POSIX list.
///
/// The listing is repeated while it fills the buffer, so any number of descriptors is
/// handled. The result is `true` only when a listing came back short of the buffer, which
/// means it covered the whole table and every entry but `keep` was closed. If the kernel will
/// not list them (or `listing_fails` is set by a test) the result is `false` and the caller
/// must fail closed: nothing else can establish that no pipe writer is still open here.
#[cfg(target_os = "macos")]
unsafe fn close_all_except(keep: [RawFd; 3], listing_fails: bool) -> bool {
    const BATCH: usize = 128;
    /// Each pass that fills the buffer closes at least `BATCH - keep.len()` descriptors, so
    /// this far exceeds any real table; it only stops a table that never shrinks.
    const PASS_LIMIT: usize = 1 << 14;
    if listing_fails {
        return false;
    }
    let mut list = [libc::proc_fdinfo {
        proc_fd: 0,
        proc_fdtype: 0,
    }; BATCH];
    let pid = libc::getpid();
    let record = std::mem::size_of::<libc::proc_fdinfo>();
    for _ in 0..PASS_LIMIT {
        let bytes = libc::proc_pidinfo(
            pid,
            libc::PROC_PIDLISTFDS,
            0,
            list.as_mut_ptr().cast(),
            std::mem::size_of_val(&list) as libc::c_int,
        );
        if bytes <= 0 {
            return false;
        }
        let count = (bytes as usize / record).min(BATCH);
        for info in &list[..count] {
            if !keep.contains(&info.proc_fd) {
                libc::close(info.proc_fd);
            }
        }
        if count < BATCH {
            return true;
        }
    }
    false
}

/// Everything the helper needs, captured before the fork.
#[cfg(target_os = "macos")]
struct Plan {
    hello_read: RawFd,
    ack_write: RawFd,
    tether_read: RawFd,
    supervisor: libc::pid_t,
    fault: Fault,
}

#[cfg(target_os = "macos")]
impl Plan {
    fn listing_fails(&self) -> bool {
        #[cfg(test)]
        {
            matches!(self.fault, Fault::ListingFails)
        }
        #[cfg(not(test))]
        {
            false
        }
    }
}

/// What ended the helper's wait.
#[cfg(target_os = "macos")]
enum Wake {
    SupervisorExited,
    Released,
}

/// Helper body, reached only in the forked child. It never returns: every exit
/// path goes through `_exit`.
///
/// The helper is a fork of a multithreaded process, so after the fork it uses only system
/// calls and stack values: `close_all_except` (above), `sigaction`, `poll`, `read`, `write`,
/// `setpgid`, `kqueue`, `kevent`, `nanosleep`, `kill`, `_exit`. It runs no allocation, no
/// locks and no destructors. It never calls `fork`, so no `pthread_atfork` handler runs in it
/// beyond the child handlers of the single fork that created it.
#[cfg(target_os = "macos")]
fn reaper(plan: Plan) -> ! {
    // SAFETY: see above; every call is a system call on valid stack values.
    unsafe {
        // Drop every inherited descriptor (stdio, pipe ends, anything another thread opened
        // before this fork) so readers in the supervisor see EOF as usual. Keep three pipe ends.
        let closed_all = close_all_except(
            [plan.hello_read, plan.ack_write, plan.tether_read],
            plan.listing_fails(),
        );
        let ack = plan.ack_write;
        // A closed result pipe must not kill the helper before it can exit cleanly.
        let mut ignore: libc::sigaction = std::mem::zeroed();
        ignore.sa_sigaction = libc::SIG_IGN;
        libc::sigaction(libc::SIGPIPE, &ignore, std::ptr::null_mut());
        // The helper shares the engine's group, so the group TERM reaches it.
        // Ignore TERM so the escalation below still runs.
        libc::sigaction(libc::SIGTERM, &ignore, std::ptr::null_mut());

        if !closed_all {
            // Failing closed: this process cannot prove it holds no pipe writer that the
            // supervisor's readers wait on, and it would hold it for as long as it lives. It
            // exits now, which releases whatever it holds, and the spawn fails.
            report(ack, LISTING_FAILED);
            libc::_exit(1);
        }

        let mut raw = [0u8; 4];
        if !await_engine(plan.hello_read, plan.tether_read, &mut raw) {
            // No engine ever arrived (the spawn was abandoned): nothing to tie.
            libc::_exit(0);
        }
        libc::close(plan.hello_read);
        let engine = i32::from_ne_bytes(raw);

        match plan.fault {
            Fault::None => {}
            #[cfg(test)]
            Fault::Errno(errno) => {
                report(ack, errno);
                libc::_exit(1);
            }
            #[cfg(test)]
            Fault::Hang => park_until_tether(plan.tether_read),
            #[cfg(test)]
            Fault::Die => libc::_exit(1),
            #[cfg(test)]
            Fault::ListingFails => {}
        }

        if engine <= 0 {
            report(ack, libc::EINVAL);
            libc::_exit(1);
        }
        // Join the engine's group and stay in it: while this process is a member the group
        // id cannot be recycled, so the final `kill(-engine, ..)` can only name this group.
        if libc::setpgid(0, engine) != 0 {
            report(
                ack,
                io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EPERM),
            );
            libc::_exit(1);
        }

        // The kqueue is created here, after the fork: descriptors of this kind
        // do not survive fork(2) on macOS.
        let kq = libc::kqueue();
        if kq < 0 {
            report(
                ack,
                io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO),
            );
            libc::_exit(1);
        }
        match register_supervisor_watch(kq, plan.supervisor) {
            Ok(false) => {}
            Ok(true) => {
                // The supervisor died before the watch was armed: nothing to tie to.
                report(ack, libc::ESRCH);
                libc::_exit(1);
            }
            Err(error) => {
                report(ack, error.raw_os_error().unwrap_or(libc::EIO));
                libc::_exit(1);
            }
        }
        if let Err(error) = register_tether_watch(kq, plan.tether_read) {
            report(ack, error.raw_os_error().unwrap_or(libc::EIO));
            libc::_exit(1);
        }
        report(ack, 0);
        libc::close(ack);

        if let Wake::Released =
            wait_for_supervisor_or_release(kq, plan.supervisor, plan.tether_read)
        {
            // Told to stand down while the supervisor is alive: the owner is gone and nothing
            // is protected any more. Signal nothing.
            libc::_exit(0);
        }
        // Signalling `-engine` is safe: this process is a member of the group, so its id
        // is not reusable until this process exits.
        libc::kill(-engine, libc::SIGTERM);
        sleep_for(PARENT_EXIT_GRACE);
        libc::kill(-engine, libc::SIGKILL);
        libc::_exit(0);
    }
}

/// Waits for the engine's four-byte pid on `hello`, or for the supervisor to give up first.
/// False when the helper should just exit: the supervisor released it (or vanished) before an
/// engine arrived, or `hello` closed without a pid.
#[cfg(target_os = "macos")]
unsafe fn await_engine(hello: RawFd, tether: RawFd, buffer: &mut [u8]) -> bool {
    loop {
        let mut watch = [
            libc::pollfd {
                fd: hello,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: tether,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if libc::poll(watch.as_mut_ptr(), 2, -1) < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return false;
        }
        if watch[1].revents != 0 {
            return false;
        }
        if watch[0].revents != 0 {
            return read_all(hello, buffer);
        }
    }
}

/// Test only: a helper that never reports. It waits for the supervisor to release it (or to
/// go away), so a failed spawn can still end it without signalling its pid.
#[cfg(all(target_os = "macos", test))]
unsafe fn park_until_tether(tether: RawFd) -> ! {
    loop {
        let mut pending = libc::pollfd {
            fd: tether,
            events: libc::POLLIN,
            revents: 0,
        };
        libc::poll(&mut pending, 1, -1);
        if pending.revents != 0 {
            libc::_exit(0);
        }
    }
}

/// Blocks until the supervisor exits or the supervisor releases the helper.
///
/// Only the explicit `RELEASE` byte on the tether is a release. End of file on the tether is
/// what the supervisor's own death looks like (all its descriptors close), so it is not a
/// release: the tether watch is removed and the wait goes on for `NOTE_EXIT`.
#[cfg(target_os = "macos")]
unsafe fn wait_for_supervisor_or_release(
    kq: RawFd,
    supervisor: libc::pid_t,
    tether: RawFd,
) -> Wake {
    loop {
        let mut event = empty_kevent();
        let count = libc::kevent(kq, std::ptr::null(), 0, &mut event, 1, std::ptr::null());
        if count < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            libc::_exit(1);
        }
        if count != 1 {
            continue;
        }
        if event.filter == libc::EVFILT_PROC && event.ident == supervisor as usize {
            return Wake::SupervisorExited;
        }
        if event.filter == libc::EVFILT_READ && event.ident == tether as usize {
            let mut byte = 0u8;
            let read = libc::read(tether, (&mut byte as *mut u8).cast(), 1);
            if read == 1 && byte == RELEASE {
                return Wake::Released;
            }
            if read < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            // End of file, an unexpected byte or a read error: stop watching the tether so
            // the level-triggered watch does not fire forever, and wait for the supervisor.
            let remove = tether_watch(tether, libc::EV_DELETE);
            libc::kevent(kq, &remove, 1, std::ptr::null_mut(), 0, std::ptr::null());
        }
    }
}

/// Sleeps without allocation; resumes after EINTR.
#[cfg(target_os = "macos")]
unsafe fn sleep_for(duration: Duration) {
    let mut request = libc::timespec {
        tv_sec: duration.as_secs() as libc::time_t,
        tv_nsec: duration.subsec_nanos() as libc::c_long,
    };
    let mut remaining = request;
    while libc::nanosleep(&request, &mut remaining) != 0 {
        if io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return;
        }
        request = remaining;
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};
    use std::sync::{
        atomic::{AtomicI32, Ordering},
        Arc,
    };
    use std::time::Instant;

    /// A spawned `sleep` and its helper, cleaned up on drop.
    struct Spawned {
        child: Child,
        helper: Option<Helper>,
    }

    impl Drop for Spawned {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Some(helper) = self.helper.take() {
                helper.retire();
            }
        }
    }

    fn options() -> Options {
        Options {
            // SAFETY: getpid has no preconditions.
            supervisor: unsafe { libc::getpid() },
            ack_timeout: HELPER_ACK_TIMEOUT,
            fault: Fault::None,
            before_fork: None,
        }
    }

    fn tie() -> ParentDeathTie {
        ParentDeathTie::prepare(false).expect("prepare tie")
    }

    /// Spawns `sleep` through the same `pre_exec` hook `OwnedCommand` uses.
    fn spawn_with(tie: ParentDeathTie) -> io::Result<Spawned> {
        let engine = tie.engine_side();
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        // SAFETY: `apply_in_child` is async-signal-safe, as `pre_exec` requires.
        unsafe {
            command.pre_exec(move || {
                libc::setpgid(0, 0);
                engine.apply_in_child()
            });
        }
        let spawned = command.spawn();
        let helper = tie.settle(spawned.is_ok());
        Ok(Spawned {
            child: spawned?,
            helper,
        })
    }

    fn assert_fails_with(
        result: io::Result<Spawned>,
        errno: i32,
        within: Duration,
        started: Instant,
    ) {
        let error = match result {
            Ok(_) => panic!("spawn must fail closed, but it succeeded"),
            Err(error) => error,
        };
        assert_eq!(error.raw_os_error(), Some(errno), "{error}");
        assert!(
            started.elapsed() < within,
            "failure took {:?}",
            started.elapsed()
        );
    }

    fn gone(pid: libc::pid_t) -> bool {
        // SAFETY: signal 0 only checks existence.
        let missing = unsafe { libc::kill(pid, 0) } != 0;
        missing && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    /// A live process that is not this process's child, standing in for an unrelated process
    /// that was given a helper's recycled pid. Its parent exits at once, so it is reparented.
    fn unrelated_process() -> libc::pid_t {
        let output = Command::new("/bin/sh")
            .args(["-c", "sleep 600 >/dev/null 2>&1 </dev/null & echo $!"])
            .output()
            .expect("start the bystander");
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .expect("bystander pid")
    }

    fn alive(pid: libc::pid_t) -> bool {
        // SAFETY: signal 0 only checks existence.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    fn plain_pipe() -> (RawFd, RawFd) {
        let mut ends: [RawFd; 2] = [-1; 2];
        // SAFETY: pipe writes two descriptors into the array.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        (ends[0], ends[1])
    }

    /// Reads one byte; `None` at end of file.
    fn read_byte(fd: RawFd) -> Option<u8> {
        let mut byte = 0u8;
        // SAFETY: reads at most one byte into a one-byte buffer.
        let count = unsafe { libc::read(fd, (&mut byte as *mut u8).cast(), 1) };
        (count == 1).then_some(byte)
    }

    #[test]
    fn armed_helper_lets_the_spawn_succeed_and_sits_in_the_engines_group() {
        let spawned = spawn_with(tie()).expect("spawn with an armed helper");
        let helper = spawned.helper.as_ref().expect("helper handed back");
        // SAFETY: getpgid only reads.
        let group = unsafe { libc::getpgid(helper.pid()) };
        assert_eq!(
            group,
            spawned.child.id() as libc::pid_t,
            "helper pins the group"
        );
    }

    #[test]
    fn helper_registration_failure_fails_the_spawn() {
        let tie = ParentDeathTie::prepare_with(
            false,
            Options {
                fault: Fault::Errno(libc::EPERM),
                ..options()
            },
        )
        .expect("prepare");
        let started = Instant::now();
        assert_fails_with(
            spawn_with(tie),
            libc::EPERM,
            Duration::from_secs(4),
            started,
        );
    }

    #[test]
    fn a_failed_spawn_releases_and_collects_its_helper() {
        let tie = ParentDeathTie::prepare_with(
            false,
            Options {
                fault: Fault::Errno(libc::EPERM),
                ..options()
            },
        )
        .expect("prepare");
        let helper = tie.helper_pid();
        assert!(spawn_with(tie).is_err());
        assert!(gone(helper), "helper {helper} must be released and reaped");
    }

    #[test]
    fn helper_that_never_reports_times_out_and_fails_the_spawn() {
        let tie = ParentDeathTie::prepare_with(
            false,
            Options {
                fault: Fault::Hang,
                ack_timeout: Duration::from_millis(300),
                ..options()
            },
        )
        .expect("prepare");
        let helper = tie.helper_pid();
        let started = Instant::now();
        let result = spawn_with(tie);
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert_fails_with(result, libc::ETIMEDOUT, Duration::from_secs(4), started);
        // The hung helper is ended by the release byte, with no signal.
        assert!(gone(helper), "helper {helper} must be released and reaped");
    }

    #[test]
    fn helper_that_dies_before_reporting_fails_the_spawn() {
        let tie = ParentDeathTie::prepare_with(
            false,
            Options {
                fault: Fault::Die,
                ..options()
            },
        )
        .expect("prepare");
        let started = Instant::now();
        assert_fails_with(
            spawn_with(tie),
            libc::ECONNRESET,
            Duration::from_secs(4),
            started,
        );
    }

    #[test]
    fn supervisor_that_is_already_gone_fails_the_spawn() {
        let mut dead = Command::new("/usr/bin/true").spawn().expect("spawn true");
        let pid = dead.id() as libc::pid_t;
        dead.wait().expect("reap true");
        let tie = ParentDeathTie::prepare_with(
            false,
            Options {
                supervisor: pid,
                ..options()
            },
        )
        .expect("prepare");
        let started = Instant::now();
        assert_fails_with(
            spawn_with(tie),
            libc::ESRCH,
            Duration::from_secs(4),
            started,
        );
    }

    #[test]
    fn supervisor_that_is_not_the_parent_fails_the_spawn() {
        // A live process that is not this one: the engine's parent differs from
        // the supervisor the tie names, as after a death and pid reuse.
        let mut other = Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let tie = ParentDeathTie::prepare_with(
            false,
            Options {
                supervisor: other.id() as libc::pid_t,
                ..options()
            },
        )
        .expect("prepare");
        let started = Instant::now();
        let result = spawn_with(tie);
        let _ = other.kill();
        let _ = other.wait();
        assert_fails_with(result, libc::ESRCH, Duration::from_secs(4), started);
    }

    #[test]
    fn session_containment_is_unsupported_on_macos() {
        let Err(error) = ParentDeathTie::prepare(true) else {
            panic!("a session cannot be joined by the helper");
        };
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    /// A release makes a live, armed helper exit by itself with status 0: it was told to stand
    /// down, not signalled, and it did not run the parent-death kill sequence.
    #[test]
    fn a_released_helper_exits_by_itself_with_status_zero() {
        let mut spawned = spawn_with(tie()).expect("spawn with an armed helper");
        let mut helper = spawned.helper.take().expect("helper handed back");
        let pid = helper.pid();
        helper.release();
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            let mut status = 0;
            // SAFETY: waitpid with WNOHANG on this test's own helper.
            let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if reaped == pid {
                break status;
            }
            assert_eq!(reaped, 0, "waitpid: {}", io::Error::last_os_error());
            assert!(
                Instant::now() < deadline,
                "the released helper never exited"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "the helper must exit by itself, not be signalled (status {status:#x})"
        );
        // The group was not touched: the engine still runs.
        assert!(
            matches!(spawned.child.try_wait(), Ok(None)),
            "a release must not signal the engine's group"
        );
    }

    /// The helper was reaped by someone else in this process and its pid now belongs to an
    /// unrelated process. Dropping the child must not touch that process.
    #[test]
    fn a_helper_reaped_elsewhere_is_never_signalled_when_its_owner_drops() {
        let bystander = unrelated_process();
        let (read_end, write_end) = plain_pipe();
        let helper = Helper {
            pid: bystander,
            // SAFETY: `write_end` was just created and is owned by nothing else.
            tether: Some(unsafe { OwnedFd::from_raw_fd(write_end) }),
        };
        let leader = Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn leader");
        let mut owned = crate::unix_child::UnixChild::new(leader, Some(helper));
        owned.kill().expect("kill the leader");
        // Reaping the leader looks at the helper, finds the pid is not our child (`ECHILD`),
        // and must forget it.
        owned.wait().expect("reap the leader");
        drop(owned);
        let survived = alive(bystander);
        // The tether closed without a release byte, because there was no helper to release.
        let released = read_byte(read_end);
        // SAFETY: cleanup of this test's own descriptor and bystander.
        unsafe {
            libc::close(read_end);
            libc::kill(bystander, libc::SIGKILL);
        }
        assert!(survived, "the owner signalled a pid it no longer owned");
        assert_eq!(released, None, "no helper was left to release");
    }

    #[test]
    fn retiring_a_helper_writes_one_release_byte_and_closes_the_channel() {
        let bystander = unrelated_process();
        let (read_end, write_end) = plain_pipe();
        let helper = Helper {
            pid: bystander,
            // SAFETY: `write_end` was just created and is owned by nothing else.
            tether: Some(unsafe { OwnedFd::from_raw_fd(write_end) }),
        };
        helper.retire();
        let first = read_byte(read_end);
        let second = read_byte(read_end);
        let survived = alive(bystander);
        // SAFETY: cleanup of this test's own descriptor and bystander.
        unsafe {
            libc::close(read_end);
            libc::kill(bystander, libc::SIGKILL);
        }
        assert_eq!(first, Some(RELEASE));
        assert_eq!(second, None, "the channel closes after the release byte");
        assert!(survived, "retiring must not signal the helper's pid");
    }

    /// Another thread's `dup2` lands a pipe writer above any bound, before the helper is
    /// forked, and the kernel will not list the descriptor table. The helper cannot prove it
    /// closed the writer, so the spawn must fail, and the helper must not keep the writer.
    #[test]
    fn an_unlistable_descriptor_table_fails_the_spawn_and_frees_a_writer_above_any_bound() {
        const HIGH: libc::c_int = 3000;
        // SAFETY: plain syscalls on descriptors this test owns.
        unsafe {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit);
            let wanted = 8192.min(limit.rlim_max);
            if limit.rlim_cur < wanted {
                limit.rlim_cur = wanted;
                assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &limit), 0);
            }
        }
        let (reader, writer) = plain_pipe();
        let high = Arc::new(AtomicI32::new(-1));
        let injected = Arc::clone(&high);
        let tie = ParentDeathTie::prepare_with(
            false,
            Options {
                fault: Fault::ListingFails,
                before_fork: Some(Box::new(move || {
                    // SAFETY: duplicates a descriptor this test owns.
                    let fd = unsafe { libc::fcntl(writer, libc::F_DUPFD_CLOEXEC, HIGH) };
                    assert!(fd >= HIGH, "the writer must land above every earlier bound");
                    injected.store(fd, Ordering::SeqCst);
                })),
                ..options()
            },
        )
        .expect("prepare");
        // SAFETY: closes this process's copies; the helper alone inherited the high one.
        unsafe {
            libc::close(writer);
            libc::close(high.load(Ordering::SeqCst));
        }
        let started = Instant::now();
        let error = match spawn_with(tie) {
            Ok(_) => {
                panic!("a helper that cannot prove its descriptors closed must fail the spawn")
            }
            Err(error) => error,
        };
        assert!(
            matches!(
                error.raw_os_error(),
                Some(libc::EPIPE) | Some(LISTING_FAILED)
            ),
            "{error}"
        );
        assert!(started.elapsed() < Duration::from_secs(4));
        // The helper exited, so the writer it inherited is closed and the reader sees EOF.
        let mut pending = libc::pollfd {
            fd: reader,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut pending, 1, 10_000) };
        let byte = if ready > 0 {
            read_byte(reader)
        } else {
            Some(0)
        };
        // SAFETY: closes this test's own descriptor.
        unsafe { libc::close(reader) };
        assert!(
            ready > 0 && byte.is_none(),
            "no end of file: the helper kept the pipe writer open (poll {ready})"
        );
    }

    /// Forks a child that runs the sweep and then checks the descriptor table.
    fn sweep_in_forked_child(listing_fails: bool) {
        let mut ends: [RawFd; 2] = [0; 2];
        // SAFETY: plain descriptor calls in this test; the child uses only async-signal-safe
        // calls and `_exit`.
        unsafe {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit);
            if limit.rlim_cur < 4096 {
                limit.rlim_cur = limit.rlim_max.min(4096);
                libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
            }
            assert_eq!(libc::pipe(ends.as_mut_ptr()), 0);
            // Descriptors at assorted numbers, including far above the usual range.
            let spread = [
                libc::dup(ends[0]),
                libc::fcntl(ends[0], libc::F_DUPFD, 700),
                libc::fcntl(ends[1], libc::F_DUPFD, 1500),
            ];
            assert!(spread.iter().all(|fd| *fd >= 0));
            let keep = [ends[0], ends[1], spread[2]];
            let pid = libc::fork();
            assert!(pid >= 0);
            if pid == 0 {
                let complete = close_all_except(keep, listing_fails);
                let mut code = if complete == listing_fails { 2 } else { 0 };
                for fd in 0..2048 {
                    let open = libc::fcntl(fd, libc::F_GETFD) >= 0;
                    let kept = keep.contains(&fd);
                    // A complete sweep leaves exactly `keep` open (past stdio, which the
                    // helper closes as well). A failed listing closes nothing.
                    if !listing_fails && open != kept {
                        code = 1;
                    }
                    if listing_fails && fd == spread[0] && !open {
                        code = 1;
                    }
                }
                libc::_exit(code);
            }
            let mut status = 0;
            assert_eq!(libc::waitpid(pid, &mut status, 0), pid);
            close_all(&ends);
            close_all(&spread);
            assert!(libc::WIFEXITED(status));
            assert_eq!(
                libc::WEXITSTATUS(status),
                0,
                "sweep result wrong (listing_fails={listing_fails}): 1 = descriptors \
                 other than the kept ones survived, 2 = wrong completeness verdict"
            );
        }
    }

    #[test]
    fn the_sweep_closes_every_descriptor_but_the_three_it_keeps_and_says_so() {
        sweep_in_forked_child(false);
    }

    #[test]
    fn a_sweep_that_cannot_list_the_table_says_it_is_incomplete() {
        sweep_in_forked_child(true);
    }
}
