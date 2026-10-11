//! FFmpeg runner with `-progress` parsing, cancellation, timeout and bounded stderr.
//!
//! Donors: CutRight `cutright-project/src/worker.rs` (progress parser, stderr tail,
//! channel-fed progress loop) and `cutright-timeline/src/ffmpeg.rs`. Reconciled with
//! this crate's `rightkit-process` ownership: every FFmpeg runs as an owned child
//! tree, so cancellation/timeout/drop reap descendants (CutRight used bare
//! `Child::kill`).

use std::{
    collections::VecDeque,
    fmt,
    io::{self, BufRead, BufReader, Read},
    process::{ChildStdin, ExitStatus},
    sync::{
        mpsc::{self, RecvTimeoutError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use rightkit_process::{OwnedChild, OwnedCommand};
use std::path::{Path, PathBuf};

use crate::{CancellationToken, FfmpegBinary, FfmpegCommand};

const STDERR_TAIL_LINES: usize = 40;
const STDERR_TAIL_BYTES: usize = 8 * 1024;
/// Cap on stderr kept for `run_capture` (loudnorm JSON, `-i` probe text).
const CAPTURE_LIMIT: u64 = 4 << 20;
/// Grace between the polite stop signal and the forced tree kill, unless a caller sets its own.
const DEFAULT_KILL_GRACE: Duration = Duration::from_secs(2);

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

    pub fn with_kill_grace(mut self, grace: Duration) -> Self {
        self.kill_grace = grace;
        self
    }
}

#[derive(Default)]
struct Tail {
    lines: VecDeque<String>,
    bytes: usize,
}

impl Tail {
    fn push(&mut self, line: String) {
        self.bytes += line.len();
        self.lines.push_back(line);
        while self.lines.len() > STDERR_TAIL_LINES
            || (self.bytes > STDERR_TAIL_BYTES && self.lines.len() > 1)
        {
            if let Some(old) = self.lines.pop_front() {
                self.bytes -= old.len();
            }
        }
    }

    fn text(&self) -> String {
        self.lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

fn read_lines(reader: impl Read, mut each: impl FnMut(String)) {
    let mut r = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => each(String::from_utf8_lossy(&buf).trim_end().to_string()),
        }
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

/// Block a helper thread on the child's exit and hand the status to `deliver`. The wait is
/// the OS's own (`waitid` with `WNOWAIT` on Unix, `WaitForSingleObject` on Windows), so the
/// caller learns of an exit the moment it happens instead of on the next poll tick. The
/// thread ends when the child does, including when the caller kills it.
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

/// What the run loop wakes for: a progress block from stdout, or the child's exit.
enum Event {
    Progress(ProgressEvent),
    Exit(io::Result<ExitStatus>),
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
    if let Err(e) = watch_exit(&child, move |status| {
        let _ = exit_tx.send(Event::Exit(status));
    }) {
        let _ = child.terminate_tree();
        return Err(RunError::Io(e));
    }
    let stdout = child.take_stdout().expect("stdout piped");
    let out_thread = thread::spawn(move || {
        let mut parser = ProgressParser::default();
        read_lines(stdout, |line| {
            if let Some(ev) = parser.feed(&line) {
                let _ = tx.send(Event::Progress(ev));
            }
        });
    });
    let tail = Arc::new(Mutex::new(Tail::default()));
    let stderr = child.take_stderr().expect("stderr piped");
    let err_tail = Arc::clone(&tail);
    let err_thread = thread::spawn(move || {
        read_lines(stderr, |line| {
            if let Ok(mut t) = err_tail.lock() {
                t.push(line);
            }
        });
    });

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
        let status = loop {
            if abort.is_none() {
                if opts.cancel.is_cancelled() {
                    abort = Some(Abort::Cancelled);
                } else if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    abort = Some(Abort::TimedOut);
                }
                if abort.is_some() {
                    if let Err(e) = child.wait_or_kill(opts.kill_grace) {
                        let _ = child.terminate_tree();
                        // The deadline or the cancel caused this stop, so that is what the
                        // caller gets, whatever the kill reported (the child may already be
                        // gone, or reaped by the exit watcher while the kill ran).
                        return Err(match &abort {
                            Some(Abort::Cancelled) => RunError::Cancelled,
                            Some(Abort::TimedOut) => {
                                RunError::TimedOut(opts.timeout.unwrap_or_default())
                            }
                            None => RunError::Io(e),
                        });
                    }
                }
            }
            match rx.recv_timeout(next_wake(deadline.filter(|_| abort.is_none()))) {
                Ok(Event::Progress(ev)) => {
                    events += 1;
                    if let Some(cb) = opts.on_progress.as_deref_mut() {
                        cb(&ev);
                    }
                    last = ev;
                }
                Ok(Event::Exit(Ok(status))) => break status,
                Ok(Event::Exit(Err(e))) => {
                    let _ = child.terminate_tree();
                    return Err(RunError::Io(e));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = child.terminate_tree();
                    return Err(RunError::Io(io::Error::other("exit watcher stopped")));
                }
            }
        };
        let _ = out_thread.join();
        let _ = err_thread.join();
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
            let stderr_tail = tail.lock().map(|t| t.text()).unwrap_or_default();
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
    run_capture_inner(ffmpeg, cmd, Some(timeout), DEFAULT_KILL_GRACE, cancel, None)
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
    )
}

fn run_capture_inner(
    ffmpeg: &dyn FfmpegBinary,
    cmd: &FfmpegCommand,
    timeout: Option<Duration>,
    kill_grace: Duration,
    cancel: &CancellationToken,
    current_dir: Option<&Path>,
) -> Result<Captured, RunError> {
    if cancel.is_cancelled() {
        return Err(RunError::Cancelled);
    }
    let mut command = owned_command(ffmpeg.ffmpeg_path(), current_dir)?;
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
    let out_t = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stdout.read_to_end(&mut b);
        b
    });
    let err_t = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = (&mut stderr).take(CAPTURE_LIMIT).read_to_end(&mut b);
        // Keep draining so FFmpeg never blocks on a full pipe.
        let _ = io::copy(&mut stderr, &mut io::sink());
        b
    });
    // `None` when there is no deadline or when `now + timeout` overflows: such a deadline
    // never fires.
    let deadline = timeout.and_then(|timeout| Instant::now().checked_add(timeout));
    let (exit_tx, exit_rx) = mpsc::channel::<io::Result<ExitStatus>>();
    if let Err(e) = watch_exit(&child, move |status| {
        let _ = exit_tx.send(status);
    }) {
        let _ = child.terminate_tree();
        return Err(RunError::Io(e));
    }
    let mut abort = None;
    let status = loop {
        if abort.is_none() {
            if cancel.is_cancelled() {
                abort = Some(Abort::Cancelled);
            } else if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                abort = Some(Abort::TimedOut);
            }
            if abort.is_some() {
                let _ = child.wait_or_kill(kill_grace);
            }
        }
        match exit_rx.recv_timeout(next_wake(deadline.filter(|_| abort.is_none()))) {
            Ok(Ok(status)) => break status,
            Ok(Err(e)) => {
                let _ = child.terminate_tree();
                return Err(RunError::Io(e));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                let _ = child.terminate_tree();
                return Err(RunError::Io(io::Error::other("exit watcher stopped")));
            }
        }
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = String::from_utf8_lossy(&err_t.join().unwrap_or_default()).into_owned();
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
    if policy.stdin == StdioMode::Piped && !cmd.stdin_is_piped() {
        return Err(RunError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "stdin piping requires FfmpegCommand::stdin_piped(true)",
        )));
    }
    let mut command = owned_command(ffmpeg.ffmpeg_path(), current_dir)?;
    command
        .command_mut()
        .args(cmd.to_os_args())
        .stdin(policy.stdin.stdio())
        .stdout(policy.stdout.stdio())
        .stderr(policy.stderr.stdio());
    command.windows_hide();
    command.spawn().map_err(RunError::Spawn)
}

