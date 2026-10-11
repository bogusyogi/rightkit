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
//! * **Unreaped by this type is not unreaped by the kernel.** Other code in the process (a
//!   `waitpid(-1)` loop) can collect our child behind our back, and a replacement child can then
//!   take the same number. So every action that names the pid (a signal, a `waitpid`, a
//!   `waitid` probe, a membership probe) is preceded by [`UnixChild::prove`], which compares the
//!   kernel's description of the pid (parent and start time, read at spawn) with the process
//!   we spawned. A different or missing description marks the leader `Lost` and nothing is
//!   sent or waited. See `identity` for the rule and its one residual.
//!
//! * **Waiting is event-driven and does not hold the lock.** Each child owns an
//!   [`ExitWatch`](crate::exit_watch::ExitWatch) (a kqueue `NOTE_EXIT` knote, or a pidfd), opened at
//!   spawn. `ProcessHandle::wait*` and `OwnedChild::wait*` clone it out under the lock, release
//!   the lock, and sleep in `poll(2)` until the kernel reports the exit; then they take the lock,
//!   prove the pid and reap with one call that cannot block for long (the process has exited).
//!   So `kill`, `terminate_tree` and every other waiter run while a wait is blocked, and a
//!   wait costs one wake-up however long the child lives. The one exception is
//!   [`UnixChild::wait`] reached through the `OwnedChild::child()` guard: the caller already holds
//!   the lock, so that wait blocks (in `poll`, not in a loop) with the lock held, exactly as
//!   `std::process::Child::wait` would hold its owner.
//!
//! The state is one lock-guarded enum, never separate atomics, so no thread can observe a
//! half-published transition.
//!
//! * No wait-class system call names the pid while the lock is released. A blocked
//!   `waitpid(pid)`/`waitid(P_PID, pid)` outside the lock could be entered after another reader
//!   reaped the leader and the kernel recycled the number, and would then wait on an unrelated
//!   child. Waiters instead look under the lock, release it, and sleep in `poll(2)` on the child's
//!   exit watch until the kernel reports the exit.
//! * Seeing that the leader exited does not require reaping it (`UnixChild::leader_state`), so
//!   an exit monitor can observe the exit and leave the reap, and with it the group's identity,
//!   to the owner.

use crate::exit_watch::ExitWatch;
use crate::identity::{Origin, Proof};
use crate::process_unix::Helper;
use std::{
    io,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, ExitStatus},
    sync::Arc,
    time::Duration,
};

/// Whether this host can see that the leader exited without reaping it (`probe_exit` is real).
/// Where it cannot, `ProcessHandle::wait_exited` falls back to a reaping wait.
pub(crate) const CAN_OBSERVE_WITHOUT_REAPING: bool = cfg!(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
));

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

/// Who is in the leader's process group, besides the leader and its own helper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Members {
    Pids(Vec<u32>),
    /// The host cannot enumerate the group (or it is larger than one listing).
    Unknown,
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
    /// Who the leader was at spawn. Checked by [`UnixChild::prove`] before any action by pid.
    origin: Origin,
    /// The kernel's report of the leader's exit, opened at spawn. Shared with every waiter.
    watch: Arc<ExitWatch>,
    /// macOS parent-death helper. Never signalled by pid: this value tells it to stand down
    /// over a private channel when it drops, and only collects its zombie by `waitpid`.
    helper: Option<Helper>,
    /// Test only: the id used as the signal target, to stand in for a recycled id.
    #[cfg(test)]
    recycled_id: Option<u32>,
    /// Test only: ignore the pidfd and prove the pid from `origin` alone.
    #[cfg(test)]
    force_origin: bool,
}

impl UnixChild {
    pub(crate) fn new(mut child: Child, helper: Option<Helper>) -> Self {
        // Both are taken now, while the child is certainly still ours: the exit watch is bound
        // to the process, and the identity (when the watch cannot prove it exactly) names it.
        let watch = Arc::new(ExitWatch::open(child.id(), true));
        let origin = if watch.proves_exactly() {
            Origin::Unavailable
        } else {
            Origin::capture(child.id())
        };
        Self {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            pid: child.id(),
            origin,
            watch,
            child,
            leader: Leader::Unreaped,
            helper,
            #[cfg(test)]
            recycled_id: None,
            #[cfg(test)]
            force_origin: false,
        }
    }

