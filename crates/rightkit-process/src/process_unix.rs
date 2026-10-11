use std::{io, os::fd::RawFd};

#[cfg(target_os = "macos")]
use std::time::Duration;

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
    pub(crate) fn settle(self, _spawned: bool) -> Option<libc::pid_t> {
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
//  1. `prepare` (supervisor, spawning thread): create two pipes, `hello` (engine -> helper)
//     and `ack` (helper -> engine), then `fork` the helper. The helper closes every
//     descriptor but its two pipe ends, then blocks reading the engine's pid from `hello`.
//  2. std spawns the engine (its own fork/exec). In the engine, after `setpgid(0, 0)`,
//     `EngineTie::apply_in_child` writes the engine pid to `hello` and waits for `ack`. The
//     engine therefore runs no program code until the helper has armed.
//  3. The helper joins the engine's process group (`setpgid(0, engine)`), creates its kqueue
//     (kqueue descriptors do not survive `fork(2)`), registers the supervisor watch, and
//     writes `0` (armed) or an `errno` to `ack`. It then waits for the supervisor to exit.
//  4. `settle` (supervisor): on success the helper pid goes to the `UnixChild`, which kills
//     and reaps it when the child state is dropped. On failure the helper is killed and reaped.
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
// engine. Dropping the last owner kills the helper; nothing is protected after that.
//
// Not supported on macOS: `UnixContainment::Session`. `setsid` puts the engine in a new
// session, and a process may join a group only inside its own session, so the helper could
// not join and the group id would not be pinned. The spawn fails with `Unsupported`.

#[cfg(target_os = "macos")]
struct HelperLink {
    pid: libc::pid_t,
    hello_write: RawFd,
    ack_read: RawFd,
}

/// How long the engine waits for the helper to confirm its watch. Closing descriptors,
/// creating the kqueue and registering take well under a second.
#[cfg(target_os = "macos")]
pub(crate) const HELPER_ACK_TIMEOUT: Duration = Duration::from_secs(5);

/// Grace between the parent-death `SIGTERM` and `SIGKILL` on macOS.
#[cfg(target_os = "macos")]
pub(crate) const PARENT_EXIT_GRACE: Duration = Duration::from_secs(2);

/// Where the descriptor-by-descriptor fallback stops if the kernel will not list them.
#[cfg(target_os = "macos")]
const FALLBACK_FD_CAP: RawFd = 1 << 20;

/// Test-only helper failures.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    /// Report this `errno` instead of registering.
    #[cfg(test)]
    Errno(i32),
    /// Never report.
    #[cfg(test)]
    Hang,
    /// Exit without reporting.
    #[cfg(test)]
    Die,
}

