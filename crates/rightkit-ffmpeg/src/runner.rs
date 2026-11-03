//! FFmpeg runner with `-progress` parsing, cancellation, timeout and bounded stderr.
//!
//! Donors: CutRight `cutright-project/src/worker.rs` (progress parser, stderr tail,
//! channel-fed progress loop) and `cutright-timeline/src/ffmpeg.rs`. Reconciled with
//! this crate's `rightkit-child-process` ownership: every FFmpeg runs as an owned child
//! tree, so cancellation/timeout/drop reap descendants (CutRight used bare
//! `Child::kill`).

use std::{
    fmt,
    io::{self, BufRead, BufReader, Read},
    process::{ChildStdin, ExitStatus},
    sync::mpsc::{self, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rightkit_child_process::{OwnedChild, OwnedCommand, ProcessPriority, StderrTail, WaitOutcome};
use std::path::{Path, PathBuf};

use crate::{CancellationToken, FfmpegBinary, FfmpegCommand};

/// Retention cap for the stderr tail. `StderrTail` also bounds what it reads per line (four
/// times this), so a child that never writes a newline cannot grow memory.
const STDERR_TAIL_BYTES: usize = 8 * 1024;
/// Longest stdout line the progress reader inspects. `-progress` lines are a few dozen bytes;
/// anything longer is not progress and is read and dropped, so an unterminated stream cannot
/// grow the buffer.
const PROGRESS_LINE_LIMIT: usize = 64 * 1024;
/// Cap on stderr kept for `run_capture` (loudnorm JSON, `-i` probe text).
const CAPTURE_LIMIT: u64 = 4 << 20;
/// Grace between the polite stop signal and the forced tree kill, unless a caller sets its own.
const DEFAULT_KILL_GRACE: Duration = Duration::from_secs(2);
/// How long the pipe readers get to reach end of stream after a deadline or a cancel has
/// stopped the tree. With the tree dead they finish at once; this only bounds the wait for a
/// holder that escaped the process group, whose reader is then left to end on its own.
const POST_KILL_DRAIN: Duration = Duration::from_secs(2);

/// One `progress=` block from `-progress pipe:1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgressEvent {
    pub frame: u64,
    /// Output position in milliseconds.
    pub out_time_ms: i64,
    /// `progress=end` was reported.
    pub end: bool,
}

/// Accumulates `-progress` key/value lines and yields an event at each `progress=`.
#[derive(Debug, Default)]
pub struct ProgressParser {
    frame: u64,
    out_time_ms: i64,
}

impl ProgressParser {
    pub fn feed(&mut self, line: &str) -> Option<ProgressEvent> {
        let (key, value) = line.trim().split_once('=')?;
        let value = value.trim();
        match key {
            "frame" => self.frame = value.parse().unwrap_or(self.frame),
            // `out_time_ms` is microseconds despite its name; `out_time_us` is exact.
            "out_time_us" | "out_time_ms" => {
                if let Ok(us) = value.parse::<i64>() {
                    self.out_time_ms = us.max(0) / 1000;
                }
            }
            "progress" => {
                return Some(ProgressEvent {
                    frame: self.frame,
                    out_time_ms: self.out_time_ms,
                    end: value == "end",
                })
            }
            _ => {}
        }
        None
    }
}

#[non_exhaustive]
#[derive(Debug)]
pub enum RunError {
    /// FFmpeg could not be started.
    Spawn(io::Error),
    /// Waiting on or reading from the child failed.
    Io(io::Error),
    /// Cancelled before start or while running.
    Cancelled,
    TimedOut(Duration),
    /// The requested working directory is missing or not a directory; nothing was spawned.
    InvalidCurrentDir {
        path: PathBuf,
        reason: &'static str,
    },
    /// Non-zero exit; carries the bounded stderr tail.
    Failed {
        status: ExitStatus,
        stderr_tail: String,
    },
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "ffmpeg could not start: {e}"),
            Self::Io(e) => write!(f, "ffmpeg i/o failure: {e}"),
            Self::Cancelled => f.write_str("ffmpeg run cancelled"),
            Self::InvalidCurrentDir { path, reason } => {
                write!(f, "working directory {} {reason}", path.display())
            }
            Self::TimedOut(d) => write!(f, "ffmpeg timed out after {d:?}"),
            Self::Failed {
                status,
                stderr_tail,
            } => {
                write!(f, "ffmpeg exited with {status}: {stderr_tail}")
            }
        }
    }
}

