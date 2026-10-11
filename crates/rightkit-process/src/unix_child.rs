//! The Unix child shared by `OwnedChild` and every `ProcessHandle` clone, and the one place a
//! signal to a child or its process group is sent.
//!
//! # Identity rule
//!
//! A pid, and with it the process-group id the child leads, cannot be recycled while the
//! child is a zombie: the kernel releases the number only when the parent reaps it, and only
//! this process can reap its own child. So a group signal is safe exactly while this process
//! still holds the leader as its own **unreaped** child.
//!
//! * Every reap goes through [`UnixChild`], under the lock that guards it. The std `Child` is
//!   private, so no path can reap behind this type's back.
//! * Every group signal is sent under that same lock, after the leader is confirmed unreaped,
//!   and therefore before any reap that could follow it.
//! * After any reap, by any path (a wait, a `try_wait`, a handle or a clone, the detached
//!   reaper, the public [`OwnedChild::child`](crate::OwnedChild::child) accessor), the
//!   state is `Reaped` for good. Group signalling is over for this child and all its clones:
//!   no signal, and no signal-0 membership probe, is ever sent again.
//!
//! The state is one lock-guarded enum, never separate atomics, so no thread can observe a
//! half-published transition.

use std::{
    io,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, ExitStatus},
};

/// What this process knows about the leader. Only [`UnixChild`] changes it, under the lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Leader {
    /// Not reaped by this process: the zombie, or the running process, still pins the id.
    Unreaped,
    /// Reaped through this type; the cached status is returned to every later reader.
    Reaped(ExitStatus),
    /// The kernel says the pid is no longer our child although we never reaped it (something
    /// else in the process reaped it). Treated as reaped; the status is lost.
    Lost,
}

