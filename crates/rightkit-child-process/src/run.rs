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

mod streaming;

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
/// captured (a grandchild that inherited the pipe can keep it open).
const DRAIN_WAIT: Duration = Duration::from_secs(2);

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
/// lifecycle waiting fails. A non-zero exit is a normal `Ok` with `status` set.
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
    mut command: OwnedCommand,
    input: Option<Vec<u8>>,
    options: &RunOptions,
) -> io::Result<RunOutput> {
    // Recorded, not applied to `Command` here: `spawn` applies it to the std `Command` and to a
    // native Windows spawn config alike, after every caller edit.
    command.pipe_stdio(input.is_some());
    if options.no_window {
        command.windows_hide();
    } else {
        command.windows_visible();
    }
    let mut child = command.spawn()?;
    // Written concurrently with the output drains below; closing stdin when the writer is done
    // is the EOF. Spawn failure returns before any wait is armed. If an early `?` return skips
    // `finish`, `StdinWriter`'s `Drop` cancels the writer with the same bound.
    let writer = match (input, child.take_stdin()) {
        (Some(bytes), Some(stdin)) => Some(StdinWriter::spawn(stdin, bytes)?),
        _ => None,
    };
    let mut stdout = child
        .take_stdout()
        .map(|pipe| NativeOutputCapture::stdout(pipe, options.stdout_cap))
        .transpose()?;
    let mut stderr = child
        .take_stderr()
        .map(|pipe| NativeOutputCapture::stderr(pipe, options.stderr_cap))
        .transpose()?;
    // The command runs to completion within the deadline (`Exited`, no signal sent); only at
    // the deadline is the tree killed and the outcome `Terminated`.
    let (status, timed_out) = match child.wait_then_kill(options.timeout)? {
        crate::WaitOutcome::Exited(status) => (status, false),
        crate::WaitOutcome::Terminated(status) => (status, true),
    };
    let stdout_output = stdout
        .as_mut()
        .map_or_else(CapturedOutput::default, |capture| {
            capture.finish_within(DRAIN_WAIT)
        });
    let stderr_output = stderr
        .as_mut()
        .map_or_else(CapturedOutput::default, |capture| {
            capture.finish_within(DRAIN_WAIT)
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
    writer_result?;
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
        Some(error) => Err(io::Error::new(
            error.kind(),
            format!("{stream} capture failed, output is incomplete: {error}"),
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

/// Counts writer threads between spawn and the end of their closure, so tests can prove that
/// none is left once `run` returns.
#[cfg(test)]
static LIVE_WRITERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
struct LiveWriter;

#[cfg(test)]
impl LiveWriter {
    fn start() -> Self {
        LIVE_WRITERS.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

#[cfg(test)]
impl Drop for LiveWriter {
    fn drop(&mut self) {
        LIVE_WRITERS.fetch_sub(1, Ordering::SeqCst);
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
        #[cfg(test)]
        let live = LiveWriter::start();
        let handle = std::thread::Builder::new()
            .name("rightkit-run-stdin".into())
            .spawn(move || {
                #[cfg(test)]
                let _live = live;
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
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the stdin writer did not stop within its cancellation bound",
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(handle) = handle {
                    let _ = handle.join();
                }
                Err(io::Error::other("the stdin writer thread panicked"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::sync::atomic::Ordering::SeqCst;

    /// The writer-thread counter is process-wide, so tests that start a writer run one at a
    /// time and can assert it returns to zero.
    static WRITER_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        WRITER_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Owns a child for the test and kills it on drop.
    struct KillOnDrop(std::process::Child);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

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
    fn stdin_input_reaches_the_child_and_early_exit_is_not_an_error() {
        let _serial = serial();
        let options = RunOptions::new(Duration::from_secs(10));
        let output = run_with_stdin("sh", ["-c", "cat"], b"hello", &options).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.bytes, b"hello".to_vec());
        // The child exits at once without reading: input far larger than a pipe buffer
        // breaks the writer's pipe, which is a normal early exit with the child's status.
        let big = vec![b'x'; 1 << 20];
        let output = run_with_stdin("sh", ["-c", "exit 7"], &big, &options).unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(LIVE_WRITERS.load(SeqCst), 0);
    }

    /// A reader that stays alive and never reads (the shape of a grandchild that kept stdin
    /// past the tree kill) cannot pin the writer: `finish` cancels it, joins it and has closed
    /// the write end, within its bound. Runs on Unix and Windows.
    #[test]
    fn a_reader_that_never_reads_cannot_pin_the_writer() {
        let _serial = serial();
        #[cfg(unix)]
        let mut command = {
            let mut command = std::process::Command::new("sleep");
            command.arg("30");
            command
        };
        #[cfg(windows)]
        let mut command = {
            let mut command = std::process::Command::new("ping");
            command.args(["-n", "30", "127.0.0.1"]);
            command
        };
        let mut child = KillOnDrop(
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let stdin = child.0.stdin.take().unwrap();
        let writer = StdinWriter::spawn(stdin, vec![b'x'; 2 << 20]).unwrap();
        // Let it fill the pipe and block.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(LIVE_WRITERS.load(SeqCst), 1, "the writer should be blocked");
        let started = std::time::Instant::now();
        writer.finish().unwrap();
        let elapsed = started.elapsed();
        assert_eq!(
            LIVE_WRITERS.load(SeqCst),
            0,
            "the writer thread must be gone"
        );
        assert!(
            elapsed < STDIN_WAIT + STDIN_CANCEL_WAIT + Duration::from_secs(3),
            "finish took {elapsed:?}"
        );
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "the reader must have stayed alive throughout"
        );
    }

    /// The cancellation-timeout branch, forced deterministically: a writer thread that never
    /// reports. `finish` fails with `TimedOut` within its bound and leaves the cancel flag set,
    /// and the parent has closed the write end itself, so the reader sees EOF although the
    /// writer thread is still alive.
    #[cfg(unix)]
    #[test]
    fn a_writer_that_never_reports_times_out_and_the_parent_closes_the_write_end() {
        let _serial = serial();
        let mut child = KillOnDrop(
            std::process::Command::new("cat")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let state = Arc::new(WriterState::new());
        *state.pipe() = Some(child.0.stdin.take().unwrap());
        // The sender stays here, so the writer neither reports nor disconnects.
        let (_sender, done) = mpsc::channel::<io::Result<()>>();
        let (release, parked) = mpsc::channel::<()>();
        let stuck = std::thread::spawn(move || {
            let _ = parked.recv();
        });
        let writer = StdinWriter {
            state: Arc::clone(&state),
            done,
            handle: Some(stuck),
        };
        let started = Instant::now();
        let error = writer.finish().unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            elapsed >= STDIN_WAIT + STDIN_CANCEL_WAIT
                && elapsed < STDIN_WAIT + STDIN_CANCEL_WAIT + Duration::from_secs(3),
            "finish took {elapsed:?}"
        );
        assert!(state.cancelled(), "the cancel flag must stay set");
        assert!(
            state.pipe().is_none(),
            "the parent must have closed the write end"
        );
        // The write end is closed, so `cat` reads EOF and exits by itself.
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "the reader never saw EOF");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(status.success());
        let _ = release.send(());
    }

    /// Records the pid of the setsid grandchild in a file and kills it on drop, so a failed
    /// assertion cannot leak the 30-second sleeper this fixture starts.
    #[cfg(unix)]
    struct Grandchild(std::path::PathBuf);

    #[cfg(unix)]
    impl Grandchild {
        fn new(label: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos());
            Self(std::env::temp_dir().join(format!(
                "rk-run-stdin-{label}-{}-{nanos}.pid",
                std::process::id()
            )))
        }

        fn pid(&self) -> Option<i32> {
            let pid: i32 = std::fs::read_to_string(&self.0).ok()?.trim().parse().ok()?;
            (pid > 1).then_some(pid)
        }

        /// Where the `reads` grandchild records how many bytes it read before EOF.
        fn count_path(&self) -> std::path::PathBuf {
            let mut path = self.0.clone().into_os_string();
            path.push(".count");
            path.into()
        }

        fn alive(&self) -> bool {
            // SAFETY: signal 0 only checks existence.
            self.pid()
                .is_some_and(|pid| unsafe { libc::kill(pid, 0) } == 0)
        }
    }

    #[cfg(unix)]
    impl Drop for Grandchild {
        fn drop(&mut self) {
            if let Some(pid) = self.pid() {
                // SAFETY: the recorded pid is the fixture's own `sleep`, killed once here.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(self.count_path());
        }
    }

    /// Shell fixture: starts a grandchild in a new session (so neither the group kill nor the
    /// tree kill reaches it) that keeps this shell's stdin open and never reads it, waits for
    /// it to record its pid, then runs `tail`. The grandchild runs `body` (`exec sleep 30` to
    /// hold stdin unread, or a delayed read-to-EOF that proves the write end was closed). `$1` is the pid file. Exits 99 when the host has
    /// neither `setsid` nor `perl` to leave the session.
    #[cfg(unix)]
    fn holder_script(body: &str, tail: &str) -> String {
        format!(
            "if command -v setsid >/dev/null 2>&1; then \
               setsid sh -c 'echo $$ > \"$1\"; {body}' sh \"$1\" <&0 >/dev/null 2>&1 & \
             elif command -v perl >/dev/null 2>&1; then \
               perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV' sh -c 'echo $$ > \"$1\"; {body}' sh \"$1\" <&0 >/dev/null 2>&1 & \
             else exit 99; fi; \
             i=0; while [ ! -s \"$1\" ] && [ $i -lt 100 ]; do sleep 0.05; i=$((i+1)); done; {tail}"
        )
    }

    #[cfg(unix)]
    #[test]
    fn a_session_escaped_stdin_holder_does_not_hold_run_past_its_exit() {
        let _serial = serial();
        let holder = Grandchild::new("exit");
        // After `run` has returned, the grandchild reads stdin to EOF and records how much it
        // got. A writer left running (or a write end left open) would deliver all 2 MiB, or
        // leave it blocked; a closed write end leaves only what was already in the pipe.
        let script = holder_script("sleep 2; wc -c > \"$1.count\"; exec sleep 30", "exit 0");
        let big = vec![b'x'; 2 << 20];
        let options = RunOptions::new(Duration::from_secs(10));
        let started = std::time::Instant::now();
        let output = run_with_stdin(
            "sh",
            ["-c", &script, "sh", holder.0.to_str().unwrap()],
            &big,
            &options,
        )
        .unwrap();
        let elapsed = started.elapsed();
        if output.status.code() == Some(99) {
            eprintln!("skipped: no setsid or perl to leave the session");
            return;
        }
        assert!(
            holder.alive(),
            "the fixture's grandchild must still hold stdin"
        );
        assert!(!output.timed_out);
        assert!(output.status.success(), "{:?}", output.status);
        // Exit is immediate; the writer gets STDIN_WAIT + STDIN_CANCEL_WAIT at most.
        assert!(elapsed < Duration::from_secs(6), "run took {elapsed:?}");
        assert_eq!(
            LIVE_WRITERS.load(SeqCst),
            0,
            "the writer thread must be gone"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let received = loop {
            if let Some(count) = std::fs::read_to_string(holder.count_path())
                .ok()
                .and_then(|text| text.trim().parse::<usize>().ok())
            {
                break count;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the grandchild never reached EOF: the write end is still open"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(
            received < big.len() / 2,
            "{received} bytes reached the grandchild after run returned"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_session_escaped_stdin_holder_does_not_hold_run_past_its_timeout() {
        let _serial = serial();
        let holder = Grandchild::new("timeout");
        // The leader blocks past the deadline, so the tree kill (not an exit) ends it.
        let script = holder_script("exec sleep 30", "sleep 30");
        let big = vec![b'x'; 2 << 20];
        let timeout = Duration::from_secs(1);
        let options = RunOptions::new(timeout);
        let started = std::time::Instant::now();
        let output = run_with_stdin(
            "sh",
            ["-c", &script, "sh", holder.0.to_str().unwrap()],
            &big,
            &options,
        )
        .unwrap();
        let elapsed = started.elapsed();
        if !holder.alive() && !output.timed_out {
            eprintln!("skipped: no setsid or perl to leave the session");
            return;
        }
        assert!(
            holder.alive(),
            "the fixture's grandchild must still hold stdin"
        );
        assert!(output.timed_out);
        assert!(
            elapsed < timeout + Duration::from_secs(6),
            "run took {elapsed:?}"
        );
        assert_eq!(
            LIVE_WRITERS.load(SeqCst),
            0,
            "the writer thread must be gone"
        );
    }

    #[cfg(unix)]
    #[test]
    fn output_past_the_cap_is_truncated_not_blocking() {
        let options = RunOptions::new(Duration::from_secs(10)).stdout_cap(4);
        let output = run("sh", ["-c", "head -c 100000 /dev/zero"], &options).unwrap();
        assert_eq!(output.stdout.bytes.len(), 4);
        assert!(output.stdout.truncated);
    }

    /// `sh -c script sh args...` (the first argument is `$1`).
    #[cfg(unix)]
    fn sh(script: &str, args: &[&str]) -> OwnedCommand {
        let mut command = OwnedCommand::new("sh");
        command.command_mut().args(["-c", script, "sh"]).args(args);
        command
    }

    /// A program that outlives any test deadline.
    #[cfg(unix)]
    fn sleeper() -> OwnedCommand {
        let mut command = OwnedCommand::new("sleep");
        command.command_mut().arg("30");
        command
    }

    #[cfg(windows)]
    fn sleeper() -> OwnedCommand {
        let mut command = OwnedCommand::new("ping");
        command.command_mut().args(["-n", "30", "127.0.0.1"]);
        command
    }

    /// Runs `command` through `run_streaming`, collecting what the callbacks saw. With
    /// `cancel_on_first_chunk` the predicate turns true once the first stdout chunk arrived.
    fn stream(
        command: OwnedCommand,
        options: &StreamOptions,
        cancel_on_first_chunk: bool,
    ) -> (StreamOutput, Vec<u8>, Vec<String>) {
        let flag = std::cell::Cell::new(false);
        let mut chunks = Vec::new();
        let mut lines = Vec::new();
        let output = run_streaming(
            command,
            options,
            &|| flag.get(),
            &mut |chunk: &[u8]| {
                chunks.extend_from_slice(chunk);
                if cancel_on_first_chunk {
                    flag.set(true);
                }
            },
            &mut |line: &str| lines.push(line.to_owned()),
        )
        .unwrap();
        (output, chunks, lines)
    }

    #[test]
    fn run_command_passes_a_caller_set_env_var_to_the_child() {
        #[cfg(unix)]
        let mut command = sh("printf %s \"$RK_RUN_COMMAND_VAR\"", &[]);
        #[cfg(windows)]
        let mut command = {
            let mut command = OwnedCommand::new("cmd");
            command
                .command_mut()
                .args(["/C", "echo %RK_RUN_COMMAND_VAR%"]);
            command
        };
        command
            .command_mut()
            .env("RK_RUN_COMMAND_VAR", "from-the-caller");
        let output = run_command(command, None, &RunOptions::new(Duration::from_secs(20))).unwrap();
        assert!(output.status.success());
        assert!(!output.timed_out);
        assert!(
            String::from_utf8_lossy(&output.stdout.bytes).contains("from-the-caller"),
            "{:?}",
            output.stdout
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_command_with_input_reaches_the_child() {
        let _serial = serial();
        let options = RunOptions::new(Duration::from_secs(10));
        let output = run_command(sh("cat", &[]), Some(b"hello"), &options).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.bytes, b"hello".to_vec());
        assert_eq!(LIVE_WRITERS.load(SeqCst), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_session_escaped_stdin_holder_does_not_hold_run_command_past_its_timeout() {
        let _serial = serial();
        let holder = Grandchild::new("command-timeout");
        let script = holder_script("exec sleep 30", "sleep 30");
        let big = vec![b'x'; 2 << 20];
        let timeout = Duration::from_secs(1);
        let options = RunOptions::new(timeout);
        let started = std::time::Instant::now();
        let output = run_command(
            sh(script.as_str(), &[holder.0.to_str().unwrap()]),
            Some(&big),
            &options,
        )
        .unwrap();
        let elapsed = started.elapsed();
        if !holder.alive() && !output.timed_out {
            eprintln!("skipped: no setsid or perl to leave the session");
            return;
        }
        assert!(
            holder.alive(),
            "the fixture's grandchild must still hold stdin"
        );
        assert!(output.timed_out);
        assert!(
            elapsed < timeout + Duration::from_secs(6),
            "run_command took {elapsed:?}"
        );
        assert_eq!(
            LIVE_WRITERS.load(SeqCst),
            0,
            "the writer thread must be gone"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_streaming_forwards_stdout_chunks_in_order_including_cr() {
        let command = sh(
            "printf 'a\\r'; sleep 0.1; printf 'b\\r'; sleep 0.1; printf 'c\\n'",
            &[],
        );
        let (output, chunks, lines) = stream(command, &StreamOptions::new(), false);
        assert!(output.status.success());
        assert!(!output.cancelled && !output.timed_out);
        assert_eq!(chunks, b"a\rb\rc\n".to_vec());
        assert_eq!(output.stdout_tail, b"a\rb\rc\n".to_vec());
        assert!(lines.is_empty());
        assert_eq!(output.stderr_tail, "");
    }

    #[cfg(unix)]
    #[test]
    fn run_streaming_splits_stderr_lines_on_cr_and_lf() {
        let command = sh("printf 'x\\ry\\nz\\r\\nw' >&2; exit 3", &[]);
        let (output, chunks, lines) = stream(command, &StreamOptions::new(), false);
        assert_eq!(output.status.code(), Some(3));
        assert!(chunks.is_empty());
        assert_eq!(lines, ["x", "y", "z", "w"]);
        assert_eq!(output.stderr_tail, "x\ny\nz\nw");
    }

    #[cfg(unix)]
    #[test]
    fn run_streaming_tails_keep_the_last_bytes() {
        let command = sh(
            "head -c 100000 /dev/zero | tr '\\0' 'x'; printf END; \
             printf 'l1\\nl2\\nl3\\nl4\\n' >&2",
            &[],
        );
        let options = StreamOptions::new().stdout_tail_cap(10).stderr_tail_cap(6);
        let (output, chunks, lines) = stream(command, &options, false);
        assert!(output.status.success());
        // The callbacks saw everything; only the tails are cut.
        assert_eq!(chunks.len(), 100_003);
        assert_eq!(lines, ["l1", "l2", "l3", "l4"]);
        assert_eq!(output.stdout_tail, b"xxxxxxxEND".to_vec());
        assert_eq!(output.stderr_tail, "l3\nl4");
    }

    #[cfg(unix)]
    #[test]
    fn run_streaming_cancel_after_the_first_chunk_kills_the_tree_and_a_grandchild() {
        let _serial = serial();
        let holder = Grandchild::new("stream-cancel");
        // The grandchild shares the leader's group and both pipes; it records its pid before
        // the first byte is written.
        let command = sh(
            "sleep 30 & echo $! > \"$1\"; printf go; sleep 30",
            &[holder.0.to_str().unwrap()],
        );
        let options = StreamOptions::new().poll(Duration::from_millis(50));
        let started = Instant::now();
        let (output, chunks, _lines) = stream(command, &options, true);
        let elapsed = started.elapsed();
        assert!(output.cancelled);
        assert!(!output.timed_out);
        assert!(!output.status.success());
        assert_eq!(chunks, b"go".to_vec());
        assert_eq!(output.stdout_tail, b"go".to_vec());
        assert!(
            elapsed < Duration::from_secs(4),
            "cancel took {elapsed:?} to take effect"
        );
        assert!(holder.pid().is_some(), "the grandchild never started");
        let deadline = Instant::now() + Duration::from_secs(5);
        while holder.alive() {
            assert!(
                Instant::now() < deadline,
                "the grandchild survived the kill"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn run_streaming_cancel_before_any_output_ends_the_call() {
        let started = Instant::now();
        let output = run_streaming(
            sleeper(),
            &StreamOptions::new(),
            &|| true,
            &mut |_chunk: &[u8]| {},
            &mut |_line: &str| {},
        )
        .unwrap();
        assert!(output.cancelled);
        assert!(!output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(8));
    }

    #[test]
    fn run_streaming_timeout_sets_timed_out() {
        let options = StreamOptions::new().timeout(Duration::from_millis(300));
        let started = Instant::now();
        let (output, _chunks, lines) = stream(sleeper(), &options, false);
        let elapsed = started.elapsed();
        assert!(output.timed_out);
        assert!(!output.cancelled);
        assert!(lines.is_empty());
        assert!(elapsed >= Duration::from_millis(300), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(8), "{elapsed:?}");
    }

    #[cfg(unix)]
    #[test]
    fn run_streaming_with_input_round_trips_more_than_a_pipe_buffer() {
        let _serial = serial();
        let big = vec![b'x'; 1 << 20];
        let options = StreamOptions::new()
            .timeout(Duration::from_secs(30))
            .input(big.clone());
        let (output, chunks, _lines) = stream(sh("cat", &[]), &options, false);
        assert!(output.status.success());
        assert!(!output.timed_out);
        assert_eq!(chunks, big);
        assert_eq!(output.stdout_tail.len(), 64 * 1024);
        assert_eq!(LIVE_WRITERS.load(SeqCst), 0);
        // A child that never reads its input is a normal early exit.
        let options = StreamOptions::new().input(big);
        let (output, _chunks, _lines) = stream(sh("exit 7", &[]), &options, false);
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(LIVE_WRITERS.load(SeqCst), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_session_escaped_stdin_holder_does_not_hold_run_streaming_past_its_timeout() {
        let _serial = serial();
        let holder = Grandchild::new("stream-timeout");
        let script = holder_script("exec sleep 30", "sleep 30");
        let timeout = Duration::from_secs(1);
        let options = StreamOptions::new()
            .timeout(timeout)
            .input(vec![b'x'; 2 << 20]);
        let started = Instant::now();
        let (output, _chunks, _lines) = stream(
            sh(script.as_str(), &[holder.0.to_str().unwrap()]),
            &options,
            false,
        );
        let elapsed = started.elapsed();
        if !holder.alive() && !output.timed_out {
            eprintln!("skipped: no setsid or perl to leave the session");
            return;
        }
        assert!(
            holder.alive(),
            "the fixture's grandchild must still hold stdin"
        );
        assert!(output.timed_out);
        assert!(
            elapsed < timeout + Duration::from_secs(8),
            "run_streaming took {elapsed:?}"
        );
        assert_eq!(
            LIVE_WRITERS.load(SeqCst),
            0,
            "the writer thread must be gone"
        );
    }

    // ---- Capture failures surface as errors (not an Ok with partial output). ----

    /// A child that writes one line to each of stdout and stderr.
    fn noisy() -> OwnedCommand {
        #[cfg(unix)]
        {
            sh("printf out; printf err >&2", &[])
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let mut command = OwnedCommand::new("cmd");
            command.command_mut().raw_arg("/C echo out & echo err 1>&2");
            command
        }
    }

    #[test]
    fn a_capture_read_error_is_an_error_from_run_command() {
        use crate::native_capture::{inject_read_fault, Stream};
        let options = RunOptions::new(Duration::from_secs(20));
        for (stream, name) in [(Stream::Stdout, "stdout"), (Stream::Stderr, "stderr")] {
            inject_read_fault(stream, io::ErrorKind::PermissionDenied);
            let error = run_command(noisy(), None, &options)
                .expect_err("a stored capture error must not become an Ok with partial output");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
            assert!(error.to_string().contains(name), "{error}");
        }
        // Control: with nothing armed the same command is a plain Ok.
        let output = run_command(noisy(), None, &options).unwrap();
        assert!(output.status.success());
        assert!(!output.stdout.bytes.is_empty() && !output.stderr.bytes.is_empty());
    }

    /// Program and arguments for `run`/`run_with_stdin`, which take no `OwnedCommand`.
    fn noisy_argv() -> (&'static str, Vec<&'static str>) {
        #[cfg(unix)]
        {
            ("sh", vec!["-c", "printf out; printf err >&2"])
        }
        #[cfg(windows)]
        {
            ("cmd", vec!["/C", "echo out & echo err 1>&2"])
        }
    }

    #[test]
    fn a_capture_read_error_is_an_error_from_run_and_run_with_stdin() {
        use crate::native_capture::{inject_read_fault, Stream};
        let options = RunOptions::new(Duration::from_secs(20));
        let (program, args) = noisy_argv();
        for (stream, name) in [(Stream::Stdout, "stdout"), (Stream::Stderr, "stderr")] {
            inject_read_fault(stream, io::ErrorKind::PermissionDenied);
            let error = run(program, &args, &options).expect_err(
                "run: a stored capture error must not become an Ok with partial output",
            );
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
            assert!(error.to_string().contains(name), "{error}");

            inject_read_fault(stream, io::ErrorKind::PermissionDenied);
            let error = run_with_stdin(program, &args, b"input", &options).expect_err(
                "run_with_stdin: a stored capture error must not become an Ok with partial output",
            );
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
            assert!(error.to_string().contains(name), "{error}");
        }
        // Control: with nothing armed both runners return a plain Ok with both streams.
        let output = run(program, &args, &options).unwrap();
        assert!(output.status.success());
        assert!(!output.stdout.bytes.is_empty() && !output.stderr.bytes.is_empty());
        let output = run_with_stdin(program, &args, b"input", &options).unwrap();
        assert!(output.status.success());
    }

    #[test]
    fn a_capture_read_error_is_an_error_from_run_streaming() {
        use crate::native_capture::{inject_read_fault, Stream};
        let options = StreamOptions::new().timeout(Duration::from_secs(20));
        for (stream, name) in [(Stream::Stdout, "stdout"), (Stream::Stderr, "stderr")] {
            inject_read_fault(stream, io::ErrorKind::PermissionDenied);
            let error = run_streaming(noisy(), &options, &|| false, &mut |_| {}, &mut |_| {})
                .expect_err("a stored capture error must not become an Ok with partial output");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
            assert!(error.to_string().contains(name), "{error}");
        }
        let output = run_streaming(noisy(), &options, &|| false, &mut |_| {}, &mut |_| {}).unwrap();
        assert!(output.status.success());
    }

    // ---- One piped-stdio setup for both spawn paths. ----

    /// `command_mut` after the runners' stdio request cannot undo it: the request is applied at
    /// spawn, after every caller edit.
    #[test]
    fn command_mut_after_the_piped_setup_does_not_lose_it() {
        let mut command = sleeper();
        command.pipe_stdio(true);
        command
            .command_mut()
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        assert!(child.take_stdin().is_some(), "stdin must stay piped");
        assert!(child.take_stdout().is_some(), "stdout must stay piped");
        assert!(child.take_stderr().is_some(), "stderr must stay piped");
    }

    /// Windows: the same, on the native spawn path, where an explicit config (here with every
    /// stream null) is what drives stdio and `command_mut` runs after it was set.
    #[cfg(windows)]
    #[test]
    fn command_mut_after_an_explicit_config_does_not_lose_the_piped_setup() {
        use crate::{WindowsSpawnConfig, WindowsStdio};
        let mut command = sleeper();
        command
            .inherit_handles_only([])
            .windows_spawn_config(WindowsSpawnConfig {
                stdin: WindowsStdio::Null,
                stdout: WindowsStdio::Null,
                stderr: WindowsStdio::Null,
                ..Default::default()
            });
        command.pipe_stdio(true);
        command
            .command_mut()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        assert!(child.take_stdin().is_some(), "stdin must stay piped");
        assert!(child.take_stdout().is_some(), "stdout must stay piped");
        assert!(child.take_stderr().is_some(), "stderr must stay piped");
    }

    /// Windows: the automatic config of `OwnedCommand::new` survives the runners' stdio request
    /// (it used to be cleared by their `command_mut`, failing an allowlisted spawn).
    #[cfg(windows)]
    #[test]
    fn the_piped_setup_keeps_the_automatic_windows_config() {
        let mut command = OwnedCommand::new("sort");
        command.inherit_handles_only([]);
        command.pipe_stdio(true);
        let mut child = command.spawn().unwrap();
        assert!(child.take_stdin().is_some(), "stdin must be piped");
        assert!(child.take_stdout().is_some(), "stdout must be piped");
        assert!(child.take_stderr().is_some(), "stderr must be piped");
    }

    /// `cmd /C findstr . & echo err 1>&2`: copies stdin to stdout, then writes `err` to stderr.
    #[cfg(windows)]
    fn echo_stdin_and_report_on_stderr() -> OwnedCommand {
        use std::os::windows::process::CommandExt;
        let mut command = OwnedCommand::new("cmd");
        command
            .command_mut()
            .raw_arg("/C findstr . & echo err 1>&2");
        command
    }

    /// Explicit configs an allowlisted command may carry: the default (all inherited) and one
    /// with every stream null. The runners must replace the stdio of both.
    #[cfg(windows)]
    fn explicit_configs() -> Vec<crate::WindowsSpawnConfig> {
        use crate::{WindowsSpawnConfig, WindowsStdio};
        vec![
            WindowsSpawnConfig::default(),
            WindowsSpawnConfig {
                stdin: WindowsStdio::Null,
                stdout: WindowsStdio::Null,
                stderr: WindowsStdio::Null,
                ..Default::default()
            },
        ]
    }

    #[cfg(windows)]
    #[test]
    fn a_configured_windows_command_gets_stdin_and_captures_both_streams_through_run_command() {
        let _serial = serial();
        for config in explicit_configs() {
            let mut command = echo_stdin_and_report_on_stderr();
            command
                .inherit_handles_only([])
                .windows_spawn_config(config);
            let output = run_command(
                command,
                Some(b"hello\r\n"),
                &RunOptions::new(Duration::from_secs(30)),
            )
            .unwrap();
            assert!(!output.timed_out);
            let stdout = String::from_utf8_lossy(&output.stdout.bytes).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr.bytes).into_owned();
            assert!(
                stdout.contains("hello"),
                "stdin was not delivered: {stdout:?}"
            );
            assert!(
                stderr.contains("err"),
                "stderr was not captured: {stderr:?}"
            );
            assert_eq!(LIVE_WRITERS.load(SeqCst), 0);
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_configured_windows_command_gets_stdin_and_captures_both_streams_through_run_streaming() {
        let _serial = serial();
        for config in explicit_configs() {
            let mut command = echo_stdin_and_report_on_stderr();
            command
                .inherit_handles_only([])
                .windows_spawn_config(config);
            let options = StreamOptions::new()
                .timeout(Duration::from_secs(30))
                .input(b"hello\r\n".to_vec());
            let (output, chunks, lines) = stream(command, &options, false);
            assert!(!output.timed_out);
            let stdout = String::from_utf8_lossy(&chunks).into_owned();
            assert!(
                stdout.contains("hello"),
                "stdin was not delivered: {stdout:?}"
            );
            assert!(
                lines.iter().any(|line| line.contains("err")),
                "stderr was not captured: {lines:?}"
            );
            assert_eq!(LIVE_WRITERS.load(SeqCst), 0);
        }
    }

    /// An automatic config (a `new()` command never touched through `command_mut`) used to be
    /// cleared by the runner's own `command_mut`, failing the allowlisted spawn.
    #[cfg(windows)]
    #[test]
    fn an_allowlisted_new_command_with_the_automatic_config_runs_through_both_runners() {
        let _serial = serial();
        let sorted = |bytes: &[u8]| String::from_utf8_lossy(bytes).replace("\r\n", "\n");
        let mut command = OwnedCommand::new("sort");
        command.inherit_handles_only([]);
        let output = run_command(
            command,
            Some(b"b\r\na\r\n"),
            &RunOptions::new(Duration::from_secs(30)),
        )
        .unwrap();
        assert_eq!(sorted(&output.stdout.bytes), "a\nb\n");

        let mut command = OwnedCommand::new("sort");
        command.inherit_handles_only([]);
        let options = StreamOptions::new()
            .timeout(Duration::from_secs(30))
            .input(b"b\r\na\r\n".to_vec());
        let (_output, chunks, _lines) = stream(command, &options, false);
        assert_eq!(sorted(&chunks), "a\nb\n");
    }

    // ---- Windows: the runners end to end (large stdin, surviving descendants, output caps). ----
    //
    // The Unix tests above drive `sh`. Windows has no shell fixture that holds a pipe open without
    // reading it, so the helper is this test binary run again: a test that does nothing unless it
    // is armed (`HELPER_ENV`, or `HELPER_ARG` for `run` and `run_with_stdin`, which take no
    // environment), the pattern of `rightkit-fs`' lease kill helper. The child under test is the
    // "leader". It starts a "holder", another run of the helper that inherits the leader's three
    // stdio handles, never reads stdin and sleeps: inside the job (an ordinary descendant, which
    // the runners must take down) or, when the job allows it, outside the job with
    // `CREATE_BREAKAWAY_FROM_JOB` (the Windows counterpart of the Unix setsid holder: it survives
    // the tree kill and keeps both pipe ends open). `run` and `run_with_stdin` build their command
    // with `OwnedCommand::new`, whose job refuses breakaway, so nothing can outlive them: the
    // surviving-holder cases use `run_command` and `run_streaming`, which take an `OwnedCommand`
    // and can call `windows_allow_breakaway(true)`.
    //
    // libtest writes its own lines to the stdout it inherits, so echoed bytes are framed by
    // markers, and the leader waits until the holder is ready (its libtest header is written
    // before it signals) before it writes anything of its own. Every wait is bounded and every
    // survivor is killed (by an identity-pinned pid) when its fixture drops.

    /// Arms `windows_runner_helper_child`: the value is the spec of the role to play.
    #[cfg(windows)]
    const HELPER_ENV: &str = "RK_RUN_HELPER";
    /// The other way to arm it, for the runners that cannot set an environment variable: an
    /// extra libtest filter (it matches no test) that carries the spec.
    #[cfg(windows)]
    const HELPER_ARG: &str = "rk-run-helper=";
    #[cfg(windows)]
    const BEGIN: &[u8] = b"<<RK-BEGIN>>";
    #[cfg(windows)]
    const END: &[u8] = b"<<RK-END>>";
    /// Written by a leader to say the holder exists and it is about to block.
    #[cfg(windows)]
    const GO: &[u8] = b"<<RK-GO>>";
    /// Written to stdout by the flood roles when they have written everything: a child that
    /// blocked on a full stderr pipe never gets here.
    #[cfg(windows)]
    const DONE: &[u8] = b"<<RK-DONE>>";
    /// Longest a holder or a blocked leader lives if nothing kills it, so a leak ends by itself.
    #[cfg(windows)]
    const HOLD_SECONDS: u64 = 120;

    /// Printable bytes that do not repeat over a short period, so a dropped, duplicated or
    /// reordered stretch shows up.
    #[cfg(windows)]
    fn pattern(offset: usize, len: usize) -> Vec<u8> {
        (offset..offset + len)
            .map(|index| b'!' + ((index + index / 95) % 90) as u8)
            .collect()
    }

    #[cfg(windows)]
    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    /// What lies between the first `BEGIN` and the `END` after it.
    #[cfg(windows)]
    fn framed(output: &[u8]) -> &[u8] {
        let begin = find(output, BEGIN).expect("the begin marker is missing") + BEGIN.len();
        let end = begin + find(&output[begin..], END).expect("the end marker is missing");
        &output[begin..end]
    }

    #[cfg(windows)]
    fn helper_spec() -> Option<String> {
        if let Ok(spec) = std::env::var(HELPER_ENV) {
            return Some(spec);
        }
        std::env::args().find_map(|arg| arg.strip_prefix(HELPER_ARG).map(str::to_owned))
    }

    /// The libtest name of the helper, for `--exact`.
    #[cfg(windows)]
    fn helper_test_name() -> String {
        let module = module_path!();
        let path = module.split_once("::").map_or(module, |(_, rest)| rest);
        format!("{path}::windows_runner_helper_child")
    }

    /// The helper armed through the environment, for `run_command` and `run_streaming`.
    #[cfg(windows)]
    fn helper_command(spec: &str) -> OwnedCommand {
        let mut command = OwnedCommand::new(std::env::current_exe().unwrap());
        command
            .command_mut()
            .args([
                "--exact",
                helper_test_name().as_str(),
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HELPER_ENV, spec);
        command
    }

    /// The helper armed through an argument: the arguments for `run` and `run_with_stdin` (the
    /// program is `std::env::current_exe()`).
    #[cfg(windows)]
    fn helper_argv(spec: &str) -> Vec<String> {
        vec![
            "--exact".to_owned(),
            helper_test_name(),
            "--nocapture".to_owned(),
            "--test-threads=1".to_owned(),
            format!("{HELPER_ARG}{spec}"),
        ]
    }

    #[cfg(windows)]
    fn wait_for_file(path: &std::path::Path, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while !path.exists() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        true
    }

    /// Writes through a temporary name and renames, so a reader never sees half a file.
    #[cfg(windows)]
    fn write_atomic(path: &std::path::Path, text: &str) {
        let mut temporary = path.as_os_str().to_owned();
        temporary.push(".tmp");
        std::fs::write(&temporary, text).unwrap();
        std::fs::rename(&temporary, path).unwrap();
    }

    #[cfg(windows)]
    fn hold_for_a_while() {
        for _ in 0..HOLD_SECONDS {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    /// Role `echo`: copies stdin to stdout between the markers, then ends.
    #[cfg(windows)]
    fn echo_stdin_framed() {
        use std::io::{Read, Write};
        let mut stdin = std::io::stdin();
        let mut stdout = std::io::stdout();
        stdout.write_all(BEGIN).unwrap();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = stdin.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            stdout.write_all(&buffer[..read]).unwrap();
        }
        stdout.write_all(END).unwrap();
        stdout.flush().unwrap();
    }

    /// Role `flood`: `total` pattern bytes on stderr after the begin marker, then `DONE` on
    /// stdout.
    #[cfg(windows)]
    fn flood_stderr(total: usize) {
        use std::io::Write;
        let mut stderr = std::io::stderr();
        stderr.write_all(BEGIN).unwrap();
        let mut written = 0;
        while written < total {
            let chunk = (total - written).min(8 * 1024);
            stderr.write_all(&pattern(written, chunk)).unwrap();
            written += chunk;
        }
        let mut stdout = std::io::stdout();
        stdout.write_all(DONE).unwrap();
        stdout.flush().unwrap();
    }

    /// Role `flood-lines`: `count` stderr lines `L0000000`, `L0000001`, ..., then `DONE` on
    /// stdout.
    #[cfg(windows)]
    fn flood_stderr_lines(count: usize) {
        use std::io::Write;
        let mut stderr = std::io::stderr();
        let mut next = 0;
        while next < count {
            let upto = (next + 1000).min(count);
            let mut block = String::new();
            for index in next..upto {
                block.push_str(&format!("L{index:07}\n"));
            }
            stderr.write_all(block.as_bytes()).unwrap();
            next = upto;
        }
        let mut stdout = std::io::stdout();
        stdout.write_all(DONE).unwrap();
        stdout.flush().unwrap();
    }

    /// Role `hold-read`: signals ready, waits for the gate file, then reads stdin to its end and
    /// records how many bytes it got. With the write end closed by the parent that is only what
    /// was already in the pipe; a write end left open would deliver everything.
    #[cfg(windows)]
    fn hold_then_count_stdin(
        ready: &std::path::Path,
        gate: &std::path::Path,
        count_file: &std::path::Path,
    ) {
        use std::io::Read;
        write_atomic(ready, "ready");
        assert!(
            wait_for_file(gate, Duration::from_secs(HOLD_SECONDS)),
            "the gate never opened"
        );
        let mut stdin = std::io::stdin();
        let mut buffer = vec![0u8; 64 * 1024];
        let mut total = 0usize;
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => total += read,
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => break,
                Err(error) => panic!("reading stdin failed: {error}"),
            }
        }
        write_atomic(count_file, &total.to_string());
        hold_for_a_while();
    }

    /// Role `leader`: records its pid, starts the holder (outside the job when `breakaway`),
    /// waits for it to be ready, records its pid, then does `then`: `echo` the input, `exit`
    /// without reading it, `block` until killed, or say `GO` and block. A holder that cannot be
    /// started is reported in `holder.err` (the raw OS error, 5 when the job refused breakaway)
    /// and the leader ends.
    #[cfg(windows)]
    #[allow(clippy::zombie_processes)] // The holder is meant to outlive the leader.
    fn run_leader(dir: &std::path::Path, holder: &str, breakaway: bool, then: &str) {
        use std::io::Write;
        use std::os::windows::process::CommandExt;
        write_atomic(&dir.join("leader.pid"), &std::process::id().to_string());
        let ready = dir.join("holder.ready");
        let holder_spec = match holder {
            "hold" => format!("hold|{}", ready.display()),
            "hold-read" => format!(
                "hold-read|{}|{}|{}",
                ready.display(),
                dir.join("gate").display(),
                dir.join("count").display()
            ),
            other => panic!("unknown holder {other}"),
        };
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                helper_test_name().as_str(),
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HELPER_ENV, holder_spec)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());
        if breakaway {
            command.creation_flags(crate::CREATE_BREAKAWAY_FROM_JOB);
        }
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let report = error
                    .raw_os_error()
                    .map_or_else(|| error.to_string(), |code| code.to_string());
                write_atomic(&dir.join("holder.err"), &report);
                return;
            }
        };
        if !wait_for_file(&ready, Duration::from_secs(30)) {
            write_atomic(&dir.join("holder.err"), "the holder never became ready");
            return;
        }
        write_atomic(&dir.join("holder.pid"), &child.id().to_string());
        drop(child);
        match then {
            "echo" => echo_stdin_framed(),
            "exit" => {}
            "block" => hold_for_a_while(),
            "go" => {
                let mut stdout = std::io::stdout();
                stdout.write_all(GO).unwrap();
                stdout.flush().unwrap();
                hold_for_a_while();
            }
            other => panic!("unknown leader action {other}"),
        }
    }

    /// Not a test by itself: it returns at once unless `HELPER_ENV` or `HELPER_ARG` arms it, which
    /// only the Windows runner tests below do. Specs: `echo`, `exit-now`, `flood|<bytes>`,
    /// `flood-lines|<count>`, `hold|<ready file>`, `hold-read|<ready>|<gate>|<count file>` and
    /// `leader|<dir>|<hold or hold-read>|<1 to break away, else 0>|<echo, exit, block or go>`.
    #[cfg(windows)]
    #[test]
    fn windows_runner_helper_child() {
        let Some(spec) = helper_spec() else {
            return;
        };
        let parts: Vec<&str> = spec.split('|').collect();
        match parts.as_slice() {
            ["echo"] => echo_stdin_framed(),
            ["exit-now"] => std::process::exit(7),
            ["flood", total] => flood_stderr(total.parse().unwrap()),
            ["flood-lines", count] => flood_stderr_lines(count.parse().unwrap()),
            ["hold", ready] => {
                write_atomic(std::path::Path::new(ready), "ready");
                hold_for_a_while();
            }
            ["hold-read", ready, gate, count] => hold_then_count_stdin(
                std::path::Path::new(ready),
                std::path::Path::new(gate),
                std::path::Path::new(count),
            ),
            ["leader", dir, holder, breakaway, then] => {
                run_leader(std::path::Path::new(dir), holder, *breakaway == "1", then)
            }
            other => panic!("unknown helper spec {other:?}"),
        }
    }

    /// A scratch directory for one test's pid, ready and gate files, and the identity of the
    /// holder once it is known to be alive. Dropping it kills that holder (pinned by its start
    /// time, so a recycled pid is never signalled) and removes the directory.
    #[cfg(windows)]
    struct HelperFixture {
        dir: std::path::PathBuf,
        holder: Option<crate::AdoptedIdentity>,
    }

    #[cfg(windows)]
    impl HelperFixture {
        fn new(label: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos());
            let dir = std::env::temp_dir()
                .join(format!("rk-run-win-{label}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir, holder: None }
        }

        fn read(&self, name: &str) -> Option<String> {
            std::fs::read_to_string(self.dir.join(name)).ok()
        }

        fn pid(&self, name: &str) -> Option<u32> {
            self.read(name)?.trim().parse().ok()
        }

        fn leader_spec(&self, holder: &str, breakaway: bool, then: &str) -> String {
            format!(
                "leader|{}|{holder}|{}|{then}",
                self.dir.display(),
                u8::from(breakaway)
            )
        }

        /// The leader as an `OwnedCommand` whose job allows breakaway exactly when the leader
        /// asks for it.
        fn command(&self, holder: &str, breakaway: bool, then: &str) -> OwnedCommand {
            let mut command = helper_command(&self.leader_spec(holder, breakaway, then));
            command.windows_allow_breakaway(breakaway);
            command
        }

        /// The leader's arguments for `run` / `run_with_stdin`.
        fn argv(&self, holder: &str, breakaway: bool, then: &str) -> Vec<String> {
            helper_argv(&self.leader_spec(holder, breakaway, then))
        }

        /// The pid the leader reported for its holder, or a failure that says why there is none.
        fn holder_pid(&self) -> u32 {
            if let Some(error) = self.read("holder.err") {
                panic!(
                    "the leader could not start its holder: {error} (5 means the job refused \
                     CREATE_BREAKAWAY_FROM_JOB)"
                );
            }
            self.pid("holder.pid").expect(
                "the leader never reported its holder before the call ended (a very slow host?)",
            )
        }

        /// Asserts the holder is still alive and remembers its identity so `Drop` can kill it.
        fn track_holder(&mut self) -> u32 {
            let pid = self.holder_pid();
            let identity = crate::AdoptedIdentity::capture(std::num::NonZeroU32::new(pid).unwrap())
                .unwrap()
                .unwrap_or_else(|| {
                    panic!("the holder (pid {pid}) is gone: it did not outlive the call")
                });
            assert!(
                identity.is_running().unwrap(),
                "the holder (pid {pid}) must still hold the pipes"
            );
            self.holder = Some(identity);
            pid
        }
    }

    #[cfg(windows)]
    impl Drop for HelperFixture {
        fn drop(&mut self) {
            if let Some(identity) = self.holder.take() {
                let _ = identity.signal(crate::AdoptedSignal::Force);
                let _ = identity.wait_confirmed_gone(Duration::from_secs(5));
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Waits (bounded) until no process has `pid`; fails if one still does.
    #[cfg(windows)]
    fn assert_gone(pid: u32, what: &str) {
        let process = crate::AdoptedProcess::new(std::num::NonZeroU32::new(pid).unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        while process.is_running().unwrap() {
            assert!(
                Instant::now() < deadline,
                "{what} (pid {pid}) is still running"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// 1 MiB through stdin of a leader that echoes it back while an in-job holder keeps all
    /// three pipes open: both entry points that take input return the whole echo (neither
    /// direction deadlocks), the leader's own status, and no holder is left behind.
    #[cfg(windows)]
    #[test]
    fn run_with_stdin_and_run_command_round_trip_one_mib_and_reap_an_in_job_holder_windows() {
        let _serial = serial();
        let input = pattern(0, 1 << 20);
        let options = RunOptions::new(Duration::from_secs(60)).stdout_cap(4 << 20);
        for entry in ["run_with_stdin", "run_command"] {
            let fixture = HelperFixture::new(entry);
            let started = Instant::now();
            let result = if entry == "run_with_stdin" {
                run_with_stdin(
                    std::env::current_exe().unwrap(),
                    fixture.argv("hold", false, "echo"),
                    &input,
                    &options,
                )
            } else {
                run_command(
                    fixture.command("hold", false, "echo"),
                    Some(&input),
                    &options,
                )
            };
            let output = result.unwrap();
            let elapsed = started.elapsed();
            assert!(output.status.success(), "{entry}: {:?}", output.status);
            assert!(!output.timed_out, "{entry}: the call ran to its deadline");
            assert!(!output.stdout.truncated, "{entry}: stdout hit its cap");
            let echoed = framed(&output.stdout.bytes);
            assert!(
                echoed == input.as_slice(),
                "{entry}: echoed {} bytes, expected {}",
                echoed.len(),
                input.len()
            );
            assert!(
                elapsed < Duration::from_secs(30),
                "{entry} took {elapsed:?}"
            );
            assert_eq!(
                LIVE_WRITERS.load(SeqCst),
                0,
                "{entry}: the writer thread must be gone"
            );
            assert_gone(fixture.holder_pid(), "the in-job holder");
        }
    }

    /// The same through `run_streaming`: the callbacks see the whole echo, the stdout tail is
    /// full, and the in-job holder is gone when the call returns.
    #[cfg(windows)]
    #[test]
    fn run_streaming_with_input_round_trips_one_mib_and_reaps_an_in_job_holder_windows() {
        let _serial = serial();
        let input = pattern(0, 1 << 20);
        let fixture = HelperFixture::new("stream-round-trip");
        let options = StreamOptions::new()
            .timeout(Duration::from_secs(60))
            .input(input.clone());
        let started = Instant::now();
        let command = fixture.command("hold", false, "echo");
        let (output, chunks, _lines) = stream(command, &options, false);
        let elapsed = started.elapsed();
        assert!(output.status.success(), "{:?}", output.status);
        assert!(!output.timed_out && !output.cancelled);
        let echoed = framed(&chunks);
        assert!(
            echoed == input.as_slice(),
            "echoed {} bytes, expected {}",
            echoed.len(),
            input.len()
        );
        assert_eq!(output.stdout_tail.len(), 64 * 1024);
        assert!(
            elapsed < Duration::from_secs(30),
            "run_streaming took {elapsed:?}"
        );
        assert_eq!(
            LIVE_WRITERS.load(SeqCst),
            0,
            "the writer thread must be gone"
        );
        assert_gone(fixture.holder_pid(), "the in-job holder");
    }

    /// Windows counterpart of the Unix early-exit cases: a child that exits at once without
    /// reading 1 MiB of input is a normal early exit (the broken pipe is not an error) through
    /// every runner that takes input, with the child's own status.
    #[cfg(windows)]
    #[test]
    fn a_child_that_exits_without_reading_its_input_is_a_normal_early_exit_through_every_runner_windows(
    ) {
        let _serial = serial();
        let big = pattern(0, 1 << 20);
        let options = RunOptions::new(Duration::from_secs(60));
        let output = run_with_stdin(
            std::env::current_exe().unwrap(),
            helper_argv("exit-now"),
            &big,
            &options,
        )
        .unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert!(!output.timed_out);
        let output = run_command(helper_command("exit-now"), Some(&big), &options).unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert!(!output.timed_out);
        let options = StreamOptions::new()
            .timeout(Duration::from_secs(60))
            .input(big.clone());
        let (output, _chunks, _lines) = stream(helper_command("exit-now"), &options, false);
        assert_eq!(output.status.code(), Some(7));
        assert!(!output.timed_out && !output.cancelled);
        assert_eq!(
            LIVE_WRITERS.load(SeqCst),
            0,
            "the writer thread must be gone"
        );
    }

    /// Windows counterpart of `a_session_escaped_stdin_holder_does_not_hold_run_past_its_exit`:
    /// the leader exits at once without reading 2 MiB of input while a holder that broke away
    /// from the job keeps stdin, stdout and stderr open and never reads. `run_command` returns
    /// well inside its deadline with the leader's status and no writer thread; the holder is
    /// still alive; and the write end really is closed: once the test opens the gate the holder
    /// reads stdin to its end and got only what was already in the pipe.
    #[cfg(windows)]
    #[test]
    fn a_breakaway_stdin_holder_does_not_hold_run_command_past_its_exit_windows() {
        let _serial = serial();
        let mut fixture = HelperFixture::new("command-exit");
        let big = pattern(0, 2 << 20);
        let options = RunOptions::new(Duration::from_secs(60));
        let started = Instant::now();
        let output = run_command(
            fixture.command("hold-read", true, "exit"),
            Some(&big),
            &options,
        )
        .unwrap();
        let elapsed = started.elapsed();
        fixture.track_holder();
        assert!(!output.timed_out);
        assert!(output.status.success(), "{:?}", output.status);
        // The leader's exit is immediate; the drains get 2 s each and the writer 0.5 s + 2 s.
        assert!(
            elapsed < Duration::from_secs(20),
            "run_command took {elapsed:?}"
        );
        assert_eq!(
            LIVE_WRITERS.load(SeqCst),
            0,
            "the writer thread must be gone"
        );
        std::fs::write(fixture.dir.join("gate"), "open").unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let received = loop {
            let count = fixture.read("count");
            if let Some(count) = count.and_then(|text| text.trim().parse::<usize>().ok()) {
                break count;
            }
            assert!(
                Instant::now() < deadline,
                "the holder never reached the end of stdin: the write end is still open"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(
            received < big.len() / 2,
            "{received} bytes reached the holder after run_command returned"
        );
    }

    /// Windows counterpart of `a_session_escaped_stdin_holder_does_not_hold_run_command_past_its_timeout`:
    /// the leader blocks past the deadline, so the tree kill (not an exit) ends it, while a
    /// holder outside the job keeps all three pipes. The call still returns after the drain and
    /// writer bounds, reports `timed_out`, leaves no writer thread, and the holder survives.
    #[cfg(windows)]
    #[test]
    fn a_breakaway_stdin_holder_does_not_hold_run_command_past_its_timeout_windows() {
        let _serial = serial();
        let mut fixture = HelperFixture::new("command-timeout");
        let big = pattern(0, 2 << 20);
        let timeout = Duration::from_secs(8);
        let options = RunOptions::new(timeout);
        let started = Instant::now();
        let output =
            run_command(fixture.command("hold", true, "block"), Some(&big), &options).unwrap();
        let elapsed = started.elapsed();
        fixture.track_holder();
        assert!(output.timed_out);
        assert!(
            elapsed < timeout + Duration::from_secs(15),
            "run_command took {elapsed:?}"
        );
        assert_eq!(
            LIVE_WRITERS.load(SeqCst),
            0,
            "the writer thread must be gone"
        );
    }

    /// Windows counterpart of `a_session_escaped_stdin_holder_does_not_hold_run_streaming_past_its_timeout`.
    #[cfg(windows)]
    #[test]
    fn a_breakaway_stdin_holder_does_not_hold_run_streaming_past_its_timeout_windows() {
        let _serial = serial();
        let mut fixture = HelperFixture::new("stream-timeout");
        let timeout = Duration::from_secs(8);
        let options = StreamOptions::new()
            .timeout(timeout)
            .input(pattern(0, 2 << 20));
        let started = Instant::now();
        let (output, _chunks, _lines) =
            stream(fixture.command("hold", true, "block"), &options, false);
        let elapsed = started.elapsed();
        fixture.track_holder();
        assert!(output.timed_out && !output.cancelled);
        assert!(
            elapsed < timeout + Duration::from_secs(15),
            "run_streaming took {elapsed:?}"
        );
        assert_eq!(
            LIVE_WRITERS.load(SeqCst),
            0,
            "the writer thread must be gone"
        );
    }

    /// The premise of the two tests above, and of why `run` and `run_with_stdin` have no
    /// surviving-holder case: their job refuses breakaway, so a descendant that asks for it is
    /// refused (`ERROR_ACCESS_DENIED`, 5) and nothing can outlive the call.
    #[cfg(windows)]
    #[test]
    fn run_with_stdin_contains_a_descendant_that_asks_to_break_away_windows() {
        let _serial = serial();
        let fixture = HelperFixture::new("no-breakaway");
        let output = run_with_stdin(
            std::env::current_exe().unwrap(),
            fixture.argv("hold", true, "exit"),
            b"",
            &RunOptions::new(Duration::from_secs(60)),
        )
        .unwrap();
        assert!(output.status.success(), "{:?}", output.status);
        assert!(!output.timed_out);
        assert_eq!(
            fixture.read("holder.err").as_deref(),
            Some("5"),
            "the job must refuse CREATE_BREAKAWAY_FROM_JOB"
        );
        assert!(fixture.pid("holder.pid").is_none());
        assert_eq!(LIVE_WRITERS.load(SeqCst), 0);
    }

    /// Windows counterpart of
    /// `run_streaming_cancel_after_the_first_chunk_kills_the_tree_and_a_grandchild`: the cancel
    /// predicate turns true when the leader's `GO` marker arrives (after its in-job holder is
    /// ready and both pids are recorded). The call ends promptly as cancelled, not as a timeout,
    /// and by pid the leader and the holder are both gone.
    #[cfg(windows)]
    #[test]
    fn run_streaming_cancel_after_the_first_chunk_kills_the_tree_and_a_grandchild_windows() {
        let fixture = HelperFixture::new("stream-cancel");
        let options = StreamOptions::new()
            .poll(Duration::from_millis(50))
            .timeout(Duration::from_secs(60));
        let cancel = std::cell::Cell::new(false);
        let seen_at = std::cell::Cell::new(None::<Instant>);
        let mut chunks = Vec::new();
        let output = run_streaming(
            fixture.command("hold", false, "go"),
            &options,
            &|| cancel.get(),
            &mut |chunk: &[u8]| {
                chunks.extend_from_slice(chunk);
                if !cancel.get() && find(&chunks, GO).is_some() {
                    seen_at.set(Some(Instant::now()));
                    cancel.set(true);
                }
            },
            &mut |_line: &str| {},
        )
        .unwrap();
        let seen = seen_at.get().expect("the leader's GO marker never arrived");
        assert!(output.cancelled);
        assert!(!output.timed_out);
        assert!(!output.status.success(), "{:?}", output.status);
        assert!(
            seen.elapsed() < Duration::from_secs(8),
            "cancel took {:?} to take effect",
            seen.elapsed()
        );
        let leader = fixture
            .pid("leader.pid")
            .expect("the leader recorded no pid");
        let holder = fixture.holder_pid();
        assert_ne!(leader, holder);
        assert_gone(leader, "the leader");
        assert_gone(holder, "the in-job holder");
    }

    /// What every flood case asserts about a call whose child wrote `flood` bytes to stderr
    /// with a `cap`-byte capture: the child finished (it never blocked on a full pipe), the
    /// kept bytes are exactly the first `cap`, `truncated` says so, and `total_bytes` counts
    /// everything the child wrote.
    #[cfg(windows)]
    fn assert_flood_capped(output: &RunOutput, cap: usize, flood: usize) {
        assert!(
            !output.timed_out,
            "the flood held the child until the deadline"
        );
        assert!(output.status.success(), "{:?}", output.status);
        assert_eq!(output.stderr.bytes.len(), cap);
        assert!(output.stderr.truncated);
        assert!(
            output.stderr.total_bytes >= (flood + BEGIN.len()) as u64,
            "stderr counted {} bytes, the child wrote {}",
            output.stderr.total_bytes,
            flood + BEGIN.len()
        );
        let kept = &output.stderr.bytes;
        let at = find(kept, BEGIN).expect("the begin marker is not in the kept bytes");
        let body = &output.stderr.bytes[at + BEGIN.len()..];
        assert!(
            body == pattern(0, body.len()).as_slice(),
            "the kept bytes are not the first ones the child wrote"
        );
        assert!(
            !output.stdout.truncated,
            "the stderr cap must not touch stdout"
        );
        assert!(
            find(&output.stdout.bytes, DONE).is_some(),
            "the child never finished writing"
        );
    }

    /// A stderr flood far past the capture cap, through `run` (explicit cap) and `run_command`
    /// (the 1 MiB default): the cap is enforced exactly, the call completes instead of
    /// stalling on a full pipe, and the truncation is reported.
    #[cfg(windows)]
    #[test]
    fn a_stderr_flood_past_the_cap_is_truncated_not_blocking_through_run_and_run_command_windows() {
        let cap = 64 * 1024;
        let flood = 4 << 20;
        let started = Instant::now();
        let output = run(
            std::env::current_exe().unwrap(),
            helper_argv(&format!("flood|{flood}")),
            &RunOptions::new(Duration::from_secs(60)).stderr_cap(cap),
        )
        .unwrap();
        assert_flood_capped(&output, cap, flood);
        let flood = 3 << 20;
        let output = run_command(
            helper_command(&format!("flood|{flood}")),
            None,
            &RunOptions::new(Duration::from_secs(60)),
        )
        .unwrap();
        assert_flood_capped(&output, 1024 * 1024, flood);
        assert!(
            started.elapsed() < Duration::from_secs(90),
            "the floods took {:?}",
            started.elapsed()
        );
    }

    /// A stderr flood through `run_streaming`: every line reaches the callback in order, only
    /// the tail is kept (within its cap, ending with the last line), and the call completes.
    #[cfg(windows)]
    #[test]
    fn a_stderr_flood_does_not_block_run_streaming_and_keeps_only_the_tail_windows() {
        const LINES: usize = 400_000;
        let options = StreamOptions::new()
            .timeout(Duration::from_secs(60))
            .stderr_tail_cap(1024);
        let started = Instant::now();
        let (output, _chunks, lines) = stream(
            helper_command(&format!("flood-lines|{LINES}")),
            &options,
            false,
        );
        assert!(!output.timed_out && !output.cancelled);
        assert!(output.status.success(), "{:?}", output.status);
        let ours: Vec<&str> = lines
            .iter()
            .map(String::as_str)
            .filter(|line| line.len() == 8 && line.starts_with('L'))
            .collect();
        assert_eq!(ours.len(), LINES, "lines were lost or never delivered");
        for (index, line) in ours.iter().enumerate() {
            assert_eq!(*line, format!("L{index:07}"));
        }
        assert!(
            output.stderr_tail.len() <= 1024,
            "{}",
            output.stderr_tail.len()
        );
        assert!(
            output.stderr_tail.ends_with(&format!("L{:07}", LINES - 1)),
            "the tail does not end with the last line"
        );
        assert!(
            find(&output.stdout_tail, DONE).is_some(),
            "the child never finished writing"
        );
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "run_streaming took {:?}",
            started.elapsed()
        );
    }
}