impl std::error::Error for RunError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunReport {
    pub frames: u64,
    pub out_time_ms: i64,
    pub elapsed_ms: u64,
    pub progress_events: u64,
}

/// Run policy. `on_progress` is invoked on the calling thread.
///
/// `timeout` is a deadline measured from spawn; `None` means no deadline. Cancellation
/// applies in every mode: a cancelled run stops its process tree and returns
/// [`RunError::Cancelled`] whether or not a deadline is set. A deadline too large to
/// represent (`started + timeout` overflows, for example `Duration::MAX`) never fires,
/// so it behaves like `None`.
#[non_exhaustive]
pub struct RunOptions<'a> {
    pub timeout: Option<Duration>,
    /// Grace between the polite stop signal and the forced tree kill.
    pub kill_grace: Duration,
    pub cancel: CancellationToken,
    /// Working directory for the child; `None` inherits the caller's. See
    /// [`RunOptions::with_current_dir`].
    pub current_dir: Option<PathBuf>,
    /// Scheduling priority for the ffmpeg child. See [`RunOptions::with_priority`].
    pub priority: ProcessPriority,
    pub on_progress: Option<&'a mut dyn FnMut(&ProgressEvent)>,
}

/// No deadline, a fresh cancellation token, the default kill grace, and no progress callback.
impl Default for RunOptions<'_> {
    fn default() -> Self {
        Self::unbounded(CancellationToken::new())
    }
}

impl RunOptions<'_> {
    /// A run with a deadline of `timeout` from spawn.
    pub fn new(timeout: Duration, cancel: CancellationToken) -> Self {
        Self::unbounded(cancel).with_deadline(Some(timeout))
    }

    /// A run with no deadline. Use it for multi-hour inputs; cancellation still applies.
    pub fn unbounded(cancel: CancellationToken) -> Self {
        Self {
            timeout: None,
            kill_grace: DEFAULT_KILL_GRACE,
            cancel,
            current_dir: None,
            priority: ProcessPriority::Normal,
            on_progress: None,
        }
    }

    /// Replace the deadline. `None` removes it.
    pub fn with_deadline(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Replace the cancellation token.
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Report each progress block to `on_progress` on the calling thread
    /// ([`run_with_progress`] and [`run_with_stdin`] only).
    pub fn with_progress<'b>(
        self,
        on_progress: &'b mut dyn FnMut(&ProgressEvent),
    ) -> RunOptions<'b> {
        RunOptions {
            timeout: self.timeout,
            kill_grace: self.kill_grace,
            cancel: self.cancel,
            current_dir: self.current_dir,
            priority: self.priority,
            on_progress: Some(on_progress),
        }
    }

    /// Run the child in `dir`, so a relative path in the arguments (a `sendcmd` file, say)
    /// resolves there. The directory must exist, or the run fails with
    /// [`RunError::InvalidCurrentDir`] before anything is spawned. The ffmpeg path is made
    /// absolute against the caller's directory first, so the child can never pick up a
    /// binary from `dir`. Unset, the child inherits the caller's directory.
    pub fn with_current_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(dir.into());
        self
    }

    /// Run the ffmpeg child at a lower scheduling priority (see [`ProcessPriority`]: below
    /// normal, idle, or `Efficient` for EcoQoS on Windows and the utility quality of service
    /// on macOS). The default, `Normal`, leaves the spawn exactly as it was. Applies to the
    /// whole process tree the child starts, because children inherit the priority.
    pub fn with_priority(mut self, priority: ProcessPriority) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_kill_grace(mut self, grace: Duration) -> Self {
        self.kill_grace = grace;
        self
    }
}

/// Reads `reader` line by line, handing each line of at most [`PROGRESS_LINE_LIMIT`] bytes to
/// `each`. A longer line is read to its newline and dropped without being buffered whole.
fn read_lines(reader: impl Read, mut each: impl FnMut(String)) {
    let mut r = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r
            .by_ref()
            .take(PROGRESS_LINE_LIMIT as u64 + 1)
            .read_until(b'\n', &mut buf)
        {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if buf.last() != Some(&b'\n') && buf.len() > PROGRESS_LINE_LIMIT {
            if discard_rest_of_line(&mut r) {
                break;
            }
            continue;
        }
        each(String::from_utf8_lossy(&buf).trim_end().to_string());
    }
}

