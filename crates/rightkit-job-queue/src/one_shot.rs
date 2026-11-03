//! One-shot command client: run `<engine> <words> [--input <json> | --input - with stdin]` once
//! and return its exit code, stdout and stderr. The client-side counterpart of the
//! `<words> --input <json>` shape that `run_cli` serves, for an engine binary that is run once
//! per call instead of supervised. Typed replies, capability policy and error mapping stay with
//! the caller.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use rightkit_child_process::{OutputCapture, OwnedChild, OwnedCommand};

use crate::{JobError, JobResult};

/// Default for [`OneShot::timeout`].
pub const DEFAULT_ONE_SHOT_TIMEOUT: Duration = Duration::from_secs(60);
/// Default for [`OneShot::max_arg_bytes`]: a payload up to this size is passed as the argument of
/// `--input`, a larger one on stdin through `--input -`.
pub const DEFAULT_MAX_ARG_BYTES: usize = 8 * 1024;
/// Default for [`OneShot::capture_limit`], applied to stdout and to stderr each.
pub const DEFAULT_CAPTURE_LIMIT: usize = 64 * 1024 * 1024;
/// Bounded time to bring a timed-out or failed child down before the call reports its error.
pub const ONE_SHOT_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

/// What one run of an engine command produced. A non-zero `code` is not an error here: the caller
/// maps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawOutput {
    /// The exit code, or `None` when the process was ended by a signal.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl RawOutput {
    /// `stdout` as text, with invalid UTF-8 replaced.
    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
    /// `stderr` as text, with invalid UTF-8 replaced.
    pub fn stderr_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// A one-shot command runner for one engine binary.
///
/// `run` spawns `<engine> <words...>`; a payload is appended as `--input <json>` when it fits in
/// [`max_arg_bytes`](Self::max_arg_bytes), and otherwise as `--input -` with the payload written
/// to the child's stdin on a thread of its own (so the timeout still applies to a child that
/// never reads it). Without a payload stdin is null, so a call can never block on an inherited
/// pipe. Stdout and stderr are drained while the child runs, each capped at
/// [`capture_limit`](Self::capture_limit). The whole call, including the drain, is bounded by
/// [`timeout`](Self::timeout); on expiry the child's process tree is killed.
///
/// The call fails (never returns partial output) when the timeout passes, when a pipe does not
/// reach end of file, when a stream could not be read, or when either stream was truncated at the
/// cap.
#[derive(Debug, Clone)]
pub struct OneShot {
    engine: PathBuf,
    timeout: Duration,
    max_arg_bytes: usize,
    capture_limit: usize,
    env: Vec<(OsString, OsString)>,
    env_remove: Vec<OsString>,
}

