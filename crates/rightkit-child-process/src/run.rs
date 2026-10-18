//! One call that runs a program to completion with a deadline, bounded output capture, and no
//! console window on Windows: the shape of the hand-rolled timeout loops that apps wrote around
//! [`OwnedCommand`](crate::OwnedCommand) and [`NativeOutputCapture`](crate::NativeOutputCapture).
//!
//! The child is owned: it runs in its own process group (or session on Unix) inside a Job
//! Object on Windows, and when the deadline passes its whole tree is terminated, not only the
//! leader. Stdin is null. Output beyond each cap is drained and discarded, so a chatty child
//! never blocks on a full pipe, and [`CapturedOutput::truncated`] says when that happened.

use crate::{CapturedOutput, NativeOutputCapture, OwnedCommand};
use std::{
    ffi::OsStr,
    io,
    process::{ChildStdin, ExitStatus, Stdio},
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
/// within it. Past this `run` fails with `TimedOut` rather than leave the writer running.
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
/// non-blocking write end; Windows: `CancelIoEx`) and joined. Every `Ok` return, and every
/// `Err` other than `TimedOut`, leaves no writer thread and no open write end behind. A
/// writer that cannot be stopped within its bound makes the call fail with
/// `ErrorKind::TimedOut`; then the parent has closed the write end on Unix (the thread, if
/// still alive, can deliver nothing and ends on its next step), while on Windows the writer
/// thread may still be alive and blocked in a write, owning the write end, with its cancel flag
/// set (a pending `CancelIoEx` failing to abort a write is not expected).
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
    command
        .command_mut()
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if options.no_window {
        command.windows_hide();
    } else {
        command.windows_visible();
    }
    let mut child = command.spawn()?;
    // Written concurrently with the output drains below; the writer owns stdin and drops it
    // at the end, which is the EOF. Spawn failure returns before any wait is armed. The guard
    // cancels the writer if an early `?` return skips `finish`.
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
    // The tree is down, but a process that escaped it (a grandchild that left the group or the
    // job) may still hold the read end of stdin without reading, so the writer is not
    // guaranteed to end on its own. Wait a bounded time, then cancel it; never join unbounded.
    // BrokenPipe is the child leaving early, not a failure of this run.
    if let Some(writer) = writer {
        writer.finish()?;
    }
    Ok(RunOutput {
        status,
        timed_out,
        stdout,
        stderr,
    })
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
    /// bound is an error, never left running: `run` fails closed instead of returning while
    /// input could still be delivered.
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
        // the same bound. The result is irrelevant; the run is already failing.
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
}
