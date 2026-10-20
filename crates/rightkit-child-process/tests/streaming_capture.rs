#![cfg(any(unix, windows))]

use rightkit_child_process::{run_streaming, OwnedCommand, StreamOptions, WaitOutcome};
use std::{io::Write, time::Duration};

const MODE: &str = "RIGHTKIT_SLOW_STREAM_FIXTURE";
const BLOCKS: usize = 96;
const WIDTH: usize = 8192;

#[test]
fn streaming_child() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    let mut block = vec![b'x'; WIDTH];
    if mode == "stderr" {
        block[WIDTH - 1] = b'\n';
        let mut pipe = std::io::stderr().lock();
        for _ in 0..BLOCKS {
            pipe.write_all(&block).unwrap();
        }
        pipe.write_all(b"END\n").unwrap();
        pipe.flush().unwrap();
    } else {
        let mut pipe = std::io::stdout().lock();
        for _ in 0..BLOCKS {
            pipe.write_all(&block).unwrap();
        }
        pipe.write_all(b"END").unwrap();
        pipe.flush().unwrap();
    }
    std::process::exit(0);
}

// Bound each scenario in its own process so a capture regression cannot hang CI.
fn isolated(name: &str) -> bool {
    const CASE: &str = "RIGHTKIT_SLOW_STREAM_CASE";
    if std::env::var(CASE).ok().as_deref() == Some(name) {
        return false;
    }
    let mut command = OwnedCommand::new(std::env::current_exe().unwrap());
    command.windows_hide();
    command
        .command_mut()
        .args(["--exact", name, "--nocapture"])
        .env(CASE, name);
    let mut child = command.spawn().unwrap();
    assert!(
        matches!(child.wait_then_kill(Duration::from_secs(30)).unwrap(),
        WaitOutcome::Exited(status) if status.success())
    );
    true
}

fn command(mode: &str) -> OwnedCommand {
    let mut command = OwnedCommand::new(std::env::current_exe().unwrap());
    command
        .command_mut()
        .args(["--exact", "streaming_child", "--nocapture"])
        .env(MODE, mode);
    command
}

#[test]
fn slow_stdout_callbacks_preserve_every_byte_after_leader_exit() {
    if isolated("slow_stdout_callbacks_preserve_every_byte_after_leader_exit") {
        return;
    }
    let mut bytes = Vec::new();
    let output = run_streaming(
        command("stdout"),
        &StreamOptions::new(),
        &|| false,
        &mut |chunk| {
            bytes.extend_from_slice(chunk);
            std::thread::sleep(Duration::from_millis(80));
        },
        &mut |_| {},
    )
    .unwrap();
    assert!(output.status.success());
    assert!(!output.cancelled && !output.timed_out);
    // libtest emits a prelude on stdout; count fixture payload independently of it.
    assert_eq!(
        bytes.iter().filter(|byte| **byte == b'x').count(),
        BLOCKS * WIDTH
    );
    assert!(bytes.ends_with(b"END"));
    assert!(output.stdout_tail.ends_with(b"END"));
}

#[test]
fn slow_stderr_callbacks_preserve_every_line_after_leader_exit() {
    if isolated("slow_stderr_callbacks_preserve_every_line_after_leader_exit") {
        return;
    }
    let mut lines = Vec::new();
    let output = run_streaming(
        command("stderr"),
        &StreamOptions::new(),
        &|| false,
        &mut |_| {},
        &mut |line| {
            lines.push(line.to_owned());
            std::thread::sleep(Duration::from_millis(80));
        },
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(lines.len(), BLOCKS + 1);
    assert!(lines[..BLOCKS]
        .iter()
        .all(|line| line.len() == WIDTH - 1 && line.bytes().all(|byte| byte == b'x')));
    assert_eq!(lines.last().unwrap(), "END");
    assert!(output.stderr_tail.ends_with("END"));
}
