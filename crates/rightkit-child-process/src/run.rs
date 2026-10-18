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
    time::Duration,
};

/// How long to wait for a pipe to end after the child has exited, before returning what was
/// captured (a grandchild that inherited the pipe can keep it open).
const DRAIN_WAIT: Duration = Duration::from_secs(2);

/// How long the stdin writer gets to end by itself once the child is gone, before it is
/// cancelled. A writer facing a closed pipe ends at once; one still blocked is facing a reader
/// that outlived the tree (a grandchild that kept stdin and does not read it).
const STDIN_WAIT: Duration = Duration::from_millis(500);

/// How long a cancelled writer gets to report before it is detached.
const STDIN_CANCEL_WAIT: Duration = Duration::from_secs(1);

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
/// deadline: once the tree is down the writer gets a short grace, then it is cancelled (Unix)
/// or detached (Windows) and the result is returned.
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

/// The thread that feeds `input` to the child's stdin, with a bounded, cancellable shutdown.
struct StdinWriter {
    cancel: Arc<AtomicBool>,
    done: mpsc::Receiver<io::Result<()>>,
    handle: Option<JoinHandle<()>>,
}

impl StdinWriter {
    fn spawn(stdin: ChildStdin, bytes: Vec<u8>) -> io::Result<Self> {
        let cancel = Arc::new(AtomicBool::new(false));
        let (sender, done) = mpsc::channel();
        let flag = Arc::clone(&cancel);
        let handle = std::thread::Builder::new()
            .name("rightkit-run-stdin".into())
            .spawn(move || {
                let mut stdin = stdin;
                let result = write_input(&mut stdin, &bytes, &flag);
                // Dropping the pipe is the EOF, and releases the descriptor even when the
                // result below is never read.
                drop(stdin);
                let _ = sender.send(result);
            })?;
        Ok(Self {
            cancel,
            done,
            handle: Some(handle),
        })
    }

    /// Waits [`STDIN_WAIT`] for the writer to end by itself, then cancels it and waits
    /// [`STDIN_CANCEL_WAIT`] more. A writer that still has not ended (Windows, where a blocking
    /// write cannot be interrupted from here) is detached: it ends, and closes its pipe, when
    /// the last reader of that pipe goes away.
    fn finish(mut self) -> io::Result<()> {
        let result = match self.done.recv_timeout(STDIN_WAIT) {
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.cancel.store(true, Ordering::Release);
                self.done.recv_timeout(STDIN_CANCEL_WAIT)
            }
            other => other,
        };
        match result {
            Ok(result) => {
                if let Some(handle) = self.handle.take() {
                    let _ = handle.join();
                }
                match result {
                    Err(error) if error.kind() != io::ErrorKind::BrokenPipe => Err(error),
                    _ => Ok(()),
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Detached: dropping the handle leaves the thread running.
                self.handle.take();
                Ok(())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(io::Error::other("the stdin writer thread panicked"))
            }
        }
    }
}

impl Drop for StdinWriter {
    fn drop(&mut self) {
        // Reached without `finish` on an early error return; a cancelled Unix writer ends
        // within one poll interval, so nothing is joined here.
        self.cancel.store(true, Ordering::Release);
    }
}

/// Writes all of `bytes`, or stops early (returning `Ok`) once `cancel` is set. Unix puts the
/// parent's write end in non-blocking mode and polls, so a pipe whose reader never reads cannot
/// pin the thread; the flag is checked at least every [`WRITE_POLL`].
#[cfg(unix)]
fn write_input(stdin: &mut ChildStdin, bytes: &[u8], cancel: &AtomicBool) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let fd = stdin.as_raw_fd();
    // SAFETY: fcntl on a descriptor `stdin` owns for the whole call. O_NONBLOCK is a property of
    // this open file description (the write end only); the child's read end is unaffected.
    let nonblocking = unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        flags >= 0 && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0
    };
    if !nonblocking {
        // Could not switch modes: the blocking write still ends when the pipe breaks, and the
        // caller detaches it if it does not.
        use std::io::Write;
        return stdin.write_all(bytes);
    }
    let mut written = 0;
    while written < bytes.len() {
        if cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        let rest = &bytes[written..];
        let chunk = rest.len().min(1 << 20);
        // SAFETY: `rest` is valid for `chunk` bytes and `fd` is open (see above).
        let count = unsafe { libc::write(fd, rest.as_ptr().cast(), chunk) };
        if count >= 0 {
            written += count as usize;
            continue;
        }
        let error = io::Error::last_os_error();
        match error.kind() {
            io::ErrorKind::Interrupted => {}
            io::ErrorKind::WouldBlock => {
                let mut entry = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: one valid pollfd, as counted. A negative result (EINTR) just loops.
                unsafe { libc::poll(&mut entry, 1, WRITE_POLL.as_millis() as libc::c_int) };
            }
            _ => return Err(error),
        }
    }
    Ok(())
}

/// Windows: the job holds the whole tree (so the tree kill also frees the pipe), and a blocking
/// write cannot be cancelled from another thread without the raw handle, so cancellation here
/// is the detach in [`StdinWriter::finish`].
#[cfg(not(unix))]
fn write_input(stdin: &mut ChildStdin, bytes: &[u8], _cancel: &AtomicBool) -> io::Result<()> {
    use std::io::Write;
    stdin.write_all(bytes)
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
    fn stdin_input_reaches_the_child_and_early_exit_is_not_an_error() {
        let options = RunOptions::new(Duration::from_secs(10));
        let output = run_with_stdin("sh", ["-c", "cat"], b"hello", &options).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.bytes, b"hello".to_vec());
        // The child exits at once without reading: input far larger than a pipe buffer
        // breaks the writer's pipe, which is a normal early exit with the child's status.
        let big = vec![b'x'; 1 << 20];
        let output = run_with_stdin("sh", ["-c", "exit 7"], &big, &options).unwrap();
        assert_eq!(output.status.code(), Some(7));
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
        }
    }

    /// Shell fixture: starts a grandchild in a new session (so neither the group kill nor the
    /// tree kill reaches it) that keeps this shell's stdin open and never reads it, waits for
    /// it to record its pid, then runs `tail`. `$1` is the pid file. Exits 99 when the host has
    /// neither `setsid` nor `perl` to leave the session.
    #[cfg(unix)]
    fn holder_script(tail: &str) -> String {
        format!(
            "if command -v setsid >/dev/null 2>&1; then \
               setsid sh -c 'echo $$ > \"$1\"; exec sleep 30' sh \"$1\" <&0 >/dev/null 2>&1 & \
             elif command -v perl >/dev/null 2>&1; then \
               perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV' sh -c 'echo $$ > \"$1\"; exec sleep 30' sh \"$1\" <&0 >/dev/null 2>&1 & \
             else exit 99; fi; \
             i=0; while [ ! -s \"$1\" ] && [ $i -lt 100 ]; do sleep 0.05; i=$((i+1)); done; {tail}"
        )
    }

    #[cfg(unix)]
    #[test]
    fn a_session_escaped_stdin_holder_does_not_hold_run_past_its_exit() {
        let holder = Grandchild::new("exit");
        let script = holder_script("exit 0");
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
    }

    #[cfg(unix)]
    #[test]
    fn a_session_escaped_stdin_holder_does_not_hold_run_past_its_timeout() {
        let holder = Grandchild::new("timeout");
        // The leader blocks past the deadline, so the tree kill (not an exit) ends it.
        let script = holder_script("sleep 30");
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