#[cfg(all(test, unix))]
mod tests {
    use std::{io::Write, path::PathBuf};

    use super::*;
    use crate::{DevFfmpeg, FfmpegArg};

    /// A POSIX shell stand-in for FFmpeg: `body` runs with the arguments in `$@`.
    fn fake_ffmpeg(dir: &std::path::Path, body: &str) -> DevFfmpeg {
        use std::os::unix::fs::PermissionsExt;
        let path: PathBuf = dir.join("fake-ffmpeg");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        DevFfmpeg::unverified(path)
    }

    fn policy_opts(timeout: Duration, cancel: &CancellationToken) -> RunOptions<'static> {
        RunOptions::new(timeout, cancel.clone())
    }

    /// The deadline fires with zero kill grace while the child is exiting on its own and the
    /// exit watcher is reaping it. The caller must see `TimedOut` (or the child's own result
    /// when it finished first), never `RunError::Io` from the kill racing the exit.
    #[test]
    fn a_deadline_with_zero_grace_is_a_timeout_even_when_the_child_exits_during_the_kill() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_ffmpeg(dir.path(), "exec sleep 0.05");
        let token = CancellationToken::new();
        for round in 0..200u64 {
            let opts = RunOptions::new(Duration::from_millis(30 + round % 40), token.clone())
                .with_kill_grace(Duration::ZERO);
            match run_with_progress(&bin, &FfmpegCommand::base("error"), opts) {
                Ok(_) | Err(RunError::TimedOut(_)) => {}
                other => panic!("round {round}: expected TimedOut or a finished run, got {other:?}"),
            }
        }
    }

    #[test]
    fn stdin_feed_reaches_the_child_and_removes_nostdin() {
        let dir = tempfile::tempdir().unwrap();
        // Exits 3 if -nostdin survived, otherwise reports how many bytes it read.
        let bin = fake_ffmpeg(
            dir.path(),
            r#"case " $* " in *" -nostdin "*) exit 3;; esac
n=$(cat | wc -c)
printf 'frame=%s\nout_time_us=2000000\nprogress=end\n' "$n""#,
        );
        let mut cmd = FfmpegCommand::base("error");
        cmd.stdin_piped(true).texts(["-i", "pipe:0"]);
        assert!(!cmd
            .args()
            .iter()
            .any(|a| matches!(a, FfmpegArg::Text(t) if t == "-nostdin")));
        let token = CancellationToken::new();
        let report = run_with_stdin(
            &bin,
            &cmd,
            policy_opts(Duration::from_secs(20), &token),
            |mut stdin| stdin.write_all(b"hello"),
        )
        .unwrap();
        assert_eq!(report.frames, 5);
        assert_eq!(report.out_time_ms, 2000);
        assert_eq!(report.progress_events, 1);
    }

    #[test]
    fn plain_commands_keep_nostdin_and_feeds_need_a_piped_command() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_ffmpeg(
            dir.path(),
            r#"case " $* " in *" -nostdin "*) exit 3;; esac
exit 0"#,
        );
        let token = CancellationToken::new();
        let plain = FfmpegCommand::base("error");
        assert!(matches!(
            run_with_progress(&bin, &plain, policy_opts(Duration::from_secs(20), &token)),
            Err(RunError::Failed { .. })
        ));
        let result = run_with_stdin(
            &bin,
            &plain,
            policy_opts(Duration::from_secs(20), &token),
            |_| Ok(()),
        );
        assert!(matches!(
            result,
            Err(RunError::Io(e)) if e.kind() == io::ErrorKind::InvalidInput
        ));
    }

    #[test]
    fn a_feed_error_other_than_broken_pipe_fails_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_ffmpeg(dir.path(), "cat >/dev/null");
        let mut cmd = FfmpegCommand::base("error");
        cmd.stdin_piped(true).texts(["-i", "pipe:0"]);
        let token = CancellationToken::new();
        let result = run_with_stdin(
            &bin,
            &cmd,
            policy_opts(Duration::from_secs(20), &token),
            |_| Err(io::Error::other("source vanished")),
        );
        assert!(matches!(result, Err(RunError::Io(_))));
    }

    #[test]
    fn a_child_that_stops_reading_early_after_success_is_not_a_feed_error() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_ffmpeg(dir.path(), "exit 0");
        let mut cmd = FfmpegCommand::base("error");
        cmd.stdin_piped(true).texts(["-i", "pipe:0"]);
        let token = CancellationToken::new();
        let result = run_with_stdin(
            &bin,
            &cmd,
            policy_opts(Duration::from_secs(20), &token),
            |mut stdin| {
                // Far more than a pipe buffer, so the write must hit the closed pipe.
                let block = vec![0u8; 1 << 20];
                for _ in 0..8 {
                    stdin.write_all(&block)?;
                }
                Ok(())
            },
        );
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn cancelling_a_blocked_feed_stops_the_child_and_returns() {
        let dir = tempfile::tempdir().unwrap();
        // Never reads stdin, so a large write blocks until the child is stopped.
        let bin = fake_ffmpeg(dir.path(), "exec sleep 30");
        let mut cmd = FfmpegCommand::base("error");
        cmd.stdin_piped(true).texts(["-i", "pipe:0"]);
        let token = CancellationToken::new();
        let canceller = token.clone();
        let started = Instant::now();
        let trigger = thread::spawn(move || {
            thread::sleep(Duration::from_millis(200));
            canceller.cancel();
        });
        let result = run_with_stdin(
            &bin,
            &cmd,
            policy_opts(Duration::from_secs(60), &token),
            |mut stdin| {
                let block = vec![0u8; 16 << 20];
                stdin.write_all(&block)
            },
        );
        trigger.join().unwrap();
        assert!(matches!(result, Err(RunError::Cancelled)), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn unbounded_runs_finish_without_a_deadline_and_still_honour_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let quick = fake_ffmpeg(dir.path(), "printf 'progress=end\\n'");
        let cmd = FfmpegCommand::base("error");
        let report = run_with_progress(
            &quick,
            &cmd,
            RunOptions::unbounded(CancellationToken::new()),
        )
        .unwrap();
        assert_eq!(report.progress_events, 1);

        let dir2 = tempfile::tempdir().unwrap();
        let slow = fake_ffmpeg(dir2.path(), "exec sleep 30");
        let token = CancellationToken::new();
        let canceller = token.clone();
        let trigger = thread::spawn(move || {
            thread::sleep(Duration::from_millis(200));
            canceller.cancel();
        });
        let started = Instant::now();
        let result = run_with_progress(&slow, &cmd, RunOptions::unbounded(token));
        trigger.join().unwrap();
        assert!(matches!(result, Err(RunError::Cancelled)), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn a_deadline_too_large_to_represent_never_fires() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_ffmpeg(dir.path(), "printf 'progress=end\\n'");
        let cmd = FfmpegCommand::base("error");
        let report = run_with_progress(
            &bin,
            &cmd,
            RunOptions::new(Duration::MAX, CancellationToken::new()),
        )
        .unwrap();
        assert_eq!(report.progress_events, 1);
        let captured = run_capture_with(
            &bin,
            &cmd,
            RunOptions::unbounded(CancellationToken::new()).with_deadline(Some(Duration::MAX)),
        )
        .unwrap();
        assert!(captured.status.success());
    }

    #[test]
    fn spawn_streaming_gives_the_caller_both_pipes_and_stops_the_child_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_ffmpeg(dir.path(), "cat");
        let mut cmd = FfmpegCommand::base("error");
        cmd.stdin_piped(true).texts(["-i", "pipe:0"]);
        let mut child = spawn_streaming(
            &bin,
            &cmd,
            StdioPolicy {
                stdin: StdioMode::Piped,
                stdout: StdioMode::Piped,
                stderr: StdioMode::Null,
            },
        )
        .unwrap();
        let mut stdin = child.take_stdin().unwrap();
        let mut stdout = child.take_stdout().unwrap();
        stdin.write_all(b"streamed").unwrap();
        drop(stdin);
        let mut echoed = Vec::new();
        stdout.read_to_end(&mut echoed).unwrap();
        assert_eq!(echoed, b"streamed");
        assert!(child.wait().unwrap().success());

        let dir2 = tempfile::tempdir().unwrap();
        let slow = fake_ffmpeg(dir2.path(), "exec sleep 30");
        let child =
            spawn_streaming(&slow, &FfmpegCommand::base("error"), StdioPolicy::default()).unwrap();
        let started = Instant::now();
        drop(child);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn spawn_streaming_refuses_stdin_piping_without_a_piped_command() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_ffmpeg(dir.path(), "exit 0");
        let result = spawn_streaming(
            &bin,
            &FfmpegCommand::base("error"),
            StdioPolicy {
                stdin: StdioMode::Piped,
                ..StdioPolicy::default()
            },
        );
        assert!(matches!(result, Err(RunError::Io(e)) if e.kind() == io::ErrorKind::InvalidInput));
    }

    fn true_binary() -> DevFfmpeg {
        let path = ["/usr/bin/true", "/bin/true"]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
            .expect("a `true` executable");
        DevFfmpeg::unverified(path)
    }

    /// Mean wall time of `run`, in microseconds, over `runs` calls.
    fn mean_micros(runs: u32, mut run: impl FnMut()) -> f64 {
        let started = Instant::now();
        for _ in 0..runs {
            run();
        }
        started.elapsed().as_secs_f64() * 1e6 / f64::from(runs)
    }

    #[test]
    fn a_trivial_child_adds_well_under_five_milliseconds_over_a_blocking_output_call() {
        const RUNS: u32 = 50;
        let bin = true_binary();
        let cmd = FfmpegCommand::base("error");
        let token = CancellationToken::new();
        let blocking = |_: ()| {
            let output = std::process::Command::new(bin.ffmpeg_path())
                .args(cmd.to_os_args())
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            assert!(output.status.success());
        };
        // Interleave so machine load drifts through both sides equally; the best of three
        // rounds discards a round that a busy host slowed.
        let (mut base, mut capture, mut progress) = (f64::MAX, f64::MAX, f64::MAX);
        for _ in 0..3 {
            base = base.min(mean_micros(RUNS, || blocking(())));
            capture = capture.min(mean_micros(RUNS, || {
                let out = run_capture(&bin, &cmd, Duration::from_secs(20), &token).unwrap();
                assert!(out.status.success());
            }));
            progress = progress.min(mean_micros(RUNS, || {
                run_with_progress(&bin, &cmd, policy_opts(Duration::from_secs(20), &token))
                    .unwrap();
            }));
        }
        // The progress path starts a third helper thread and shows more load noise, hence its
        // looser bound. A 10 ms polling sleep adds about 5 ms on average; the exit-driven wait adds
        // only thread and pipe setup, far below that.
        assert!(
            capture - base < 3_500.0,
            "run_capture overhead {:.0} us over blocking ({base:.0} us)",
            capture - base
        );
        assert!(
            progress - base < 6_000.0,
            "run_with_progress overhead {:.0} us over blocking ({base:.0} us)",
            progress - base
        );
    }

    #[test]
    fn cancel_and_deadline_still_stop_a_waiting_run_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_ffmpeg(dir.path(), "exec sleep 30");
        let cmd = FfmpegCommand::base("error");
        let started = Instant::now();
        let result = run_capture(
            &bin,
            &cmd,
            Duration::from_millis(150),
            &CancellationToken::new(),
        );
        assert!(matches!(result, Err(RunError::TimedOut(_))));
        assert!(started.elapsed() < Duration::from_secs(10));

        let token = CancellationToken::new();
        let canceller = token.clone();
        let helper = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            canceller.cancel();
        });
        let started = Instant::now();
        let result = run_capture_with(&bin, &cmd, RunOptions::unbounded(token));
        helper.join().unwrap();
        assert!(matches!(result, Err(RunError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn current_dir_resolves_relative_files_and_bad_dirs_are_typed_errors() {
        let bin_dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        std::fs::write(work.path().join("cmds.txt"), "hello").unwrap();
        // Fails (exit 1) unless the relative file is visible; prints it otherwise.
        let bin = fake_ffmpeg(bin_dir.path(), "cat cmds.txt || exit 1");
        let cmd = FfmpegCommand::base("error");
        let token = CancellationToken::new();
        let without = run_capture(&bin, &cmd, Duration::from_secs(20), &token).unwrap();
        assert!(
            !without.status.success(),
            "inherited cwd must not see the file"
        );
        let opts =
            RunOptions::new(Duration::from_secs(20), token.clone()).with_current_dir(work.path());
        let with = run_capture_with(&bin, &cmd, opts).unwrap();
        assert!(with.status.success());
        assert_eq!(with.stdout, b"hello");

        let missing = work.path().join("nope");
        let opts =
            RunOptions::new(Duration::from_secs(20), token.clone()).with_current_dir(&missing);
        assert!(matches!(
            run_capture_with(&bin, &cmd, opts),
            Err(RunError::InvalidCurrentDir {
                reason: "does not exist",
                ..
            })
        ));
        let file = work.path().join("cmds.txt");
        let opts = RunOptions::new(Duration::from_secs(20), token.clone()).with_current_dir(&file);
        assert!(matches!(
            run_with_progress(&bin, &cmd, opts),
            Err(RunError::InvalidCurrentDir {
                reason: "is not a directory",
                ..
            })
        ));
        assert!(matches!(
            spawn_streaming_in(&bin, &cmd, StdioPolicy::default(), Some(&missing)),
            Err(RunError::InvalidCurrentDir { .. })
        ));
        let mut child =
            spawn_streaming_in(&bin, &cmd, StdioPolicy::default(), Some(work.path())).unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn a_relative_program_path_is_pinned_before_the_directory_changes() {
        let work = tempfile::tempdir().unwrap();
        // A decoy with the same bare name inside the new cwd must never run.
        let decoy = work.path().join("true");
        std::fs::write(&decoy, "#!/bin/sh\nexit 9\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&decoy, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let bin = DevFfmpeg::unverified(PathBuf::from("true"));
        let opts = RunOptions::new(Duration::from_secs(20), CancellationToken::new())
            .with_current_dir(work.path());
        let out = run_capture_with(&bin, &FfmpegCommand::base("error"), opts).unwrap();
        assert!(out.status.success(), "decoy in cwd must not be executed");
    }
}