/// Reads and drops bytes up to and including the next newline. True when the stream ended
/// first.
fn discard_rest_of_line(reader: &mut impl BufRead) -> bool {
    loop {
        let (used, found) = match reader.fill_buf() {
            Ok([]) => return true,
            Ok(buffer) => match buffer.iter().position(|&byte| byte == b'\n') {
                Some(index) => (index + 1, true),
                None => (buffer.len(), false),
            },
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return true,
        };
        reader.consume(used);
        if found {
            return false;
        }
    }
}

/// Passes reads through and tells the run loop once, when the stream ends (EOF or a read
/// error). `StderrTail` owns its drain thread and does not report the end of the stream, so
/// the end is observed on the reader it consumes.
struct EndSignal<R> {
    inner: R,
    done: Option<mpsc::Sender<Event>>,
}

impl<R: Read> Read for EndSignal<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let result = self.inner.read(buf);
        if matches!(result, Ok(0) | Err(_)) && !buf.is_empty() {
            if let Some(done) = self.done.take() {
                let _ = done.send(Event::StderrDone);
            }
        }
        result
    }
}

/// How the run ends the tree once the loop is over, then reaps the leader. After an abort
/// `wait_or_kill` has already done both. Otherwise the leader has exited on its own but is
/// still unreaped, and closed pipes do not mean the tree is gone: a descendant that
/// redirected its output (or kept stdin) is still running. Reaping with a bare `wait` would
/// end the group's killability on Unix and leave it behind; `wait_or_kill` kills the rest of
/// the tree while the leader still pins the group (Unix) or terminates the retained job
/// (Windows), and only then reaps. The caller does this before joining the stdin writer, which
/// a descendant holding the pipe would otherwise block past the deadline.
fn finish_tree(child: &mut OwnedChild, aborted: bool, grace: Duration) -> io::Result<ExitStatus> {
    if aborted {
        return child.wait();
    }
    match child.wait_or_kill(grace)? {
        WaitOutcome::Exited(status) | WaitOutcome::Terminated(status) => Ok(status),
    }
}

enum Abort {
    Cancelled,
    TimedOut,
}

/// How often a blocked wait wakes to look at the cancellation flag, which cannot wake a
/// thread by itself. Exit and deadline wake the wait exactly when they happen, so this only
/// bounds cancel latency; it adds nothing to a run that simply finishes.
const CANCEL_CHECK: Duration = Duration::from_millis(25);

/// Block a helper thread on the child's exit and hand the status to `deliver`. It **reaps**
/// the leader as soon as it exits, which on Unix ends the owner's ability to kill descendants
/// that outlive it. Use [`watch_exited`] where the owner must finish tree cleanup first.
pub(crate) fn watch_exit(
    child: &OwnedChild,
    deliver: impl FnOnce(io::Result<ExitStatus>) + Send + 'static,
) -> io::Result<()> {
    let handle = child.process_handle()?;
    thread::Builder::new()
        .name("rightkit-media-exit".into())
        .spawn(move || deliver(handle.wait()))?;
    Ok(())
}

/// Like [`watch_exit`], but **does not reap**: `deliver` is called once the leader is no longer
/// running (exited and still unreaped, or already reaped by the owner). The status is not
/// delivered; the owner reads it with `wait` after it has drained the pipes and, if a deadline
/// or cancel fired, killed the group. Until then an exited leader stays a zombie that pins the
/// process-group id, so a TERM-resistant descendant that holds the pipes can still be killed
/// with the group. A reaping watcher would end that ability the moment the leader exited.
pub(crate) fn watch_exited(
    child: &OwnedChild,
    deliver: impl FnOnce(io::Result<()>) + Send + 'static,
) -> io::Result<()> {
    let handle = child.process_handle()?;
    thread::Builder::new()
        .name("rightkit-media-exit".into())
        .spawn(move || deliver(handle.wait_exited()))?;
    Ok(())
}

/// How long the next blocking wait may last: until the deadline, but never past the next
/// cancellation check.
fn next_wake(deadline: Option<Instant>) -> Duration {
    deadline.map_or(CANCEL_CHECK, |deadline| {
        deadline
            .saturating_duration_since(Instant::now())
            .min(CANCEL_CHECK)
    })
}

