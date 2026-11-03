//! One call that runs a program to completion with a deadline, bounded output capture, and no
//! console window on Windows: the shape of the hand-rolled timeout loops that apps wrote around
//! [`OwnedCommand`](crate::OwnedCommand) and [`NativeOutputCapture`](crate::NativeOutputCapture).
//!
//! The child is owned: it runs in its own process group (or session on Unix) inside a Job
//! Object on Windows, and when the deadline passes its whole tree is terminated, not only the
//! leader. Stdin is null. Output beyond each cap is drained and discarded, so a chatty child
//! never blocks on a full pipe, and [`CapturedOutput::truncated`] says when that happened.
//!
//! [`run_command`] is the same runner for a command the caller configured (environment, working
//! directory), and [`run_streaming`] is the variant that shows output while it runs, with a cancel
//! predicate and tail-only retention.

mod failure;
mod streaming;

pub(crate) use failure::{failure, stop_after_failure, terminate_runner};
pub use failure::{LeaderTermination, RunnerFailure, RunnerPhase};
pub use streaming::{run_streaming, StreamOptions, StreamOutput};

use crate::{CapturedOutput, NativeOutputCapture, OwnedCommand};
use std::{
    ffi::OsStr,
    io,
    process::{ChildStdin, ExitStatus},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// How long to wait for a pipe to end after the child has exited, before returning what was
/// captured (a grandchild that inherited the pipe can keep it open). A capture that fails to
/// reach EOF within this bound makes the runner return an error.
const DRAIN_WAIT: Duration = Duration::from_secs(2);
pub(crate) const RUNNER_REAP_WAIT: Duration = Duration::from_secs(5);

/// How long the stdin writer gets to end by itself once the child is gone, before it is
/// cancelled. A writer facing a closed pipe ends at once; one still blocked is facing a reader
/// that outlived the tree (a grandchild that kept stdin and does not read it).
const STDIN_WAIT: Duration = Duration::from_millis(500);

/// How long a cancelled writer gets to report; the cancel is repeated every [`CANCEL_RETRY`]
/// within it. Past this `run` stops waiting and fails: with `TimedOut`, or with the earlier
/// error on an early return. See [`run_with_stdin`] for what may remain on Windows.
const STDIN_CANCEL_WAIT: Duration = Duration::from_secs(2);

/// Pause between repeated cancels.
const CANCEL_RETRY: Duration = Duration::from_millis(25);

/// Most bytes a Windows writer hands to one blocking write, so cancellation is also checked
/// between chunks.
#[cfg(windows)]
const WRITE_CHUNK: usize = 64 * 1024;

/// How often a Unix writer waiting for pipe space re-checks its cancel flag.
#[cfg(unix)]
const WRITE_POLL: Duration = Duration::from_millis(50);

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

/// Options for raw first-N capture, including commands with no execution deadline.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct CaptureOptions {
    /// Execution deadline; `None` waits for natural leader exit. Cleanup still has finite bounds.
    pub timeout: Option<Duration>,
    /// First raw bytes retained from stdout; remaining bytes are drained through EOF.
    pub stdout_cap: usize,
    /// First raw bytes retained from stderr; remaining bytes are drained through EOF.
    pub stderr_cap: usize,
    /// Hide the Windows console window (default true).
    pub no_window: bool,
    /// Post-exit EOF wait per stream (default 2 s), separate from execution timeout.
    pub drain_grace: Duration,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            timeout: None,
            stdout_cap: 1024 * 1024,
            stderr_cap: 1024 * 1024,
            no_window: true,
            drain_grace: DRAIN_WAIT,
        }
    }
}

