//! Chrome as an owned child. Spawned through `rightkit-process` so it is a
//! process-group leader on Unix and a Windows job object member
//! (KILL_ON_JOB_CLOSE, no breakaway), bound first to the caller's own job when one
//! is supplied; a parent-death watchdog covers a host that dies without running
//! destructors on Unix.
use crate::error::{BrowserError, Result};
use rightkit_process::{OwnedChild, OwnedCommand};
use std::ffi::OsString;
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

impl ChromeProc {
    /// Unix: SIGTERM Chrome's whole process group (Chrome's own clean-shutdown path),
    /// wait up to `grace`, then SIGKILL the group. Windows: terminate the job's tree.
    /// Then stops the watchdog.
    pub(crate) fn terminate_gracefully(&mut self, grace: Duration) {
        #[cfg(unix)]
        if !matches!(self.child.try_wait(), Ok(Some(_))) && self.child.wait_or_kill(grace).is_err()
        {
            let _ = self.child.terminate_tree();
        }
        // Windows: always terminate the retained job, even when the browser process has
        // already exited: its renderer/GPU/utility helpers outlive it and keep the profile
        // directory open. Bounded so Drop can never hang.
        #[cfg(not(unix))]
        {
            let _ = grace;
            let _ = self.child.terminate_tree_bounded(Duration::from_secs(2));
        }
        if let Some(w) = self._watchdog.as_mut() {
            let _ = w.terminate_tree();
        }
    }
}

/// The caller's job object to bind Chrome to (Windows only).
#[cfg(windows)]
pub(crate) type CallerJob = Option<std::sync::Arc<std::os::windows::io::OwnedHandle>>;
#[cfg(not(windows))]
pub(crate) type CallerJob = Option<std::convert::Infallible>;

/// One argv element, preserving native path bytes. Command handles OS quoting;
/// adding shell quotes here would make them part of the directory name.
pub(crate) fn profile_arg(path: &Path) -> OsString {
    let mut arg = OsString::from("--user-data-dir=");
    arg.push(path.as_os_str());
    arg
}

fn chrome_command(exe: &Path, args: &[OsString]) -> OwnedCommand {
    let mut cmd = OwnedCommand::new(exe);
    cmd.windows_hide();
    cmd.command_mut()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    cmd
}

pub(crate) fn spawn_chrome(
    exe: &Path,
    args: &[OsString],
    timeout: Duration,
    job: CallerJob,
) -> Result<ChromeProc> {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut cmd = chrome_command(exe, args);
    #[cfg(windows)]
    if let Some(job) = &job {
        use std::os::windows::io::AsHandle;
        cmd.windows_job(job.as_handle())
            .map_err(|e| BrowserError::Launch(e.to_string()))?;
    }
    #[cfg(not(windows))]
    let _ = job;
    let mut child = cmd
        .spawn()
        .map_err(|e| BrowserError::Launch(e.to_string()))?;
    let stderr = child
        .take_stderr()
        .ok_or_else(|| BrowserError::Launch("no stderr".into()))?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    // Keeps draining stderr after the URL line so Chrome never blocks on a full pipe. The
    // drain reads bytes, not text: a line that is not UTF-8 (Windows writes some messages in
    // the system code page) must not end it, or the pipe fills and Chrome stalls on its next
    // write.
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let text = String::from_utf8_lossy(&line);
            if let Some(u) = text.split("DevTools listening on ").nth(1) {
                let _ = tx.send(u.trim().to_string());
            }
        }
    });
    let ws_url = rx
        .recv_timeout(timeout)
        .map_err(|_| BrowserError::Launch("chrome did not report a DevTools endpoint".into()))?;
    let watchdog = spawn_watchdog(child.id());
    Ok(ChromeProc {
        child,
        _watchdog: watchdog,
        ws_url,
    })
}

#[cfg(unix)]
fn spawn_watchdog(chrome_pgid: u32) -> Option<OwnedChild> {
    // Polls the host and Chrome; when the host is gone, SIGTERMs Chrome's whole
    // process group, waits up to 5 s, then SIGKILLs what is left. Exits by itself
    // once Chrome is gone.
    let script = r#"while kill -0 "$1" 2>/dev/null && kill -0 "$2" 2>/dev/null; do sleep 0.2; done
if kill -0 "$2" 2>/dev/null; then
  kill -TERM -- "-$2" 2>/dev/null
  i=0
  while [ "$i" -lt 25 ] && kill -0 -- "-$2" 2>/dev/null; do sleep 0.2; i=$((i+1)); done
  kill -KILL -- "-$2" 2>/dev/null
fi
exit 0"#;
    let mut cmd = OwnedCommand::new("/bin/sh");
    cmd.command_mut()
        .args([
            "-c",
            script,
            "rk-watchdog",
            &std::process::id().to_string(),
            &chrome_pgid.to_string(),
        ])
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_path_is_one_native_argv_element() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("profile with spaces & ü 'quotes'");
        std::fs::create_dir(&path).unwrap();
        let arg = profile_arg(&path);
        let mut expected = OsString::from("--user-data-dir=");
        expected.push(path.as_os_str());
        let mut cmd = chrome_command(Path::new("unused-chrome"), &[arg]);
        let argv: Vec<_> = cmd.command_mut().get_args().map(|a| a.to_owned()).collect();
        assert_eq!(argv, vec![expected]);
    }

    // APFS (macOS) rejects non-UTF-8 file names with EILSEQ; Linux filesystems accept them.
    #[cfg(target_os = "linux")]
    #[test]
    fn profile_argv_preserves_non_utf8_bytes_and_shell_characters() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let temp = tempfile::tempdir().unwrap();
        let path = temp
            .path()
            .join(OsString::from_vec(b"profile \xff \"$;\n".to_vec()));
        std::fs::create_dir(&path).unwrap();
        let mut cmd = chrome_command(Path::new("unused-chrome"), &[profile_arg(&path)]);
        let mut expected = b"--user-data-dir=".to_vec();
        expected.extend_from_slice(path.as_os_str().as_bytes());
        let argv: Vec<_> = cmd.command_mut().get_args().map(|a| a.as_bytes()).collect();
        assert_eq!(argv, vec![expected.as_slice()]);
    }
}
