//! The process-wide list of owned children, for [`terminate_all_owned`].
//!
//! Every child started by [`OwnedCommand::spawn`](crate::OwnedCommand::spawn) is listed here
//! until its `OwnedChild` is dropped. Detached children
//! ([`spawn_uncontained_detached`](crate::OwnedCommand::spawn_uncontained_detached)) are not
//! owned and are never listed. The list holds only `Weak` references (Unix: to the shared
//! child; Windows: to the kill-on-close job, plus a duplicated process handle), so listing a
//! child never keeps it, its pipes or its job alive, and a dropped child leaves the list.
//!
//! Identity: on Unix every signal still goes through `UnixChild::signal`, which proves the
//! pid is the child this process spawned and has not reaped before it is used, exactly as for
//! the owner's own calls; an entry whose leader was reaped is reported `already_gone` and
//! nothing is sent. On Windows termination uses the process handle and the job, which cannot
//! name a recycled pid.

use crate::{TerminationOptions, process_handle::ProcessHandle};
use std::{
    io,
    sync::{Arc, Mutex, MutexGuard, Weak},
};

#[cfg(unix)]
type PlatformChild = crate::unix_child::UnixChild;
#[cfg(windows)]
type PlatformChild = crate::spawn_windows::WindowsChild;

struct Entry {
    /// Address of the shared child; identifies the entry for `forget`.
    key: usize,
    pid: u32,
    child: Weak<Mutex<PlatformChild>>,
    #[cfg(windows)]
    process: ProcessHandle,
    #[cfg(windows)]
    job: Weak<crate::process_windows::KillOnCloseJob>,
}

static OWNED: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

fn owned() -> MutexGuard<'static, Vec<Entry>> {
    OWNED.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn key_of(child: &Arc<Mutex<PlatformChild>>) -> usize {
    Arc::as_ptr(child) as usize
}

#[cfg(unix)]
pub(crate) fn register(child: &Arc<Mutex<PlatformChild>>, pid: u32) {
    let mut list = owned();
    list.retain(|entry| entry.child.strong_count() > 0);
    list.push(Entry {
        key: key_of(child),
        pid,
        child: Arc::downgrade(child),
    });
}

#[cfg(windows)]
pub(crate) fn register(
    child: &Arc<Mutex<PlatformChild>>,
    handle: &ProcessHandle,
    job: Weak<crate::process_windows::KillOnCloseJob>,
    pid: u32,
) {
    // A handle that cannot be duplicated leaves this child unlisted (it is still owned and
    // killed on drop as before); the duplicate is what lets the list act without the child.
    let Ok(process) = handle.try_clone() else {
        return;
    };
    let mut list = owned();
    list.retain(|entry| entry.child.strong_count() > 0);
    list.push(Entry {
        key: key_of(child),
        pid,
        child: Arc::downgrade(child),
        process,
        job,
    });
}

/// Removes the entry for `child`, if any. Called when its `OwnedChild` is dropped.
pub(crate) fn forget(child: &Arc<Mutex<PlatformChild>>) {
    let key = key_of(child);
    owned().retain(|entry| entry.key != key);
}

/// How many owned children are listed and still alive (their `OwnedChild` has not been
/// dropped). A child that exited but was not dropped is still counted.
pub fn owned_count() -> usize {
    let mut list = owned();
    list.retain(|entry| entry.child.strong_count() > 0);
    list.len()
}

/// The result of [`terminate_all_owned`], as pids.
#[derive(Debug, Default)]
pub struct TerminateAllReport {
    /// Stopped by this call.
    pub terminated: Vec<u32>,
    /// Nothing was left to stop: the leader had been reaped or its group was already empty
    /// (Unix), or the process had exited and had no live job (Windows).
    pub already_gone: Vec<u32>,
    /// Could not be stopped, with the reason. The rest were still attempted.
    pub failed: Vec<(u32, io::Error)>,
}