impl CaptureOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn timeout(mut self, value: Duration) -> Self {
        self.timeout = Some(value);
        self
    }
    pub fn stdout_cap(mut self, value: usize) -> Self {
        self.stdout_cap = value;
        self
    }
    pub fn stderr_cap(mut self, value: usize) -> Self {
        self.stderr_cap = value;
        self
    }
    pub fn no_window(mut self, value: bool) -> Self {
        self.no_window = value;
        self
    }
    pub fn drain_grace(mut self, value: Duration) -> Self {
        self.drain_grace = value;
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

/// [`run`] with `input` written to the child's stdin. The bytes are written on their own
/// thread while output is drained concurrently, so input larger than a pipe buffer cannot
/// deadlock against a chatty child; the writer closes stdin when done, delivering EOF. A
/// child that exits before consuming the input closes the pipe: that broken-pipe outcome is
/// a normal early exit, not an error, and the child's status/output are returned as usual.
/// Any other stdin write failure is returned as `Err` after the tree is down. A process that
/// outlived the tree and still holds stdin without reading it cannot hold `run` past its
/// deadline: once the tree is down the writer gets a short grace, then it is cancelled (Unix:
/// non-blocking write end; Windows: `CancelIoEx`) and joined.
///
/// What a return guarantees about the writer:
/// - Every `Ok` return: the writer thread has been joined and the write end is closed.
/// - A writer that cannot be stopped within its bound makes the call fail with
///   `ErrorKind::TimedOut`.
/// - Any other `Err` (an early failure, such as output capture or waiting on the child) still
///   cancels the writer with the same bound before returning, but reports that original error,
///   not the cleanup outcome.
/// - Unix, every return: the parent has closed the write end, so a writer thread that is
///   still alive can deliver nothing more and ends on its next step.
/// - Windows, after `TimedOut` or an early `Err`: a writer that did not stop within the bound
///   may still be alive and blocked in a write, owning the write end, with its cancel flag set.
///   This needs a pending `CancelIoEx` that fails to abort the write, which is not expected.
pub fn run_with_stdin<I, S>(
    program: impl AsRef<OsStr>,
    args: I,
    input: &[u8],
    options: &RunOptions,
) -> io::Result<RunOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_inner(program, args, Some(input.to_vec()), options)
}

/// Runs `program` with `args` to completion or until `options.timeout`, then returns its status
/// and bounded output. Fails when the program or native output capture cannot start, or
/// lifecycle waiting fails. A non-zero exit is a normal `Ok` with `status` set, provided capture
/// reaches EOF & cleanup succeeds. Drain expiry is `Err(TimedOut)` carrying
/// [`RunnerPhase::Drain`], including after an execution timeout or successful leader exit.
/// Errors carry [`RunnerFailure`]; use its source for the original OS error code.
pub fn run<I, S>(program: impl AsRef<OsStr>, args: I, options: &RunOptions) -> io::Result<RunOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_inner(program, args, None, options)
}

fn run_inner<I, S>(
    program: impl AsRef<OsStr>,
    args: I,
    input: Option<Vec<u8>>,
    options: &RunOptions,
) -> io::Result<RunOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = OwnedCommand::new(program);
    command.command_mut().args(args);
    run_owned(command, input, options)
}

/// [`run`] / [`run_with_stdin`] for a command the caller configured: its program, arguments,
/// environment (including a cleared or allow-listed one), working directory and any other
/// [`OwnedCommand`] setting are used as given. The crate sets stdin, stdout and stderr (pipes;
/// stdin is null when `input` is `None`) and the window mode from `options.no_window`, replacing
/// whatever the caller set for those, including the stdio of an explicit or automatic
/// `WindowsSpawnConfig` on a Windows `inherit_handles_only` command (its environment, raw
/// arguments and security settings are kept). If a capture's pipe read fails the call returns
/// that error instead of an `Ok` with partial output.
///
/// `input` of `Some(bytes)` is written to the child's stdin on its own thread while output is
/// drained concurrently, so input larger than a pipe buffer cannot deadlock against a chatty
/// child; the writer closes stdin when done, delivering EOF (`Some(&[])` delivers EOF at once).
/// A child that exits before consuming the input closes the pipe: that broken-pipe outcome is
/// a normal early exit, not an error, and the child's status/output are returned as usual.
/// Any other stdin write failure is returned as `Err` after the tree is down. A process that
/// outlived the tree and still holds stdin without reading it cannot hold the call past its
/// deadline: once the tree is down the writer gets a short grace, then it is cancelled (Unix:
/// non-blocking write end; Windows: `CancelIoEx`) and joined.
///
/// What a return guarantees about the writer:
/// - Every `Ok` return: the writer thread has been joined and the write end is closed.
/// - A writer that cannot be stopped within its bound makes the call fail with
///   `ErrorKind::TimedOut`.
/// - Any other `Err` (an early failure, such as output capture or waiting on the child) still
///   cancels the writer with the same bound before returning, but reports that original error,
///   not the cleanup outcome.
/// - Unix, every return: the parent has closed the write end, so a writer thread that is
///   still alive can deliver nothing more and ends on its next step.
/// - Windows, after `TimedOut` or an early `Err`: a writer that did not stop within the bound
///   may still be alive and blocked in a write, owning the write end, with its cancel flag set.
///   This needs a pending `CancelIoEx` that fails to abort the write, which is not expected.
///
/// Everything else (the deadline, the tree kill, the output caps, what `Err` means before the
/// program starts) is as documented on [`run`], which delegates here.
pub fn run_command(
    command: OwnedCommand,
    input: Option<&[u8]>,
    options: &RunOptions,
) -> io::Result<RunOutput> {
    run_owned(command, input.map(<[u8]>::to_vec), options)
}

