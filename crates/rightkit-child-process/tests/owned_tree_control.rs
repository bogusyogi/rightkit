//! `terminate_all_owned`, `owned_count` and `OwnedChild::terminate_tree_deep` (issue #171).
//!
//! `terminate_all_owned` acts on every owned child of the whole process, so each test takes
//! `SERIAL` and no other test binary shares this process.

use std::{
    num::NonZeroU32,
    process::Stdio,
    sync::Mutex,
    time::{Duration, Instant},
};

use rightkit_child_process::{
    owned_count, terminate_all_owned, AdoptedProcess, OwnedChild, OwnedCommand, TerminationOptions,
};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn quiet(command: &mut OwnedCommand) {
    command
        .command_mut()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
}

/// A long-lived leaf process.
fn sleeper() -> OwnedCommand {
    let mut command = if cfg!(windows) {
        let mut command = OwnedCommand::new("powershell.exe");
        command.command_mut().args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 300",
        ]);
        command
    } else {
        let mut command = OwnedCommand::new("sleep");
        command.command_mut().arg("300");
        command
    };
    quiet(&mut command);
    command
}

/// A leader that keeps one long-lived descendant.
fn tree() -> OwnedCommand {
    let mut command = if cfg!(windows) {
        let mut command = OwnedCommand::new("cmd.exe");
        command
            .command_mut()
            .args(["/c", "ping", "-n", "300", "127.0.0.1"]);
        command
    } else {
        let mut command = OwnedCommand::new("/bin/sh");
        command.command_mut().args(["-c", "sleep 300 & wait"]);
        command
    };
    quiet(&mut command);
    command
}

fn running(pid: u32) -> bool {
    AdoptedProcess::new(NonZeroU32::new(pid).unwrap())
        .is_running()
        .unwrap_or(false)
}

fn gone_within(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while running(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

fn descendant_pids(child: &OwnedChild) -> Vec<u32> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let found: Vec<u32> = child
            .descendant_identities()
            .expect("descendants")
            .iter()
            .map(|identity| identity.pid())
            .collect();
        if !found.is_empty() || Instant::now() >= deadline {
            return found;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn options() -> TerminationOptions {
    TerminationOptions::default().with_grace(Duration::from_millis(500))
}

#[test]
fn terminate_all_owned_stops_every_owned_child_and_leaves_detached_ones() {
    let _serial = serial();
    let mut first = sleeper().spawn().expect("spawn first");
    let mut second = tree().spawn().expect("spawn second");
    let mut detached = sleeper()
        .spawn_uncontained_detached()
        .expect("spawn detached");
    assert_eq!(owned_count(), 2, "detached children are not owned");
    let grandchild = descendant_pids(&second);

    let report = terminate_all_owned(options());
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert!(report.terminated.contains(&first.id()), "{report:?}");
    assert!(report.terminated.contains(&second.id()), "{report:?}");
    assert!(!report.terminated.contains(&detached.id()), "{report:?}");

    // The owners still hold valid children whose status is available at once.
    assert!(first
        .wait_timeout(Duration::from_secs(5))
        .expect("wait first")
        .is_some());
    assert!(second
        .wait_timeout(Duration::from_secs(5))
        .expect("wait second")
        .is_some());
    for pid in grandchild {
        assert!(gone_within(pid, Duration::from_secs(5)), "descendant {pid}");
    }
    assert!(running(detached.id()), "a detached child must not be touched");

    let result = detached
        .terminate_tree_deep(options())
        .expect("stop the detached child on demand");
    assert!(!result.status.success());
    assert!(gone_within(detached.id(), Duration::from_secs(5)));
}

#[test]
fn a_dropped_child_leaves_the_list_and_a_second_call_finds_nothing() {
    let _serial = serial();
    let child = sleeper().spawn().expect("spawn");
    assert_eq!(owned_count(), 1);
    drop(child);
    assert_eq!(owned_count(), 0);

    let kept = sleeper().spawn().expect("spawn again");
    let first = terminate_all_owned(options());
    assert_eq!(first.terminated, vec![kept.id()], "{first:?}");
    // The leader may still be unreaped (the owner holds it) or already reaped; either way a
    // repeat call never fails and never signals a pid it cannot prove.
    let second = terminate_all_owned(options());
    assert!(second.failed.is_empty(), "{:?}", second.failed);
    drop(kept);
    assert_eq!(owned_count(), 0);
}

#[test]
fn deep_termination_reaches_a_detached_childs_descendants() {
    let _serial = serial();
    let mut child = tree()
        .spawn_uncontained_detached()
        .expect("spawn detached tree");
    let descendants = descendant_pids(&child);
    assert!(!descendants.is_empty(), "the leader never started a descendant");

    let result = child
        .terminate_tree_deep(options())
        .expect("terminate the detached tree");
    assert!(!result.status.success());
    for pid in descendants {
        assert!(
            gone_within(pid, Duration::from_secs(5)),
            "descendant {pid} outlived the tree termination"
        );
    }
    assert!(gone_within(child.id(), Duration::from_secs(5)));
}

/// An ordinary child's whole tree goes through the same call, and a second call after the
/// leader was reaped signals nothing and still returns the cached status.
#[test]
fn deep_termination_of_an_ordinary_child_is_repeatable() {
    let _serial = serial();
    let mut child = tree().spawn().expect("spawn tree");
    let descendants = descendant_pids(&child);
    let first = child.terminate_tree_deep(options()).expect("first call");
    for pid in descendants {
        assert!(gone_within(pid, Duration::from_secs(5)), "descendant {pid}");
    }
    let second = child.terminate_tree_deep(options()).expect("second call");
    assert_eq!(first.status, second.status);
}
