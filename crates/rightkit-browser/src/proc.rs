//! Chrome as an owned child. Spawned through `rightkit-process` so it is a
//! process-group leader on Unix and a Windows job object member
//! (KILL_ON_JOB_CLOSE, no breakaway); a parent-death watchdog covers a host
//! that dies without running destructors.
use crate::error::{BrowserError, Result};
use rightkit_process::{OwnedChild, OwnedCommand};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

pub(crate) struct ChromeProc {
    pub child: OwnedChild,
    /// Unix only: kills Chrome's process group when this host process vanishes.
    pub _watchdog: Option<OwnedChild>,
    pub ws_url: String,
}

pub(crate) fn spawn_chrome(exe: &Path, args: &[String], timeout: Duration) -> Result<ChromeProc> {
    let mut cmd = OwnedCommand::new(exe);
    cmd.windows_hide();
    cmd.command_mut().args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| BrowserError::Launch(e.to_string()))?;
    let stderr = child.take_stderr().ok_or_else(|| BrowserError::Launch("no stderr".into()))?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    // Keeps draining stderr after the URL line so Chrome never blocks on a full pipe.
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(|l| l.ok()) {
            if let Some(u) = line.split("DevTools listening on ").nth(1) {
                let _ = tx.send(u.trim().to_string());
            }
        }
    });
    let ws_url = rx
        .recv_timeout(timeout)
        .map_err(|_| BrowserError::Launch("chrome did not report a DevTools endpoint".into()))?;
    let watchdog = spawn_watchdog(child.id());
    Ok(ChromeProc { child, _watchdog: watchdog, ws_url })
}

#[cfg(unix)]
fn spawn_watchdog(chrome_pgid: u32) -> Option<OwnedChild> {
    // Polls the host and Chrome; when the host is gone, kills Chrome's whole
    // process group. Exits by itself once Chrome is gone.
    let script = r#"while kill -0 "$1" 2>/dev/null && kill -0 "$2" 2>/dev/null; do sleep 0.2; done
kill -0 "$2" 2>/dev/null && kill -KILL -- "-$2" 2>/dev/null
exit 0"#;
    let mut cmd = OwnedCommand::new("/bin/sh");
    cmd.command_mut()
        .args(["-c", script, "rk-watchdog", &std::process::id().to_string(), &chrome_pgid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn().ok()
}

// Windows: the job object created by `OwnedCommand` closes with this process
// and kills Chrome (KILL_ON_JOB_CLOSE), so no watchdog is needed.
#[cfg(not(unix))]
fn spawn_watchdog(_chrome_pid: u32) -> Option<OwnedChild> {
    None
}