fn run_owned(
    command: OwnedCommand,
    input: Option<Vec<u8>>,
    options: &RunOptions,
) -> io::Result<RunOutput> {
    run_captured(
        command,
        input,
        &CaptureOptions {
            timeout: Some(options.timeout),
            stdout_cap: options.stdout_cap,
            stderr_cap: options.stderr_cap,
            no_window: options.no_window,
            drain_grace: DRAIN_WAIT,
        },
    )
}

/// Configured command capture with an optional deadline. Retains the first N raw bytes per
/// stream & drains discarded bytes to EOF. `None` timeout waits for natural leader exit;
/// cleanup & post-exit output draining remain bounded. Errors carry [`RunnerFailure`].
pub fn run_command_capture(
    command: OwnedCommand,
    input: Option<&[u8]>,
    options: &CaptureOptions,
) -> io::Result<RunOutput> {
    run_captured(command, input.map(<[u8]>::to_vec), options)
}

fn run_captured(
    mut command: OwnedCommand,
    input: Option<Vec<u8>>,
    options: &CaptureOptions,
) -> io::Result<RunOutput> {
    // Recorded, not applied to `Command` here: `spawn` applies it to the std `Command` and to a
    // native Windows spawn config alike, after every caller edit.
    command.pipe_stdio(input.is_some());
    command.bound_runner_cleanup(RUNNER_REAP_WAIT);
    if options.no_window {
        command.windows_hide();
    } else {
        command.windows_visible();
    }
    let mut child = command
        .spawn()
        .map_err(|error| failure(RunnerPhase::Spawn, LeaderTermination::NotStarted, error))?;
    // Written concurrently with the output drains below; closing stdin when the writer is done
    // is the EOF. Spawn failure returns before any wait is armed. If an early `?` return skips
    // `finish`, `StdinWriter`'s `Drop` cancels the writer with the same bound.
    let writer = match (input, child.take_stdin()) {
        (Some(bytes), Some(stdin)) => Some(
            StdinWriter::spawn(stdin, bytes)
                .map_err(|error| stop_after_failure(&mut child, RunnerPhase::Setup, error))?,
        ),
        _ => None,
    };
    let mut stdout = child
        .take_stdout()
        .map(|pipe| NativeOutputCapture::stdout(pipe, options.stdout_cap))
        .transpose()
        .map_err(|error| stop_after_failure(&mut child, RunnerPhase::Setup, error))?;
    let mut stderr = child
        .take_stderr()
        .map(|pipe| NativeOutputCapture::stderr(pipe, options.stderr_cap))
        .transpose()
        .map_err(|error| stop_after_failure(&mut child, RunnerPhase::Setup, error))?;
    // The command runs to completion within the deadline (`Exited`, no signal sent); only at
    // the deadline is the tree killed and the outcome `Terminated`.
    let (status, timed_out) = match options.timeout {
        Some(timeout) => match child
            .wait_exit_then_sweep(timeout)
            .map_err(|error| stop_after_failure(&mut child, RunnerPhase::Wait, error))?
        {
            Some(status) => (status, false),
            None => (terminate_runner(&mut child, RUNNER_REAP_WAIT)?, true),
        },
        None => loop {
            if let Some(status) = child
                .wait_exit_then_sweep(Duration::from_millis(100))
                .map_err(|error| stop_after_failure(&mut child, RunnerPhase::Wait, error))?
            {
                break (status, false);
            }
        },
    };
    let stdout_output = stdout
        .as_mut()
        .map_or_else(CapturedOutput::default, |capture| {
            capture.finish_within(options.drain_grace)
        });
    let stderr_output = stderr
        .as_mut()
        .map_or_else(CapturedOutput::default, |capture| {
            capture.finish_within(options.drain_grace)
        });
    // The tree is down, but a process that escaped it (a grandchild that left the group or the
    // job) may still hold the read end of stdin without reading, so the writer is not
    // guaranteed to end on its own. Wait a bounded time, then cancel it; never join unbounded.
    // BrokenPipe is the child leaving early, not a failure of this run.
    let writer_result = writer.map(StdinWriter::finish).transpose();
    // Both readers are joined, so a read or shutdown failure they stored is final. Taken for
    // both streams before any return, so neither is left behind.
    let stdout_failure = capture_failure("stdout", stdout.as_ref());
    let stderr_failure = capture_failure("stderr", stderr.as_ref());
    stdout_failure?;
    stderr_failure?;
    writer_result
        .map_err(|error| failure(RunnerPhase::Stdin, LeaderTermination::Confirmed, error))?;
    Ok(RunOutput {
        status,
        timed_out,
        stdout: stdout_output,
        stderr: stderr_output,
    })
}

