//! Run a launch or an installer for a bounded time and collect what it printed, through RightKit's
//! `OwnedCommand::spawn_uncontained_detached` (no published runner starts a detached child): it
//! may start things meant to outlive the call, so only the program itself is ever stopped, and
//! only at the limit. A detached launch on Windows must be allowed to break away from any job
//! the host app runs in. Programs the host app runs to read an answer are owned and go through
//! `rightkit_child_process::run_command_capture` instead.

use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use rightkit_child_process::{OutputCapture, OwnedChild, OwnedCommand, ReapOutcome};

/// How long a child that passed its limit gets to be confirmed gone.
const REAP_BOUND: Duration = Duration::from_secs(5);

/// Keep every byte.
pub const UNBOUNDED: usize = usize::MAX;

/// What to do with one output stream.
#[derive(Clone, Copy, Debug)]
pub enum Capture {
    /// Connect it to the null device.
    Discard,
    /// Read it to the end on its own thread, keeping at most this many bytes. The rest is
    /// read and dropped, so a chatty child never blocks on a full pipe.
    Keep(usize),
}

/// One bounded run.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// The most the program may run.
    pub timeout: Duration,
    pub stdout: Capture,
    pub stderr: Capture,
    /// After the program exits, the longest to wait for an output stream to reach its end
    /// (a stray descendant can keep the pipe open). What was read by then is returned.
    pub drain_grace: Duration,
}

/// A program that exited within its limit.
#[derive(Debug)]
pub struct Done {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Why a run produced no exit status.
#[derive(Debug)]
pub enum Failure {
    /// The program could not be started.
    Spawn(io::Error),
    /// Its state could not be read.
    Wait(io::Error),
    /// It was still running at its limit and was stopped.
    TimedOut,
}

fn capture_of<R: Read + Send + 'static>(stream: Option<R>, how: Capture) -> Option<OutputCapture> {
    match (stream, how) {
        (Some(stream), Capture::Keep(cap)) => Some(OutputCapture::capture(stream, cap)),
        _ => None,
    }
}

fn stdio(how: Capture) -> Stdio {
    match how {
        Capture::Discard => Stdio::null(),
        Capture::Keep(_) => Stdio::piped(),
    }
}

/// Stop `child` and wait up to the reap bound for its exit. True when the exit was seen.
/// A child that was not seen to exit is leaked rather than dropped: dropping an owned
/// child waits for it without a bound.
fn stop(mut child: OwnedChild) -> bool {
    match child.terminate_tree_bounded(REAP_BOUND) {
        Ok(ReapOutcome::Exited(_)) => true,
        Ok(ReapOutcome::TimedOut) | Err(_) => {
            std::mem::forget(child);
            false
        }
    }
}

/// Run `command` detached to its end or its limit. Standard input, output and error are set
/// here; the rest of `command` (arguments, environment, working directory) is the caller's.
pub fn run_detached(mut command: Command, options: &Options) -> Result<Done, Failure> {
    command
        .stdin(Stdio::null())
        .stdout(stdio(options.stdout))
        .stderr(stdio(options.stderr));
    let mut child = OwnedCommand::from_command(command)
        .spawn_uncontained_detached()
        .map_err(Failure::Spawn)?;
    let mut out = capture_of(child.take_stdout(), options.stdout);
    let mut err = capture_of(child.take_stderr(), options.stderr);
    match child.wait_timeout(options.timeout) {
        Ok(Some(status)) => {
            let grace = options.drain_grace;
            Ok(Done {
                status,
                stdout: out
                    .as_mut()
                    .map(|c| c.finish_within(grace).bytes)
                    .unwrap_or_default(),
                stderr: err
                    .as_mut()
                    .map(|c| c.finish_within(grace).bytes)
                    .unwrap_or_default(),
            })
        }
        Ok(None) => {
            stop(child);
            Err(Failure::TimedOut)
        }
        Err(error) => {
            stop(child);
            Err(Failure::Wait(error))
        }
    }
}
