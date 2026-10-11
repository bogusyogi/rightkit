//! Isolated child-per-request native OCR worker (ViewRight `ocr_worker.rs`).
//!
//! Accurate Vision requests can fail on the target Mac with precompiled-
//! operation errors once a process has served one, so each request runs in a
//! short-lived child. Protocol: raw encoded image bytes on stdin, one bounded
//! JSON [`WorkerResponse`] line on stdout. Paths, environment and recognized
//! text are never written to stderr.
//!
//! Server side: call [`run_if_requested`] first thing in `main` (before any
//! UI/single-instance init). Client side: [`WorkerCommand::run`], or
//! [`WorkerCommand::run_observed`] when the host wants parent-side lifecycle events
//! (child pid, wait, response, exit, timeout) and a [`RunRecord`] for diagnostics.
//!
//! ```rust,ignore
//! let cmd = WorkerCommand::current_exe(DEFAULT_WORKER_ARG)?;
//! let log = |event: WorkerEvent| tracing::info!(?event, "ocr_worker");
//! match cmd.run_observed(&png, &log) {
//!     Ok(run) => use_lines(run.lines),
//!     Err(failure) => match failure.kind {
//!         OcrErrorKind::WorkerTimeout => retry_later(),
//!         _ => show_error(&failure.message),
//!     },
//! }
//! ```

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::types::{OcrErrorKind, OcrLine};

pub const MAX_INPUT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 1024 * 1024;
/// Default deadline for one run and for one canary (see [`WorkerCommand::canary_deadline`]).
///
/// The child's own watchdog is fixed at this value and is not configurable through the
/// parent: a larger `deadline` is still cut short by the child at 90 seconds.
pub const WORKER_DEADLINE: Duration = Duration::from_secs(90);
/// Exit code the worker uses when its own deadline fires.
pub const SELF_DEADLINE_EXIT: i32 = 124;
/// Default argv[1] selecting worker mode.
pub const DEFAULT_WORKER_ARG: &str = "--rightkit-ocr-worker";

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<Vec<OcrLine>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One parent-side lifecycle fact from [`WorkerCommand::run_observed`].
///
/// Order for a child that was spawned: `Spawned`, `Waiting`, then either `TimedOut` followed by
/// `Exited` (the parent's deadline killed it) or `Exited` followed by `TimedOut` (the child's own
/// watchdog exited with [`SELF_DEADLINE_EXIT`]), and finally `Responded`. Every spawned child
/// yields exactly one `Exited`. A run that never spawned emits no events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkerEvent {
    /// The child started. `pid` is its process id, for host diagnostics.
    Spawned { pid: u32 },
    /// The request was handed to the child and the parent is waiting within its deadline.
    Waiting,
    /// The child's stdout was captured. `bytes` is the captured length (up to the protocol limit)
    /// and `elapsed` is measured from spawn.
    Responded { bytes: usize, elapsed: Duration },
    /// The child was reaped. `code` is `None` when it was killed by a signal or its status could
    /// not be read; `success` is `true` only for a zero exit.
    Exited { code: Option<i32>, success: bool },
    /// The run ran out of time: the parent's deadline, or the child's watchdog exit code.
    TimedOut,
}

/// Receives [`WorkerEvent`]s synchronously on the calling thread.
///
/// Any `Fn(WorkerEvent)` closure implements it, so `&|event| ...` works without a named type.
/// Keep implementations quick and non-blocking, since the run waits on them.
pub trait WorkerEvents {
    fn on_event(&self, event: WorkerEvent);
}

impl<F> WorkerEvents for F
where
    F: Fn(WorkerEvent),
{
    fn on_event(&self, event: WorkerEvent) {
        self(event)
    }
}

/// Facts about one isolated run, returned on success and carried by [`WorkerFailure`] on failure.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    /// Child process id; `None` when the child never started.
    pub pid: Option<u32>,
    /// Exit code; `None` for a signal kill or when the status was unavailable.
    pub exit_code: Option<i32>,
    /// `true` only when the child exited with status zero.
    pub exit_success: bool,
    /// Bytes captured from the child's stdout, up to the protocol limit.
    pub response_bytes: usize,
    /// Time from spawn to the end of the run (zero when the child never started).
    pub elapsed: Duration,
    /// `true` when the run ended by deadline, the parent's or the child's watchdog.
    pub timed_out: bool,
}

/// A successful isolated run: the recognized lines and what happened.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkerRun {
    pub lines: Vec<OcrLine>,
    pub record: RunRecord,
}