/// Turns a stored failure of a joined capture into the call's error: its output is incomplete,
/// so returning `Ok` would pass partial bytes off as the child's whole output. The error keeps
/// its [`io::ErrorKind`] and names the stream. Call only after the capture has been joined
/// (`finish_within` / `cancel`).
pub(crate) fn capture_failure(
    stream: &str,
    capture: Option<&NativeOutputCapture>,
) -> io::Result<()> {
    match capture.and_then(NativeOutputCapture::take_error) {
        Some(error) => Err(failure(
            if capture.is_some_and(NativeOutputCapture::cleanup_failed) { RunnerPhase::Cleanup }
            else if stream == "stdout" { RunnerPhase::Stdout } else { RunnerPhase::Stderr },
            LeaderTermination::Confirmed,
            error,
        )),
        None if capture.is_some_and(|capture| !capture.is_complete()) => Err(failure(
            RunnerPhase::Drain, LeaderTermination::Confirmed,
            io::Error::new(io::ErrorKind::TimedOut,
                format!("{stream} capture did not reach EOF within its drain bound, output is incomplete")),
        )),
        None => Ok(()),
    }
}

/// State shared between the thread running [`run_inner`] and the stdin writer thread.
struct WriterState {
    cancel: AtomicBool,
    /// Windows: the parent's write handle while the writer may have a write pending on it. The
    /// writer clears it (under this lock) before it drops the pipe, so a cancel that sees
    /// `Some` always acts on an open handle.
    #[cfg(windows)]
    handle: std::sync::Mutex<Option<usize>>,
    /// Unix: the parent's write end. It lives here, not in the writer thread, so the parent can
    /// close it (`close_pipe`) even if the thread never comes back. Writes happen only while
    /// holding this lock and seeing `Some`, so no write can target a closed or reused
    /// descriptor.
    #[cfg(unix)]
    pipe: std::sync::Mutex<Option<ChildStdin>>,
}

impl WriterState {
    fn new() -> Self {
        Self {
            cancel: AtomicBool::new(false),
            #[cfg(windows)]
            handle: std::sync::Mutex::new(None),
            #[cfg(unix)]
            pipe: std::sync::Mutex::new(None),
        }
    }