impl OneShot {
    /// `engine` must be an absolute path to the binary; `PATH` lookup is never used.
    pub fn new(engine: impl Into<PathBuf>) -> Self {
        Self {
            engine: engine.into(),
            timeout: DEFAULT_ONE_SHOT_TIMEOUT,
            max_arg_bytes: DEFAULT_MAX_ARG_BYTES,
            capture_limit: DEFAULT_CAPTURE_LIMIT,
            env: Vec::new(),
            env_remove: Vec::new(),
        }
    }
    /// Deadline for the whole call (spawn to the end of the drain).
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }
    /// Largest payload passed on the command line; larger payloads go on stdin.
    pub fn max_arg_bytes(mut self, n: usize) -> Self {
        self.max_arg_bytes = n;
        self
    }
    /// Bytes kept from each of stdout and stderr; more is an error.
    pub fn capture_limit(mut self, n: usize) -> Self {
        self.capture_limit = n;
        self
    }
    /// Set an environment variable for the child.
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
    /// Remove an inherited environment variable from the child.
    pub fn env_remove(mut self, key: impl Into<OsString>) -> Self {
        self.env_remove.push(key.into());
        self
    }

    /// Runs `<engine> <words> [--input <json> | --input - with stdin]`. Errors (all
    /// `ErrorOrigin::Supervisor`): `bad_input` (relative engine path), `spawn_failed`,
    /// `one_shot_timeout` (also a zero timeout), `one_shot_read_failed`,
    /// `one_shot_output_limit`, `wait_failed`.
    pub fn run(&self, words: &[&str], input: Option<&str>) -> JobResult<RawOutput> {
        if !self.engine.is_absolute() {
            return Err(JobError::supervisor(
                "bad_input",
                "a one-shot command needs an absolute engine path",
            ));
        }
        let started = Instant::now();
        let timeout = || {
            JobError::supervisor(
                "one_shot_timeout",
                "the command did not finish within its deadline",
            )
        };
        if self.timeout.is_zero() {
            return Err(timeout());
        }
        let via_stdin = input.is_some_and(|json| json.len() > self.max_arg_bytes);
        let mut command = OwnedCommand::new(&self.engine);
        command.windows_hide();
        {
            let c = command.command_mut();
            c.args(words);
            match input {
                Some(_) if via_stdin => {
                    c.args(["--input", "-"]);
                }
                Some(json) => {
                    c.args(["--input", json]);
                }
                None => {}
            }
            for (key, value) in &self.env {
                c.env(key, value);
            }
            for key in &self.env_remove {
                c.env_remove(key);
            }
            c.stdin(if via_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        }
        let mut child = command.spawn().map_err(|e| {
            JobError::supervisor(
                "spawn_failed",
                format!("cannot run {}: {e}", self.engine.display()),
            )
        })?;
        child.set_drop_reap_timeout(ONE_SHOT_CLEANUP_TIMEOUT);
        let bring_down = |child: &mut OwnedChild| {
            let _ = child.terminate_tree_bounded(ONE_SHOT_CLEANUP_TIMEOUT);
        };
        let writer = if via_stdin {
            let payload = input.unwrap_or_default().as_bytes().to_vec();
            let stdin = child.take_stdin();
            let spawned = thread::Builder::new()
                .name("one-shot-stdin".into())
                .spawn(move || {
                    if let Some(mut stdin) = stdin {
                        // A child that exits without reading it all breaks the pipe: its exit
                        // code is the answer, so the write error is not one.
                        let _ = stdin.write_all(&payload);
                        let _ = stdin.flush();
                    }
                });
            match spawned {
                Ok(handle) => Some(handle),
                Err(e) => {
                    bring_down(&mut child);
                    return Err(JobError::supervisor(
                        "spawn_failed",
                        format!("cannot start the stdin writer: {e}"),
                    ));
                }
            }
        } else {
            None
        };
        let (Some(stdout), Some(stderr)) = (child.take_stdout(), child.take_stderr()) else {
            bring_down(&mut child);
            return Err(JobError::supervisor(
                "spawn_failed",
                "the command's output was not piped",
            ));
        };
        let limit = self.capture_limit;
        let (capture_out, capture_err) = match (
            OutputCapture::try_capture(stdout, limit),
            OutputCapture::try_capture(stderr, limit),
        ) {
            (Ok(out), Ok(err)) => (out, err),
            (Err(e), _) | (_, Err(e)) => {
                bring_down(&mut child);
                return Err(JobError::supervisor(
                    "spawn_failed",
                    format!("cannot capture the command's output: {e}"),
                ));
            }
        };
        // Wait for both streams to end before reaping the leader: if a descendant holds a pipe
        // open the deadline passes and bringing the tree down releases it.
        while !(capture_out.is_finished() && capture_err.is_finished()) {
            let remaining = self.timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                bring_down(&mut child);
                return Err(timeout());
            }
            thread::sleep(remaining.min(Duration::from_millis(5)));
        }
        if let Some(e) = capture_out
            .take_error()
            .or_else(|| capture_err.take_error())
        {
            bring_down(&mut child);
            return Err(JobError::supervisor(
                "one_shot_read_failed",
                format!("cannot read the command's output: {e}"),
            ));
        }
        let status = match child.wait_timeout(self.timeout.saturating_sub(started.elapsed())) {
            Ok(Some(status)) => status,
            Ok(None) => {
                bring_down(&mut child);
                return Err(timeout());
            }
            Err(e) => {
                bring_down(&mut child);
                return Err(JobError::supervisor("wait_failed", e.to_string()));
            }
        };
        // The leader is gone, so a writer still blocked on a full pipe ends on its own; never
        // wait for it.
        if let Some(handle) = writer {
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
        let out = capture_out.snapshot();
        let err = capture_err.snapshot();
        if out.truncated || err.truncated {
            return Err(JobError::supervisor(
                "one_shot_output_limit",
                format!("the command's output exceeded the {limit}-byte capture limit"),
            ));
        }
        Ok(RawOutput {
            code: status.code(),
            stdout: out.bytes,
            stderr: err.bytes,
        })
    }
}
