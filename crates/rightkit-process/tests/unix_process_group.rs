#![cfg(unix)]

//! macOS/Linux mirror of `windows_process_tree.rs`. Windows job-object evidence says nothing about
//! POSIX process groups, so the same four ownership guarantees are proven again here.

use std::{
    fs,
    num::NonZeroU32,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use rightkit_process::{AdoptedProcess, OwnedCommand, WaitOutcome};

fn shell(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn wait_for_pid(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        assert!(Instant::now() < deadline, "child PID file was not created");
        thread::sleep(Duration::from_millis(20));
    }
}

/// A child that spawns a grandchild, publishes the grandchild PID, then blocks on it. The
/// grandchild only dies if the whole process group goes down.
fn tree_command(pid_file: &Path) -> Command {
    let quoted = pid_file.display().to_string().replace('\'', "'\\''");
    shell(&format!(
        "sleep 60 & child=$!; printf '%s' \"$child\" > '{quoted}'; wait $child"
    ))
}

/// Same tree, but parent and grandchild both ignore SIGTERM. `wait_or_kill` asks politely with
/// SIGTERM first, so only a tree that refuses it reaches the forced-kill path Windows always takes.
fn unkillable_tree_command(pid_file: &Path) -> Command {
    let quoted = pid_file.display().to_string().replace('\'', "'\\''");
    shell(&format!(
        "trap '' TERM; /bin/sh -c 'trap \"\" TERM; sleep 60' & child=$!; \
         printf '%s' \"$child\" > '{quoted}'; wait $child"
    ))
}

fn wait_until_stopped(process: &AdoptedProcess) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while process.is_running().unwrap_or(false) {
        assert!(
            Instant::now() < deadline,
            "process {} remained alive",
            process.id()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn temp_pid_file(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("rightkit-process-{tag}-{}", std::process::id()));
    let _ = fs::remove_file(&path);
    path
}

#[test]
fn terminating_owned_tree_kills_parent_and_grandchild_but_not_sibling() {
    let temp = temp_pid_file("tree");
    // Spawned unowned, so it sits in the test's own process group and must survive.
    let mut sibling = shell("sleep 60").spawn().unwrap();
    let sibling_pid = NonZeroU32::new(sibling.id()).unwrap();

    let mut owned = OwnedCommand::from_command(tree_command(&temp))
        .spawn()
        .unwrap();
    let parent = AdoptedProcess::new(NonZeroU32::new(owned.id()).unwrap());
    let grandchild = AdoptedProcess::new(NonZeroU32::new(wait_for_pid(&temp)).unwrap());

    owned.terminate_tree().unwrap();
    wait_until_stopped(&parent);
    wait_until_stopped(&grandchild);
    assert!(AdoptedProcess::new(sibling_pid).is_running().unwrap());

    sibling.kill().unwrap();
    sibling.wait().unwrap();
    let _ = fs::remove_file(temp);
}

#[test]
fn graceful_exit_does_not_report_a_forced_kill() {
    let mut child = OwnedCommand::from_command(shell("sleep 0.05"))
        .spawn()
        .unwrap();

    assert!(matches!(
        child.wait_or_kill(Duration::from_secs(2)).unwrap(),
        WaitOutcome::Exited(_)
    ));
}

#[test]
fn sigterm_shutdown_takes_the_grandchild_down_too() {
    let temp = temp_pid_file("sigterm");
    let mut child = OwnedCommand::from_command(tree_command(&temp))
        .spawn()
        .unwrap();
    let grandchild = AdoptedProcess::new(NonZeroU32::new(wait_for_pid(&temp)).unwrap());

    // A tree that honours SIGTERM shuts itself down inside the grace window: that is an exit, not
    // a forced kill. The grandchild must still go with it — the signal reaches the whole group.
    assert!(matches!(
        child.wait_or_kill(Duration::from_secs(2)).unwrap(),
        WaitOutcome::Exited(_)
    ));
    wait_until_stopped(&grandchild);
    let _ = fs::remove_file(temp);
}

#[test]
fn grace_expiry_terminates_the_whole_tree() {
    let temp = temp_pid_file("grace");
    let mut child = OwnedCommand::from_command(unkillable_tree_command(&temp))
        .spawn()
        .unwrap();
    let grandchild = AdoptedProcess::new(NonZeroU32::new(wait_for_pid(&temp)).unwrap());

    assert!(matches!(
        child.wait_or_kill(Duration::from_millis(50)).unwrap(),
        WaitOutcome::Terminated(_)
    ));
    wait_until_stopped(&grandchild);
    let _ = fs::remove_file(temp);
}

#[test]
fn dropping_owned_child_leaves_no_orphan() {
    let temp = temp_pid_file("drop");
    let grandchild = {
        let mut child = OwnedCommand::from_command(tree_command(&temp))
            .spawn()
            .unwrap();
        let pid = wait_for_pid(&temp);
        assert!(child.try_wait().unwrap().is_none());
        AdoptedProcess::new(NonZeroU32::new(pid).unwrap())
    };

    wait_until_stopped(&grandchild);
    let _ = fs::remove_file(temp);
}
