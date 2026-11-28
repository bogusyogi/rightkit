//! The one place that hands a URL to the desktop's default handler.
//!
//! The launcher is an OS API difference (Windows has no `xdg-open`), so it is the only per-OS
//! part; every caller shares it so no call site can miss a platform.

use std::process::{ExitStatus, Stdio};

pub(crate) fn open_url(url: &str) -> std::io::Result<ExitStatus> {
    let mut command = if cfg!(windows) {
        let mut command = rightkit_child_process::command_std("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    } else if cfg!(target_os = "macos") {
        rightkit_child_process::command_std("open")
    } else {
        rightkit_child_process::command_std("xdg-open")
    };
    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
}
