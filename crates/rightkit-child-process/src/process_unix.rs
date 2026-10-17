use std::{io, os::fd::RawFd};

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
        let maximum = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
        if maximum < 0 {
            return Err(io::Error::last_os_error());
        }
        // Include descriptors above a subsequently lowered soft limit.
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
        Ok(Self {
            allowed,
            upper_bound: upper_bound.max(highest.saturating_add(1)),
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

/// Raises the child's nice value to at least `floor`, between `fork` and `exec`. It only ever
/// lowers priority, which an unprivileged process may do: a value already at or above `floor`
/// is left alone. Async-signal-safe: `getpriority` and `setpriority` only, no allocation.
pub(crate) fn raise_nice_to(floor: libc::c_int) -> io::Result<()> {
    // SAFETY: getpriority takes no pointers. On error it returns -1, which is a valid nice
    // value below any floor used here, so the raise below still runs and is always permitted.
    let current = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
    if current >= floor {
        return Ok(());
    }
    // SAFETY: setpriority takes no pointers; `0` names the calling process.
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, floor) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// macOS: request the utility quality of service for the calling thread, which becomes the
/// child's initial thread across exec. Advisory: a refusal is ignored. Called between fork and
/// exec; it is a thin kernel call with no allocation.
#[cfg(target_os = "macos")]
pub(crate) fn set_utility_qos() {
    // SAFETY: takes a class value and a relative priority only.
    unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0) };
}

/// How long the macOS watchdog waits for the child to leave after `SIGTERM` before `SIGKILL`.
#[cfg(target_os = "macos")]
const WATCHDOG_GRACE_STEPS: u32 = 40; // 40 steps of 50 ms: 2 s
#[cfg(target_os = "macos")]
const WATCHDOG_STEP_NANOS: libc::c_long = 50_000_000;
/// How long the spawning side waits for the watchdog to report that it is armed.
#[cfg(target_os = "macos")]
const WATCHDOG_ARM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// macOS parent-death tie. macOS has no `PR_SET_PDEATHSIG`, so a small watchdog process is
/// started for `child` (a direct, unreaped child of this process that leads its own process
/// group). The watchdog is forked twice (the middle process exits at once and is reaped here,
/// so the watchdog is re-parented to launchd and never becomes a zombie of this process), calls
/// `setsid`, closes every inherited descriptor but its report pipe, and then blocks in a kqueue
/// holding two `EVFILT_PROC | NOTE_EXIT` knotes: one on this process (the supervisor), one on
/// the child. It acts as follows:
///
/// - the child exits first: the watchdog exits, nothing is signalled;
/// - the supervisor exits first (including `SIGKILL`, with no cleanup): unless the child has
///   exited meanwhile, `SIGTERM` goes to the child's process group, and `SIGKILL` two seconds
///   later if the child is still there, then the watchdog exits.
///
/// This function returns only after the watchdog reported that both knotes are registered
/// (the report pipe), while the child is still this process's unreaped child, so its pid cannot
/// have been recycled when it was registered. A child that already exited when the knote was
/// registered (`ESRCH`) needs no tie and counts as armed. On any failure the caller must stop
/// the child: it has no tie.
///
/// Only async-signal-safe calls run in the forked processes (`fork`, `setsid`, `open`, `dup2`,
/// `close`, `kqueue`, `kevent`, `kill`, `write`, `_exit`); nothing allocates there.
///
/// Residual windows, documented in the changelog: the child runs untied between its spawn and
/// the moment this returns; and the watchdog's final `kill` names a pid after the supervisor
/// died, so a child that exits and whose number is recycled inside that gap (microseconds, and
/// the number space must wrap) could be mistaken for the child (the user-space gap
/// [`crate::identity`] describes).
#[cfg(target_os = "macos")]
pub(crate) fn arm_parent_death_watch(child: u32) -> io::Result<()> {
    // SAFETY: getpid has no preconditions.
    let supervisor = unsafe { libc::getpid() };
    let child = child as libc::pid_t;
    let mut ends = [0 as libc::c_int; 2];
    // SAFETY: `ends` holds the two descriptors pipe(2) writes.
    if unsafe { libc::pipe(ends.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let [ack_read, ack_write] = ends;
    for fd in ends {
        // SAFETY: fcntl on descriptors created just above.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
    // Listed before the fork (allocation is not allowed after it) so the watchdog can close
    // every descriptor it inherited: holding another child's pipe end would stop its reader
    // from ever seeing EOF.
    let inherited = open_descriptors();
    // SAFETY: the forked processes only make the async-signal-safe calls listed above.
    let middle = unsafe { libc::fork() };
    if middle < 0 {
        let error = io::Error::last_os_error();
        // SAFETY: closing the two descriptors created above.
        unsafe {
            libc::close(ack_read);
            libc::close(ack_write);
        }
        return Err(error);
    }
    if middle == 0 {
        // SAFETY: as above; the middle process only forks and exits.
        let watchdog = unsafe { libc::fork() };
        if watchdog == 0 {
            run_watchdog(supervisor, child, ack_read, ack_write, &inherited);
        }
        // SAFETY: _exit is async-signal-safe and runs no destructors.
        unsafe { libc::_exit(if watchdog < 0 { 1 } else { 0 }) }
    }
    // SAFETY: closing this process's copy of the write end; the watchdog has its own.
    unsafe { libc::close(ack_write) };
    let mut status: libc::c_int = 0;
    let reaped = loop {
        // SAFETY: `middle` is a child this function just forked and nothing else reaps.
        let result = unsafe { libc::waitpid(middle, &mut status, 0) };
        if result >= 0 {
            break true;
        }
        match io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            // Collected elsewhere (a foreign waitpid(-1)): the report pipe still decides.
            _ => break false,
        }
    };
    let middle_ok = !reaped || (libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
    let armed = middle_ok && read_ack(ack_read);
    // SAFETY: closing the read end created above.
    unsafe { libc::close(ack_read) };
    if armed {
        Ok(())
    } else {
        Err(io::Error::other(
            "the macOS parent-death watchdog could not be armed",
        ))
    }
}

/// True when the watchdog wrote its `A` (armed) byte within [`WATCHDOG_ARM_TIMEOUT`].
#[cfg(target_os = "macos")]
fn read_ack(fd: libc::c_int) -> bool {
    let deadline = std::time::Instant::now() + WATCHDOG_ARM_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return false;
        }
        let mut entry = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = left.as_millis().min(libc::c_int::MAX as u128) as libc::c_int;
        // SAFETY: one valid pollfd, as counted.
        let ready = unsafe { libc::poll(&mut entry, 1, millis.max(1)) };
        if ready < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return false;
        }
        if ready == 0 {
            continue; // re-check the deadline
        }
        let mut byte = 0u8;
        // SAFETY: reading one byte into a local.
        let count = unsafe { libc::read(fd, (&mut byte as *mut u8).cast(), 1) };
        return count == 1 && byte == b'A';
    }
}

