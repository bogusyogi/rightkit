//! Processes this program did not spawn: a pid read from a pid file, a helper another
//! launcher started, a descendant that outlived its parent.
//!
//! [`AdoptedProcess`](crate::AdoptedProcess) can only say a pid is running. A pid alone is a
//! name the kernel reuses, so signalling a remembered number can reach an unrelated process.
//! [`AdoptedIdentity`] remembers the pid together with the process's start time (and its
//! executable path, informational), read while the process was known to be the right one, and
//! every signal first checks that the pid still names that process:
//!
//! * a pid that now names a different process (other start time; the executable path is not
//!   compared because a process that `exec`s keeps its pid and start time) is refused with [`SignalResult::IdentityMismatch`] and nothing is
//!   sent;
//! * a pid that names nothing is [`SignalResult::Gone`];
//! * this process is never signalled, and neither is a pid that cannot be described.
//!
//! The parent pid is deliberately not part of the identity: an orphan is re-parented without
//! becoming a different process.
//!
//! # Limits
//!
//! Unix: the check and the `kill` are two system calls. The pid can change hands in the
//! microseconds between them only if the original process exits and the number wraps, the
//! gap [`identity`](crate::identity) documents; it cannot be closed from user space. Windows
//! has no such gap: the check and `TerminateProcess` use one handle. An exited process that
//! nobody has reaped yet (a Unix zombie) still verifies as the remembered identity, because
//! its description must stay readable while its owner finishes with its group; liveness
//! ([`AdoptedIdentity::is_running`]) treats it as dead. This crate never reaps a foreign
//! zombie: reaping is its parent's job, and a `waitpid` here could steal a status.
//!
//! Descendants are found by scanning the process table for parent links, so only descendants
//! whose parent is still alive can be found: once a parent exits, its children are
//! re-parented away. Take the list ([`AdoptedIdentity::descendants`]) while the root is
//! alive, and sweep it ([`sweep`]) later; the identities keep the sweep safe.

mod scan;

pub use scan::{
    find_processes, sweep_matching, MatchSweepOptions, MatchSweepReport, MatchedProcess,
    ProcessQuery,
};

use std::{
    io,
    num::NonZeroU32,
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime},
};

/// When a process started, in the host's own unit (macOS: seconds and microseconds; Linux:
/// clock ticks since boot and 0; Windows: 100 ns ticks since 1601 and 0). Comparable only on
/// the host that produced it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StartToken {
    pub major: u64,
    pub minor: u64,
}

/// A pid together with the start time and executable path that make it a name for exactly one
/// process. See the module documentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdoptedIdentity {
    pid: NonZeroU32,
    start: StartToken,
    exe: Option<PathBuf>,
}

/// What a pid names now, compared with a remembered identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityCheck {
    /// The pid still names the remembered process.
    Same,
    /// No process has this pid.
    Gone,
    /// The pid names a different process.
    Replaced,
}

/// How hard to stop a process. Windows has one way to stop a process, so both are the same
/// there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdoptedSignal {
    /// `SIGTERM`.
    Graceful,
    /// `SIGKILL`.
    Force,
}

/// What `sys::signal` delivers: the public [`AdoptedSignal`] plus suspend and resume, which
/// are separate methods so the public enum stays exhaustive and unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Delivery {
    Graceful,
    Force,
    Stop,
    Continue,
}

impl From<AdoptedSignal> for Delivery {
    fn from(signal: AdoptedSignal) -> Self {
        match signal {
            AdoptedSignal::Graceful => Self::Graceful,
            AdoptedSignal::Force => Self::Force,
        }
    }
}

/// The outcome of [`AdoptedIdentity::signal`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalResult {
    /// The identity matched and the signal was sent.
    Delivered,
    /// No process has the pid any more.
    Gone,
    /// The pid names a different process; nothing was sent.
    IdentityMismatch,
}

/// The result of [`sweep`], as pids.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SweepReport {
    /// Signalled and no longer present.
    pub terminated: Vec<u32>,
    /// Already absent when the sweep reached them.
    pub gone: Vec<u32>,
    /// The pid names a different process now; nothing was sent.
    pub refused: Vec<u32>,
    /// Signalled (forced) and still present at the end of the sweep.
    pub survivors: Vec<u32>,
    /// The signal could not be sent (for example, permission denied).
    pub failed: Vec<u32>,
}

