//! `tokio` integration (cargo feature `tokio`): the no-window helper for
//! `tokio::process::Command`, and an async staged kill of a tree started from a command the
//! caller built and spawned itself.
//!
//! [`OwnedCommand`](crate::OwnedCommand) owns the spawn. A caller that already builds tokio
//! commands (and puts the child in its own process group or session) keeps its spawn and uses
//! [`terminate_tree`] on the `tokio::process::Child`, or [`terminate_process_group`] when it
//! holds only a group id.
//!
//! # What is and is not proven
//!
//! The pid of a child this function was handed is safe to signal while the `Child` has not
//! been reaped: [`terminate_tree`] checks `try_wait` before it signals. On Unix the group is
//! signalled only when the child leads it (`getpgid(pid) == pid`: it called `setsid` or
//! `setpgid(0, 0)`, for instance through `process_group(0)`); otherwise only the child itself
//! is signalled and its descendants are swept by identity (pid plus start time), as on Windows.
//! There is no job object here, so on Windows a descendant that started after the scan, or
//! whose parent exited before it, is not reached; use [`OwnedCommand`](crate::OwnedCommand)
//! when that matters.

use crate::{NoWindow, TerminationOptions};
use std::{io, process::ExitStatus, time::Duration};
use tokio::process::Child;

#[cfg(unix)]
use crate::{AdoptedIdentity, AdoptedSignal};
#[cfg(unix)]
use std::num::NonZeroU32;
#[cfg(unix)]
use tokio::time::Instant;

/// How often the group and the child are looked at while waiting out the grace.
#[cfg(unix)]
const POLL: Duration = Duration::from_millis(20);
/// How long the final wait after the forced stage may take.
const KILL_WAIT: Duration = Duration::from_secs(10);

impl NoWindow for tokio::process::Command {
    fn no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            self.creation_flags(crate::CREATE_NO_WINDOW);
        }
        self
    }
}

/// A `tokio::process::Command` for `program` that already has the no-window flag set on
/// Windows, returned by value so it chains. The async counterpart of
/// [`command_std`](crate::command_std).
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    // windowless: no_window() is applied on the next line
    let mut command = tokio::process::Command::new(program);
    command.no_window();
    command
}

/// Makes the command's child lead a new session (`setsid`) on Unix, so a later
/// [`terminate_tree`] or session sweep finds it as the session leader without a hand-written
/// `pre_exec`. A no-op on Windows (use [`OwnedCommand`](crate::OwnedCommand) for job containment).
/// The child runs in its own session and process group from the spawn onward.
pub fn lead_session(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(unix)]
    {
        // SAFETY: the closure makes one `setsid` call, which is async-signal-safe, and maps its
        // failure to an error for the spawn.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    command
}

/// Stops every process group of session `sid` (the session id is the pid of its leader, as for
/// a pseudo-terminal the caller created): `SIGTERM` to each group now, up to `options.grace`
/// for them to leave, then `SIGKILL` to those still present. Returns `true` when at least one
/// group was found. Unix only. The groups are found by a process-table scan (see
/// [`signal_session`](crate::signal_session)), so this is best effort for a group that starts
/// after the scan. This process's own group is never signalled.
#[cfg(unix)]
pub async fn terminate_session(
    sid: std::num::NonZeroU32,
    options: TerminationOptions,
) -> io::Result<bool> {
    let groups: Vec<u32> = crate::foreign::session_groups(sid)?
        .into_iter()
        .filter_map(|group| u32::try_from(group).ok())
        .collect();
    if groups.is_empty() {
        return Ok(false);
    }
    for &group in &groups {
        signal(group, true, libc::SIGTERM)?;
    }
    let deadline = Instant::now() + options.grace;
    while groups.iter().any(|&group| group_exists(group)) && Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
    }
    for &group in &groups {
        if group_exists(group) {
            signal(group, true, libc::SIGKILL)?;
        }
    }
    let kill_deadline = Instant::now() + KILL_WAIT;
    while groups.iter().any(|&group| group_exists(group)) && Instant::now() < kill_deadline {
        tokio::time::sleep(POLL).await;
    }
    Ok(true)
}