/// Stops every child this process owns (everything started with `OwnedCommand::spawn` whose
/// `OwnedChild` has not been dropped), tree and all, in one call: for an application's
/// shutdown path, replacing an app-kept list of running children.
///
/// Unix: `SIGTERM` to every child's process group first, then at most `options.grace` in
/// total (not per child) for the groups to leave, then `SIGKILL` to every group, and a bounded
/// reap of each leader (2 s in total). Windows: each child's retained job is terminated and
/// the child itself, with `options.windows_exit_code`; there is no graceful stage and
/// `grace` is not used. Every group signal is sent only while the leader is proven to be the
/// child this process spawned and has not reaped (see `unix_child`), so a recycled pid is
/// never signalled.
///
/// The `OwnedChild` values stay valid: `wait` returns the cached status, `Drop` finds nothing
/// left to kill. Children started detached are not owned and are not touched; stop those with
/// [`OwnedChild::terminate_tree_deep`](crate::OwnedChild::terminate_tree_deep).
pub fn terminate_all_owned(options: TerminationOptions) -> TerminateAllReport {
    #[cfg(unix)]
    {
        terminate_all_unix(options)
    }
    #[cfg(windows)]
    {
        terminate_all_windows(options)
    }
}

#[cfg(unix)]
fn terminate_all_unix(options: TerminationOptions) -> TerminateAllReport {
    use crate::{
        owned::{await_tree_shared, signal_shared},
        unix_child::{SignalOutcome, SignalTarget},
    };
    use std::time::{Duration, Instant, SystemTime};

    const REAP_BUDGET: Duration = Duration::from_secs(2);

    let live: Vec<(u32, Arc<Mutex<PlatformChild>>)> = {
        let mut list = owned();
        list.retain(|entry| entry.child.strong_count() > 0);
        list.iter()
            .filter_map(|entry| entry.child.upgrade().map(|child| (entry.pid, child)))
            .collect()
    };
    let mut report = TerminateAllReport::default();

    // Stage 1: ask every group to stop.
    let mut asked: Vec<(u32, Arc<Mutex<PlatformChild>>, ProcessHandle)> = Vec::new();
    for (pid, child) in live {
        let handle = match ProcessHandle::new(&child, SystemTime::now()) {
            Ok(handle) => handle,
            Err(error) => {
                report.failed.push((pid, error));
                continue;
            }
        };
        match signal_shared(&child, SignalTarget::Group, libc::SIGTERM) {
            Ok(SignalOutcome::Sent) => asked.push((pid, child, handle)),
            Ok(_) => report.already_gone.push(pid),
            Err(error) => report.failed.push((pid, error)),
        }
    }

    // Stage 2: one shared grace.
    let deadline = Instant::now().checked_add(options.grace);
    for (pid, child, handle) in &asked {
        let left = deadline.map_or(Duration::ZERO, |at| {
            at.saturating_duration_since(Instant::now())
        });
        if let Err(error) = await_tree_shared(child, handle, left, true) {
            report.failed.push((*pid, error));
        }
    }

    // Stage 3: force every group down, then reap each leader within one shared budget.
    let reap_deadline = Instant::now().checked_add(REAP_BUDGET);
    for (pid, child, handle) in &asked {
        match signal_shared(child, SignalTarget::Group, libc::SIGKILL) {
            Ok(_) => {
                let left = reap_deadline.map_or(Duration::ZERO, |at| {
                    at.saturating_duration_since(Instant::now())
                });
                // A leader that does not reap in time is left to its owner; the group is dead.
                let _ = handle.wait_timeout(left);
                if !report.failed.iter().any(|(failed, _)| failed == pid) {
                    report.terminated.push(*pid);
                }
            }
            Err(error) => report.failed.push((*pid, error)),
        }
    }
    report
}

#[cfg(windows)]
fn terminate_all_windows(options: TerminationOptions) -> TerminateAllReport {
    let live: Vec<(u32, ProcessHandle, Option<Arc<crate::process_windows::KillOnCloseJob>>)> = {
        let mut list = owned();
        list.retain(|entry| entry.child.strong_count() > 0);
        list.iter()
            .filter_map(|entry| {
                Some((entry.pid, entry.process.try_clone().ok()?, entry.job.upgrade()))
            })
            .collect()
    };
    let mut report = TerminateAllReport::default();
    for (pid, process, job) in live {
        if job.is_none() && matches!(process.try_wait(), Ok(Some(_))) {
            report.already_gone.push(pid);
            continue;
        }
        match crate::process_windows::terminate(&process, job.as_deref(), options.windows_exit_code)
        {
            Ok(()) => report.terminated.push(pid),
            Err(error) => report.failed.push((pid, error)),
        }
    }
    report
}