/// Where the leader is, as far as a non-reaping look can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LeaderState {
    Running,
    /// Exited but not reaped: a zombie that still pins the id. The last moment a group
    /// signal is possible.
    Exited,
    Reaped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignalTarget {
    /// `kill(-pgid, ..)`: the leader's whole process group.
    Group,
    /// `kill(pid, ..)`: the leader alone (detached children, which own only their pid).
    Leader,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignalOutcome {
    Sent,
    /// The leader was already reaped, so nothing was sent.
    Retired,
    /// The target was already gone (`ESRCH`, or only zombies were left).
    Gone,
}

/// A spawned Unix child. The same API as Windows' `WindowsChild`: public stdio fields, `id`,
/// `try_wait`, `wait` and `kill`. It does not expose the std `Child`, because a std reap the
/// crate cannot see would make later group signals unsafe (see the module documentation).
///
/// Obtained through [`OwnedChild::child`](crate::OwnedChild::child), which holds the shared
/// lock while the guard lives. `wait` blocks while holding it; prefer `OwnedChild::wait` or
/// `ProcessHandle::wait`, which do not.
#[derive(Debug)]
pub struct UnixChild {
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    child: Child,
    pid: u32,
    leader: Leader,
    /// macOS parent-death helper: our own child, killed and reaped when this value drops.
    helper: Option<libc::pid_t>,
    /// Test only: the id used as the signal target, to stand in for a recycled id.
    #[cfg(test)]
    recycled_id: Option<u32>,
}

impl UnixChild {
    pub(crate) fn new(mut child: Child, helper: Option<libc::pid_t>) -> Self {
        Self {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            pid: child.id(),
            child,
            leader: Leader::Unreaped,
            helper,
            #[cfg(test)]
            recycled_id: None,
        }
    }

    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Non-blocking. A `Some` result means the leader has been reaped, and ends group
    /// signalling for this child.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self.leader {
            Leader::Reaped(status) => return Ok(Some(status)),
            Leader::Lost => return Err(io::Error::from_raw_os_error(libc::ECHILD)),
            Leader::Unreaped => {}
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.note_reaped(status);
                Ok(Some(status))
            }
            Ok(None) => Ok(None),
            Err(error) => Err(self.note_error(error)),
        }
    }

    /// Closes stdin as std does, then blocks until the leader is reaped. The lock stays held
    /// for the whole wait.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        drop(self.stdin.take());
        self.wait_reaping()
    }

    /// `SIGKILL` to the leader alone, only while it is unreaped. Nothing is sent, and `Ok` is
    /// returned, once it has been reaped.
    pub fn kill(&mut self) -> io::Result<()> {
        self.signal(SignalTarget::Leader, libc::SIGKILL).map(|_| ())
    }

    /// Reaps without touching stdin, for independent readers that must not close the owner's.
    pub(crate) fn wait_reaping(&mut self) -> io::Result<ExitStatus> {
        match self.leader {
            Leader::Reaped(status) => return Ok(status),
            Leader::Lost => return Err(io::Error::from_raw_os_error(libc::ECHILD)),
            Leader::Unreaped => {}
        }
        match self.child.wait() {
            Ok(status) => {
                self.note_reaped(status);
                Ok(status)
            }
            Err(error) => Err(self.note_error(error)),
        }
    }

    /// Looks at the leader without reaping it. `Lost` is recorded here when the kernel no
    /// longer lists the pid as our child.
    pub(crate) fn leader_state(&mut self) -> io::Result<LeaderState> {
        if !matches!(self.leader, Leader::Unreaped) {
            return Ok(LeaderState::Reaped);
        }
        match probe_exit(self.pid) {
            Ok(true) => Ok(LeaderState::Exited),
            Ok(false) => Ok(LeaderState::Running),
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
                self.leader = Leader::Lost;
                Ok(LeaderState::Reaped)
            }
            Err(error) => Err(error),
        }
    }

    /// Sends `signal` only if the leader is still unreaped, and does so under the caller's
    /// lock, so no reap can fall between the check and the signal.
    pub(crate) fn signal(
        &mut self,
        target: SignalTarget,
        signal: i32,
    ) -> io::Result<SignalOutcome> {
        if self.leader_state()? == LeaderState::Reaped {
            return Ok(SignalOutcome::Retired);
        }
        let id = self.signal_id() as i32;
        let target_id = match target {
            SignalTarget::Group => -id,
            SignalTarget::Leader => id,
        };
        let mut denied = 0;
        loop {
            if raw_kill(self.pid, target_id, signal) == 0 {
                return Ok(SignalOutcome::Sent);
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::ESRCH) => return Ok(SignalOutcome::Gone),
                // macOS answers a group signal with EPERM, not ESRCH, when no member can
                // take it: only zombies are left, or the last members are mid-exit. Our own
                // children are always signalable, so this is never a real denial. Once the
                // leader is no longer running (exited, or reaped by a handle clone) the group
                // is gone. A leader that still reads as running is mid-exit: retry briefly
                // (the lock is held, so nothing can reap it meanwhile) before giving up.
                Some(libc::EPERM) if target == SignalTarget::Group => {
                    if self.leader_state()? != LeaderState::Running {
                        return Ok(SignalOutcome::Gone);
                    }
                    denied += 1;
                    if denied >= 100 {
                        return Err(error);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                _ => return Err(error),
            }
        }
    }

    /// True when a process other than the leader (and this child's own helper) is still in
    /// the group. Valid only while the leader is unreaped. Unknown answers are `true`: the
    /// caller then waits out its grace instead of cutting it short.
    pub(crate) fn group_has_other_members(&mut self) -> io::Result<bool> {
        if self.leader_state()? == LeaderState::Reaped {
            return Ok(false);
        }
        group_members_besides(self.pid, self.signal_id(), self.helper)
    }

    fn signal_id(&self) -> u32 {
        #[cfg(test)]
        if let Some(id) = self.recycled_id {
            return id;
        }
        self.pid
    }

    /// Test only: aim every later signal at `id`, as if the kernel had recycled the leader's
    /// pid and group id for an unrelated group.
    #[cfg(test)]
    pub(crate) fn simulate_recycled_id(&mut self, id: u32) {
        self.recycled_id = Some(id);
    }

    fn note_reaped(&mut self, status: ExitStatus) {
        self.leader = Leader::Reaped(status);
        #[cfg(test)]
        trace::record(trace::Event::Reaped { leader: self.pid });
        self.reap_helper_if_exited();
    }

    fn note_error(&mut self, error: io::Error) -> io::Error {
        if error.raw_os_error() == Some(libc::ECHILD) {
            self.leader = Leader::Lost;
        }
        error
    }

    /// Collects the helper's zombie when a group kill has already ended it. The helper is our
    /// own unreaped child until this runs, so its pid is stable until then.
    fn reap_helper_if_exited(&mut self) {
        let Some(helper) = self.helper else { return };
        let mut status = 0;
        // SAFETY: waitpid on our own child with WNOHANG.
        let reaped = unsafe { libc::waitpid(helper, &mut status, libc::WNOHANG) };
        if reaped == helper {
            self.helper = None;
        }
    }
}

impl Drop for UnixChild {
    fn drop(&mut self) {
        if let Some(helper) = self.helper.take() {
            // SAFETY: the helper is our own child and has not been reaped (the field is
            // cleared on reap), so its pid cannot name any other process.
            unsafe {
                libc::kill(helper, libc::SIGKILL);
                let mut status = 0;
                while libc::waitpid(helper, &mut status, 0) < 0
                    && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                {
                }
            }
        }
    }
}