/// Stops the tree under `child`: `SIGTERM` now (Unix), up to `options.grace` for it to leave,
/// then `SIGKILL`. On Windows there is no graceful stage: every member is terminated at once,
/// and `options.windows_exit_code` is not applied (tokio's kill uses exit code 1). Returns the
/// leader's exit status once it is reaped; `TimedOut` if it is still not gone ten seconds
/// after the forced stage.
///
/// No lock or thread is held across the awaits, so dropping the future between stages leaves
/// the child signalled but possibly unreaped.
pub async fn terminate_tree(
    child: &mut Child,
    options: TerminationOptions,
) -> io::Result<ExitStatus> {
    if let Some(status) = child.try_wait()? {
        return Ok(status);
    }
    let Some(pid) = child.id() else {
        return wait_bounded(child).await;
    };
    terminate_platform(child, pid, options).await
}

#[cfg(unix)]
async fn terminate_platform(
    child: &mut Child,
    pid: u32,
    options: TerminationOptions,
) -> io::Result<ExitStatus> {
    // SAFETY: getpgid takes no pointers; the child is unreaped (checked by the caller).
    let leads_group = unsafe { libc::getpgid(pid as libc::pid_t) } == pid as libc::pid_t;
    // Descendants of a child that does not lead a group cannot be reached by a group signal:
    // remember them by identity while the root is alive.
    let swept = if leads_group {
        Vec::new()
    } else {
        sweep_targets(pid)
    };
    signal(pid, leads_group, libc::SIGTERM)?;
    let deadline = Instant::now() + options.grace;
    let mut status = None;
    loop {
        if status.is_none() {
            status = child.try_wait()?;
        }
        let rest_gone = status.is_some()
            && if leads_group {
                !group_exists(pid)
            } else {
                swept.iter().all(|t| !is_present(t))
            };
        if rest_gone || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL.min(deadline.saturating_duration_since(Instant::now()))).await;
    }
    if status.is_none() {
        // Still unreaped, so the pid is still ours.
        signal(pid, leads_group, libc::SIGKILL)?;
    } else if leads_group && group_exists(pid) {
        // The leader is reaped; the group id stays valid while any member lives.
        signal(pid, true, libc::SIGKILL)?;
    }
    for target in &swept {
        let _ = target.signal(AdoptedSignal::Force);
    }
    match status {
        Some(status) => Ok(status),
        None => wait_bounded(child).await,
    }
}

#[cfg(unix)]
fn signal(pid: u32, group: bool, signal: libc::c_int) -> io::Result<()> {
    let target = if group { -(pid as i32) } else { pid as i32 };
    // SAFETY: kill takes no pointers. The caller has just seen the child unreaped (or, for a
    // group, a living member), so the number still names the child's group.
    if unsafe { libc::kill(target, signal) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // Gone already.
        Some(libc::ESRCH) => Ok(()),
        // macOS: a group holding only zombies or mid-exit members answers EPERM.
        Some(libc::EPERM) if group => Ok(()),
        _ => Err(error),
    }
}

