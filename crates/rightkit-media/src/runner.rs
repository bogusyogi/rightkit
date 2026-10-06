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
    process::ExitStatus,
    sync::{
        mpsc::{self, RecvTimeoutError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use rightkit_process::OwnedCommand;

use crate::{CancellationToken, FfmpegBinary, FfmpegCommand};

const STDERR_TAIL_LINES: usize = 40;
const STDERR_TAIL_BYTES: usize = 8 * 1024;
const POLL: Duration = Duration::from_millis(25);
/// Cap on stderr kept for `run_capture` (loudnorm JSON, `-i` probe text).
const CAPTURE_LIMIT: u64 = 4 << 20;

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

#[derive(Debug)]
pub enum RunError {
    /// FFmpeg could not be started.
    Spawn(io::Error),
    /// Waiting on or reading from the child failed.
    Io(io::Error),
    /// Cancelled before start or while running.
    Cancelled,
    TimedOut(Duration),
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
            Self::TimedOut(d) => write!(f, "ffmpeg timed out after {d:?}"),
            Self::Failed { status, stderr_tail } => {
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
pub struct RunOptions<'a> {
    pub timeout: Duration,
    /// Grace between the polite stop signal and the forced tree kill.
    pub kill_grace: Duration,
    pub cancel: CancellationToken,
    pub on_progress: Option<&'a mut dyn FnMut(&ProgressEvent)>,
}

impl RunOptions<'_> {
    pub fn new(timeout: Duration, cancel: CancellationToken) -> Self {
        Self {
            timeout,
            kill_grace: Duration::from_secs(2),
            cancel,
            on_progress: None,
        }
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

/// Run FFmpeg, parsing `-progress pipe:1` from stdout (the command must request it,
/// e.g. via [`FfmpegCommand::progress_pipe`]). Stderr is kept as a bounded tail.
pub fn run_with_progress(
    ffmpeg: &dyn FfmpegBinary,
    cmd: &FfmpegCommand,
    mut opts: RunOptions<'_>,
) -> Result<RunReport, RunError> {
    if opts.cancel.is_cancelled() {
        return Err(RunError::Cancelled);
    }
    let mut command = OwnedCommand::new(ffmpeg.ffmpeg_path());
    command
        .command_mut()
        .args(cmd.to_os_args())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command.windows_hide();
    let started = Instant::now();
    let mut child = command.spawn().map_err(RunError::Spawn)?;

    let (tx, rx) = mpsc::channel::<ProgressEvent>();
    let stdout = child.take_stdout().expect("stdout piped");
    let out_thread = thread::spawn(move || {
        let mut parser = ProgressParser::default();
        read_lines(stdout, |line| {
            if let Some(ev) = parser.feed(&line) {
                let _ = tx.send(ev);
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

    let deadline = started + opts.timeout;
    let mut last = ProgressEvent {
        frame: 0,
        out_time_ms: 0,
        end: false,
    };
    let mut events = 0u64;
    let mut abort: Option<Abort> = None;
    let status = loop {
        match rx.recv_timeout(POLL) {
            Ok(ev) => {
                events += 1;
                if let Some(cb) = opts.on_progress.as_deref_mut() {
                    cb(&ev);
                }
                last = ev;
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
        }
        if abort.is_none() {
            if opts.cancel.is_cancelled() {
                abort = Some(Abort::Cancelled);
            } else if Instant::now() >= deadline {
                abort = Some(Abort::TimedOut);
            }
            if abort.is_some() {
                if let Err(e) = child.wait_or_kill(opts.kill_grace) {
                    let _ = child.terminate_tree();
                    return Err(RunError::Io(e));
                }
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                let _ = child.terminate_tree();
                return Err(RunError::Io(e));
            }
        }
    };
    let _ = out_thread.join();
    let _ = err_thread.join();
    while let Ok(ev) = rx.try_recv() {
        events += 1;
        if let Some(cb) = opts.on_progress.as_deref_mut() {
            cb(&ev);
        }
        last = ev;
    }
    match abort {
        Some(Abort::Cancelled) => return Err(RunError::Cancelled),
        Some(Abort::TimedOut) => return Err(RunError::TimedOut(opts.timeout)),
        None => {}
    }
    if !status.success() {
        let stderr_tail = tail.lock().map(|t| t.text()).unwrap_or_default();
        return Err(RunError::Failed { status, stderr_tail });
    }
    Ok(RunReport {
        frames: last.frame,
        out_time_ms: last.out_time_ms,
        elapsed_ms: started.elapsed().as_millis() as u64,
        progress_events: events,
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
/// timeout; does NOT treat a non-zero exit as an error (probes rely on that).
pub fn run_capture(
    ffmpeg: &dyn FfmpegBinary,
    cmd: &FfmpegCommand,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Captured, RunError> {
    if cancel.is_cancelled() {
        return Err(RunError::Cancelled);
    }
    let mut command = OwnedCommand::new(ffmpeg.ffmpeg_path());
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
    let deadline = Instant::now() + timeout;
    let mut abort = None;
    let status = loop {
        if abort.is_none() {
            if cancel.is_cancelled() {
                abort = Some(Abort::Cancelled);
            } else if Instant::now() >= deadline {
                abort = Some(Abort::TimedOut);
            }
            if abort.is_some() {
                let _ = child.wait_or_kill(Duration::from_secs(2));
            }
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                let _ = child.terminate_tree();
                return Err(RunError::Io(e));
            }
        }
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = String::from_utf8_lossy(&err_t.join().unwrap_or_default()).into_owned();
    match abort {
        Some(Abort::Cancelled) => Err(RunError::Cancelled),
        Some(Abort::TimedOut) => Err(RunError::TimedOut(timeout)),
        None => Ok(Captured { stdout, stderr, status }),
    }
}

/// Like [`run_capture`] but a non-zero exit is [`RunError::Failed`] with a stderr tail.
pub fn run_checked(
    ffmpeg: &dyn FfmpegBinary,
    cmd: &FfmpegCommand,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Captured, RunError> {
    let c = run_capture(ffmpeg, cmd, timeout, cancel)?;
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