/// Build the owned command for `program`. With a working directory, check it first and pin
/// the program to an absolute path (relative paths against the caller's directory, bare
/// names through the caller's absolute `PATH` entries), so the child cannot resolve the
/// binary inside the new directory.
fn owned_command(program: &Path, current_dir: Option<&Path>) -> Result<OwnedCommand, RunError> {
    let Some(dir) = current_dir else {
        return Ok(OwnedCommand::new(program));
    };
    let invalid = |reason| RunError::InvalidCurrentDir {
        path: dir.to_owned(),
        reason,
    };
    match std::fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(invalid("is not a directory")),
        Err(_) => return Err(invalid("does not exist")),
    }
    let program = if program.components().count() > 1 || program.is_absolute() {
        std::path::absolute(program).map_err(RunError::Spawn)?
    } else {
        find_on_path(program).ok_or_else(|| {
            RunError::Spawn(io::Error::new(
                io::ErrorKind::NotFound,
                "ffmpeg not found on an absolute PATH entry",
            ))
        })?
    };
    let mut command = OwnedCommand::new(program);
    command.command_mut().current_dir(dir);
    Ok(command)
}

fn find_on_path(name: &Path) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    let suffixes: &[&str] = if cfg!(windows) { &["", ".exe"] } else { &[""] };
    std::env::split_paths(&paths)
        .filter(|entry| entry.is_absolute())
        .flat_map(|entry| {
            suffixes.iter().map(move |suffix| {
                let mut file = name.as_os_str().to_owned();
                file.push(suffix);
                entry.join(file)
            })
        })
        .find(|candidate| candidate.is_file())
}

/// What the run loop wakes for.
enum Event {
    /// A progress block from stdout.
    Progress(ProgressEvent),
    /// The leader is no longer running (exited and unreaped, or reaped by the owner).
    Exit(io::Result<()>),
    /// The stdout reader reached end of stream.
    StdoutDone,
    /// The stderr reader reached end of stream.
    StderrDone,
}

/// Where a run stands while it waits for the leader and for both pipes. A run is over when the
/// leader has exited and both streams have ended; until then the deadline and cancel stay live,
/// because a descendant can hold a pipe open long after the leader is gone.
#[derive(Default)]
struct RunState {
    exited: bool,
    stdout_done: bool,
    stderr_done: bool,
}

impl RunState {
    fn complete(&self) -> bool {
        self.exited && self.stdout_done && self.stderr_done
    }

    /// Record a lifecycle event; progress blocks and exit failures are the caller's.
    fn note(&mut self, event: &Event) {
        match event {
            Event::Exit(Ok(())) => self.exited = true,
            Event::StdoutDone => self.stdout_done = true,
            Event::StderrDone => self.stderr_done = true,
            Event::Progress(_) | Event::Exit(Err(_)) => {}
        }
    }
}

/// Join a reader that has finished; a reader still blocked (a holder outside the killed
/// process group) is detached instead, so a stuck pipe cannot hold the caller past its deadline.
fn join_if_done<T>(done: bool, thread: JoinHandle<T>) -> Option<T> {
    if done {
        thread.join().ok()
    } else {
        None
    }
}

/// Run FFmpeg, parsing `-progress pipe:1` from stdout (the command must request it,
/// e.g. via [`FfmpegCommand::progress_pipe`]). Stderr is kept as a bounded tail.
pub fn run_with_progress(
    ffmpeg: &dyn FfmpegBinary,
    cmd: &FfmpegCommand,
    opts: RunOptions<'_>,
) -> Result<RunReport, RunError> {
    run_inner::<NoStdinFeed>(ffmpeg, cmd, opts, None)
}

/// Like [`run_with_progress`], but the child's stdin is a pipe that `feed` writes on its
/// own scoped thread. `feed` receives the [`ChildStdin`] by value; returning drops it,
/// which sends EOF to FFmpeg. `cmd` must come from [`FfmpegCommand::stdin_piped`]`(true)`.
///
/// Cancellation and timeout behave exactly as in [`run_with_progress`]: the child is
/// stopped, which closes the pipe and fails any blocked write, so `feed` returns. A
/// write that fails with `BrokenPipe` after FFmpeg exits successfully (it stopped reading
/// early) is not an error; any other write failure is [`RunError::Io`]. `feed` must not
/// block on something other than the pipe, because the runner joins it before returning.
pub fn run_with_stdin<F>(
    ffmpeg: &dyn FfmpegBinary,
    cmd: &FfmpegCommand,
    opts: RunOptions<'_>,
    feed: F,
) -> Result<RunReport, RunError>
where
    F: FnOnce(ChildStdin) -> io::Result<()> + Send,
{
    if !cmd.stdin_is_piped() {
        return Err(RunError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "stdin feed requires FfmpegCommand::stdin_piped(true)",
        )));
    }
    run_inner(ffmpeg, cmd, opts, Some(feed))
}