impl AdoptedIdentity {
    /// Reads the identity of `pid`. `Ok(None)` when no process has that pid. The caller is
    /// responsible for `pid` naming the intended process at this moment (it was just spawned
    /// by a launcher, or it was just read from the owner's own record).
    pub fn capture(pid: NonZeroU32) -> io::Result<Option<Self>> {
        Ok(sys::read(pid.get())?.map(|seen| Self {
            pid,
            start: seen.start,
            exe: seen.exe,
        }))
    }

    /// Rebuilds an identity persisted earlier (a pid file that also records
    /// [`start_token`](Self::start_token) and [`exe`](Self::exe)).
    pub fn from_recorded(pid: NonZeroU32, start: StartToken, exe: Option<PathBuf>) -> Self {
        Self { pid, start, exe }
    }

    pub fn pid(&self) -> u32 {
        self.pid.get()
    }

    pub fn start_token(&self) -> StartToken {
        self.start
    }

    pub fn exe(&self) -> Option<&std::path::Path> {
        self.exe.as_deref()
    }

    fn matches(&self, seen: &sys::Seen) -> bool {
        // pid plus start time is the identity. A process keeps both across `exec`, and the
        // executable path changes then (on macOS `/bin/sh` and other launchers re-exec the
        // real program after the pid was captured), so the path is recorded for the caller
        // but never compared: a recycled pid has a different start time.
        seen.start == self.start
    }