/// The descriptors this process has open, for the watchdog to close.
#[cfg(target_os = "macos")]
fn open_descriptors() -> Vec<RawFd> {
    let listed: Option<Vec<RawFd>> = std::fs::read_dir("/dev/fd").ok().map(|entries| {
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str()?.parse::<RawFd>().ok())
            .collect()
    });
    listed.unwrap_or_else(|| {
        // SAFETY: getdtablesize has no preconditions.
        let limit = unsafe { libc::getdtablesize() }.clamp(3, 65_536);
        (3..limit).collect()
    })
}

#[cfg(target_os = "macos")]
fn proc_exit_change(pid: libc::pid_t, tag: usize) -> libc::kevent {
    libc::kevent {
        ident: pid as usize,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_RECEIPT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: tag as *mut libc::c_void,
    }
}

/// The watchdog process body. Never returns and never allocates. See
/// [`arm_parent_death_watch`].
#[cfg(target_os = "macos")]
fn run_watchdog(
    supervisor: libc::pid_t,
    child: libc::pid_t,
    ack_read: libc::c_int,
    ack_write: libc::c_int,
    inherited: &[RawFd],
) -> ! {
    const PARENT_TAG: usize = 1;
    const CHILD_TAG: usize = 2;
    // SAFETY: only async-signal-safe libc calls, on descriptors and values owned by this
    // process; every path ends in `_exit`.
    unsafe {
        let report = |byte: u8| {
            libc::write(ack_write, (&byte as *const u8).cast(), 1);
            libc::close(ack_write);
        };
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        libc::setsid();
        libc::close(ack_read);
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if null >= 0 {
            for target in 0..3 {
                libc::dup2(null, target);
            }
            if null > 2 {
                libc::close(null);
            }
        }
        for &fd in inherited {
            if fd > 2 && fd != ack_write {
                libc::close(fd);
            }
        }
        let kq = libc::kqueue();
        if kq < 0 {
            report(b'F');
            libc::_exit(1);
        }
        let changes = [
            proc_exit_change(supervisor, PARENT_TAG),
            proc_exit_change(child, CHILD_TAG),
        ];
        let mut receipts: [libc::kevent; 2] = std::mem::zeroed();
        let no_wait = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let count = libc::kevent(kq, changes.as_ptr(), 2, receipts.as_mut_ptr(), 2, &no_wait);
        if count != 2 {
            report(b'F');
            libc::_exit(1);
        }
        let (mut parent_error, mut child_error) = (-1, -1);
        for receipt in &receipts {
            let error = if receipt.flags & libc::EV_ERROR != 0 {
                receipt.data as i32
            } else {
                0
            };
            match receipt.udata as usize {
                PARENT_TAG => parent_error = error,
                CHILD_TAG => child_error = error,
                _ => {}
            }
        }
        // The child already exited (the number is released only when it is reaped, which has not
        // happened, but XNU hides an exiting process from registration): nothing to tie.
        if child_error == libc::ESRCH {
            report(b'A');
            libc::_exit(0);
        }
        if child_error != 0 || (parent_error != 0 && parent_error != libc::ESRCH) {
            report(b'F');
            libc::_exit(1);
        }
        report(b'A');
        // `ESRCH` for the supervisor: it died before registration, so act at once.
        let mut parent_dead = parent_error == libc::ESRCH;
        let mut out: [libc::kevent; 2] = std::mem::zeroed();
        while !parent_dead {
            let count = libc::kevent(kq, std::ptr::null(), 0, out.as_mut_ptr(), 2, std::ptr::null());
            if count < 0 {
                if *libc::__error() == libc::EINTR {
                    continue;
                }
                libc::_exit(1);
            }
            let events = &out[..count as usize];
            if events.iter().any(|e| e.ident == child as usize) {
                libc::_exit(0); // the child left first: nothing to do
            }
            if events.iter().any(|e| e.ident == supervisor as usize) {
                parent_dead = true;
            }
        }
        // One last non-blocking look: a child that exited with the supervisor is left alone.
        let count = libc::kevent(kq, std::ptr::null(), 0, out.as_mut_ptr(), 2, &no_wait);
        if count > 0 && out[..count as usize].iter().any(|e| e.ident == child as usize) {
            libc::_exit(0);
        }
        // The child leads its own group (setpgid/setsid before exec), so -child is its group.
        libc::kill(-child, libc::SIGTERM);
        let step = libc::timespec {
            tv_sec: 0,
            tv_nsec: WATCHDOG_STEP_NANOS,
        };
        for _ in 0..WATCHDOG_GRACE_STEPS {
            let count = libc::kevent(kq, std::ptr::null(), 0, out.as_mut_ptr(), 2, &step);
            if count > 0 && out[..count as usize].iter().any(|e| e.ident == child as usize) {
                libc::_exit(0);
            }
        }
        libc::kill(-child, libc::SIGKILL);
        libc::_exit(0);
    }
}

/// Linux: `PR_SET_PDEATHSIG(SIGTERM)` on the direct child, called in the new process between
/// `fork` and `exec` (async-signal-safe: `prctl` and `getppid` only). The kernel fires it when
/// the thread that forked the child exits, and it targets only that child:
///
/// - it signals the direct child only, never the rest of its process group, so grandchildren
///   the child started survive unless the child cleans them up;
/// - it is a catchable `SIGTERM`, so a child that ignores or handles it can keep running, and
///   there is no `SIGKILL` escalation;
/// - the kernel keys it to the forking *thread*, so a supervisor that spawns from a short-lived
///   thread terminates its children when that thread ends.
///
/// `supervisor` is the pid of the process that spawned the child.
#[cfg(target_os = "linux")]
pub(crate) fn die_with_parent(supervisor: libc::pid_t) -> io::Result<()> {
    // SAFETY: PR_SET_PDEATHSIG takes a signal number and no pointers.
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // The supervisor may have died between fork and prctl. The death signal would then never
    // arrive, so fail the spawn rather than run untied.
    // SAFETY: getppid has no preconditions.
    if unsafe { libc::getppid() } != supervisor {
        return Err(io::Error::from_raw_os_error(libc::ESRCH));
    }
    Ok(())
}
