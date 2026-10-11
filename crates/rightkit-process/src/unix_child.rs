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
//!   sent or waited. The proof and the call that follows it are two system calls: foreign code
//!   that reaps the child in exactly that gap, and a replacement that takes the number there,
//!   defeat it. This is a known limit; see `identity`.
//!
//! # Waiting
//!
//! * **A wait on a shared child is a loop of non-blocking looks.** [`wait_shared`] takes the
//!   lock, makes one non-blocking `try_wait` (a `waitpid(WNOHANG)`), releases the lock, sleeps,
//!   and looks again until the leader is reaped or the caller's deadline passes.
//!   [`observe_shared`] does the same with the non-reaping `waitid(WNOHANG | WNOWAIT)` probe.
//!   `ProcessHandle::wait`, `wait_timeout` and `wait_exited*`, `OwnedChild::wait*`,
//!   `wait_or_kill`, `terminate_tree*`, `Drop`, the detached reaper thread
//!   (`rightkit-detached-reap`) and, up to its timeout, the failed-spawn cleanup all wait this
//!   way.
//!   None of them reaps with a blocking call or sleeps while holding the lock, so `kill`,
//!   `terminate_tree` and every other waiter run while one of them is in progress, and a
//!   deadline is honoured whatever the child does.
//! * The sleep is [`ExitWatch::pause`](crate::exit_watch::ExitWatch::pause): a hint. With an
//!   event source it ends early at the child's exit; without one it is a 5 ms interval. The
//!   event never decides anything, so it cannot make a wait return early or hang.
//! * **Where the lock is held across a wait or a long action (the complete list).** None of
//!   these is a bounded wait that entered a blocking reap; the first is the only one a
//!   deadline-bearing caller on another thread can be held behind.
//!   1. [`UnixChild::wait`] through the `OwnedChild::child()` guard. The caller holds the lock
//!      by holding the guard, so the wait cannot release it: a bounded wait on a clone, a
//!      `kill`, `terminate_tree` or `Drop` on another thread waits behind it until the child
//!      has exited, and can exceed its own deadline. That is the caller's choice, not the
//!      crate's: `std::process::Child::wait` through the same guard did exactly this in
//!      0.3.3 (documented: "release this guard before another accessor or exit reader").
//!      The wait still reaps only with non-blocking looks. A program that needs a deadline
//!      or a cancel to stay live must wait through `OwnedChild`/`ProcessHandle` instead.
//!   2. `cleanup_direct` with no timeout (`partial_spawn_cleanup_timeout` not set), and
//!   3. the `rightkit-partial-reap` thread it hands a timed-out child to. Both call
//!      `UnixChild::wait` under the lock, as 0.3.3 called `std::process::Child::wait`. They
//!      run only for a child whose spawn failed after the process existed, and that `Arc`
//!      is held by nothing but the spawning thread (the partial `OwnedChild`, which no
//!      caller has seen and from which no `ProcessHandle` was cloned) and then by the reaper
//!      thread: no other thread can reach the lock, so nothing can be held behind it.
//!   4. `OwnedChild::while_unreaped` holds the lock while the caller's closure runs, which
//!      is its purpose (no reap can fall between the proof and the action). A bounded wait on
//!      another thread can be delayed by as long as the closure takes.
//!      `rightkit-sandbox` passes a descendant sweep with a 200 ms grace. New since 0.3.3.
//!   Also, [`UnixChild::require_identity`] reaps a just-killed child with a blocking
//!   `Child::wait`, before the child is shared at all.
//! * No wait-class system call names the pid while the lock is released. A blocked
//!   `waitpid(pid)`/`waitid(P_PID, pid)` outside the lock could be entered after another reader
//!   reaped the leader and the kernel recycled the number, and would then wait on an unrelated
//!   child.
//! * Seeing that the leader exited does not require reaping it (`UnixChild::leader_state`), so
//!   an exit monitor can observe the exit and leave the reap, and with it the group's identity,
//!   to the owner.
//!
//! The state is one lock-guarded enum, never separate atomics, so no thread can observe a
//! half-published transition.

use crate::exit_watch::ExitWatch;
use crate::identity::{Origin, Proof};
use crate::process_handle::lock_child;
use std::{
    io,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, ExitStatus},
    sync::{Arc, Mutex},
    time::Instant,
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