type NoStdinFeed = fn(ChildStdin) -> io::Result<()>;

fn run_inner<F>(
    ffmpeg: &dyn FfmpegBinary,
    cmd: &FfmpegCommand,
    mut opts: RunOptions<'_>,
    feed: Option<F>,
) -> Result<RunReport, RunError>
where
    F: FnOnce(ChildStdin) -> io::Result<()> + Send,
{
    if opts.cancel.is_cancelled() {
        return Err(RunError::Cancelled);
    }
    let mut command = owned_command(ffmpeg.ffmpeg_path(), opts.current_dir.as_deref())?;
    command.process_priority(opts.priority);
    command
        .command_mut()
        .args(cmd.to_os_args())
        .stdin(if feed.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command.windows_hide();
    let started = Instant::now();
    let mut child = command.spawn().map_err(RunError::Spawn)?;
    let feed = match feed {
        None => None,
        Some(feed) => match child.take_stdin() {
            Some(stdin) => Some((feed, stdin)),
            None => {
                let _ = child.terminate_tree();
                return Err(RunError::Io(io::Error::other("ffmpeg stdin was not piped")));
            }
        },
    };

    let (tx, rx) = mpsc::channel::<Event>();
    let exit_tx = tx.clone();
    if let Err(e) = watch_exited(&child, move |result| {
        let _ = exit_tx.send(Event::Exit(result));
    }) {
        let _ = child.terminate_tree();
        return Err(RunError::Io(e));
    }
    let stdout = child.take_stdout().expect("stdout piped");
    let stderr_tx = tx.clone();
    let out_thread = thread::spawn(move || {
        let mut parser = ProgressParser::default();
        read_lines(stdout, |line| {
            if let Some(ev) = parser.feed(&line) {
                let _ = tx.send(Event::Progress(ev));
            }
        });
        let _ = tx.send(Event::StdoutDone);
    });
    let stderr = child.take_stderr().expect("stderr piped");
    // Bounded on both axes: whole lines past four times the cap are discarded unbuffered, and
    // retention never exceeds the cap (a single longer line is cut to its tail).
    let mut tail = match StderrTail::try_capture(
        EndSignal {
            inner: stderr,
            done: Some(stderr_tx),
        },
        STDERR_TAIL_BYTES,
        |line| Some(line.trim_end().to_owned()),
        |_| {},
    ) {
        Ok(tail) => tail,
        Err(e) => {
            let _ = child.terminate_tree();
            return Err(RunError::Io(e));
        }
    };

    // The writer runs on a scoped thread so a blocked write can never stall the
    // cancel/timeout loop below; stopping the child is what unblocks it.
    thread::scope(|scope| -> Result<RunReport, RunError> {
        let writer = feed.map(|(feed, stdin)| scope.spawn(move || feed(stdin)));
        // Checked: `started + timeout` can overflow for a huge timeout, which then never fires.
        let deadline = opts
            .timeout
            .and_then(|timeout| started.checked_add(timeout));
        let mut last = ProgressEvent {
            frame: 0,
            out_time_ms: 0,
            end: false,
        };
        let mut events = 0u64;
        let mut abort: Option<Abort> = None;
        let mut run = RunState::default();
        // Set once an abort has stopped the tree: the readers get until then to end.
        let mut drain_until: Option<Instant> = None;
        // The run is over when the leader has exited and both streams have ended. The leader
        // is only observed to exit, never reaped, until then: a descendant that holds a pipe
        // (and ignores TERM) is killed with the group, which only the unreaped leader pins.
        // The deadline and cancel stay live the whole time, including while the pipes drain.
        loop {
            if abort.is_none() {
                if opts.cancel.is_cancelled() {
                    abort = Some(Abort::Cancelled);
                } else if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    abort = Some(Abort::TimedOut);
                }
                if abort.is_some() {
                    // TERM to the group, the grace, then KILL to the group while the leader is
                    // still unreaped (an exited leader is not reaped first), then the reap.
                    if let Err(e) = child.wait_or_kill(opts.kill_grace) {
                        let _ = child.terminate_tree();
                        // The deadline or the cancel caused this stop, so that is what the
                        // caller gets, whatever the kill reported (the child may already be
                        // gone, or reaped while the kill ran).
                        return Err(match &abort {
                            Some(Abort::Cancelled) => RunError::Cancelled,
                            Some(Abort::TimedOut) => {
                                RunError::TimedOut(opts.timeout.unwrap_or_default())
                            }
                            None => RunError::Io(e),
                        });
                    }
                    run.exited = true;
                    drain_until = Instant::now().checked_add(POST_KILL_DRAIN);
                }
            }
            if run.complete() || drain_until.is_some_and(|at| Instant::now() >= at) {
                break;
            }
            let mut wake = next_wake(deadline.filter(|_| abort.is_none()));
            if let Some(at) = drain_until {
                wake = wake.min(at.saturating_duration_since(Instant::now()));
            }
            match rx.recv_timeout(wake) {
                Ok(Event::Progress(ev)) => {
                    events += 1;
                    if let Some(cb) = opts.on_progress.as_deref_mut() {
                        cb(&ev);
                    }
                    last = ev;
                }
                Ok(Event::Exit(Err(e))) => {
                    let _ = child.terminate_tree();
                    return Err(RunError::Io(e));
                }
                Ok(event) => run.note(&event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = child.terminate_tree();
                    return Err(RunError::Io(io::Error::other("child event channel closed")));
                }
            }
        }
        // Reap. When the run ended on its own the leader is still an unreaped zombie, kept so
        // the tree stayed killable until now; after an abort `wait_or_kill` already did it.
        let status = match finish_tree(&mut child, abort.is_some(), opts.kill_grace) {
            Ok(status) => status,
            Err(e) => {
                let _ = child.terminate_tree();
                return Err(RunError::Io(e));
            }
        };
        let _ = join_if_done(run.stdout_done, out_thread);
        // The tree is down, so the stderr drain ends at once unless an escaped holder keeps
        // it open; then it is left to end on its own and what it kept so far is used.
        let stderr_tail = if run.stderr_done {
            tail.finish()
        } else {
            tail.snapshot()
        };
        let writer_result = match writer {
            Some(writer) => writer
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("stdin writer panicked"))),
            None => Ok(()),
        };
        while let Ok(event) = rx.try_recv() {
            if let Event::Progress(ev) = event {
                events += 1;
                if let Some(cb) = opts.on_progress.as_deref_mut() {
                    cb(&ev);
                }
                last = ev;
            }
        }
        match abort {
            Some(Abort::Cancelled) => return Err(RunError::Cancelled),
            Some(Abort::TimedOut) => {
                return Err(RunError::TimedOut(opts.timeout.unwrap_or_default()))
            }
            None => {}
        }
        if !status.success() {
            return Err(RunError::Failed {
                status,
                stderr_tail,
            });
        }
        if let Err(e) = writer_result {
            if e.kind() != io::ErrorKind::BrokenPipe {
                return Err(RunError::Io(e));
            }
        }
        Ok(RunReport {
            frames: last.frame,
            out_time_ms: last.out_time_ms,
            elapsed_ms: started.elapsed().as_millis() as u64,
            progress_events: events,
        })
    })
}