#[cfg(target_os = "macos")]
struct Options {
    supervisor: libc::pid_t,
    ack_timeout: Duration,
    fault: Fault,
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
        let fallback_limit = fd_upper_bound()?.min(FALLBACK_FD_CAP);
        let (hello_read, hello_write) = close_on_exec_pipe()?;
        let (ack_read, ack_write) = match close_on_exec_pipe() {
            Ok(ends) => ends,
            Err(error) => {
                close_all(&[hello_read, hello_write]);
                return Err(error);
            }
        };
        // SAFETY: the child branch below runs `reaper`, which uses only syscalls, stack
        // values and `_exit`, and never returns.
        let helper = unsafe { libc::fork() };
        if helper < 0 {
            let error = io::Error::last_os_error();
            close_all(&[hello_read, hello_write, ack_read, ack_write]);
            return Err(error);
        }
        if helper == 0 {
            reaper(Plan {
                hello_read,
                ack_write,
                supervisor: options.supervisor,
                fallback_limit,
                fault: options.fault,
            })
        }
        // The supervisor keeps the engine's two ends only.
        close_all(&[hello_read, ack_write]);
        Ok(Self {
            supervisor: options.supervisor,
            helper: Some(HelperLink {
                pid: helper,
                hello_write,
                ack_read,
            }),
            ack_timeout: options.ack_timeout,
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

    /// Ends the supervisor's part. Returns the helper pid when the spawn succeeded (the
    /// caller now owns it and must kill and reap it), and kills and reaps it otherwise.
    pub(crate) fn settle(mut self, spawned: bool) -> Option<libc::pid_t> {
        let link = self.helper.take()?;
        close_all(&[link.hello_write, link.ack_read]);
        if spawned {
            Some(link.pid)
        } else {
            kill_and_reap(link.pid);
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
            close_all(&[link.hello_write, link.ack_read]);
            kill_and_reap(link.pid);
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

/// SIGKILL then reap our own child helper, so the failure leaves neither a watcher nor a
/// zombie. The pid is stable: it is unreaped until this returns.
#[cfg(target_os = "macos")]
fn kill_and_reap(pid: libc::pid_t) {
    // SAFETY: `pid` is this process's own unreaped child.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
        let mut status = 0;
        while libc::waitpid(pid, &mut status, 0) < 0
            && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
        {}
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

/// Closes every descriptor except the two in `keep`, in the helper right after `fork`.
///
/// Why it cannot be a bounded walk: the helper never execs, so close-on-exec releases
/// nothing here, and any descriptor it keeps (a pipe writer another thread duplicated high,
/// std's exec-error pipe) holds that pipe open for as long as the helper lives.
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
/// handled. If the kernel will not list them (or `use_fallback` is set by a test), every
/// number below `fallback_limit` is closed one by one: slow but complete (`EBADF` from
/// closing a number that is not open is expected and ignored). It cannot fail.
#[cfg(target_os = "macos")]
unsafe fn close_all_except(keep: [RawFd; 2], fallback_limit: RawFd, use_fallback: bool) {
    const BATCH: usize = 128;
    if !use_fallback {
        let mut list = [libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0,
        }; BATCH];
        let pid = libc::getpid();
        let record = std::mem::size_of::<libc::proc_fdinfo>();
        loop {
            let bytes = libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                list.as_mut_ptr().cast(),
                std::mem::size_of_val(&list) as libc::c_int,
            );
            if bytes <= 0 {
                break; // fall through to the complete, slower walk
            }
            let count = (bytes as usize / record).min(BATCH);
            for info in &list[..count] {
                if info.proc_fd != keep[0] && info.proc_fd != keep[1] {
                    libc::close(info.proc_fd);
                }
            }
            if count < BATCH {
                return;
            }
        }
    }
    for fd in 0..fallback_limit {
        if fd != keep[0] && fd != keep[1] {
            libc::close(fd);
        }
    }
}

/// Everything the helper needs, captured before the fork.
#[cfg(target_os = "macos")]
struct Plan {
    hello_read: RawFd,
    ack_write: RawFd,
    supervisor: libc::pid_t,
    fallback_limit: RawFd,
    fault: Fault,
}

/// Helper body, reached only in the forked child. It never returns: every exit
/// path goes through `_exit`.
///
/// The helper is a fork of a multithreaded process, so after the fork it uses only system
/// calls and stack values: `close_all_except` (above), `sigaction`, `read`, `write`,
/// `setpgid`, `kqueue`, `kevent`, `nanosleep`, `kill`, `_exit`. It runs no allocation, no
/// locks and no destructors. It never calls `fork`, so no `pthread_atfork` handler runs in it
/// beyond the child handlers of the single fork that created it.
#[cfg(target_os = "macos")]
fn reaper(plan: Plan) -> ! {
    // SAFETY: see above; every call is a system call on valid stack values.
    unsafe {
        // Drop every inherited descriptor (stdio, pipe ends, anything another thread opened
        // before this fork) so readers in the supervisor see EOF as usual. Keep two pipe ends.
        close_all_except(
            [plan.hello_read, plan.ack_write],
            plan.fallback_limit,
            false,
        );
        let ack = plan.ack_write;
        // A closed result pipe must not kill the helper before it can exit cleanly.
        let mut ignore: libc::sigaction = std::mem::zeroed();
        ignore.sa_sigaction = libc::SIG_IGN;
        libc::sigaction(libc::SIGPIPE, &ignore, std::ptr::null_mut());
        // The helper shares the engine's group, so the group TERM reaches it.
        // Ignore TERM so the escalation below still runs.
        libc::sigaction(libc::SIGTERM, &ignore, std::ptr::null_mut());

        let mut raw = [0u8; 4];
        if !read_all(plan.hello_read, &mut raw) {
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
            Fault::Hang => loop {
                sleep_for(Duration::from_secs(60));
            },
            #[cfg(test)]
            Fault::Die => libc::_exit(1),
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
        report(ack, 0);
        libc::close(ack);

        wait_for_supervisor(kq, plan.supervisor);
        // Signalling `-engine` is safe: this process is a member of the group, so its id
        // is not reusable until this process exits.
        libc::kill(-engine, libc::SIGTERM);
        sleep_for(PARENT_EXIT_GRACE);
        libc::kill(-engine, libc::SIGKILL);
        libc::_exit(0);
    }
}

/// Blocks until the supervisor exits.
#[cfg(target_os = "macos")]
unsafe fn wait_for_supervisor(kq: RawFd, supervisor: libc::pid_t) {
    loop {
        let mut event = empty_kevent();
        let count = libc::kevent(kq, std::ptr::null(), 0, &mut event, 1, std::ptr::null());
        if count < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            libc::_exit(1);
        }
        if count == 1 && event.ident == supervisor as usize {
            return;
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
    use std::time::Instant;

    /// A spawned `sleep` and its helper, cleaned up on drop.
    struct Spawned {
        child: Child,
        helper: Option<libc::pid_t>,
    }

    impl Drop for Spawned {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Some(helper) = self.helper.take() {
                kill_and_reap(helper);
            }
        }
    }

    fn options() -> Options {
        Options {
            // SAFETY: getpid has no preconditions.
            supervisor: unsafe { libc::getpid() },
            ack_timeout: HELPER_ACK_TIMEOUT,
            fault: Fault::None,
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

    #[test]
    fn armed_helper_lets_the_spawn_succeed_and_sits_in_the_engines_group() {
        let spawned = spawn_with(tie()).expect("spawn with an armed helper");
        let helper = spawned.helper.expect("helper handed back");
        // SAFETY: getpgid only reads.
        let group = unsafe { libc::getpgid(helper) };
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
    fn a_failed_spawn_kills_and_reaps_its_helper() {
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
        assert!(gone(helper), "helper {helper} must be killed and reaped");
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
        let started = Instant::now();
        let result = spawn_with(tie);
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert_fails_with(result, libc::ETIMEDOUT, Duration::from_secs(4), started);
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

    /// Forks a child that runs the sweep and then checks the descriptor table.
    fn sweep_in_forked_child(use_fallback: bool) {
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
            let pid = libc::fork();
            assert!(pid >= 0);
            if pid == 0 {
                close_all_except(ends, 2048, use_fallback);
                let mut code = 0;
                for fd in 0..2048 {
                    let open = libc::fcntl(fd, libc::F_GETFD) >= 0;
                    let kept = fd == ends[0] || fd == ends[1];
                    if open != kept {
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
                "descriptors other than the two kept survived the sweep (fallback={use_fallback})"
            );
        }
    }

    #[test]
    fn the_sweep_closes_every_descriptor_but_the_two_it_keeps() {
        sweep_in_forked_child(false);
    }

    #[test]
    fn the_fallback_walk_closes_every_descriptor_but_the_two_it_keeps() {
        sweep_in_forked_child(true);
    }
}
