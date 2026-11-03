//! Forced-kill backstop & descendant containment for the RightKit daemon
//! (DL-012).
//!
//! Termination is RightKit's identity-pinned sweep (`rightkit_child_process::sweep_descendants`
//! plus `AdoptedIdentity::signal`), the same on every platform: descendants leaves first,
//! then the root. A disappeared root alone is never accepted as proof; descendants are part
//! of the contract.
//!
//! `terminate_pid_pinned` adds the exact-process-identity re-check
//! (`rightkit_child_process::AdoptedIdentity::from_recorded_unix_ms` against the recorded
//! start epoch) before every signal, closing the check-then-kill PID-reuse race for
//! `takeover.rs` (DL-002): if the pid's start time no longer matches, the
//! original process is already gone and re-signaling the (possibly reused)
//! pid is skipped.

use std::io;
use std::time::Duration;

/// Grace after `SIGTERM`/graceful signal before escalating to forceful kill.
const TERMINATION_GRACE: Duration = Duration::from_secs(5);
/// Bound to confirm the process/tree is actually gone after a kill.
const TERMINATION_CONFIRM: Duration = Duration::from_secs(1);
/// Allowed gap between a recorded start epoch and the live process's start time.
pub(super) const PIN_TOLERANCE: Duration = Duration::from_millis(2_000);

/// Terminate `pid` (and, on POSIX, its whole process group / on Windows its
/// process tree). `forceful` selects `SIGKILL`/forced kill directly instead
/// of the graceful `SIGTERM` first step — callers that already tried a
/// graceful signal and waited out `TERMINATION_GRACE` pass `true`.
///
/// Returns `Ok(())` only once the target is confirmed gone (or was already
/// gone). Never partial-succeeds on a disappeared root while descendants
/// remain — matches the Archon donor contract.
pub(crate) fn terminate_pid(pid: u32, forceful: bool) -> io::Result<()> {
    if pid == 0 || pid == std::process::id() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to terminate invalid target pid {pid}"),
        ));
    }
    let Some(target) = std::num::NonZeroU32::new(pid) else {
        return Ok(());
    };
    // A pid that no longer exists is already gone.
    match rightkit_child_process::AdoptedIdentity::capture(target)? {
        Some(root) => terminate_identity(&root, forceful),
        None => Ok(()),
    }
}

/// [`terminate_pid`] with an exact-identity re-check against `boot_epoch_ms`
/// immediately before signaling, so a pid recycled between pin and signal is
/// never touched. Used by `takeover.rs`'s stale-owner escalation.
pub(crate) fn terminate_pid_pinned(
    pid: u32,
    boot_epoch_ms: u128,
    forceful: bool,
) -> io::Result<()> {
    use rightkit_child_process::AdoptedIdentity;

    let recorded_ms = u64::try_from(boot_epoch_ms).unwrap_or(u64::MAX);
    let Some(target) = std::num::NonZeroU32::new(pid) else {
        return Ok(());
    };
    match AdoptedIdentity::from_recorded_unix_ms(target, recorded_ms, PIN_TOLERANCE) {
        // Every later signal is pinned to this identity (pid plus start time).
        Ok(Some(root)) => terminate_identity(&root, forceful),
        // Identity no longer matches (or process already gone, or the host reports no
        // start time): treat as already-terminated rather than signal a possibly-reused pid.
        Ok(None) | Err(_) => Ok(()),
    }
}

/// RightKit identity-pinned termination of `root` and its descendants (found through the
/// parent links the host reports, captured before the root is signalled): descendants leaves
/// first, then the root. Unless `forceful`, Unix gives each a graceful signal and
/// `TERMINATION_GRACE` before the forced one; Windows has one way to stop a process, so it
/// is always forced.
fn terminate_identity(
    root: &rightkit_child_process::AdoptedIdentity,
    forceful: bool,
) -> io::Result<()> {
    use rightkit_child_process::{sweep, AdoptedSignal};

    let pid = root.pid();
    let force_now = forceful || cfg!(windows);
    let grace = if force_now {
        Duration::ZERO
    } else {
        TERMINATION_GRACE
    };
    // `sweep_descendants`, with the identities kept so a `failed` entry can be re-checked.
    let mut descendants = root.descendants()?;
    descendants.reverse();
    let report = sweep(&descendants, grace)?;
    if !report.survivors.is_empty() {
        return Err(io::Error::other(format!(
            "process tree {pid} still has descendants after termination: survivors {:?}",
            report.survivors
        )));
    }
    let root_gone = (!force_now && root.quit_gracefully(grace)?) || {
        root.signal(AdoptedSignal::Force)?;
        root.wait_confirmed_gone(TERMINATION_CONFIRM)?
    };
    if !root_gone {
        return Err(io::Error::other(format!(
            "process tree {pid} is still running after termination"
        )));
    }
    // A failed signal is not a survivor by itself: on Windows `TerminateProcess` errors for a
    // process that is already exiting, and a console host leaves with its parent. Only a
    // failed descendant still present once the root is gone is an error.
    let mut still_running = Vec::new();
    for target in descendants
        .iter()
        .filter(|target| report.failed.contains(&target.pid()))
    {
        if !target.wait_confirmed_gone(TERMINATION_CONFIRM)? {
            still_running.push(target.pid());
        }
    }
    if !still_running.is_empty() {
        return Err(io::Error::other(format!(
            "process tree {pid} still has descendants after termination: failed {still_running:?}"
        )));
    }
    Ok(())
}

/// Put the current (daemon) process in a kill-on-close Job Object so every
/// descendant (membrane worker, PTY hosts, shell children) dies with it, even
/// when the daemon itself is force-killed or crashes. Breakaway is allowed
/// explicitly, so the reload replacement daemon (reload.rs, which launches with
/// `CREATE_BREAKAWAY_FROM_JOB`) outlives the old one; everything else stays contained.
/// Best-effort: failure (e.g. an outer job forbidding nesting) is ignored.
#[cfg(windows)]
pub(crate) fn contain_descendants_in_job() {
    use rightkit_child_process::WindowsJob;
    let _ = WindowsJob::contain_current_process_with(WindowsJob::builder().allow_breakaway(true));
}

#[cfg(not(windows))]
pub(crate) fn contain_descendants_in_job() {}
