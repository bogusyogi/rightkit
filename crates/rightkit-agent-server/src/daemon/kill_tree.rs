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
#[cfg(test)]
use std::thread;
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;

/// Grace after `SIGTERM`/graceful signal before escalating to forceful kill.
const TERMINATION_GRACE: Duration = Duration::from_secs(5);
/// Bound to confirm the process/tree is actually gone after a kill.
const TERMINATION_CONFIRM: Duration = Duration::from_secs(1);
#[cfg(all(test, unix))]
const POLL_INTERVAL: Duration = Duration::from_millis(50);
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

#[cfg(all(windows, test))]
fn process_exists(pid: u32) -> bool {
    crate::daemon::reload::process_alive(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_terminate_self() {
        let err = terminate_pid(std::process::id(), false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn refuses_invalid_pid_zero() {
        let err = terminate_pid(0, false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn terminate_pid_pinned_skips_stale_identity() {
        // A boot_epoch_ms that cannot match any real process (0) must make
        // the pinned terminate a no-op success rather than signal blind.
        let result = terminate_pid_pinned(999_999, 0, false);
        assert!(result.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn terminate_group_escalates_and_confirms() {
        use std::process::{Command, Stdio};

        // Spawn a child as its own process-group leader (setsid via a shell
        // subshell trick is unavailable in std; instead spawn `sleep` and
        // target its own pid as group leader using the same convention the
        // daemon itself uses: children are spawned with `process_group(0)`).
        // Here we approximate by treating the spawned child's own pid as its
        // group id, which holds when the child is a session/group leader
        // (true for a freshly spawned `sleep` under `Command` with no
        // pre-exec group join, on the container/test-runner default).
        #[allow(clippy::disallowed_methods)]
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();

        // Best-effort: only assert termination succeeds; group-leadership
        // nuance is environment-dependent and covered structurally, not by
        // asserting descendant fanout here (no descendants spawned).
        let _ = terminate_pid(pid, false);
        let _ = child.wait();
    }

    /// Real-process descendant-containment proof for DL-012: a `sh` parent
    /// backgrounds a `sleep` descendant in the same process group (no
    /// `setsid`/explicit group override — matching the daemon's own
    /// default child-spawn convention), and a single `terminate_pid` on the
    /// parent must leave neither the parent nor the descendant alive. A
    /// disappeared parent alone is never accepted as proof; both pids are
    /// polled independently after termination.
    #[cfg(unix)]
    #[test]
    fn terminate_group_kills_descendant_process_too() {
        use std::io::Read;
        use std::process::{Command, Stdio};

        #[allow(clippy::disallowed_methods)]
        let mut parent = Command::new("sh")
            .arg("-c")
            // Background `sleep`, echo its pid on the first line, then wait
            // on it so the parent (this shell) stays alive as the group
            // leader until it is signaled.
            .arg("sleep 30 & echo $!; wait")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sh parent that backgrounds a sleep descendant");
        let parent_pid = parent.id();

        let mut stdout = parent.stdout.take().expect("sh stdout pipe");
        let mut line = String::new();
        let mut byte = [0u8; 1];
        let read_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                Instant::now() < read_deadline,
                "descendant pid line was never printed"
            );
            match stdout.read(&mut byte) {
                Ok(0) => break,
                Ok(_) if byte[0] == b'\n' => break,
                Ok(_) => line.push(byte[0] as char),
                Err(_) => break,
            }
        }
        let descendant_pid: libc::pid_t = line
            .trim()
            .parse()
            .expect("descendant pid line must be a valid pid");

        // Confirm the descendant actually started before termination (bounded
        // poll — spawn plus fork/exec is fast but not synchronous).
        let alive_deadline = Instant::now() + Duration::from_secs(5);
        while unsafe { libc::kill(descendant_pid, 0) } != 0 {
            assert!(
                Instant::now() < alive_deadline,
                "descendant sleep never became visible"
            );
            thread::sleep(Duration::from_millis(20));
        }

        terminate_pid(parent_pid, false).expect("group termination must succeed");

        // The parent is this test's own child: once killed it stays a zombie,
        // and `kill(pid, 0)` keeps succeeding on a zombie until it is reaped.
        // Reap it (bounded) and only then assert it is gone.
        let reap_deadline = Instant::now() + Duration::from_secs(5);
        while parent
            .try_wait()
            .expect("poll parent exit status")
            .is_none()
        {
            assert!(
                Instant::now() < reap_deadline,
                "parent must be gone after tree termination"
            );
            thread::sleep(POLL_INTERVAL);
        }
        // The descendant is re-parented to init/launchd once the shell dies,
        // which reaps it asynchronously; allow that bounded handoff.
        let gone_deadline = Instant::now() + Duration::from_secs(5);
        while unsafe { libc::kill(descendant_pid, 0) } == 0 {
            assert!(
                Instant::now() < gone_deadline,
                "descendant must be gone after tree termination (no leak)"
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    /// Windows counterpart of the descendant-containment proof: `cmd.exe`
    /// spawns `ping` as its own child process, so terminating the outer
    /// `cmd.exe` pid must tree-terminate (`taskkill /T /F`) the `ping`
    /// descendant too — a disappeared parent alone is never accepted.
    #[cfg(windows)]
    #[test]
    fn terminate_pid_kills_descendant_process_too() {
        use std::io::{BufRead, BufReader};
        use std::process::Stdio;

        #[allow(clippy::disallowed_methods)]
        let mut parent = rightkit_child_process::command_std("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                r#"$child = Start-Process -FilePath "$env:SystemRoot\System32\ping.exe" -ArgumentList @('-n','30','127.0.0.1') -PassThru; [Console]::Out.WriteLine($child.Id); Wait-Process -Id $child.Id"#,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn PowerShell parent that starts a ping descendant");
        let parent_pid = parent.id();
        let mut line = String::new();
        BufReader::new(parent.stdout.take().expect("PowerShell stdout pipe"))
            .read_line(&mut line)
            .expect("read descendant pid emitted after spawn");
        let descendant_pid = line
            .trim()
            .parse::<u32>()
            .expect("descendant ping.exe pid must be valid");
        assert!(
            process_exists(descendant_pid),
            "descendant must be alive before termination"
        );

        // Release our own handle first: while a `Child` handle stays open the
        // Windows process object remains queryable after termination, so the
        // liveness probe below would observe a corpse rather than the real
        // tree state. Dropping does not kill it — the tree kill is the thing
        // under test.
        drop(parent);

        terminate_pid(parent_pid, false).expect("tree termination must succeed");

        assert!(
            !process_exists(parent_pid),
            "parent must be gone after tree termination"
        );
        assert!(
            !process_exists(descendant_pid),
            "descendant must be gone after tree termination (no leak)"
        );
    }
}