/// A failed isolated run: a typed kind, the legacy message text, and what happened.
///
/// `Display` is the legacy message (including any `...-unavailable:` prefix), so existing string
/// comparisons keep working. Key on [`WorkerFailure::kind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerFailure {
    pub kind: OcrErrorKind,
    pub message: String,
    pub record: RunRecord,
}

impl fmt::Display for WorkerFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for WorkerFailure {}

/// How to launch the worker: the executable (usually the app itself, which
/// calls [`run_if_requested`]) plus the selecting argument.
#[derive(Debug, Clone)]
pub struct WorkerCommand {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    /// Deadline for one recognition run.
    pub deadline: Duration,
    /// Deadline for one canary run, separate from `deadline`. Defaults to [`WORKER_DEADLINE`],
    /// so behavior is unchanged until a host sets it. A canary that gets only the budget left
    /// after the request, as ViewRight does, can use [`WorkerCommand::with_canary_deadline`] to
    /// give the check its own bound instead.
    pub canary_deadline: Duration,
}

impl WorkerCommand {
    /// Re-invoke the current executable with `arg` (the ViewRight shape).
    pub fn current_exe(arg: &str) -> Result<Self, String> {
        let executable = std::env::current_exe()
            .map_err(|_| "OCR worker executable path unavailable".to_string())?;
        Ok(Self::new(executable, [arg]))
    }

    pub fn new<I, S>(executable: impl Into<PathBuf>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            executable: executable.into(),
            args: args.into_iter().map(Into::into).collect(),
            deadline: WORKER_DEADLINE,
            canary_deadline: WORKER_DEADLINE,
        }
    }

    /// Set the deadline for one recognition run.
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// Set the deadline for canary runs, independently of [`WorkerCommand::with_deadline`].
    pub fn with_canary_deadline(mut self, deadline: Duration) -> Self {
        self.canary_deadline = deadline;
        self
    }

    /// This command with its deadline replaced by [`WorkerCommand::canary_deadline`]. Use it for
    /// canary requests (the embedded known-text PNG) so they get their own bound.
    pub fn for_canary(&self) -> Self {
        let mut canary = self.clone();
        canary.deadline = self.canary_deadline;
        canary
    }

    /// One isolated recognition request in a fresh child.
    pub fn run(&self, bytes: &[u8]) -> Result<Vec<OcrLine>, String> {
        let silent = |_: WorkerEvent| {};
        self.run_observed(bytes, &silent)
            .map(|run| run.lines)
            .map_err(|failure| failure.message)
    }

    /// One isolated recognition request in a fresh child, reporting lifecycle events to `events`
    /// as they happen and returning a [`RunRecord`] with the same facts.
    ///
    /// Use [`WorkerCommand::run`] when neither is needed; its behavior and messages are unchanged.
    pub fn run_observed(
        &self,
        bytes: &[u8],
        events: &dyn WorkerEvents,
    ) -> Result<WorkerRun, WorkerFailure> {
        run_isolated(self, bytes, events)
    }
}

#[derive(Debug)]
struct CapturedOutput {
    bytes: Vec<u8>,
    exceeded: bool,
}

/// If argv[1] equals `arg`, serve one request and exit; otherwise return
/// `false` immediately. The worker never creates a window.
pub fn run_if_requested(arg: &str) -> bool {
    if std::env::args().nth(1).as_deref() != Some(arg) {
        return false;
    }
    run_worker_process()
}

fn run_worker_process() -> ! {
    worker_event("start", "stdin");
    // The parent may die while Vision is inside its synchronous call; bound the
    // child independently so losing the supervisor cannot leave it running.
    thread::spawn(|| {
        thread::sleep(WORKER_DEADLINE);
        worker_event("timeout", "self-deadline");
        std::process::exit(SELF_DEADLINE_EXIT);
    });
    let result = read_input_from(io::stdin())
        .and_then(|bytes| {
            let img = crate::types::decode_encoded(&bytes)?;
            worker_event("start", "recognize");
            crate::native::recognize_once(&img)
        })
        .map(|lines| WorkerResponse {
            ok: true,
            lines: Some(lines),
            error: None,
        })
        .unwrap_or_else(|error| WorkerResponse {
            ok: false,
            lines: None,
            error: Some(error),
        });
    worker_event(if result.ok { "success" } else { "failure" }, "recognize");
    std::process::exit(write_response(result));
}

fn read_input_from(reader: impl Read) -> Result<Vec<u8>, String> {
    let mut input = Vec::new();
    reader
        .take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .map_err(|_| "OCR worker could not read request".to_string())?;
    if input.len() > MAX_INPUT_BYTES {
        return Err("OCR worker request exceeds the 32 MiB limit".to_string());
    }
    Ok(input)
}

