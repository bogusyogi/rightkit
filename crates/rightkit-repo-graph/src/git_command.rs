//! The one bounded `git` runner for Blueprint, on `rightkit_child_process::run_command_capture`:
//! stdout kept to a byte cap and the wall clock bounded.

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use rightkit_child_process::{run_command_capture, CaptureOptions, OwnedCommand};

/// Why a bounded git run produced no output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitFailure {
    Spawn,
    Timeout,
    Wait,
    /// Non-zero exit status.
    Failed,
}

/// Stdout of a successful (exit 0) git run: the first `max_bytes` bytes, and whether more
/// were produced.
pub(crate) struct GitOutput {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

/// Runs `git <args>` (in `cwd` when given) with stdin and stderr closed. `timeout` bounds the
/// whole run, process exit and stdout end-of-stream together; `None` waits without a bound.
/// Output beyond `max_bytes` is drained and discarded so git never blocks on a full pipe.
pub(crate) fn run_git<I, S>(
    cwd: Option<&Path>,
    args: I,
    timeout: Option<Duration>,
    max_bytes: usize,
) -> Result<GitOutput, GitFailure>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = OwnedCommand::new("git");
    {
        let inner = command.command_mut();
        inner.args(args);
        if let Some(cwd) = cwd {
            inner.current_dir(cwd);
        }
    }
    // The owner's capture: first `max_bytes` of stdout kept and the rest drained, stderr drained
    // and dropped, stdin closed, the whole tree killed at the deadline (`None` waits).
    let mut options = CaptureOptions::new().stdout_cap(max_bytes).stderr_cap(0);
    if let Some(timeout) = timeout {
        options = options.timeout(timeout);
    }
    let output =
        run_command_capture(command, None, &options).map_err(|error| match error.kind() {
            std::io::ErrorKind::TimedOut => GitFailure::Timeout,
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
                GitFailure::Spawn
            }
            _ => GitFailure::Wait,
        })?;
    if output.timed_out {
        return Err(GitFailure::Timeout);
    }
    if !output.status.success() {
        return Err(GitFailure::Failed);
    }
    Ok(GitOutput {
        bytes: output.stdout.bytes,
        truncated: output.stdout.truncated,
    })
}
