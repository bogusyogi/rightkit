#![cfg(any(unix, windows))]

use rightkit_child_process::{run_command, status_inherited_terminal, OwnedCommand, RunOptions};
use std::{
    io::{Read, Write},
    process::Command,
    time::Duration,
};

const MODE: &str = "RIGHTKIT_INHERITED_TERMINAL_FIXTURE";
#[cfg(unix)]
const GROUP: &str = "RIGHTKIT_INHERITED_TERMINAL_GROUP";

#[test]
fn terminal_fixture() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    if mode == "relay" {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "terminal_fixture", "--nocapture"])
            .env(MODE, "echo");
        #[cfg(unix)]
        {
            // SAFETY: getpgrp has no parameters & reads this process's group identity.
            command.env(GROUP, unsafe { libc::getpgrp() }.to_string());
        }
        let status = status_inherited_terminal(&mut command).unwrap();
        std::process::exit(status.code().unwrap_or(1));
    }
    assert_eq!(mode, "echo");
    #[cfg(unix)]
    {
        let expected: i32 = std::env::var(GROUP).unwrap().parse().unwrap();
        // SAFETY: getpgrp has no parameters & reads this process's group identity.
        assert_eq!(
            unsafe { libc::getpgrp() },
            expected,
            "terminal child changed process group"
        );
    }
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    std::io::stdout().write_all(&input).unwrap();
    std::io::stdout().flush().unwrap();
    std::io::stderr().write_all(b"terminal-stderr\r\n").unwrap();
    std::io::stderr().flush().unwrap();
    std::process::exit(7);
}

#[test]
fn terminal_status_preserves_inherited_stdio_group_and_exit_code() {
    let mut command = OwnedCommand::new(std::env::current_exe().unwrap());
    command
        .command_mut()
        .args(["--exact", "terminal_fixture", "--nocapture"])
        .env(MODE, "relay");
    // Outer containment bounds only this test harness. The terminal pass-through inside it
    // must inherit that harness's group & all three of its streams without inserting pipes.
    let input = b"terminal-input\0\r\n";
    let output = run_command(
        command,
        Some(input),
        &RunOptions::new(Duration::from_secs(10)),
    )
    .unwrap();
    assert!(!output.timed_out);
    assert_eq!(output.status.code(), Some(7));
    assert!(output.stdout.bytes.ends_with(input));
    assert_eq!(output.stderr.bytes, b"terminal-stderr\r\n");
}
