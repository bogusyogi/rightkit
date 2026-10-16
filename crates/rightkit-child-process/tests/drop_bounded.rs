#![cfg(unix)]

//! Dropping an `OwnedChild` kills at once and waits for the reap only for a bounded time.

use rightkit_child_process::{OwnedCommand, DEFAULT_DROP_REAP_TIMEOUT};
use std::{
    process::Stdio,
    time::{Duration, Instant},
};

fn sleeper() -> rightkit_child_process::OwnedChild {
    let mut command = OwnedCommand::new("/bin/sleep");
    command
        .command_mut()
        .arg("600")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn().expect("spawn")
}

fn exists(pid: u32) -> bool {
    // SAFETY: signal 0 only checks existence.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn wait_until_gone(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while exists(pid) {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

#[test]
fn the_default_bound_is_two_seconds_and_can_be_changed() {
    let mut child = sleeper();
    assert_eq!(child.drop_reap_timeout(), DEFAULT_DROP_REAP_TIMEOUT);
    assert_eq!(DEFAULT_DROP_REAP_TIMEOUT, Duration::from_secs(2));
    child.set_drop_reap_timeout(Duration::from_millis(50));
    assert_eq!(child.drop_reap_timeout(), Duration::from_millis(50));
}

#[test]
fn drop_kills_the_child_and_returns_inside_the_bound() {
    let child = sleeper();
    let pid = child.id();
    let started = Instant::now();
    drop(child);
    assert!(
        started.elapsed() < DEFAULT_DROP_REAP_TIMEOUT + Duration::from_secs(1),
        "drop took {:?}",
        started.elapsed()
    );
    assert!(wait_until_gone(pid), "the dropped child is gone and reaped");
}

#[test]
fn a_zero_bound_does_not_wait_and_the_child_is_still_reaped() {
    let mut child = sleeper();
    child.set_drop_reap_timeout(Duration::ZERO);
    let pid = child.id();
    drop(child);
    // Whether or not the reap raced the drop, the process ends and is reaped (a background
    // thread finishes the wait), so the pid soon names nothing.
    assert!(wait_until_gone(pid), "the child was never reaped");
}
