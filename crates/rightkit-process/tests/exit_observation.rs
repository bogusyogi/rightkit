#![cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    )
))]

//! `ProcessHandle::wait_exited` and `wait_exited_timeout` observe the leader's exit without
//! reaping it, so the owner can still kill the group afterwards. This is the call a monitor
//! uses when a descendant may outlive the leader and hold its pipes (the shape of a media
//! runner or a sandbox with a deadline): the leader exits at once, a background process keeps
//! stdout open, and the pipe can only reach end of file if that process is killed.

use std::{
    io::Read,
    process::Stdio,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use rightkit_process::{OwnedChild, OwnedCommand};

const LONG: Duration = Duration::from_secs(20);

/// The leader prints a line and exits at once. `sleep 600` stays in its group holding stdout.
fn leader_exits_and_a_straggler_holds_stdout() -> OwnedChild {
    let mut command = OwnedCommand::new("/bin/sh");
    command
        .command_mut()
        .args(["-c", "sleep 600 & echo started; exit 0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.spawn().expect("spawn")
}

/// Reads stdout to end of file on its own thread and sends everything it read.
fn drain(mut child: OwnedChild) -> (OwnedChild, mpsc::Receiver<Vec<u8>>) {
    let mut stdout = child.take_stdout().expect("stdout is piped");
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = sender.send(bytes);
    });
    (child, receiver)
}

#[test]
fn the_group_is_still_killable_after_the_exit_was_observed_so_the_pipe_reaches_eof() {
    let (mut child, output) = drain(leader_exits_and_a_straggler_holds_stdout());
    let handle = child.process_handle().expect("handle");
    // The deadline-bounded observation, as a runner with a timeout makes it.
    assert!(
        handle.wait_exited_timeout(LONG).expect("observe the exit"),
        "the leader exits at once"
    );
    // The straggler still holds the pipe, so the reader has not finished.
    assert!(
        output.recv_timeout(Duration::from_millis(200)).is_err(),
        "the straggler should still hold stdout open"
    );
    // The leader is an unreaped zombie, so the group id is still ours and the KILL reaches it.
    let started = Instant::now();
    child.terminate_tree().expect("terminate the tree");
    let bytes = output
        .recv_timeout(LONG)
        .expect("the pipe never reached end of file: the straggler survived");
    assert_eq!(bytes, b"started\n");
    assert!(started.elapsed() < LONG);
}

#[test]
fn wait_exited_alone_has_the_same_property() {
    let (mut child, output) = drain(leader_exits_and_a_straggler_holds_stdout());
    child
        .process_handle()
        .expect("handle")
        .wait_exited()
        .expect("observe the exit");
    child.terminate_tree().expect("terminate the tree");
    let bytes = output
        .recv_timeout(LONG)
        .expect("the pipe never reached end of file: the straggler survived");
    assert_eq!(bytes, b"started\n");
}