/// `waitid(P_PID, pid, WEXITED | WNOHANG | WNOWAIT)`: has the child exited, without reaping
/// it? `ECHILD` means the pid is not (or no longer) our child.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
fn probe_exit(pid: u32) -> io::Result<bool> {
    loop {
        // SAFETY: an all-zero siginfo_t is valid, and waitid fills it.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: valid out pointer; WNOWAIT leaves the zombie in place.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            #[cfg(any(target_os = "linux", target_os = "android"))]
            // SAFETY: si_pid is valid after a successful waitid.
            let reported = unsafe { info.si_pid() };
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            let reported = info.si_pid;
            return Ok(reported as u32 == pid);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR) {
            return Err(error);
        }
    }
}

/// Other Unix hosts have no verified non-reaping probe here. The reaped state still guards
/// every signal; only the early "leader exited" observation is unavailable.
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
fn probe_exit(_pid: u32) -> io::Result<bool> {
    Ok(false)
}

/// The single place a signal reaches a child or its group, so tests can observe every one
/// (signal 0 included).
fn raw_kill(leader: u32, target: i32, signal: i32) -> i32 {
    #[cfg(test)]
    trace::record(trace::Event::Signal {
        leader,
        target,
        signal,
    });
    #[cfg(not(test))]
    let _ = leader;
    // SAFETY: plain kill(2). The callers hold the lock and have just confirmed the leader is
    // unreaped, which pins the target id.
    unsafe { libc::kill(target, signal) }
}

#[cfg(target_os = "macos")]
fn group_members_besides(leader: u32, group: u32, helper: Option<libc::pid_t>) -> io::Result<bool> {
    /// `PROC_PGRP_ONLY` from `<libproc.h>`: list the members of one process group.
    const PROC_PGRP_ONLY: u32 = 2;
    const CAPACITY: usize = 256;
    let mut pids = [0 as libc::pid_t; CAPACITY];
    // SAFETY: the buffer is CAPACITY pids and the size passed says so.
    let bytes = unsafe {
        libc::proc_listpids(
            PROC_PGRP_ONLY,
            group,
            pids.as_mut_ptr().cast(),
            std::mem::size_of_val(&pids) as libc::c_int,
        )
    };
    if bytes <= 0 {
        // Fall back to signal 0: ESRCH, or EPERM when only zombies remain, means empty.
        if raw_kill(leader, -(group as i32), 0) == 0 {
            return Ok(true);
        }
        return Ok(!matches!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH) | Some(libc::EPERM)
        ));
    }
    let count = (bytes as usize / std::mem::size_of::<libc::pid_t>()).min(CAPACITY);
    if count >= CAPACITY {
        return Ok(true);
    }
    Ok(pids[..count]
        .iter()
        .any(|&pid| pid > 0 && pid as u32 != leader && Some(pid) != helper))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn group_members_besides(
    leader: u32,
    group: u32,
    _helper: Option<libc::pid_t>,
) -> io::Result<bool> {
    // kill(-pgid, 0) cannot tell an exited, unreaped leader from a live member, so read
    // /proc: a member is any non-zombie process whose group is this one.
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Ok(true);
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == leader {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue; // exited while we looked
        };
        let Some(rest) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
            return Ok(true);
        };
        let mut fields = rest.split_whitespace();
        let (Some(state), Some(_parent), Some(pgrp)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Ok(true);
        };
        if pgrp.parse::<u32>().ok() == Some(group) && state != "Z" && state != "X" {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
fn group_members_besides(
    _leader: u32,
    _group: u32,
    _helper: Option<libc::pid_t>,
) -> io::Result<bool> {
    Ok(true)
}

/// Test instrumentation: every signal, and every reap, in the order they happened. Both are
/// recorded under the child's lock, so the order is the real order.
#[cfg(test)]
pub(crate) mod trace {
    use std::sync::Mutex;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Event {
        Signal {
            leader: u32,
            target: i32,
            signal: i32,
        },
        Reaped {
            leader: u32,
        },
    }

    static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());

    pub(crate) fn record(event: Event) {
        EVENTS.lock().unwrap_or_else(|p| p.into_inner()).push(event);
    }

    pub(crate) fn mark() -> usize {
        EVENTS.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// Events about `leader` recorded since `mark`.
    pub(crate) fn since(mark: usize, leader: u32) -> Vec<Event> {
        EVENTS.lock().unwrap_or_else(|p| p.into_inner())[mark..]
            .iter()
            .copied()
            .filter(|event| match event {
                Event::Signal { leader: l, .. } | Event::Reaped { leader: l } => *l == leader,
            })
            .collect()
    }
}