    /// The exit watch, for waiters that block without holding the lock.
    pub(crate) fn exit_watch(&self) -> Arc<ExitWatch> {
        Arc::clone(&self.watch)
    }

    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Spawn-time check: a host that has an identity source but would not describe the child
    /// cannot prove later that its pid is still the child's, so the child is not handed out.
    /// It is killed and reaped here, where the pid is certainly still its own.
    pub(crate) fn require_identity(&mut self) -> io::Result<()> {
        if !matches!(self.origin, Origin::Missing) {
            return Ok(());
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.leader = Leader::Lost;
        Err(io::Error::other(
            "the spawned child could not be identified; it was stopped because a later \
             signal could not be proven to reach it",
        ))
    }

    /// The one proof every action by pid starts with: `Ok(true)` when the pid still names the
    /// process this type spawned and has not reaped. `Ok(false)` (and the leader becomes
    /// `Lost`, never touched again) when the number changed hands or the process is gone.
    /// `Err` when the kernel cannot be asked: the caller must then act on nothing.
    fn prove(&mut self) -> io::Result<bool> {
        if !matches!(self.leader, Leader::Unreaped) {
            return Ok(false);
        }
        #[cfg(test)]
        trace::record(trace::Event::Proof { leader: self.pid });
        // A pidfd answers for the original process whatever its number now names.
        #[cfg(test)]
        let exact = if self.force_origin {
            None
        } else {
            self.watch.exact_proof()
        };
        #[cfg(not(test))]
        let exact = self.watch.exact_proof();
        let same = match exact {
            Some(alive) => alive?,
            None => self.origin.check(self.pid)? == Proof::Same,
        };
        if same {
            Ok(true)
        } else {
            self.leader = Leader::Lost;
            Ok(false)
        }
    }

    /// `prove`, as the guard of a wait: an unproven pid is reported as `ECHILD`, as the kernel
    /// would for a child that is not ours.
    fn require_ours(&mut self) -> io::Result<()> {
        if self.prove()? {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(libc::ECHILD))
        }
    }

    /// Test only: pretend the pid was described differently at spawn, as if our child had been
    /// collected elsewhere and the number given to the process now holding it.
    #[cfg(test)]
    pub(crate) fn simulate_replaced(&mut self) {
        self.force_origin = true;
        self.origin = Origin::Known(crate::identity::Identity {
            parent: 0,
            started_sec: 1,
            started_usec: 1,
        });
    }

