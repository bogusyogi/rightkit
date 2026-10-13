//! Identity-checked handling of processes this test spawned with plain `std`, so the crate
//! sees them as foreign. Covers capture, verification, mismatch refusal, the never-signal-self
//! rule, descendant enumeration and the identity-checked sweep.

use rightkit_process::{
    sweep, AdoptedIdentity, AdoptedProcess, AdoptedSignal, IdentityCheck, SignalResult, StartToken,
};
use std::{
    num::NonZeroU32,
    process::{Child, Command, Stdio},
    time::Duration,
};

fn sleeper() -> Child {
    #[cfg(unix)]
    let mut command = {
        let mut command = Command::new("/bin/sleep");
        command.arg("600");
        command
    };
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 600",
        ]);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a long sleeper")
}

fn pid(child: &Child) -> NonZeroU32 {
    NonZeroU32::new(child.id()).expect("a child pid is never zero")
}

fn identity(child: &Child) -> AdoptedIdentity {
    AdoptedIdentity::capture(pid(child))
        .expect("capture")
        .expect("the child is running")
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn a_captured_identity_verifies_and_reports_its_parts() {
    let child = sleeper();
    let identity = identity(&child);
    assert_eq!(identity.pid(), child.id());
    assert_eq!(identity.verify().unwrap(), IdentityCheck::Same);
    assert!(identity.start_token().major > 0);
    // The bridge from an AdoptedProcess captures the same process.
    let bridged = AdoptedProcess::new(pid(&child))
        .identity()
        .unwrap()
        .expect("running");
    assert_eq!(bridged.start_token(), identity.start_token());
    stop(child);
}

#[test]
fn capture_of_a_pid_nobody_has_is_none() {
    let mut child = sleeper();
    let number = pid(&child);
    child.kill().unwrap();
    child.wait().unwrap();
    // Reaped: the pid names nothing (a recycled pid inside this window is not plausible).
    assert!(AdoptedIdentity::capture(number).unwrap().is_none());
}

#[test]
fn a_matching_identity_is_signalled_and_the_process_ends() {
    let mut child = sleeper();
    let identity = identity(&child);
    assert_eq!(
        identity.signal(AdoptedSignal::Graceful).unwrap(),
        SignalResult::Delivered
    );
    let status = child.wait().unwrap();
    assert!(!status.success());
    // Reaped now: nothing is left to signal.
    assert_eq!(identity.verify().unwrap(), IdentityCheck::Gone);
    assert_eq!(
        identity.signal(AdoptedSignal::Force).unwrap(),
        SignalResult::Gone
    );
}

#[test]
fn a_different_start_time_is_refused_and_the_process_keeps_running() {
    let mut child = sleeper();
    let real = identity(&child);
    let token = real.start_token();
    let stale = AdoptedIdentity::from_recorded(
        pid(&child),
        StartToken {
            major: token.major,
            minor: token.minor + 1,
        },
        real.exe().map(ToOwned::to_owned),
    );
    assert_eq!(stale.verify().unwrap(), IdentityCheck::Replaced);
    for signal in [AdoptedSignal::Graceful, AdoptedSignal::Force] {
        assert_eq!(
            stale.signal(signal).unwrap(),
            SignalResult::IdentityMismatch
        );
    }
    assert!(
        child.try_wait().unwrap().is_none(),
        "a refused signal must not reach the process"
    );
    stop(child);
}

#[test]
fn a_different_executable_is_refused_and_the_process_keeps_running() {
    let mut child = sleeper();
    let real = identity(&child);
    if real.exe().is_none() {
        stop(child);
        return; // the host does not reveal executables to this user
    }
    let wrong = AdoptedIdentity::from_recorded(
        pid(&child),
        real.start_token(),
        Some("/definitely/not/this/program".into()),
    );
    assert_eq!(wrong.verify().unwrap(), IdentityCheck::Replaced);
    assert_eq!(
        wrong.signal(AdoptedSignal::Force).unwrap(),
        SignalResult::IdentityMismatch
    );
    assert!(child.try_wait().unwrap().is_none());
    stop(child);
}

#[test]
fn this_process_is_never_signalled() {
    let me = NonZeroU32::new(std::process::id()).unwrap();
    let identity = AdoptedIdentity::capture(me).unwrap().expect("self exists");
    let error = identity.signal(AdoptedSignal::Force).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn a_sweep_refuses_a_stale_target_and_stops_a_genuine_one() {
    let stale_child = sleeper();
    let mut real_child = sleeper();
    let real = identity(&real_child);
    let real_pid = real_child.id();
    let stale_real = identity(&stale_child);
    let stale = AdoptedIdentity::from_recorded(
        pid(&stale_child),
        StartToken {
            major: stale_real.start_token().major,
            minor: stale_real.start_token().minor + 1,
        },
        stale_real.exe().map(ToOwned::to_owned),
    );
    // The genuine target is our own child: reap it as soon as it dies, as its real parent would.
    let reaper = std::thread::spawn(move || real_child.wait());
    let report = sweep(&[stale, real], Duration::from_secs(5)).unwrap();
    reaper.join().unwrap().unwrap();
    assert_eq!(report.refused, vec![stale_child.id()]);
    assert_eq!(report.terminated, vec![real_pid], "{report:?}");
    assert!(report.survivors.is_empty() && report.failed.is_empty());
    let mut stale_child = stale_child;
    assert!(
        stale_child.try_wait().unwrap().is_none(),
        "the refused target must keep running"
    );
    stop(stale_child);
}

#[cfg(unix)]
mod descendants {
    use super::*;
    use rightkit_process::sweep_descendants;
    use std::{path::Path, thread, time::Instant};

    /// A shell that keeps two `sleep` children and waits on them.
    fn shell_with_two_sleepers() -> Child {
        Command::new("/bin/sh")
            .args(["-c", "sleep 600 & sleep 600 & wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the shell")
    }

    fn wait_for_descendants(root: &AdoptedIdentity, count: usize) -> Vec<AdoptedIdentity> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let found = root.descendants().unwrap();
            if found.len() >= count || Instant::now() > deadline {
                return found;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn descendants_are_found_with_their_own_identities() {
        let shell = shell_with_two_sleepers();
        let root = identity(&shell);
        let found = wait_for_descendants(&root, 2);
        assert_eq!(found.len(), 2, "{found:?}");
        for child in &found {
            assert_eq!(child.verify().unwrap(), IdentityCheck::Same);
            assert_ne!(child.pid(), root.pid());
            if let Some(exe) = child.exe() {
                assert_eq!(exe.file_name(), Some(Path::new("sleep").as_os_str()));
            }
        }
        stop(shell);
        for child in &found {
            // The shell is gone; its sleepers were not killed by the std kill of the shell.
            let _ = child.signal(AdoptedSignal::Force);
        }
    }

    #[test]
    fn a_stale_root_has_no_descendants() {
        let shell = shell_with_two_sleepers();
        let real = identity(&shell);
        let _ = wait_for_descendants(&real, 2);
        let stale = AdoptedIdentity::from_recorded(
            pid(&shell),
            StartToken {
                major: real.start_token().major,
                minor: real.start_token().minor + 1,
            },
            None,
        );
        assert!(stale.descendants().unwrap().is_empty());
        let _ = sweep_descendants(&real, Duration::from_secs(2));
        stop(shell);
    }

    #[test]
    fn the_sweep_stops_every_descendant_and_leaves_the_root() {
        let mut shell = shell_with_two_sleepers();
        let root = identity(&shell);
        let found = wait_for_descendants(&root, 2);
        assert_eq!(found.len(), 2, "{found:?}");
        let report = sweep_descendants(&root, Duration::from_secs(5)).unwrap();
        let mut stopped = report.terminated.clone();
        stopped.sort_unstable();
        let mut expected: Vec<u32> = found.iter().map(AdoptedIdentity::pid).collect();
        expected.sort_unstable();
        assert_eq!(stopped, expected, "{report:?}");
        assert!(
            report.refused.is_empty() && report.survivors.is_empty() && report.failed.is_empty()
        );
        for child in &found {
            assert_ne!(child.verify().unwrap(), IdentityCheck::Same);
        }
        // With its sleepers gone the shell's `wait` returns and it exits by itself.
        shell.wait().unwrap();
    }

    #[test]
    fn an_owned_child_lists_its_descendants_and_none_once_reaped() {
        let mut command = rightkit_process::OwnedCommand::new("/bin/sh");
        command
            .command_mut()
            .args(["-c", "sleep 600 & sleep 600 & wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().expect("spawn");
        let deadline = Instant::now() + Duration::from_secs(10);
        let found = loop {
            let found = child.descendant_identities().unwrap();
            if found.len() >= 2 || Instant::now() > deadline {
                break found;
            }
            thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(found.len(), 2, "{found:?}");
        let report = sweep(&found, Duration::from_secs(5)).unwrap();
        assert!(
            report.refused.is_empty() && report.failed.is_empty(),
            "{report:?}"
        );
        child.terminate_tree().unwrap();
        assert!(child.descendant_identities().unwrap().is_empty());
    }
}