    /// Verifies a pid recorded by an earlier run of this program as Unix epoch milliseconds.
    /// Returns the identity (with the host's exact start token) when the pid names a process
    /// that started within `tolerance` of `recorded_unix_ms`, and `None` when the pid names no
    /// process, names one that started at a different time (a recycled pid), or the host cannot
    /// report start times. Epoch milliseconds are a coarse name, so the tolerance matters: a
    /// start that is off by more than it is refused.
    pub fn from_recorded_unix_ms(
        pid: NonZeroU32,
        recorded_unix_ms: u64,
        tolerance: Duration,
    ) -> io::Result<Option<Self>> {
        let Some(current) = Self::capture(pid)? else {
            return Ok(None);
        };
        let Some(started) = current.start_time() else {
            return Ok(None);
        };
        let Some(recorded) =
            SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(recorded_unix_ms))
        else {
            return Ok(None);
        };
        let gap = match started.duration_since(recorded) {
            Ok(gap) => gap,
            Err(early) => early.duration(),
        };
        Ok((gap <= tolerance).then_some(current))
    }

    /// When the process started, as a wall-clock time: exact on macOS (microseconds) and
    /// Windows (100 ns); on Linux, the boot time plus the start ticks (one clock tick of
    /// resolution). `None` where the host reports no start time or the value is out of range.
    pub fn start_time(&self) -> Option<SystemTime> {
        sys::start_time(self.start)
    }

    /// Whether the pid still names the remembered process.
    pub fn verify(&self) -> io::Result<IdentityCheck> {
        Ok(match sys::read(self.pid())? {
            None => IdentityCheck::Gone,
            Some(seen) if self.matches(&seen) => IdentityCheck::Same,
            Some(_) => IdentityCheck::Replaced,
        })
    }

    /// Whether the remembered process is still running. A zombie (exited, awaiting its own
    /// parent's reap) is dead here although [`verify`](Self::verify) still names it `Same`;
    /// this crate never reaps it. Windows has no zombies: an exited process already reads
    /// `Gone`.
    pub fn is_running(&self) -> io::Result<bool> {
        Ok(self.verify()? == IdentityCheck::Same && !process_is_zombie(self.pid()))
    }

    /// Confirmed gone: the remembered process itself is not running any more (the pid names
    /// nothing, names a different process, or is a dead zombie). Descendants and a process
    /// group are separate observations: take [`descendants`](Self::descendants) while the
    /// root lives and check each, or wait on the group with `wait_process_group_empty`.
    pub fn confirmed_gone(&self) -> io::Result<bool> {
        Ok(!self.is_running()?)
    }

    /// Waits at most `timeout` for [`confirmed_gone`](Self::confirmed_gone); `Ok(false)` when
    /// the remembered process is still running then. Sends no signal.
    pub fn wait_confirmed_gone(&self, timeout: Duration) -> io::Result<bool> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            if self.confirmed_gone()? {
                return Ok(true);
            }
            let Some(deadline) = deadline else {
                return Ok(false);
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            thread::sleep(SWEEP_POLL.min(remaining));
        }
    }

    /// Identity-pinned graceful quit: one `Graceful` signal to exactly this process, then at
    /// most `timeout` of waiting for it to be confirmed gone. Never escalates; a target that
    /// stays up yields `Ok(false)`. A pid that changed hands is never signalled.
    pub fn quit_gracefully(&self, timeout: Duration) -> io::Result<bool> {
        match self.signal(AdoptedSignal::Graceful)? {
            SignalResult::Gone | SignalResult::IdentityMismatch => self.confirmed_gone(),
            SignalResult::Delivered => self.wait_confirmed_gone(timeout),
        }
    }

    /// Sends `signal` only if the pid still names the remembered process. Never signals this
    /// process: that is `InvalidInput`.
    pub fn signal(&self, signal: AdoptedSignal) -> io::Result<SignalResult> {
        if self.pid() == std::process::id() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing to signal this process",
            ));
        }
        sys::signal(self, signal.into())
    }

    /// Suspends the process (`SIGSTOP`) only if the pid still names the remembered process.
    /// Unix only; Windows returns `Unsupported`. Never signals this process (`InvalidInput`).
    pub fn suspend(&self) -> io::Result<SignalResult> {
        self.deliver(Delivery::Stop)
    }

    /// Resumes a process stopped by [`suspend`](Self::suspend) (`SIGCONT`) only if the pid
    /// still names the remembered process. Unix only; Windows returns `Unsupported`.
    pub fn resume(&self) -> io::Result<SignalResult> {
        self.deliver(Delivery::Continue)
    }

    fn deliver(&self, delivery: Delivery) -> io::Result<SignalResult> {
        if self.pid() == std::process::id() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing to signal this process",
            ));
        }
        sys::signal(self, delivery)
    }

    /// The descendants of this process, parents before children, each with its own identity.
    /// Empty when this process is gone or the pid now names a different one. A child is
    /// accepted only if it did not start before its parent, so a stale parent link to a
    /// recycled pid adds nothing.
    pub fn descendants(&self) -> io::Result<Vec<AdoptedIdentity>> {
        if self.verify()? != IdentityCheck::Same {
            return Ok(Vec::new());
        }
        let pairs = sys::process_pairs()?;
        let mut found: Vec<AdoptedIdentity> = Vec::new();
        // (pid, start) of every process whose children are still to be looked up.
        let mut frontier = vec![(self.pid(), self.start)];
        let mut next = 0;
        while next < frontier.len() {
            let (parent, parent_start) = frontier[next];
            next += 1;
            for &(child, claimed_parent) in &pairs {
                if claimed_parent != parent
                    || child == parent
                    || child == self.pid()
                    || frontier.iter().any(|(pid, _)| *pid == child)
                {
                    continue;
                }
                let (Some(pid), Ok(Some(seen))) = (NonZeroU32::new(child), sys::read(child)) else {
                    continue;
                };
                if seen.parent.is_some_and(|actual| actual != parent) || seen.start < parent_start {
                    continue; // the pid changed hands since the table was read
                }
                frontier.push((child, seen.start));
                found.push(AdoptedIdentity {
                    pid,
                    start: seen.start,
                    exe: seen.exe,
                });
            }
        }
        // The root must have stayed itself for the whole scan.
        if self.verify()? != IdentityCheck::Same {
            return Ok(Vec::new());
        }
        Ok(found)
    }
}

const SWEEP_POLL: Duration = Duration::from_millis(20);
const FORCE_WAIT: Duration = Duration::from_millis(500);

/// Stops every process in `targets` that is still the process it was remembered as: a graceful
/// signal to each, up to `grace` for them to leave, then a forced signal to those still
/// present. A target whose pid names a different process is `refused`, never signalled.
/// Targets are signalled in the order given; pass leaves first (the reverse of
/// [`AdoptedIdentity::descendants`]).
pub fn sweep(targets: &[AdoptedIdentity], grace: Duration) -> io::Result<SweepReport> {
    let mut report = SweepReport::default();
    let mut pending: Vec<&AdoptedIdentity> = Vec::new();
    for target in targets {
        match target.signal(AdoptedSignal::Graceful) {
            Ok(SignalResult::Delivered) => pending.push(target),
            Ok(SignalResult::Gone) => report.gone.push(target.pid()),
            Ok(SignalResult::IdentityMismatch) => report.refused.push(target.pid()),
            Err(_) => report.failed.push(target.pid()),
        }
    }
    drain(&mut pending, grace, &mut report)?;
    let mut forced: Vec<&AdoptedIdentity> = Vec::new();
    for target in pending {
        match target.signal(AdoptedSignal::Force) {
            Ok(SignalResult::Delivered) => forced.push(target),
            Ok(SignalResult::Gone) => report.terminated.push(target.pid()),
            Ok(SignalResult::IdentityMismatch) => report.refused.push(target.pid()),
            Err(_) => report.failed.push(target.pid()),
        }
    }
    drain(&mut forced, FORCE_WAIT, &mut report)?;
    report
        .survivors
        .extend(forced.iter().map(|target| target.pid()));
    Ok(report)
}

