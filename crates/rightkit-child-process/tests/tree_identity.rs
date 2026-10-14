#![cfg(unix)]

//! Process-group identity rule, observed through the public API.
//!
//! A group signal may be sent only while this process still holds the group leader as its
//! own unreaped child, because the kernel cannot recycle that id until the leader is reaped.
//! Once any path has reaped the leader, group signalling is over for the `OwnedChild` and all
//! of its handle clones. Each test uses a survivor: a grandchild that outlives the leader.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use rightkit_child_process::{OwnedChild, OwnedCommand, TerminationOptions, WaitOutcome};

fn pid_file(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("rightkit-identity-{tag}-{}", std::process::id()));
    let _ = fs::remove_file(&path);
    path
}

fn quiet(script: &str, file: &Path) -> OwnedCommand {
    let mut command = OwnedCommand::new("/bin/sh");
    command
        .command_mut()
        .args(["-c", script])
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// The leader exits at once and leaves `sleep 600` running in its group.
fn leader_with_survivor(file: &Path) -> OwnedCommand {
    quiet("sleep 600 & echo $! > \"$0\"", file)
}

fn read_pid(file: &Path) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(pid) = fs::read_to_string(file)
            .ok()
            .and_then(|text| text.trim().parse().ok())
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "pid file was not written");
        thread::sleep(Duration::from_millis(5));
    }
}

fn is_running(pid: i32) -> bool {
    // SAFETY: signal 0 only checks existence.
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    let state = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();
    !state.is_empty() && !state.starts_with('Z')
}

fn wait_until_gone(pid: i32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while is_running(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    true
}

/// The survivor must still be running after `settle`; always cleans it up.
fn assert_survives(pid: i32, settle: Duration, why: &str) {
    thread::sleep(settle);
    let alive = is_running(pid);
    // SAFETY: cleanup of this test's own sleeper.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    assert!(
        alive,
        "{why}: the group was signalled after the leader was reaped"
    );
}

fn start_survivor(tag: &str) -> (OwnedChild, i32, PathBuf) {
    let file = pid_file(tag);
    let child = leader_with_survivor(&file).spawn().expect("spawn");
    let survivor = read_pid(&file);
    assert!(is_running(survivor));
    (child, survivor, file)
}

const SETTLE: Duration = Duration::from_millis(500);

#[test]
fn reaping_through_the_child_accessor_retires_group_signalling() {
    let (child, survivor, file) = start_survivor("accessor");
    child.child().wait().expect("reap through the accessor");
    drop(child);
    assert_survives(survivor, SETTLE, "accessor reap then drop");
    let _ = fs::remove_file(file);
}

#[test]
fn a_plain_wait_followed_by_drop_signals_nothing() {
    let (mut child, survivor, file) = start_survivor("plainwait");
    child.wait().expect("wait");
    drop(child);
    assert_survives(survivor, SETTLE, "wait then drop");
    let _ = fs::remove_file(file);
}

#[test]
fn a_try_wait_reap_followed_by_terminate_tree_signals_nothing() {
    let (mut child, survivor, file) = start_survivor("trywait");
    let started = Instant::now();
    while child.try_wait().expect("try_wait").is_none() {
        assert!(started.elapsed() < Duration::from_secs(10));
        thread::sleep(Duration::from_millis(5));
    }
    child.terminate_tree().expect("terminate after reap");
    assert_survives(survivor, SETTLE, "try_wait reap then terminate_tree");
    let _ = fs::remove_file(file);
}

#[test]
fn a_clone_that_reaps_retires_the_owner() {
    let (mut child, survivor, file) = start_survivor("clone");
    let handle = child.process_handle().expect("handle");
    handle.wait().expect("clone reaps");
    child
        .terminate_tree()
        .expect("terminate after the clone reaped");
    assert_survives(survivor, SETTLE, "clone reap then owner terminate_tree");
    let _ = fs::remove_file(file);
}

