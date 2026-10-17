//! One call that runs a program to completion with a deadline, bounded output capture, and no
//! console window on Windows: the shape of the hand-rolled timeout loops that apps wrote around
//! [`OwnedCommand`](crate::OwnedCommand) and [`OutputCapture`](crate::OutputCapture).
//!
//! The child is owned: it runs in its own process group (or session on Unix) inside a Job
//! Object on Windows, and when the deadline passes its whole tree is terminated, not only the
//! leader. Stdin is null. Output beyond each cap is drained and discarded, so a chatty child
//! never blocks on a full pipe, and [`CapturedOutput::truncated`] says when that happened.

use crate::{CapturedOutput, OutputCapture, OwnedCommand};
use std::{
    ffi::OsStr,
    io,
    process::{ExitStatus, Stdio},
    time::Duration,
};

/// How long to wait for a pipe to end after the child has exited, before returning what was
/// captured (a grandchild that inherited the pipe can keep it open).
const DRAIN_WAIT: Duration = Duration::from_secs(2);

/// Limits for [`run`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RunOptions {
    /// How long the program may run before its tree is terminated.
    pub timeout: Duration,
    /// Most bytes kept from stdout (the rest is drained and discarded).
    pub stdout_cap: usize,
    /// Most bytes kept from stderr.
    pub stderr_cap: usize,
    /// Hide the console window on Windows. The default. `false` leaves it visible.
    pub no_window: bool,
}

impl RunOptions {
    /// A deadline with 1 MiB caps on each stream and no window.
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            stdout_cap: 1024 * 1024,
            stderr_cap: 1024 * 1024,
            no_window: true,
        }
    }

    pub fn stdout_cap(mut self, bytes: usize) -> Self {
        self.stdout_cap = bytes;
        self
    }

    pub fn stderr_cap(mut self, bytes: usize) -> Self {
        self.stderr_cap = bytes;
        self
    }

    pub fn no_window(mut self, hidden: bool) -> Self {
        self.no_window = hidden;
        self
    }
}

/// What [`run`] observed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunOutput {
    /// The program's exit status. When `timed_out` is true, the status of the terminated tree's
    /// leader (on Unix, usually a signal).
    pub status: ExitStatus,
    /// True when the deadline passed and the tree was terminated.
    pub timed_out: bool,
    pub stdout: CapturedOutput,
    pub stderr: CapturedOutput,
}

/// Runs `program` with `args` to completion or until `options.timeout`, then returns its status
/// and bounded output. Fails only when the program cannot be started; a non-zero exit is a
/// normal `Ok` with `status` set, so the caller decides what it means.
pub fn run<I, S>(program: impl AsRef<OsStr>, args: I, options: &RunOptions) -> io::Result<RunOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = OwnedCommand::new(program);
    command
        .command_mut()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if options.no_window {
        command.windows_hide();
    } else {
        command.windows_visible();
    }
    let mut child = command.spawn()?;
    let mut stdout = child
        .take_stdout()
        .map(|pipe| OutputCapture::capture(pipe, options.stdout_cap));
    let mut stderr = child
        .take_stderr()
        .map(|pipe| OutputCapture::capture(pipe, options.stderr_cap));
    // The command runs to completion within the deadline (`Exited`, no signal sent); only at
    // the deadline is the tree killed and the outcome `Terminated`.
    let (status, timed_out) = match child.wait_then_kill(options.timeout)? {
        crate::WaitOutcome::Exited(status) => (status, false),
        crate::WaitOutcome::Terminated(status) => (status, true),
    };
    let stdout = stdout
        .as_mut()
        .map_or_else(CapturedOutput::default, |capture| {
            capture.finish_within(DRAIN_WAIT)
        });
    let stderr = stderr
        .as_mut()
        .map_or_else(CapturedOutput::default, |capture| {
            capture.finish_within(DRAIN_WAIT)
        });
    Ok(RunOutput {
        status,
        timed_out,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_hide_the_window_and_cap_both_streams() {
        let options = RunOptions::new(Duration::from_secs(1));
        assert!(options.no_window);
        assert_eq!(options.stdout_cap, 1024 * 1024);
        assert_eq!(options.stderr_cap, 1024 * 1024);
        let options = options.stdout_cap(4).stderr_cap(8).no_window(false);
        assert_eq!((options.stdout_cap, options.stderr_cap), (4, 8));
        assert!(!options.no_window);
    }

    #[cfg(unix)]
    #[test]
    fn a_quick_program_returns_its_output_and_status() {
        let options = RunOptions::new(Duration::from_secs(10));
        let output = run("sh", ["-c", "printf hi; exit 3"], &options).unwrap();
        assert!(!output.timed_out);
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(output.stdout.bytes, b"hi".to_vec());
    }

    #[cfg(unix)]
    #[test]
    fn a_program_running_within_the_deadline_is_not_signalled() {
        let options = RunOptions::new(Duration::from_secs(10));
        let output = run("sh", ["-c", "sleep 1; printf done"], &options).unwrap();
        assert!(!output.timed_out);
        assert!(output.status.success());
        assert_eq!(output.stdout.bytes, b"done".to_vec());
    }

    #[cfg(unix)]
    #[test]
    fn a_slow_program_is_terminated_at_the_deadline() {
        let options = RunOptions::new(Duration::from_millis(200));
        let output = run("sh", ["-c", "sleep 30"], &options).unwrap();
        assert!(output.timed_out);
    }

    #[cfg(unix)]
    #[test]
    fn output_past_the_cap_is_truncated_not_blocking() {
        let options = RunOptions::new(Duration::from_secs(10)).stdout_cap(4);
        let output = run("sh", ["-c", "head -c 100000 /dev/zero"], &options).unwrap();
        assert_eq!(output.stdout.bytes.len(), 4);
        assert!(output.stdout.truncated);
    }
}