    #[cfg(unix)]
    fn pipe(&self) -> std::sync::MutexGuard<'_, Option<ChildStdin>> {
        self.pipe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Closes the write end (the EOF). Idempotent; callable from any thread.
    #[cfg(unix)]
    fn close_pipe(&self) {
        drop(self.pipe().take());
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    /// Asks the writer to stop. Safe to repeat, and it must be: a cancel that lands between two
    /// writes finds nothing pending, so the caller repeats it until the writer reports.
    fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        #[cfg(windows)]
        {
            let held = self
                .handle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(raw) = *held {
                // SAFETY: `raw` is the writer's still-open pipe handle (it is cleared under
                // this lock before the pipe is dropped). CancelIoEx with no OVERLAPPED cancels
                // every pending operation on that handle, which is only the writer's write.
                unsafe { crate::native_capture::platform::cancel_all_io(raw as _) };
            }
        }
    }
}

/// The thread that feeds `input` to the child's stdin. Its shutdown is bounded and verified:
/// when [`finish`](Self::finish) or `Drop` returns `Ok` (or any error but `TimedOut`), the
/// thread has been joined and the write end is closed. On `TimedOut` see
/// [`settle`](Self::settle) for exactly what may remain.
struct StdinWriter {
    state: Arc<WriterState>,
    done: mpsc::Receiver<io::Result<()>>,
    handle: Option<JoinHandle<()>>,
}

impl StdinWriter {
    fn spawn(stdin: ChildStdin, bytes: Vec<u8>) -> io::Result<Self> {
        let state = Arc::new(WriterState::new());
        let (sender, done) = mpsc::channel();
        let shared = Arc::clone(&state);
        #[cfg(unix)]
        {
            *state.pipe() = Some(stdin);
        }
        let handle = std::thread::Builder::new()
            .name("rightkit-run-stdin".into())
            .spawn(move || {
                // Closing the pipe is the EOF, and closes the write end before the result
                // below is reported.
                #[cfg(unix)]
                let result = {
                    let result = write_input(&bytes, &shared);
                    shared.close_pipe();
                    result
                };
                #[cfg(windows)]
                let result = {
                    let mut stdin = stdin;
                    let result = write_input(&mut stdin, &bytes, &shared);
                    drop(stdin);
                    result
                };
                let _ = sender.send(result);
            })?;
        Ok(Self {
            state,
            done,
            handle: Some(handle),
        })
    }

    /// Waits [`STDIN_WAIT`] for the writer to end by itself, then cancels it (repeatedly, for
    /// up to [`STDIN_CANCEL_WAIT`]) and joins it. A writer that cannot be stopped within that
    /// bound is an error: `run` fails with `TimedOut` instead of returning success. On Unix the
    /// write end is closed first, so nothing more is delivered; on Windows the thread may remain
    /// (see `settle`).
    fn finish(mut self) -> io::Result<()> {
        let result = match self.done.recv_timeout(STDIN_WAIT) {
            Err(mpsc::RecvTimeoutError::Timeout) => self.cancel_and_wait(),
            other => other,
        };
        self.settle(result)
    }

    /// Cancels until the writer reports or [`STDIN_CANCEL_WAIT`] passes.
    fn cancel_and_wait(&self) -> Result<io::Result<()>, mpsc::RecvTimeoutError> {
        let deadline = Instant::now() + STDIN_CANCEL_WAIT;
        loop {
            self.state.cancel();
            match self.done.recv_timeout(CANCEL_RETRY) {
                Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                other => return other,
            }
        }
    }