/// Who is in the leader's process group, besides the leader.
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
    /// A group signal answered `EPERM` while the leader still reads as running (macOS does
    /// this when the last members are mid-exit). Nothing was sent. The caller may look again
    /// after the lock is released: the group is gone as soon as the exit completes.
    MidExit,
}

/// A spawned Unix child. The same API as Windows' `WindowsChild`: public stdio fields, `id`,
/// `try_wait`, `wait` and `kill`. It does not expose the std `Child`, because a std reap the
/// crate cannot see would make later group signals unsafe (see the module documentation).
///
/// Obtained through [`OwnedChild::child`](crate::OwnedChild::child), which holds the shared
/// lock while the guard lives. `wait` through the guard therefore holds that lock until the
/// child has exited, and every other thread's access to the same child (a bounded wait on a
/// clone, `kill`, `terminate_tree`, `Drop`) waits behind it, as `std::process::Child::wait`
/// through the guard did in 0.3.3. Prefer `OwnedChild::wait`, `wait_timeout` or
/// `ProcessHandle::wait`, which release the lock between looks.
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
    /// The wake-up hint for waiters (never a verdict), opened at spawn and shared with every
    /// waiter.
    watch: Arc<ExitWatch>,
    /// Test only: the id used as the signal target, to stand in for a recycled id.
    #[cfg(test)]
    recycled_id: Option<u32>,
}

impl UnixChild {
    pub(crate) fn new(mut child: Child) -> Self {
        // Both are taken now, while the child is certainly still ours: the hint is bound to the
        // process, and the identity names it. (A child collected by foreign code before this
        // point would hand its replacement's description over as the baseline; that is part of
        // the known limit described in `identity`.)
        let watch = Arc::new(ExitWatch::open(child.id()));
        let origin = Origin::capture(child.id());
        Self {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            pid: child.id(),
            origin,
            watch,
            child,
            leader: Leader::Unreaped,
            #[cfg(test)]
            recycled_id: None,
        }
    }

    /// The wake-up hint, for waiters that sleep without holding the lock.
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
        if self.origin.check(self.pid)? == Proof::Same {
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

    /// Closes stdin as std does, then waits until the leader is reaped. The reap itself is only
    /// ever a non-blocking `waitpid(WNOHANG)` between sleeps, but this method cannot release
    /// the lock: when it is reached through the `OwnedChild::child()` guard the caller holds
    /// it for the whole wait, so other threads' waits, kills and drops on the same child
    /// queue behind it (as with `std::process::Child::wait` in 0.3.3). Use `OwnedChild::wait`
    /// or `ProcessHandle::wait` where that matters.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        drop(self.stdin.take());
        self.wait_reaping()
    }

    /// `SIGKILL` to the leader alone, only while it is unreaped. Nothing is sent, and `Ok` is
    /// returned, once it has been reaped.
    pub fn kill(&mut self) -> io::Result<()> {
        self.signal(SignalTarget::Leader, libc::SIGKILL).map(|_| ())
    }

    /// Reaps without touching stdin, for the guard's `wait` and for cleanup of a child that
    /// has just been killed. Each look is proven and non-blocking; between looks the thread
    /// sleeps on the hint. A blocking `waitpid(pid)` from the start would name the number for
    /// as long as it sleeps, and if other code collected the leader meanwhile it would wait on
    /// whatever child the kernel gave the number to, and take its status.
    pub(crate) fn wait_reaping(&mut self) -> io::Result<ExitStatus> {
        let watch = Arc::clone(&self.watch);
        watch
            .wait_for(None, || self.try_wait())?
            .ok_or_else(|| io::Error::other("an unbounded wait ended without a status"))
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
        if raw_kill(self.pid, target_id, signal) == 0 {
            return Ok(SignalOutcome::Sent);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(SignalOutcome::Gone),
            // macOS answers a group signal with EPERM, not ESRCH, when no member can take it:
            // only zombies are left, or the last members are mid-exit. Our own children are
            // always signalable, so this is never a real denial. Once the leader is no longer
            // running (exited, or reaped by a handle clone) the group is gone. A leader that
            // still reads as running is mid-exit: say so, and let the caller look again once it
            // has released the lock (`OwnedChild::signal`).
            Some(libc::EPERM) if target == SignalTarget::Group => {
                Ok(if self.leader_state()? == LeaderState::Running {
                    SignalOutcome::MidExit
                } else {
                    SignalOutcome::Gone
                })
            }
            _ => Err(error),
        }
    }

