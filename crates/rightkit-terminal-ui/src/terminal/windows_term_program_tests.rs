//! Regression coverage for bounded Windows terminal detection and child cleanup.

use super::VscodeDetection;
use super::keyboard_enhancement_disabled_for;
use super::read_windows_vscode_detection_with_timeout;
use super::run_bounded;
use pretty_assertions::assert_eq;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

#[test]
fn reads_term_program_only_from_successful_probe() {
    for (script, expected) in [
        ("printf 'TERM_PROGRAM=vscode\r\n'", VscodeDetection::VsCode),
        (
            "printf 'TERM_PROGRAM=vscode\r\n'; exit 1",
            VscodeDetection::Other,
        ),
        (
            "printf 'TERM_PROGRAM=WindowsTerminal'",
            VscodeDetection::Other,
        ),
        ("exit 1", VscodeDetection::Other),
        ("exit 2", VscodeDetection::Unknown),
    ] {
        assert_eq!(
            read_windows_vscode_detection_with_timeout(
                move |timeout| probe_sh(script, timeout),
                Duration::from_secs(/*secs*/ 5),
            ),
            expected,
        );
    }
}

#[test]
fn failed_launch_is_unknown() {
    assert_eq!(
        read_windows_vscode_detection_with_timeout(|_| None, Duration::from_secs(/*secs*/ 5)),
        VscodeDetection::Unknown,
    );
}

#[test]
fn times_out_and_reaps_running_probe() {
    let pid_file = pid_file("running");
    let script = pid_script(&pid_file);
    let started = Instant::now();
    let result = read_windows_vscode_detection_with_timeout(
        move |timeout| probe_sh(&script, timeout),
        Duration::from_millis(/*millis*/ 100),
    );
    assert_eq!(result, VscodeDetection::Unknown);
    assert!(keyboard_enhancement_disabled_for(
        /*disable_env*/ None, /*is_wsl*/ true, result
    ));
    assert!(started.elapsed() < Duration::from_secs(/*secs*/ 2));
    assert_child_reaped(read_pid(&pid_file));
}

#[test]
fn times_out_during_launch_and_reaps_child_when_launch_finishes() {
    let (release, blocked) = mpsc::channel();
    let pid_file = pid_file("launch");
    let script = pid_script(&pid_file);
    let started = Instant::now();
    let result = read_windows_vscode_detection_with_timeout(
        move |timeout| {
            // Model a WSL launch that cannot return until interop recovers.
            blocked
                .recv_timeout(Duration::from_secs(/*secs*/ 5))
                .unwrap();
            probe_sh(&script, timeout)
        },
        Duration::from_millis(/*millis*/ 100),
    );
    release.send(()).unwrap();
    assert_eq!(result, VscodeDetection::Unknown);
    assert!(keyboard_enhancement_disabled_for(
        /*disable_env*/ None, /*is_wsl*/ true, result
    ));
    assert!(started.elapsed() < Duration::from_secs(/*secs*/ 2));
    assert_child_reaped(read_pid(&pid_file));
}

fn probe_sh(script: &str, timeout: Duration) -> Option<(ExitStatus, Vec<u8>)> {
    let mut owned = rightkit_child_process::OwnedCommand::new("/bin/sh");
    owned.command_mut().args(["-c", script]);
    run_bounded(owned, timeout)
}

fn pid_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("rightkit-term-probe-{}-{name}", std::process::id()))
}

/// A probe that records its own pid, then outlives the deadline.
fn pid_script(file: &std::path::Path) -> String {
    format!("echo $$ > '{}'; exec sleep 30", file.display())
}

fn read_pid(file: &std::path::Path) -> u32 {
    let started = Instant::now();
    loop {
        if let Some(pid) = std::fs::read_to_string(file)
            .ok()
            .and_then(|text| text.trim().parse().ok())
        {
            let _ = std::fs::remove_file(file);
            return pid;
        }
        assert!(
            started.elapsed() < Duration::from_secs(/*secs*/ 5),
            "probe never recorded its pid"
        );
        std::thread::sleep(Duration::from_millis(/*millis*/ 10));
    }
}

fn assert_child_reaped(pid: u32) {
    let started = Instant::now();
    loop {
        // SAFETY: signal 0 only checks whether this process still exists.
        if unsafe {
            libc::kill(pid as libc::pid_t, /*sig*/ 0)
        } == -1
        {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(/*secs*/ 5),
            "timed-out terminal probe was not reaped"
        );
        std::thread::sleep(Duration::from_millis(/*millis*/ 10));
    }
}