/// Output of [`run_capture`].
#[derive(Debug)]
pub struct Captured {
    pub stdout: Vec<u8>,
    pub stderr: String,
    /// Exit status. FFmpeg exits non-zero for `-i`-only probes, so the caller decides.
    pub status: ExitStatus,
}

/// Run FFmpeg and capture stdout (binary) and stderr (text, capped). Honors cancel and
/// `timeout`; does NOT treat a non-zero exit as an error (probes rely on that). A stopped
/// run gets a 2 s grace; use [`run_capture_with`] to choose the grace or drop the deadline.
pub fn run_capture(
    ffmpeg: &dyn FfmpegBinary,
    cmd: &FfmpegCommand,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Captured, RunError> {
    run_capture_inner(
        ffmpeg,
        cmd,
        Some(timeout),
        DEFAULT_KILL_GRACE,
        cancel,
        None,
        ProcessPriority::Normal,
    )
}

/// [`run_capture`] with the deadline, kill grace, and cancellation taken from `opts`.
/// `opts.timeout` of `None` means no deadline (cancellation still applies).
/// `opts.on_progress` is ignored: capture runs do not parse progress.
pub fn run_capture_with(
    ffmpeg: &dyn FfmpegBinary,
    cmd: &FfmpegCommand,
    opts: RunOptions<'_>,
) -> Result<Captured, RunError> {
    run_capture_inner(
        ffmpeg,
        cmd,
        opts.timeout,
        opts.kill_grace,
        &opts.cancel,
        opts.current_dir.as_deref(),
        opts.priority,
    )
}

fn run_capture_inner(
    ffmpeg: &dyn FfmpegBinary,
    cmd: &FfmpegCommand,
    timeout: Option<Duration>,
    kill_grace: Duration,
    cancel: &CancellationToken,
    current_dir: Option<&Path>,
    priority: ProcessPriority,
) -> Result<Captured, RunError> {
    if cancel.is_cancelled() {
        return Err(RunError::Cancelled);
    }
    let mut command = owned_command(ffmpeg.ffmpeg_path(), current_dir)?;
    command.process_priority(priority);
    command
        .command_mut()
        .args(cmd.to_os_args())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command.windows_hide();
    let mut child = command.spawn().map_err(RunError::Spawn)?;
    let mut stdout = child.take_stdout().expect("stdout piped");
    let mut stderr = child.take_stderr().expect("stderr piped");
    let (tx, rx) = mpsc::channel::<Event>();
    let out_tx = tx.clone();
    let out_t = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stdout.read_to_end(&mut b);
        let _ = out_tx.send(Event::StdoutDone);
        b
    });
    let err_tx = tx.clone();
    let err_t = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = (&mut stderr).take(CAPTURE_LIMIT).read_to_end(&mut b);
        // Keep draining so FFmpeg never blocks on a full pipe.
        let _ = io::copy(&mut stderr, &mut io::sink());
        let _ = err_tx.send(Event::StderrDone);
        b
    });
    // `None` when there is no deadline or when `now + timeout` overflows: such a deadline
    // never fires.
    let deadline = timeout.and_then(|timeout| Instant::now().checked_add(timeout));
    if let Err(e) = watch_exited(&child, move |result| {
        let _ = tx.send(Event::Exit(result));
    }) {
        let _ = child.terminate_tree();
        return Err(RunError::Io(e));
    }
    let mut abort = None;
    let mut run = RunState::default();
    // Set once an abort has stopped the tree: the readers get until then to end.
    let mut drain_until: Option<Instant> = None;
    // Same shape as `run_inner`: the leader is observed, not reaped, until both streams have
    // ended, so a TERM-resistant descendant holding a pipe is killed with the group, and the
    // deadline and cancel stay live while the pipes drain.
    loop {
        if abort.is_none() {
            if cancel.is_cancelled() {
                abort = Some(Abort::Cancelled);
            } else if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                abort = Some(Abort::TimedOut);
            }
            if abort.is_some() {
                if let Err(e) = child.wait_or_kill(kill_grace) {
                    let _ = child.terminate_tree();
                    return Err(match &abort {
                        Some(Abort::Cancelled) => RunError::Cancelled,
                        Some(Abort::TimedOut) => RunError::TimedOut(timeout.unwrap_or_default()),
                        None => RunError::Io(e),
                    });
                }
                run.exited = true;
                drain_until = Instant::now().checked_add(POST_KILL_DRAIN);
            }
        }
        if run.complete() || drain_until.is_some_and(|at| Instant::now() >= at) {
            break;
        }
        let mut wake = next_wake(deadline.filter(|_| abort.is_none()));
        if let Some(at) = drain_until {
            wake = wake.min(at.saturating_duration_since(Instant::now()));
        }
        match rx.recv_timeout(wake) {
            Ok(Event::Exit(Err(e))) => {
                let _ = child.terminate_tree();
                return Err(RunError::Io(e));
            }
            Ok(event) => run.note(&event),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                let _ = child.terminate_tree();
                return Err(RunError::Io(io::Error::other("child event channel closed")));
            }
        }
    }
    let status = match finish_tree(&mut child, abort.is_some(), kill_grace) {
        Ok(status) => status,
        Err(e) => {
            let _ = child.terminate_tree();
            return Err(RunError::Io(e));
        }
    };
    let stdout = join_if_done(run.stdout_done, out_t).unwrap_or_default();
    let stderr = String::from_utf8_lossy(&join_if_done(run.stderr_done, err_t).unwrap_or_default())
        .into_owned();
    match abort {
        Some(Abort::Cancelled) => Err(RunError::Cancelled),
        Some(Abort::TimedOut) => Err(RunError::TimedOut(timeout.unwrap_or_default())),
        None => Ok(Captured {
            stdout,
            stderr,
            status,
        }),
    }
}