#[cfg(unix)]
fn group_exists(pgid: u32) -> bool {
    // SAFETY: signal 0 only tests for existence.
    if unsafe { libc::kill(-(pgid as i32), 0) } == 0 {
        return true;
    }
    // EPERM: members exist that this process may not signal.
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Observation-only existence probe for process group `pgid` (signal 0; `EPERM` counts as
/// present), so a caller can confirm a tree is fully gone after [`terminate_tree`] without a
/// raw `killpg`. Group 1 is refused with `InvalidInput` (and 0 by the `NonZeroU32` type: it
/// would name this process's own group). Never sends a real signal. Unix only.
#[cfg(unix)]
pub fn process_group_exists(pgid: NonZeroU32) -> io::Result<bool> {
    crate::foreign::process_group_exists(pgid)
}

/// Async wait of at most `timeout` for process group `pgid` to empty; `Ok(false)` when
/// members remain then. Observation only. Unix only.
#[cfg(unix)]
pub async fn wait_process_group_empty(pgid: NonZeroU32, timeout: Duration) -> io::Result<bool> {
    crate::foreign::check_group_id(pgid)?;
    let deadline = Instant::now() + timeout;
    loop {
        if !crate::foreign::process_group_exists(pgid)? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(POLL.min(deadline.saturating_duration_since(Instant::now()))).await;
    }
}

#[cfg(unix)]
fn sweep_targets(pid: u32) -> Vec<AdoptedIdentity> {
    let Some(root) = NonZeroU32::new(pid) else {
        return Vec::new();
    };
    let Ok(Some(identity)) = AdoptedIdentity::capture(root) else {
        return Vec::new();
    };
    let mut targets = identity.descendants().unwrap_or_default();
    targets.reverse(); // leaves first
    targets
}

#[cfg(unix)]
fn is_present(target: &AdoptedIdentity) -> bool {
    matches!(target.verify(), Ok(crate::IdentityCheck::Same))
}

#[cfg(not(unix))]
async fn terminate_platform(
    child: &mut Child,
    pid: u32,
    _options: TerminationOptions,
) -> io::Result<ExitStatus> {
    use crate::{AdoptedIdentity, AdoptedSignal};
    let mut targets = std::num::NonZeroU32::new(pid)
        .and_then(|pid| AdoptedIdentity::capture(pid).ok().flatten())
        .and_then(|root| root.descendants().ok())
        .unwrap_or_default();
    targets.reverse(); // leaves first
    for target in &targets {
        let _ = target.signal(AdoptedSignal::Force);
    }
    match child.start_kill() {
        Ok(()) => {}
        // Exited between the check and the kill.
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
        Err(error) => return Err(error),
    }
    wait_bounded(child).await
}

async fn wait_bounded(child: &mut Child) -> io::Result<ExitStatus> {
    match tokio::time::timeout(KILL_WAIT, child.wait()).await {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the child did not exit after the forced stage",
        )),
    }
}

/// Unix only. Stops every process in group `pgid` when no `Child` is at hand (a PTY or
/// session leader started elsewhere): `SIGTERM` to the group, up to `options.grace` for it to
/// empty, then `SIGKILL`. Returns `true` when the group is empty at the end.
///
/// A group id cannot be reused while any member lives, so the signals reach the intended
/// group. But if the whole group has already gone and the number was recycled for a new,
/// unrelated group between the caller's decision and this call, that group would be signalled.
/// Prefer [`terminate_tree`] whenever the `Child` is available. This process's own group is
/// refused with `InvalidInput`.
#[cfg(unix)]
pub async fn terminate_process_group(
    pgid: NonZeroU32,
    options: TerminationOptions,
) -> io::Result<bool> {
    crate::foreign::check_group_id(pgid)?;
    // SAFETY: getpgrp has no preconditions.
    if pgid.get() as libc::pid_t == unsafe { libc::getpgrp() } {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to signal this process's own group",
        ));
    }
    let pgid = pgid.get();
    if !group_exists(pgid) {
        return Ok(true);
    }
    signal(pgid, true, libc::SIGTERM)?;
    let deadline = Instant::now() + options.grace;
    while group_exists(pgid) && Instant::now() < deadline {
        tokio::time::sleep(POLL.min(deadline.saturating_duration_since(Instant::now()))).await;
    }
    if group_exists(pgid) {
        signal(pgid, true, libc::SIGKILL)?;
        let end = Instant::now() + Duration::from_secs(2);
        while group_exists(pgid) && Instant::now() < end {
            tokio::time::sleep(POLL).await;
        }
    }
    Ok(!group_exists(pgid))
}