    /// Non-blocking. A `Some` result means the leader has been reaped, and ends group
    /// signalling for this child.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self.leader {
            Leader::Reaped(status) => return Ok(Some(status)),
            Leader::Lost => return Err(io::Error::from_raw_os_error(libc::ECHILD)),
            Leader::Unreaped => {}
        }
        if let Err(error) = self.require_ours() {
            return Err(self.note_error(error));
        }
        #[cfg(test)]
        trace::record(trace::Event::Wait { leader: self.pid });
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
    /// for the whole wait (the caller holds it); the thread sleeps in `poll`, it does not poll.
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
    ///
    /// Sleeps in the kernel until the exit event, then proves the pid and reaps (see
    /// [`reap_exited`](Self::reap_exited)). A blocking `waitpid(pid)` from the start would name
    /// the number for as long as it sleeps, and if other code collected the leader meanwhile it
    /// would wait on whatever child the kernel gave the number to, and take its status.
    pub(crate) fn wait_reaping(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.try_wait()? {
            return Ok(status);
        }
        match self.watch.wait(None) {
            Ok(_) => {}
            // No event source on this host: the reap below blocks, as std's wait does.
            Err(error) if error.kind() == io::ErrorKind::Unsupported => {}
            Err(error) => return Err(error),
        }
        self.reap_exited()
    }

    /// The reap that follows an exit event (or stands in for one on a host without events):
    /// returns the cached status, or proves the pid and collects the zombie. The process has
    /// exited, so the underlying `waitpid` returns at once. Where there is no event source it
    /// blocks until the exit instead.
    pub(crate) fn reap_exited(&mut self) -> io::Result<ExitStatus> {
        match self.leader {
            Leader::Reaped(status) => return Ok(status),
            Leader::Lost => return Err(io::Error::from_raw_os_error(libc::ECHILD)),
            Leader::Unreaped => {}
        }
        if let Err(error) = self.require_ours() {
            return Err(self.note_error(error));
        }
        #[cfg(test)]
        trace::record(trace::Event::Wait { leader: self.pid });
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
        if !self.prove()? {
            return Ok(LeaderState::Reaped);
        }
        match probe_exit(self.pid) {
            // The probe names the number once more; a replacement that took it in between and
            // has itself exited would read as our zombie. A zombie is describable, so ask again.
            Ok(true) => Ok(if self.prove()? {
                LeaderState::Exited
            } else {
                LeaderState::Reaped
            }),
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
        // A leader signal goes through the pidfd where there is one: no pid number is involved,
        // so there is no window between a proof and the signal.
        if target == SignalTarget::Leader && self.pidfd_signals() {
            if let Some(result) = self.watch.send_signal(signal) {
                #[cfg(test)]
                trace::record(trace::Event::Signal {
                    leader: self.pid,
                    target: self.pid as i32,
                    signal,
                });
                return match result {
                    Ok(()) => Ok(SignalOutcome::Sent),
                    Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
                        Ok(SignalOutcome::Gone)
                    }
                    Err(error) => Err(error),
                };
            }
        }
        // The state above looked at the pid; prove it once more immediately before it is used
        // as a signal target.
        if !self.prove()? {
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
                // is gone. A leader that still reads as running is mid-exit: wait for its exit
                // event, for at most 100 ms (the lock is held, so nothing can reap it meanwhile),
                // before giving up.
                Some(libc::EPERM) if target == SignalTarget::Group => {
                    if self.leader_state()? != LeaderState::Running {
                        return Ok(SignalOutcome::Gone);
                    }
                    // Mid-exit: wait for the leader's exit event (bounded), not for a timer.
                    denied += 1;
                    if denied >= 3
                        || !self
                            .watch
                            .wait(Some(Duration::from_millis(100)))
                            .unwrap_or(false)
                    {
                        return Err(error);
                    }
                }
                _ => return Err(error),
            }
        }
    }

    /// The processes other than the leader (and this child's own helper) still in the group.
    /// Valid only while the leader is unreaped; a reaped leader has no group to speak of. An
    /// unknown answer is [`Members::Unknown`]: the caller then waits out its grace instead of
    /// cutting it short.
    pub(crate) fn group_other_members(&mut self) -> io::Result<Members> {
        if self.leader_state()? == LeaderState::Reaped {
            return Ok(Members::Pids(Vec::new()));
        }
        group_members_besides(
            self.pid,
            self.signal_id(),
            self.helper.as_ref().map(Helper::pid),
        )
    }

    /// True when a process other than the leader is still in the group (unknown counts as yes).
    #[cfg(test)]
    pub(crate) fn group_has_other_members(&mut self) -> io::Result<bool> {
        Ok(match self.group_other_members()? {
            Members::Pids(pids) => !pids.is_empty(),
            Members::Unknown => true,
        })
    }

    /// Test hooks that aim signals elsewhere (`simulate_*`) bypass the pidfd.
    fn pidfd_signals(&self) -> bool {
        #[cfg(test)]
        if self.force_origin || self.recycled_id.is_some() {
            return false;
        }
        true
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

    /// Lets go of the helper once it is gone: reaped here, or reaped by another reaper in this
    /// process (`ECHILD`). Either way its pid is no longer ours, so nothing may ever be done to
    /// it again. The helper is our own child until then, so its pid is stable until then.
    fn reap_helper_if_exited(&mut self) {
        if self.helper.as_ref().is_some_and(Helper::has_gone) {
            self.helper = None;
        }
    }
}