    /// The processes other than the leader still in the group.
    /// Valid only while the leader is unreaped; a reaped leader has no group to speak of. An
    /// unknown answer is [`Members::Unknown`]: the caller then waits out its grace instead of
    /// cutting it short.
    pub(crate) fn group_other_members(&mut self) -> io::Result<Members> {
        if self.leader_state()? == LeaderState::Reaped {
            return Ok(Members::Pids(Vec::new()));
        }
        group_members_besides(self.pid, self.signal_id())
    }

    /// True when a process other than the leader is still in the group (unknown counts as yes).
    #[cfg(test)]
    pub(crate) fn group_has_other_members(&mut self) -> io::Result<bool> {
        Ok(match self.group_other_members()? {
            Members::Pids(pids) => !pids.is_empty(),
            Members::Unknown => true,
        })
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
    }

    fn note_error(&mut self, error: io::Error) -> io::Error {
        if error.raw_os_error() == Some(libc::ECHILD) {
            self.leader = Leader::Lost;
        }
        error
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
fn group_members_besides(leader: u32, group: u32) -> io::Result<Members> {
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
            .filter(|&&pid| pid > 0 && pid as u32 != leader)
            .map(|&pid| pid as u32)
            .collect(),
    ))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn group_members_besides(leader: u32, group: u32) -> io::Result<Members> {
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
fn group_members_besides(_leader: u32, _group: u32) -> io::Result<Members> {
    Ok(Members::Unknown)
}

/// Waits for the leader of `shared` to be reaped, or for `deadline` to pass (`Ok(None)`); `None`
/// waits without limit.
///
/// Every look is one non-blocking `try_wait` under the lock, released before the thread sleeps
/// (see the module documentation), so a deadline is honoured whatever the child does, a
/// `kill` or `terminate_tree` from another thread runs at once, and no wait-class system call
/// names the pid while the lock is free. `ProcessHandle` waits this way, and so does the
/// failed-spawn cleanup that has a timeout; the cleanup without one waits through
/// `UnixChild::wait` instead (see the module documentation, item 2).
pub(crate) fn wait_shared(
    shared: &Mutex<UnixChild>,
    deadline: Option<Instant>,
) -> io::Result<Option<ExitStatus>> {
    let (watch, pid) = {
        let child = lock_child(shared);
        (child.exit_watch(), child.id())
    };
    let mut first = true;
    watch.wait_for(deadline, || {
        let looked = lock_child(shared).try_wait();
        pause_for_test(&mut first, pid, matches!(looked, Ok(None)));
        looked
    })
}

/// Like [`wait_shared`] but does not reap: `Ok(true)` once the leader has exited (or was reaped
/// by someone), `Ok(false)` when `deadline` passed first. The zombie keeps pinning the group id.
pub(crate) fn observe_shared(
    shared: &Mutex<UnixChild>,
    deadline: Option<Instant>,
) -> io::Result<bool> {
    let (watch, pid) = {
        let child = lock_child(shared);
        (child.exit_watch(), child.id())
    };
    let mut first = true;
    let seen = watch.wait_for(deadline, || {
        let state = lock_child(shared).leader_state();
        pause_for_test(&mut first, pid, matches!(state, Ok(LeaderState::Running)));
        Ok(match state? {
            LeaderState::Running => None,
            LeaderState::Exited | LeaderState::Reaped => Some(()),
        })
    })?;
    Ok(seen.is_some())
}

/// Test seam: after the first look that found the child still running, with the lock already
/// released and before the thread sleeps, run the hook installed for `pid` (see [`gate`]).
#[cfg(test)]
fn pause_for_test(first: &mut bool, pid: u32, running: bool) {
    if *first && running {
        *first = false;
        gate::pause(pid);
    }
}

#[cfg(not(test))]
fn pause_for_test(_first: &mut bool, _pid: u32, _running: bool) {}

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