/// Like [`run_capture`] but a non-zero exit is [`RunError::Failed`] with a stderr tail.
pub fn run_checked(
    ffmpeg: &dyn FfmpegBinary,
    cmd: &FfmpegCommand,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Captured, RunError> {
    checked_status(run_capture(ffmpeg, cmd, timeout, cancel)?)
}

/// [`run_checked`] with options, as [`run_capture_with`].
pub fn run_checked_with(
    ffmpeg: &dyn FfmpegBinary,
    cmd: &FfmpegCommand,
    opts: RunOptions<'_>,
) -> Result<Captured, RunError> {
    checked_status(run_capture_with(ffmpeg, cmd, opts)?)
}

fn checked_status(c: Captured) -> Result<Captured, RunError> {
    if c.status.success() {
        Ok(c)
    } else {
        let tail: Vec<&str> = c.stderr.lines().rev().take(8).collect();
        Err(RunError::Failed {
            status: c.status,
            stderr_tail: tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
        })
    }
}

/// What a streaming child does with one standard stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StdioMode {
    /// Connected to the null device. The default, and what every other runner uses.
    #[default]
    Null,
    /// A pipe that the caller reads or writes through the returned [`OwnedChild`].
    Piped,
    /// Shares the caller's handle.
    Inherit,
}

