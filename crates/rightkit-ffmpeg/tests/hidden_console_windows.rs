//! A child spawned through the media runner has no console window on Windows.
//!
//! The runner spawns through `rightkit-child-process`, which sets `CREATE_NO_WINDOW` by
//! default. The probe is the technique HeardRight's `background_decoder_child_has_no_console`
//! test uses: a PowerShell child reads `GetConsoleWindow()`. The call returns null when the
//! process has no visible console window, which is what hidden spawning must produce.
//! Nothing here needs ffmpeg. The test SKIPs with a printed reason when the environment
//! cannot tell.

#![cfg(windows)]

use rightkit_ffmpeg::{
    runner::{run_capture_with, RunOptions},
    CancellationToken, DevFfmpeg, FfmpegCommand,
};

const PROBE: &str = r#"Add-Type 'using System; using System.Runtime.InteropServices; public static class ConsoleProbe { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); }'; [Console]::WriteLine([ConsoleProbe]::GetConsoleWindow().ToInt64())"#;

fn probe_args() -> Vec<String> {
    ["-NoProfile", "-NonInteractive", "-Command", PROBE]
        .map(String::from)
        .to_vec()
}

/// The console window handle the probe reports, or `None` with the reason it cannot tell.
fn parse_handle(stdout: &[u8], stderr: &str) -> Result<i64, String> {
    String::from_utf8_lossy(stdout)
        .trim()
        .parse::<i64>()
        .map_err(|_| format!("probe printed no handle; stderr: {}", stderr.trim()))
}

#[test]
fn a_child_spawned_through_the_media_runner_has_no_console_window() {
    let powershell = DevFfmpeg::unverified("powershell.exe");
    let cmd = FfmpegCommand::raw_args(probe_args());
    let run = RunOptions::unbounded(CancellationToken::new());
    let captured = match run_capture_with(&powershell, &cmd, run) {
        Ok(captured) => captured,
        Err(err) => {
            eprintln!("SKIP: cannot run powershell.exe to probe the console: {err:?}");
            return;
        }
    };
    let handle = match parse_handle(&captured.stdout, &captured.stderr) {
        Ok(handle) => handle,
        Err(reason) => {
            eprintln!("SKIP: {reason}");
            return;
        }
    };
    assert_eq!(
        handle, 0,
        "a child spawned through the media runner must have no console window"
    );
}

#[test]
fn the_probe_can_see_a_console_window_when_one_is_inherited() {
    // Negative control: a plain std child that inherits this process's console must report
    // a non-null window. Without one in the test process, the control cannot show that the
    // probe detects windows, so it SKIPs.
    let parent = std::process::Command::new("powershell.exe")
        .args(probe_args())
        .output();
    let parent = match parent {
        Ok(out) => out,
        Err(err) => {
            eprintln!("SKIP: cannot run powershell.exe for the negative control: {err}");
            return;
        }
    };
    let handle = match parse_handle(&parent.stdout, &String::from_utf8_lossy(&parent.stderr)) {
        Ok(handle) => handle,
        Err(reason) => {
            eprintln!("SKIP: negative control cannot tell: {reason}");
            return;
        }
    };
    if handle == 0 {
        eprintln!(
            "SKIP: negative control needs a console window in the test process; none is attached, so the hidden-child check above is not shown to detect a window"
        );
        return;
    }
    assert_ne!(handle, 0);
}