    fn settle(&mut self, result: Result<io::Result<()>, mpsc::RecvTimeoutError>) -> io::Result<()> {
        let handle = self.handle.take();
        match result {
            Ok(result) => {
                if let Some(handle) = handle {
                    let _ = handle.join();
                }
                match result {
                    Err(error) if error.kind() != io::ErrorKind::BrokenPipe => Err(error),
                    _ => Ok(()),
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // The one path where the thread may outlive this call, and the handle is
                // dropped here rather than joined. The cancel flag stays set. Unix: the
                // parent closes the write end now, so the thread can deliver nothing more and
                // ends on its next step. Windows: the thread may still be blocked in a write
                // and still owns the write end; it is not expected to survive repeated
                // `CancelIoEx`, but nothing here can close the handle safely under it.
                #[cfg(unix)]
                self.state.close_pipe();
                Err(failure(
                    RunnerPhase::Cleanup,
                    LeaderTermination::Confirmed,
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the stdin writer did not stop within its cancellation bound",
                    ),
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(handle) = handle {
                    let _ = handle.join();
                }
                Err(failure(
                    RunnerPhase::Cleanup,
                    LeaderTermination::Confirmed,
                    io::Error::other("the stdin writer thread panicked"),
                ))
            }
        }
    }
}

impl Drop for StdinWriter {
    fn drop(&mut self) {
        // Reached without `finish` (an early `?` return in `run_inner`): stop the writer with
        // the same bound. The cleanup result is discarded and the caller gets the original
        // error. A cleanup timeout leaves exactly what `settle` describes: on Unix the write end
        // is closed; on Windows the writer may still be blocked in a write.
        if self.handle.is_some() {
            let result = self.cancel_and_wait();
            let _ = self.settle(result);
        }
    }
}

/// Writes all of `bytes`, or stops early (returning `Ok`) once the state is cancelled. Unix puts
/// the parent's write end in non-blocking mode and polls, so a pipe whose reader never reads
/// cannot pin the thread; the flag is checked at least every [`WRITE_POLL`]. If the mode cannot
/// be switched the write fails closed: a blocking write could not be cancelled.
#[cfg(unix)]
fn write_input(bytes: &[u8], state: &WriterState) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let fd = {
        let guard = state.pipe();
        let Some(pipe) = guard.as_ref() else {
            return Ok(());
        };
        let fd = pipe.as_raw_fd();
        // SAFETY: fcntl on a descriptor the guarded `pipe` owns. O_NONBLOCK is a property of
        // this open file description (the write end only); the child's read end is unaffected.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        fd
    };
    let mut written = 0;
    while written < bytes.len() {
        if state.cancelled() {
            return Ok(());
        }
        let rest = &bytes[written..];
        let chunk = rest.len().min(1 << 20);
        // The write happens only while the lock shows the pipe still open, and its errno is
        // read before the lock is released.
        let outcome = {
            let guard = state.pipe();
            let Some(pipe) = guard.as_ref() else {
                // The parent closed the write end: stop, deliver nothing more.
                return Ok(());
            };
            // SAFETY: `rest` is valid for `chunk` bytes and the pipe's descriptor is open
            // while `guard` holds it.
            let count = unsafe { libc::write(pipe.as_raw_fd(), rest.as_ptr().cast(), chunk) };
            if count >= 0 {
                Ok(count as usize)
            } else {
                Err(io::Error::last_os_error())
            }
        };
        match outcome {
            Ok(count) => written += count,
            Err(error) => match error.kind() {
                io::ErrorKind::Interrupted => {}
                io::ErrorKind::WouldBlock => {
                    let mut entry = libc::pollfd {
                        fd,
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    // SAFETY: one valid pollfd, as counted; it only observes readiness, so a
                    // closed or reused number is harmless. A negative result (EINTR) loops.
                    unsafe { libc::poll(&mut entry, 1, WRITE_POLL.as_millis() as libc::c_int) };
                }
                _ => return Err(error),
            },
        }
    }
    Ok(())
}

/// Windows: writes in chunks, checking the state between chunks, and publishes the pipe handle
/// so [`WriterState::cancel`] can abort a write that is blocked on a reader that never reads
/// (`CancelIoEx`). An error caused by that cancel is a normal stop, not a failure.
#[cfg(windows)]
fn write_input(stdin: &mut ChildStdin, bytes: &[u8], state: &WriterState) -> io::Result<()> {
    use std::{io::Write, os::windows::io::AsRawHandle};
    let publish = |value: Option<usize>| {
        *state
            .handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = value;
    };
    publish(Some(stdin.as_raw_handle() as usize));
    let mut result = Ok(());
    for chunk in bytes.chunks(WRITE_CHUNK) {
        if state.cancelled() {
            break;
        }
        if let Err(error) = stdin.write_all(chunk) {
            result = Err(error);
            break;
        }
    }
    // Taking the lock waits out any cancel in progress; after this the handle may be closed.
    publish(None);
    match result {
        Err(_) if state.cancelled() => Ok(()),
        other => other,
    }
}