fn write_response(response: WorkerResponse) -> i32 {
    let mut encoded = match serde_json::to_vec(&response) {
        Ok(bytes) => bytes,
        Err(_) => return 1,
    };
    if encoded.len() > MAX_OUTPUT_BYTES {
        encoded = serde_json::to_vec(&WorkerResponse {
            ok: false,
            lines: None,
            error: Some("OCR worker result exceeds the 8 MiB limit".to_string()),
        })
        .expect("fixed worker error must serialize");
    }
    encoded.push(b'\n');
    if io::stdout().write_all(&encoded).is_err() {
        worker_event("failure", "stdout");
        return 1;
    }
    0
}

fn worker_event(event: &str, stage: &str) {
    eprintln!(
        "ocr_worker event={event} pid={} stage={stage}",
        std::process::id()
    );
}

fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    limit: usize,
    exceeded: Arc<AtomicBool>,
) -> JoinHandle<CapturedOutput> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let read_result = reader
            .by_ref()
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes);
        let over = bytes.len() > limit;
        if read_result.is_err() || over {
            exceeded.store(true, Ordering::Release);
        }
        CapturedOutput {
            bytes,
            exceeded: over,
        }
    })
}

/// Kill and reap the child, returning its status when the reap succeeded.
fn kill_and_reap(child: &mut Child) -> Option<ExitStatus> {
    let _ = child.kill();
    child.wait().ok()
}

/// How the parent's wait ended. Every variant carries the reaped status when it was available.
enum Settled {
    Exited(ExitStatus),
    DeadlineExceeded(Option<ExitStatus>),
    OutputExceeded(Option<ExitStatus>),
    WaitFailed(Option<ExitStatus>),
}

impl Settled {
    fn status(&self) -> Option<ExitStatus> {
        match *self {
            Self::Exited(status) => Some(status),
            Self::DeadlineExceeded(status)
            | Self::OutputExceeded(status)
            | Self::WaitFailed(status) => status,
        }
    }

    /// The fault that ended the wait before the child could answer, if any.
    fn fault(&self) -> Option<(OcrErrorKind, &'static str)> {
        match self {
            Self::Exited(_) => None,
            Self::DeadlineExceeded(_) => Some((
                OcrErrorKind::WorkerTimeout,
                "OCR worker exceeded its deadline",
            )),
            Self::OutputExceeded(_) => Some((
                OcrErrorKind::WorkerOutputExceeded,
                "OCR worker output exceeds the bounded protocol limit",
            )),
            Self::WaitFailed(_) => {
                Some((OcrErrorKind::WorkerFailed, "OCR worker process wait failed"))
            }
        }
    }
}