/// [`sweep`] of `root`'s current descendants, leaves first. The root itself is left alone.
pub fn sweep_descendants(root: &AdoptedIdentity, grace: Duration) -> io::Result<SweepReport> {
    let mut targets = root.descendants()?;
    targets.reverse();
    sweep(&targets, grace)
}

/// Whether `pid` names an exited-but-unreaped process (a zombie). Linux/Android read the
/// state field of `/proc/<pid>/stat`; macOS reads `pbi_status` with `proc_pidinfo` in its
/// zombies-allowed form. Anything unreadable, another Unix, and Windows (no zombies) read
/// `false`: only a positively identified zombie is one, so callers fall back to existence.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn process_is_zombie(pid: u32) -> bool {
    let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // `pid (comm) state ...`: comm may hold spaces and parentheses, so start after the last ')'.
    text.rsplit_once(')')
        .is_some_and(|(_, rest)| matches!(rest.trim_start().chars().next(), Some('Z' | 'X' | 'x')))
}

#[cfg(target_os = "macos")]
pub(crate) fn process_is_zombie(pid: u32) -> bool {
    /// `SZOMB` from `<sys/proc.h>`: process is a zombie state.
    const SZOMB: u32 = 5;
    // SAFETY: an all-zero `proc_bsdinfo` is a valid value (plain integers and char arrays).
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `info` is a writable buffer of exactly `size` bytes; arg 1 describes zombies.
    let written = unsafe {
        libc::proc_pidinfo(
            pid as libc::pid_t,
            libc::PROC_PIDTBSDINFO,
            1,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    written == size && info.pbi_pid == pid && info.pbi_status == SZOMB
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
pub(crate) fn process_is_zombie(_pid: u32) -> bool {
    false
}

/// Observation-only existence probe for a Unix process group: sends signal 0 and nothing
/// else. `EPERM` means members exist that this process may not signal, so the group is
/// present. Group 1 (init's group) and any id that cannot name a group are refused with
/// `InvalidInput`; `NonZeroU32` already excludes 0, which would name this process's own
/// group. A group holding only zombies whose parent is elsewhere stays present until they
/// are reaped: this is a group observation, not a per-member liveness check (use
/// [`AdoptedIdentity::is_running`] for a remembered member).
#[cfg(unix)]
pub fn process_group_exists(pgid: NonZeroU32) -> io::Result<bool> {
    let group = check_group_id(pgid)?;
    loop {
        // SAFETY: signal 0 performs only existence and permission checks; nothing is sent.
        if unsafe { libc::killpg(group, 0) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EPERM) => return Ok(true),
            Some(libc::ESRCH) => return Ok(false),
            _ => return Err(error),
        }
    }
}

/// Non-Unix hosts have no process groups to observe.
#[cfg(not(unix))]
pub fn process_group_exists(_pgid: NonZeroU32) -> io::Result<bool> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// Waits at most `timeout` for a process group to empty; `Ok(false)` when members remain
/// then. Observation only (see [`process_group_exists`]); it never signals the group.
pub fn wait_process_group_empty(pgid: NonZeroU32, timeout: Duration) -> io::Result<bool> {
    #[cfg(not(unix))]
    {
        let _ = timeout;
        return process_group_exists(pgid);
    }
    #[cfg(unix)]
    {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            if !process_group_exists(pgid)? {
                return Ok(true);
            }
            let Some(deadline) = deadline else {
                return Ok(false);
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            thread::sleep(SWEEP_POLL.min(remaining));
        }
    }
}

/// A group id this crate will observe: never 0 (excluded by the type), 1 (init's group), or
/// a value the kernel's signed pid type cannot hold.
#[cfg(unix)]
pub(crate) fn check_group_id(pgid: NonZeroU32) -> io::Result<libc::pid_t> {
    if pgid.get() == 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to observe process group 1",
        ));
    }
    libc::pid_t::try_from(pgid.get()).map_err(io::Error::other)
}

