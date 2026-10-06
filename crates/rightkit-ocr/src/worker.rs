//! Isolated child-per-request native OCR worker (ViewRight `ocr_worker.rs`).
//!
//! Accurate Vision requests can fail on the target Mac with precompiled-
//! operation errors once a process has served one, so each request runs in a
//! short-lived child. Protocol: raw encoded image bytes on stdin, one bounded
//! JSON [`WorkerResponse`] line on stdout. Paths, environment and recognized
//! text are never written to stderr.
//!
//! Server side: call [`run_if_requested`] first thing in `main` (before any
//! UI/single-instance init). Client side: [`WorkerCommand::run`].

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::types::OcrLine;

pub const MAX_INPUT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 1024 * 1024;
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

/// How to launch the worker: the executable (usually the app itself, which
/// calls [`run_if_requested`]) plus the selecting argument.
#[derive(Debug, Clone)]
pub struct WorkerCommand {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub deadline: Duration,
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
        }
    }

    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// One isolated recognition request in a fresh child.
    pub fn run(&self, bytes: &[u8]) -> Result<Vec<OcrLine>, String> {
        run_isolated(self, bytes)
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

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn wait_with_deadline(
    child: &mut Child,
    exceeded: &AtomicBool,
    deadline: Duration,
) -> Result<ExitStatus, String> {
    let deadline = Instant::now() + deadline;
    loop {
        if exceeded.load(Ordering::Acquire) {
            kill_and_reap(child);
            return Err("OCR worker output exceeds the bounded protocol limit".to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                kill_and_reap(child);
                return Err("OCR worker exceeded its deadline".to_string());
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                kill_and_reap(child);
                return Err("OCR worker process wait failed".to_string());
            }
        }
    }
}

fn parse_response(stdout: &[u8], child_succeeded: bool) -> Result<Vec<OcrLine>, String> {
    if stdout.len() > MAX_OUTPUT_BYTES {
        return Err("OCR worker stdout exceeds the 8 MiB limit".to_string());
    }
    let response: WorkerResponse = serde_json::from_slice(stdout)
        .map_err(|_| "OCR worker returned malformed JSON".to_string())?;
    if !child_succeeded {
        return Err("OCR worker exited with a failure status".to_string());
    }
    if response.ok {
        response
            .lines
            .ok_or_else(|| "OCR worker success response omitted lines".to_string())
    } else {
        Err(response
            .error
            .unwrap_or_else(|| "OCR worker returned an unspecified error".to_string()))
    }
}

fn run_isolated(cmd: &WorkerCommand, bytes: &[u8]) -> Result<Vec<OcrLine>, String> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err("OCR worker request exceeds the 32 MiB limit".to_string());
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
    let mut child = command
        .spawn()
        .map_err(|_| "OCR worker process could not start".to_string())?;

    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        kill_and_reap(&mut child);
        return Err("OCR worker pipes unavailable".to_string());
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

    let status = wait_with_deadline(&mut child, &exceeded, cmd.deadline);
    let write_result = writer
        .join()
        .unwrap_or(Err("OCR worker request writer panicked".to_string()));
    let out = out_reader
        .join()
        .map_err(|_| "OCR worker stdout reader panicked".to_string())?;
    let err = err_reader
        .join()
        .map_err(|_| "OCR worker stderr reader panicked".to_string())?;

    let status = status?;
    if status.code() == Some(SELF_DEADLINE_EXIT) {
        return Err("OCR worker exceeded its deadline".to_string());
    }
    write_result?;
    if out.exceeded || err.exceeded {
        return Err("OCR worker output exceeds the bounded protocol limit".to_string());
    }
    parse_response(&out.bytes, status.success())
}
