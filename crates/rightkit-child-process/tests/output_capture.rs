//! Byte-exact bounded capture of a real child's stdout (issue #120).

use rightkit_child_process::{read_bounded, OutputCapture, OwnedCommand};
use std::{process::Stdio, time::Duration};

/// A child that prints `{"a":1}` plus a newline, with no other output.
fn json_child() -> OwnedCommand {
    let mut command = if cfg!(windows) {
        let mut c = OwnedCommand::new("cmd");
        c.command_mut().args(["/C", "echo {\"a\":1}"]);
        c
    } else {
        let mut c = OwnedCommand::new("/bin/sh");
        c.command_mut().args(["-c", "printf '{\"a\":1}\\n'"]);
        c
    };
    command
        .command_mut()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

#[test]
fn a_real_childs_stdout_is_captured_with_its_trailing_newline() {
    let mut child = json_child().spawn().expect("spawn");
    let stdout = child.take_stdout().expect("piped stdout");
    let mut capture = OutputCapture::capture(stdout, 4096);
    let _ = child.wait().expect("wait");
    let out = capture.finish();
    assert!(!out.truncated);
    let text = String::from_utf8(out.bytes).expect("utf8");
    assert!(text.starts_with("{\"a\":1}"), "got {text:?}");
    assert!(text.ends_with('\n'), "the trailing newline was dropped: {text:?}");
}

#[test]
fn a_child_that_writes_far_past_the_cap_does_not_block_and_is_marked_truncated() {
    let mut command = if cfg!(windows) {
        let mut c = OwnedCommand::new("powershell");
        c.command_mut().args([
            "-NoProfile",
            "-Command",
            "[Console]::Out.Write('x' * 1000000)",
        ]);
        c
    } else {
        let mut c = OwnedCommand::new("/bin/sh");
        c.command_mut()
            .args(["-c", "head -c 1000000 /dev/zero | tr '\\0' x"]);
        c
    };
    command
        .command_mut()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().expect("spawn");
    let mut capture = OutputCapture::capture(child.take_stdout().expect("stdout"), 100);
    let out = capture.finish_within(Duration::from_secs(60));
    let _ = child.wait();
    assert!(capture.is_finished(), "the drain did not reach the end");
    assert_eq!(out.bytes.len(), 100);
    assert!(out.bytes.iter().all(|b| *b == b'x'));
    assert!(out.truncated);
    assert!(out.total_bytes >= 1_000_000);
}

#[test]
fn the_blocking_reader_reads_a_real_childs_stdout() {
    let mut child = json_child().spawn().expect("spawn");
    let stdout = child.take_stdout().expect("piped stdout");
    let out = read_bounded(stdout, 4096).expect("read");
    let _ = child.wait();
    assert!(out.bytes.starts_with(b"{\"a\":1}"));
    assert!(!out.truncated);
}