/// Suspends the current process with `SIGTSTP` (the terminal Ctrl-Z stop), so a shell or TUI
/// can hand the terminal back and resume on `SIGCONT`. Returns once the process is continued.
/// Unix only.
#[cfg(unix)]
pub fn suspend_current_process() -> io::Result<()> {
    // SAFETY: raise takes a plain signal number.
    if unsafe { libc::raise(libc::SIGTSTP) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Windows has no `SIGTSTP`: returns `Unsupported` without stopping anything.
#[cfg(not(unix))]
pub fn suspend_current_process() -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// The Unix signal number for `signal`.
#[cfg(unix)]
fn unix_signal_number(delivery: Delivery) -> libc::c_int {
    match delivery {
        Delivery::Graceful => libc::SIGTERM,
        Delivery::Force => libc::SIGKILL,
        Delivery::Stop => libc::SIGSTOP,
        Delivery::Continue => libc::SIGCONT,
    }
}

/// The process groups that belong to session `sid` (the session id is the pid of its leader),
/// one entry per group, excluding this process's own group. Found by one scan of the process
/// table; a group that starts after the scan is not listed.
#[cfg(unix)]
pub(crate) fn session_groups(sid: NonZeroU32) -> io::Result<Vec<libc::pid_t>> {
    let wanted = sid.get() as libc::pid_t;
    // SAFETY: getpgrp takes no arguments.
    let own_group = unsafe { libc::getpgrp() };
    let mut groups: Vec<libc::pid_t> = Vec::new();
    for pid in sys::list_pids()? {
        let pid = pid as libc::pid_t;
        // SAFETY: getsid and getpgid take a pid and return -1 when it names no process.
        if unsafe { libc::getsid(pid) } != wanted {
            continue;
        }
        let group = unsafe { libc::getpgid(pid) };
        if group > 0 && group != own_group && !groups.contains(&group) {
            groups.push(group);
        }
    }
    Ok(groups)
}

/// Sends `signal` to every process group in session `sid` (see [`session_groups`]). Returns
/// `true` when at least one group was found and reached. Unix only. Best effort: the groups are
/// found by a process-table scan and signalled by group id, so a group that exits and whose id
/// is reused between the scan and the signal can be hit. Use it for a session the caller
/// created (a pseudo-terminal leader), not for an arbitrary session.
#[cfg(unix)]
pub fn signal_session(sid: NonZeroU32, signal: AdoptedSignal) -> io::Result<bool> {
    signal_session_with(sid, signal.into())
}

/// Suspends (`SIGSTOP`) every process group in session `sid`; same contract as
/// [`signal_session`]. Unix only.
#[cfg(unix)]
pub fn suspend_session(sid: NonZeroU32) -> io::Result<bool> {
    signal_session_with(sid, Delivery::Stop)
}

/// Resumes (`SIGCONT`) every process group in session `sid`; same contract as
/// [`signal_session`]. Unix only.
#[cfg(unix)]
pub fn resume_session(sid: NonZeroU32) -> io::Result<bool> {
    signal_session_with(sid, Delivery::Continue)
}

#[cfg(unix)]
fn signal_session_with(sid: NonZeroU32, delivery: Delivery) -> io::Result<bool> {
    let number = unix_signal_number(delivery);
    let mut reached = false;
    for group in session_groups(sid)? {
        // SAFETY: kill takes two integers; a negative pid names a process group.
        if unsafe { libc::kill(-group, number) } == 0 {
            reached = true;
            continue;
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(reached)
}

/// Waits until `deadline` for each pending target to stop being the remembered process; those
/// that did are moved to `report.terminated`.
fn drain(
    pending: &mut Vec<&AdoptedIdentity>,
    wait: Duration,
    report: &mut SweepReport,
) -> io::Result<()> {
    let deadline = Instant::now().checked_add(wait);
    loop {
        let mut still = Vec::new();
        for target in pending.drain(..) {
            // Zombie-aware: a foreign zombie is dead though its identity still verifies;
            // waiting for its (foreign) parent to reap it could wait for ever, and this
            // crate must not reap it.
            if target.is_running()? {
                still.push(target);
            } else {
                report.terminated.push(target.pid());
            }
        }
        *pending = still;
        let expired = deadline.is_none_or(|limit| Instant::now() >= limit);
        if pending.is_empty() || expired {
            return Ok(());
        }
        thread::sleep(SWEEP_POLL);
    }
}

mod sys {
    use super::StartToken;
    #[cfg(not(windows))]
    use super::{AdoptedIdentity, SignalResult};
    #[cfg(not(windows))]
    use std::io;
    use std::path::PathBuf;

    /// One look at a process.
    pub(super) struct Seen {
        pub(super) start: StartToken,
        pub(super) exe: Option<PathBuf>,
        /// The parent pid when the host reports it with the process (Unix); Windows reads it
        /// from the process table only.
        pub(super) parent: Option<u32>,
    }

    /// Converts a host start token to wall-clock time (see `AdoptedIdentity::start_time`).
    #[cfg(target_os = "macos")]
    pub(super) fn start_time(start: StartToken) -> Option<std::time::SystemTime> {
        // macOS tokens are the kernel's start time: seconds and microseconds since the epoch.
        let micros = u32::try_from(start.minor).ok()?;
        std::time::SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::new(
            start.major,
            micros.checked_mul(1000)?,
        ))
    }

    /// Linux tokens are clock ticks since boot: boot time (`btime` in /proc/stat) plus ticks.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(super) fn start_time(start: StartToken) -> Option<std::time::SystemTime> {
        // SAFETY: sysconf takes a plain name constant and returns a value.
        let hertz = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).ok()?;
        if hertz == 0 {
            return None;
        }
        let boot = std::fs::read_to_string("/proc/stat")
            .ok()?
            .lines()
            .find_map(|line| line.strip_prefix("btime "))
            .and_then(|value| value.trim().parse::<u64>().ok())?;
        let since_boot = std::time::Duration::new(
            start.major / hertz,
            u32::try_from((start.major % hertz) * 1_000_000_000 / hertz).ok()?,
        );
        std::time::SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(boot))?
            .checked_add(since_boot)
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "linux", target_os = "android"))
    ))]
    pub(super) fn start_time(_start: StartToken) -> Option<std::time::SystemTime> {
        None
    }

    /// Windows tokens are FILETIME: 100 ns ticks since 1601-01-01.
    #[cfg(windows)]
    pub(super) fn start_time(start: StartToken) -> Option<std::time::SystemTime> {
        /// 1601-01-01 to 1970-01-01 in 100 ns ticks.
        const EPOCH_OFFSET_TICKS: u64 = 116_444_736_000_000_000;
        let since_unix = start.major.checked_sub(EPOCH_OFFSET_TICKS)?;
        std::time::SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::new(
            since_unix / 10_000_000,
            u32::try_from((since_unix % 10_000_000) * 100).ok()?,
        ))
    }

    #[cfg(not(any(unix, windows)))]
    pub(super) fn start_time(_start: StartToken) -> Option<std::time::SystemTime> {
        None
    }

    #[cfg(unix)]
    pub(super) fn read(pid: u32) -> io::Result<Option<Seen>> {
        Ok(crate::identity::describe(pid)?.map(|identity| Seen {
            start: StartToken {
                major: identity.started_sec,
                minor: identity.started_usec,
            },
            exe: exe_path(pid),
            parent: Some(identity.parent),
        }))
    }

    #[cfg(target_os = "macos")]
    fn exe_path(pid: u32) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStringExt;
        // PROC_PIDPATHINFO_MAXSIZE is 4 * MAXPATHLEN.
        let mut buffer = vec![0u8; 4096];
        // SAFETY: the buffer is writable for exactly `buffer.len()` bytes.
        let written = unsafe {
            libc::proc_pidpath(
                pid as libc::c_int,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
            )
        };
        if written <= 0 {
            return None;
        }
        buffer.truncate(written as usize);
        Some(PathBuf::from(std::ffi::OsString::from_vec(buffer)))
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn exe_path(pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/exe")).ok()
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "linux", target_os = "android"))
    ))]
    fn exe_path(_pid: u32) -> Option<PathBuf> {
        None
    }

    #[cfg(unix)]
    pub(super) fn signal(
        identity: &AdoptedIdentity,
        signal: super::Delivery,
    ) -> io::Result<SignalResult> {
        use super::IdentityCheck;
        match identity.verify()? {
            IdentityCheck::Gone => return Ok(SignalResult::Gone),
            IdentityCheck::Replaced => return Ok(SignalResult::IdentityMismatch),
            IdentityCheck::Same => {}
        }
        if super::process_is_zombie(identity.pid()) {
            return Ok(SignalResult::Gone);
        }
        let pid = i32::try_from(identity.pid()).map_err(io::Error::other)?;
        let number = super::unix_signal_number(signal);
        // SAFETY: kill takes two integers; the pid was proven to be the remembered process
        // just above (see the module documentation for the residual gap).
        if unsafe { libc::kill(pid, number) } == 0 {
            return Ok(SignalResult::Delivered);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(SignalResult::Gone),
            _ => Err(error),
        }
    }

    /// Every (pid, parent pid) pair in the process table.
    #[cfg(unix)]
    pub(super) fn process_pairs() -> io::Result<Vec<(u32, u32)>> {
        let mut pairs = Vec::new();
        for pid in list_pids()? {
            if let Ok(Some(identity)) = crate::identity::describe(pid) {
                pairs.push((pid, identity.parent));
            }
        }
        Ok(pairs)
    }

    #[cfg(target_os = "macos")]
    pub(super) fn list_pids() -> io::Result<Vec<u32>> {
        /// `PROC_ALL_PIDS` from `<libproc.h>`.
        const PROC_ALL_PIDS: u32 = 1;
        let size = std::mem::size_of::<libc::pid_t>();
        // SAFETY: a null buffer asks only for the size needed.
        let needed = unsafe { libc::proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
        if needed <= 0 {
            return Err(io::Error::last_os_error());
        }
        // Room for processes that start between the two calls.
        let mut pids = vec![0 as libc::pid_t; needed as usize / size + 64];
        // SAFETY: the buffer holds `pids.len()` pids and the byte size passed says so.
        let written = unsafe {
            libc::proc_listpids(
                PROC_ALL_PIDS,
                0,
                pids.as_mut_ptr().cast(),
                (pids.len() * size) as libc::c_int,
            )
        };
        if written <= 0 {
            return Err(io::Error::last_os_error());
        }
        pids.truncate(written as usize / size);
        Ok(pids
            .into_iter()
            .filter(|pid| *pid > 0)
            .map(|pid| pid as u32)
            .collect())
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(super) fn list_pids() -> io::Result<Vec<u32>> {
        Ok(std::fs::read_dir("/proc")?
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .collect())
    }

    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "linux", target_os = "android"))
    ))]
    pub(super) fn list_pids() -> io::Result<Vec<u32>> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    #[cfg(windows)]
    mod windows_impl {
        use super::super::{AdoptedIdentity, SignalResult, StartToken};
        use super::Seen;
        use crate::process_windows::os_error;
        use std::{io, path::PathBuf};
        use windows::{
            core::PWSTR,
            Win32::{
                Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, HANDLE},
                System::{
                    Diagnostics::ToolHelp::{
                        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                        TH32CS_SNAPPROCESS,
                    },
                    Threading::{
                        GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
                        WaitForSingleObject, PROCESS_ACCESS_RIGHTS, PROCESS_NAME_WIN32,
                        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
                    },
                },
            },
        };

        /// A kernel handle closed on drop.
        struct Owned(HANDLE);
        impl Drop for Owned {
            fn drop(&mut self) {
                // SAFETY: the handle came from a successful open or snapshot and is closed once.
                let _ = unsafe { CloseHandle(self.0) };
            }
        }

        fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> io::Result<Option<Owned>> {
            // SAFETY: OpenProcess takes plain values and returns a new handle.
            match unsafe { OpenProcess(access, false, pid) } {
                Ok(handle) => Ok(Some(Owned(handle))),
                Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => Ok(None),
                Err(error) => Err(os_error(error)),
            }
        }

        /// `None` once the process has exited (its handle can outlive it).
        fn observe(process: &Owned) -> io::Result<Option<Seen>> {
            // SAFETY: the handle is open with SYNCHRONIZE.
            if unsafe { WaitForSingleObject(process.0, 0) }.0 == 0 {
                return Ok(None);
            }
            let (mut created, mut exited, mut kernel, mut user) = (
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
            );
            // SAFETY: the handle is open with QUERY_LIMITED_INFORMATION; the outputs are valid.
            unsafe {
                GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user)
            }
            .map_err(os_error)?;
            let ticks =
                (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
            let mut buffer = vec![0u16; 32768];
            let mut length = buffer.len() as u32;
            // SAFETY: the buffer holds `length` UTF-16 units.
            let exe = unsafe {
                QueryFullProcessImageNameW(
                    process.0,
                    PROCESS_NAME_WIN32,
                    PWSTR(buffer.as_mut_ptr()),
                    &mut length,
                )
            }
            .ok()
            .map(|()| PathBuf::from(String::from_utf16_lossy(&buffer[..length as usize])));
            Ok(Some(Seen {
                start: StartToken {
                    major: ticks,
                    minor: 0,
                },
                exe,
                parent: None,
            }))
        }

        pub(in super::super) fn read(pid: u32) -> io::Result<Option<Seen>> {
            let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
            match open(pid, access)? {
                Some(process) => observe(&process),
                None => Ok(None),
            }
        }

        pub(in super::super) fn signal(
            identity: &AdoptedIdentity,
            signal: super::super::Delivery,
        ) -> io::Result<SignalResult> {
            if matches!(
                signal,
                super::super::Delivery::Stop | super::super::Delivery::Continue
            ) {
                // Windows has no portable suspend or resume of another process.
                return Err(io::Error::from(io::ErrorKind::Unsupported));
            }
            let access =
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE;
            let Some(process) = open(identity.pid(), access)? else {
                return Ok(SignalResult::Gone);
            };
            // Proof and act use the same handle: the pid cannot change hands in between.
            let Some(seen) = observe(&process)? else {
                return Ok(SignalResult::Gone);
            };
            if !identity.matches(&seen) {
                return Ok(SignalResult::IdentityMismatch);
            }
            // SAFETY: the handle is open with PROCESS_TERMINATE.
            unsafe { TerminateProcess(process.0, 1) }.map_err(os_error)?;
            Ok(SignalResult::Delivered)
        }

        pub(in super::super) fn process_pairs() -> io::Result<Vec<(u32, u32)>> {
            // SAFETY: a snapshot request takes plain values.
            let snapshot = Owned(
                unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.map_err(os_error)?,
            );
            // SAFETY: PROCESSENTRY32W is plain data; dwSize is set below as the API requires.
            let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            // SAFETY: the snapshot is open and `entry` is a valid, sized entry.
            unsafe { Process32FirstW(snapshot.0, &mut entry) }.map_err(os_error)?;
            let mut pairs = Vec::new();
            loop {
                pairs.push((entry.th32ProcessID, entry.th32ParentProcessID));
                // SAFETY: as above.
                if unsafe { Process32NextW(snapshot.0, &mut entry) }.is_err() {
                    break;
                }
            }
            Ok(pairs)
        }
    }

    #[cfg(windows)]
    pub(super) use windows_impl::{process_pairs, read, signal};

    #[cfg(not(any(unix, windows)))]
    pub(super) fn read(_pid: u32) -> io::Result<Option<Seen>> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
    #[cfg(not(any(unix, windows)))]
    pub(super) fn signal(_: &AdoptedIdentity, _: super::Delivery) -> io::Result<SignalResult> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
    #[cfg(not(any(unix, windows)))]
    pub(super) fn process_pairs() -> io::Result<Vec<(u32, u32)>> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(all(test, unix))]
