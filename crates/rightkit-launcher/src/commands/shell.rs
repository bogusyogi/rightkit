//! Running one command line through `cmd.exe`.

use super::Outcome;
use rightkit_child_process::{OwnedChild, OwnedCommand, TerminationOptions, read_bounded};
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

/// The most bytes kept from each of stdout and stderr.
const OUTPUT_CAP: usize = 64 * 1024;
/// The most lines shown.
const LINE_CAP: usize = 200;
/// The time limit for a command, after which it is ended.
const TIME_LIMIT: Duration = Duration::from_secs(30);
/// How long a command that was ended gets to be reaped.
const REAP_BOUND: Duration = Duration::from_secs(5);
/// How long the pipes may take to end once the child has exited.
const DRAIN_WAIT: Duration = Duration::from_secs(2);

/// Runs `line` with `cmd.exe /D /S /C` and waits up to `TIME_LIMIT`. Stdout and stderr are
/// read separately and shown one after the other, not interleaved.
pub fn run(line: &str) -> Outcome {
    let mut command = OwnedCommand::new("cmd.exe");
    // Wrapped in quotes, so that `cmd /S` removes only those outer quotes.
    command
        .command_mut()
        .args(["/D", "/S", "/C"])
        .raw_arg(format!("\"{line}\""))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child: OwnedChild = match command.spawn_uncontained_detached() {
        Ok(child) => child,
        Err(error) => return Outcome::Failed(format!("Could not start the shell: {error}")),
    };
    let stdout = child.take_stdout().map(read_capped);
    let stderr = child.take_stderr().map(read_capped);
    let status = match child.wait_timeout(TIME_LIMIT) {
        Ok(Some(status)) => Some(status),
        Ok(None) => {
            // Detached: only on this timeout path is the shell's tree ended.
            let _ = child.terminate_tree_deep(TerminationOptions::default());
            None
        }
        Err(error) => {
            let _ = child.terminate_tree_bounded(REAP_BOUND);
            return Outcome::Failed(format!("The command could not be followed: {error}"));
        }
    };
    let Some(status) = status else {
        return Outcome::TimedOut;
    };
    let out = collect(stdout);
    let err = collect(stderr);
    let text: String = [out, err]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
        .lines()
        .take(LINE_CAP)
        .collect::<Vec<_>>()
        .join("\n");
    if status.success() {
        return Outcome::Output(if text.is_empty() {
            "Done. No output.".to_string()
        } else {
            text
        });
    }
    let note = format!("Exited with status {}.", status.code().unwrap_or(-1));
    Outcome::Failed(if text.is_empty() {
        note
    } else {
        format!("{text}\n\n{note}")
    })
}

/// Reads a pipe on its own thread. The first `OUTPUT_CAP` bytes are kept and the rest is
/// drained, so the child never blocks on a full pipe.
fn read_capped<R: Read + Send + 'static>(pipe: R) -> Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    // If the thread cannot start, the sender is dropped and `collect` gets nothing.
    let _ = thread::Builder::new()
        .name("launcher-command-pipe".into())
        .spawn(move || {
            let kept = read_bounded(pipe, OUTPUT_CAP)
                .map(|captured| captured.bytes)
                .unwrap_or_default();
            let _ = sender.send(kept);
        });
    receiver
}

/// The text read from one pipe, trailing whitespace removed. Empty when the pipe is missing
/// or does not end within `DRAIN_WAIT` (a grandchild may still hold it open).
fn collect(pipe: Option<Receiver<Vec<u8>>>) -> String {
    let bytes = pipe
        .and_then(|receiver| receiver.recv_timeout(DRAIN_WAIT).ok())
        .unwrap_or_default();
    String::from_utf8_lossy(&bytes).trim_end().to_string()
}

/// Runs `line` on a thread of its own and calls `done` with the outcome. If the thread
/// cannot be started, `done` is dropped without being called.
pub fn spawn(line: String, done: impl FnOnce(Outcome) + Send + 'static) {
    let _ = thread::Builder::new()
        .name("launcher-command".into())
        .spawn(move || done(run(&line)));
}