fn wait_with_deadline(child: &mut Child, exceeded: &AtomicBool, deadline: Duration) -> Settled {
    let until = Instant::now() + deadline;
    loop {
        if exceeded.load(Ordering::Acquire) {
            return Settled::OutputExceeded(kill_and_reap(child));
        }
        match child.try_wait() {
            Ok(Some(status)) => return Settled::Exited(status),
            Ok(None) if Instant::now() >= until => {
                return Settled::DeadlineExceeded(kill_and_reap(child));
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => return Settled::WaitFailed(kill_and_reap(child)),
        }
    }
}

fn parse_response(
    stdout: &[u8],
    child_succeeded: bool,
) -> Result<Vec<OcrLine>, (OcrErrorKind, String)> {
    if stdout.len() > MAX_OUTPUT_BYTES {
        return Err((
            OcrErrorKind::WorkerOutputExceeded,
            "OCR worker stdout exceeds the 8 MiB limit".to_string(),
        ));
    }
    let response: WorkerResponse = serde_json::from_slice(stdout).map_err(|_| {
        (
            OcrErrorKind::WorkerProtocol,
            "OCR worker returned malformed JSON".to_string(),
        )
    })?;
    if !child_succeeded {
        return Err((
            OcrErrorKind::WorkerFailed,
            "OCR worker exited with a failure status".to_string(),
        ));
    }
    if response.ok {
        response.lines.ok_or_else(|| {
            (
                OcrErrorKind::WorkerProtocol,
                "OCR worker success response omitted lines".to_string(),
            )
        })
    } else {
        // The child reports its own failures as text; this is the one place they are typed.
        let message = response
            .error
            .unwrap_or_else(|| "OCR worker returned an unspecified error".to_string());
        let kind = OcrErrorKind::from_message(&message).unwrap_or(OcrErrorKind::Runtime);
        Err((kind, message))
    }
}

fn failure(record: &RunRecord, kind: OcrErrorKind, message: impl Into<String>) -> WorkerFailure {
    WorkerFailure {
        kind,
        message: message.into(),
        record: record.clone(),
    }
}

/// Emit `Exited` and fill the exit fields of the record.
fn report_exit(status: Option<ExitStatus>, record: &mut RunRecord, events: &dyn WorkerEvents) {
    record.exit_code = status.and_then(|s| s.code());
    record.exit_success = status.is_some_and(|s| s.success());
    events.on_event(WorkerEvent::Exited {
        code: record.exit_code,
        success: record.exit_success,
    });
}

fn run_isolated(
    cmd: &WorkerCommand,
    bytes: &[u8],
    events: &dyn WorkerEvents,
) -> Result<WorkerRun, WorkerFailure> {
    let mut record = RunRecord::default();
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(failure(
            &record,
            OcrErrorKind::RequestTooLarge,
            "OCR worker request exceeds the 32 MiB limit",
        ));
    }
    let mut command = Command::new(&cmd.executable);
    command
        .args(&cmd.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no visible console
    }
    let mut child = command.spawn().map_err(|_| {
        failure(
            &record,
            OcrErrorKind::WorkerFailed,
            "OCR worker process could not start",
        )
    })?;
    let spawned_at = Instant::now();
    let pid = child.id();
    record.pid = Some(pid);
    events.on_event(WorkerEvent::Spawned { pid });

    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        let status = kill_and_reap(&mut child);
        report_exit(status, &mut record, events);
        record.elapsed = spawned_at.elapsed();
        return Err(failure(
            &record,
            OcrErrorKind::WorkerFailed,
            "OCR worker pipes unavailable",
        ));
    };

    let exceeded = Arc::new(AtomicBool::new(false));
    let out_reader = spawn_reader(stdout, MAX_OUTPUT_BYTES, exceeded.clone());
    let err_reader = spawn_reader(stderr, MAX_STDERR_BYTES, exceeded.clone());
    let payload = bytes.to_vec();
    let writer = thread::spawn(move || {
        let mut stdin = stdin;
        stdin
            .write_all(&payload)
            .map_err(|_| "OCR worker request write failed".to_string())
    });

    events.on_event(WorkerEvent::Waiting);
    let settled = wait_with_deadline(&mut child, &exceeded, cmd.deadline);
    let status = settled.status();
    let fault = settled.fault();
    if matches!(settled, Settled::DeadlineExceeded(_)) {
        record.timed_out = true;
        events.on_event(WorkerEvent::TimedOut);
    }
    report_exit(status, &mut record, events);
    record.elapsed = spawned_at.elapsed();
    if status.and_then(|s| s.code()) == Some(SELF_DEADLINE_EXIT) {
        record.timed_out = true;
        events.on_event(WorkerEvent::TimedOut);
    }

    let write_result = writer
        .join()
        .unwrap_or(Err("OCR worker request writer panicked".to_string()));
    let out = out_reader.join().map_err(|_| {
        failure(
            &record,
            OcrErrorKind::WorkerFailed,
            "OCR worker stdout reader panicked",
        )
    })?;
    let err = err_reader.join().map_err(|_| {
        failure(
            &record,
            OcrErrorKind::WorkerFailed,
            "OCR worker stderr reader panicked",
        )
    })?;
    record.response_bytes = out.bytes.len();
    record.elapsed = spawned_at.elapsed();
    events.on_event(WorkerEvent::Responded {
        bytes: out.bytes.len(),
        elapsed: record.elapsed,
    });

    if let Some((kind, message)) = fault {
        return Err(failure(&record, kind, message));
    }
    if status.and_then(|s| s.code()) == Some(SELF_DEADLINE_EXIT) {
        return Err(failure(
            &record,
            OcrErrorKind::WorkerTimeout,
            "OCR worker exceeded its deadline",
        ));
    }
    if let Err(message) = write_result {
        return Err(failure(&record, OcrErrorKind::WorkerFailed, message));
    }
    if out.exceeded || err.exceeded {
        return Err(failure(
            &record,
            OcrErrorKind::WorkerOutputExceeded,
            "OCR worker output exceeds the bounded protocol limit",
        ));
    }
    match parse_response(&out.bytes, status.is_some_and(|s| s.success())) {
        Ok(lines) => Ok(WorkerRun { lines, record }),
        Err((kind, message)) => Err(failure(&record, kind, message)),
    }
}