mod suspend_resume_tests {
    use super::{AdoptedIdentity, SignalResult};
    use std::{
        num::NonZeroU32,
        process::{Command, Stdio},
        thread,
        time::Duration,
    };

    fn state(pid: u32) -> String {
        let out = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("ps");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn wait_for(pid: u32, stopped: bool) -> bool {
        for _ in 0..100 {
            if state(pid).starts_with('T') == stopped {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn suspend_then_resume_stops_and_continues_the_identified_process() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = NonZeroU32::new(child.id()).unwrap();
        let identity = AdoptedIdentity::capture(pid)
            .expect("capture")
            .expect("identity");
        assert_eq!(identity.suspend().unwrap(), SignalResult::Delivered);
        assert!(wait_for(pid.get(), true), "process did not stop");
        assert_eq!(identity.resume().unwrap(), SignalResult::Delivered);
        assert!(wait_for(pid.get(), false), "process did not continue");
        child.kill().expect("kill");
        let _ = child.wait();
        assert_eq!(identity.suspend().unwrap(), SignalResult::Gone);
    }

    #[test]
    fn suspend_and_resume_refuse_this_process() {
        let pid = NonZeroU32::new(std::process::id()).unwrap();
        let identity = AdoptedIdentity::capture(pid)
            .expect("capture")
            .expect("identity");
        assert_eq!(
            identity.suspend().unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(
            identity.resume().unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
}