#[test]
fn terminating_before_the_reap_kills_the_survivor_even_after_the_leader_exited() {
    let (mut child, survivor, file) = start_survivor("pinned");
    // Let the leader exit on its own; nobody has reaped it, so it still pins the group id.
    thread::sleep(Duration::from_millis(200));
    child.terminate_tree().expect("terminate");
    let gone = wait_until_gone(survivor, Duration::from_secs(5));
    // SAFETY: cleanup only.
    unsafe { libc::kill(survivor, libc::SIGKILL) };
    assert!(
        gone,
        "an unreaped leader must still allow the group to be killed"
    );
    let _ = fs::remove_file(file);
}

/// The leader leaves on TERM; the grandchild ignores TERM.
fn term_ignoring_straggler(file: &Path) -> OwnedCommand {
    quiet(
        "(trap '' TERM; exec sleep 600) & echo $! > \"$0\"; \
         trap 'exit 0' TERM; while :; do sleep 0.05; done",
        file,
    )
}

#[test]
fn wait_or_kill_removes_a_straggler_that_ignores_term_after_the_leader_exits() {
    let file = pid_file("straggler");
    let mut child = term_ignoring_straggler(&file).spawn().expect("spawn");
    let straggler = read_pid(&file);
    let outcome = child
        .wait_or_kill(Duration::from_secs(5))
        .expect("wait_or_kill");
    assert!(matches!(outcome, WaitOutcome::Exited(_)), "{outcome:?}");
    let gone = wait_until_gone(straggler, Duration::from_secs(2));
    // SAFETY: cleanup only.
    unsafe { libc::kill(straggler, libc::SIGKILL) };
    assert!(gone, "the straggler outlived wait_or_kill");
    let _ = fs::remove_file(file);
}

#[test]
fn tree_grace_escalates_to_kill_for_a_straggler_after_the_leader_exits() {
    let file = pid_file("grace-kill");
    let mut child = term_ignoring_straggler(&file).spawn().expect("spawn");
    let straggler = read_pid(&file);
    child
        .terminate_tree_with_options(
            TerminationOptions::default().with_grace(Duration::from_millis(300)),
        )
        .expect("terminate");
    let gone = wait_until_gone(straggler, Duration::from_secs(2));
    // SAFETY: cleanup only.
    unsafe { libc::kill(straggler, libc::SIGKILL) };
    assert!(gone, "KILL after the grace must reach a straggler");
    let _ = fs::remove_file(file);
}

#[test]
fn grace_ends_early_when_nothing_but_the_exited_leader_remains() {
    let file = pid_file("early");
    let mut child = quiet(
        "trap 'exit 0' TERM; echo $$ > \"$0\"; while :; do sleep 0.05; done",
        &file,
    )
    .spawn()
    .expect("spawn");
    read_pid(&file);
    let started = Instant::now();
    child
        .terminate_tree_with_options(
            TerminationOptions::default().with_grace(Duration::from_secs(10)),
        )
        .expect("terminate");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "grace waited {:?} although the whole tree was already gone",
        started.elapsed()
    );
    let _ = fs::remove_file(file);
}

/// A deadline fires with zero grace while the child is exiting on its own and a monitor
/// thread (like the media runner's exit watcher) is reaping through a handle clone. Whichever
/// side wins, the caller's kill must report success: a target that is already gone is not an
/// error, and signalling an exited leader is not an error either.
#[test]
fn a_zero_grace_kill_never_fails_while_the_child_exits_during_it() {
    for round in 0..250u64 {
        let file = pid_file("zero-grace");
        let mut child = quiet("sleep 0.03 & wait", &file).spawn().expect("spawn");
        let monitor = {
            let handle = child.process_handle().expect("handle");
            thread::spawn(move || handle.wait().expect("monitor wait"))
        };
        thread::sleep(Duration::from_millis(15 + round % 40));
        let outcome = child
            .wait_or_kill(Duration::ZERO)
            .unwrap_or_else(|error| panic!("round {round}: zero-grace kill failed: {error}"));
        assert!(
            matches!(outcome, WaitOutcome::Exited(_) | WaitOutcome::Terminated(_)),
            "round {round}"
        );
        monitor.join().expect("monitor");
        let _ = fs::remove_file(&file);
    }
}