impl Drop for UnixChild {
    fn drop(&mut self) {
        if let Some(helper) = self.helper.take() {
            // No signal is sent to the helper's pid: it may have been reaped elsewhere and the
            // number recycled. It is told to stand down over its private channel instead.
            helper.retire();
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
        #[cfg(test)]
        trace::record(trace::Event::Wait { leader: pid });
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
fn group_members_besides(
    leader: u32,
    group: u32,
    helper: Option<libc::pid_t>,
) -> io::Result<Members> {
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
            return Ok(Members::Unknown);
        }
        return Ok(match io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) | Some(libc::EPERM) => Members::Pids(Vec::new()),
            _ => Members::Unknown,
        });
    }
    let count = (bytes as usize / std::mem::size_of::<libc::pid_t>()).min(CAPACITY);
    if count >= CAPACITY {
        return Ok(Members::Unknown);
    }
    Ok(Members::Pids(
        pids[..count]
            .iter()
            .filter(|&&pid| pid > 0 && pid as u32 != leader && Some(pid) != helper)
            .map(|&pid| pid as u32)
            .collect(),
    ))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn group_members_besides(
    leader: u32,
    group: u32,
    _helper: Option<libc::pid_t>,
) -> io::Result<Members> {
    // kill(-pgid, 0) cannot tell an exited, unreaped leader from a live member, so read
    // /proc: a member is any non-zombie process whose group is this one.
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Ok(Members::Unknown);
    };
    let mut members = Vec::new();
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
            return Ok(Members::Unknown);
        };
        let mut fields = rest.split_whitespace();
        let (Some(state), Some(_parent), Some(pgrp)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Ok(Members::Unknown);
        };
        if pgrp.parse::<u32>().ok() == Some(group) && state != "Z" && state != "X" {
            members.push(pid);
        }
    }
    Ok(Members::Pids(members))
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
fn group_members_besides(
    _leader: u32,
    _group: u32,
    _helper: Option<libc::pid_t>,
) -> io::Result<Members> {
    Ok(Members::Unknown)
}

/// Test instrumentation: every signal, every reap and every wait-class system call that names
/// the leader's pid, in the order they happened. All are recorded under the child's lock, so the
/// order is the real order.
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
        /// The pid was proven to still name the spawned process (see `UnixChild::prove`).
        Proof {
            leader: u32,
        },
        /// A `waitpid`/`waitid` (probe or reap) was issued naming this pid. After `Reaped` the
        /// number may belong to someone else, so none may follow.
        Wait {
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

    /// Signals and reaps about `leader` recorded since `mark` (wait calls are left out; see
    /// [`since_with_waits`]).
    pub(crate) fn since(mark: usize, leader: u32) -> Vec<Event> {
        since_with_waits(mark, leader)
            .into_iter()
            .filter(|event| !matches!(event, Event::Wait { .. } | Event::Proof { .. }))
            .collect()
    }

    /// Every event about `leader` recorded since `mark`, wait calls included.
    pub(crate) fn since_with_waits(mark: usize, leader: u32) -> Vec<Event> {
        EVENTS.lock().unwrap_or_else(|p| p.into_inner())[mark..]
            .iter()
            .copied()
            .filter(|event| match event {
                Event::Signal { leader: l, .. }
                | Event::Reaped { leader: l }
                | Event::Proof { leader: l }
                | Event::Wait { leader: l } => *l == leader,
            })
            .collect()
    }
}

/// Test seam for waiters: a pause point a test can use to hold a waiter at the exact moment
/// the old blocking wait had checked the shared state and was about to name the pid.
#[cfg(test)]
pub(crate) mod gate {
    use std::sync::{Arc, Mutex};

    type Hook = Arc<dyn Fn() + Send + Sync>;

    static HOOKS: Mutex<Vec<(u32, Hook)>> = Mutex::new(Vec::new());

    /// Runs `hook` each time a waiter on `pid` reaches its pause point.
    pub(crate) fn install(pid: u32, hook: impl Fn() + Send + Sync + 'static) {
        let mut hooks = HOOKS.lock().unwrap_or_else(|p| p.into_inner());
        hooks.retain(|(id, _)| *id != pid);
        hooks.push((pid, Arc::new(hook)));
    }

    pub(crate) fn remove(pid: u32) {
        HOOKS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|(id, _)| *id != pid);
    }

    /// Called by a waiter after it released the child lock and before it sleeps.
    pub(crate) fn pause(pid: u32) {
        let hook = HOOKS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find(|(id, _)| *id == pid)
            .map(|(_, hook)| Arc::clone(hook));
        if let Some(hook) = hook {
            hook();
        }
    }
}