/// Stdio for [`spawn_streaming`]. Every stream defaults to [`StdioMode::Null`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StdioPolicy {
    pub stdin: StdioMode,
    pub stdout: StdioMode,
    pub stderr: StdioMode,
}

impl StdioMode {
    fn stdio(self) -> std::process::Stdio {
        match self {
            Self::Null => std::process::Stdio::null(),
            Self::Piped => std::process::Stdio::piped(),
            Self::Inherit => std::process::Stdio::inherit(),
        }
    }
}

/// Spawn FFmpeg or ffprobe as an owned child with caller-chosen stdio, for long-lived
/// readers and writers that the run loops above do not fit. Works for either binary,
/// since both implement [`FfmpegBinary`]. The child is owned: dropping the returned
/// [`OwnedChild`] stops its whole process tree. Waiting, deadlines, and cancellation are
/// the caller's job (see [`OwnedChild::wait_or_kill`]). `StdioMode::Piped` for stdin needs
/// `cmd` from [`FfmpegCommand::stdin_piped`]`(true)`, as in [`run_with_stdin`].
pub fn spawn_streaming(
    ffmpeg: &dyn FfmpegBinary,
    cmd: &FfmpegCommand,
    policy: StdioPolicy,
) -> Result<OwnedChild, RunError> {
    spawn_streaming_in(ffmpeg, cmd, policy, None)
}

/// [`spawn_streaming`] with a working directory for the child; the rules of
/// [`RunOptions::with_current_dir`] apply (checked before spawn, program pinned absolute).
pub fn spawn_streaming_in(
    ffmpeg: &dyn FfmpegBinary,
    cmd: &FfmpegCommand,
    policy: StdioPolicy,
    current_dir: Option<&Path>,
) -> Result<OwnedChild, RunError> {
    spawn_streaming_with_priority(ffmpeg, cmd, policy, current_dir, ProcessPriority::Normal)
}

/// [`spawn_streaming_in`] with a scheduling priority for the child (see
/// [`RunOptions::with_priority`]). `ProcessPriority::Normal` is identical to
/// [`spawn_streaming_in`].
pub fn spawn_streaming_with_priority(
    ffmpeg: &dyn FfmpegBinary,
    cmd: &FfmpegCommand,
    policy: StdioPolicy,
    current_dir: Option<&Path>,
    priority: ProcessPriority,
) -> Result<OwnedChild, RunError> {
    if policy.stdin == StdioMode::Piped && !cmd.stdin_is_piped() {
        return Err(RunError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "stdin piping requires FfmpegCommand::stdin_piped(true)",
        )));
    }
    let mut command = owned_command(ffmpeg.ffmpeg_path(), current_dir)?;
    command.process_priority(priority);
    command
        .command_mut()
        .args(cmd.to_os_args())
        .stdin(policy.stdin.stdio())
        .stdout(policy.stdout.stdio())
        .stderr(policy.stderr.stdio());
    command.windows_hide();
    command.spawn().map_err(RunError::Spawn)
}
