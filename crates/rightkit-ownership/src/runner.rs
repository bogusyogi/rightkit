//! Scan workers. Port of `runScanWorker` and `defaultScannerRoot` from
//! `ownership-admission.mjs`. The worker itself (inventory, evidence, trust loading) is
//! still the Node `ownership-scan-worker.mjs`; this module runs it under a hard timeout
//! and a hard output cap, and parses its JSON.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Staged worker entry point inside a scanner root.
pub const WORKER_FILE: &str = "ownership-scan-worker.mjs";
/// Protected catalog file that must sit beside the worker.
pub const MANIFEST_FILE: &str = "rightkit-ownership.json";
/// Node's `MAX_OUTPUT_BYTES`: combined stdout and stderr cap per worker.
pub const MAX_OUTPUT_BYTES: usize = 32 * 1024 * 1024;

const POLL_INTERVAL: Duration = Duration::from_millis(5);
const STDERR_LIMIT_CHARS: usize = 2000;

/// One worker invocation.
#[derive(Debug, Clone, Copy)]
pub struct ScanRequest<'a> {
    pub scanner_root: &'a Path,
    pub scan_root: &'a str,
    pub timeout: Duration,
    /// `--inputs`: authenticate and hash inputs only, without evidence collection.
    pub inputs_only: bool,
}

/// Runs one worker. The result is the worker's JSON object, or `{error, code?}` on
/// failure, exactly as Node's `runScanWorker` resolves.
pub trait ScanRunner: Send + Sync {
    fn run(&self, request: &ScanRequest<'_>) -> Value;
}

/// Adapts a closure into a [`ScanRunner`]. Used by tests and by callers that supply
/// their own worker.
pub struct FnScanRunner<F>(pub F);

impl<F> ScanRunner for FnScanRunner<F>
where
    F: Fn(&ScanRequest<'_>) -> Value + Send + Sync,
{
    fn run(&self, request: &ScanRequest<'_>) -> Value {
        (self.0)(request)
    }
}

/// Runs the staged Node worker through `node`.
#[derive(Debug, Clone)]
pub struct ProcessScanRunner {
    pub node: PathBuf,
    pub max_output_bytes: usize,
}

impl ProcessScanRunner {
    pub fn new(node: impl Into<PathBuf>) -> Self {
        Self {
            node: node.into(),
            max_output_bytes: MAX_OUTPUT_BYTES,
        }
    }
}

impl ScanRunner for ProcessScanRunner {
    fn run(&self, request: &ScanRequest<'_>) -> Value {
        let mut command = Command::new(&self.node);
        command
            .arg(request.scanner_root.join(WORKER_FILE))
            .arg(request.scan_root)
            .arg(request.scanner_root.join(MANIFEST_FILE));
        if request.inputs_only {
            command.arg("--inputs");
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        match command.spawn() {
            Ok(child) => supervise(child, request.timeout, self.max_output_bytes),
            Err(error) => failure(format!("scanner failed to start: {error}"), None),
        }
    }
}

/// `{error, code?}` in the shape Node resolves on failure.
pub fn failure(message: String, code: Option<&str>) -> Value {
    match code {
        Some(code) => json!({"error": message, "code": code}),
        None => json!({"error": message}),
    }
}

fn read_stream<R>(
    mut stream: R,
    sink: Arc<Mutex<Vec<u8>>>,
    total: Arc<AtomicUsize>,
    overflow: Arc<AtomicBool>,
    limit: usize,
    done: mpsc::Sender<()>,
) where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    let seen = total.fetch_add(count, Ordering::SeqCst) + count;
                    if seen > limit {
                        overflow.store(true, Ordering::SeqCst);
                        break;
                    }
                    sink.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .extend_from_slice(&chunk[..count]);
                }
            }
        }
        let _ = done.send(());
    });
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Drives one worker to completion within `timeout`, enforcing the output cap. Reader
/// threads are detached rather than joined, so a grandchild holding a pipe cannot
/// block past the deadline.
fn supervise(mut child: Child, timeout: Duration, limit: usize) -> Value {
    let deadline = Instant::now() + timeout;
    let timed_out = || failure(format!("scan exceeded {} ms", timeout.as_millis()), None);
    let overflowed = || failure(format!("scanner output exceeded {limit} bytes"), None);
    let stdout_sink = Arc::new(Mutex::new(Vec::new()));
    let stderr_sink = Arc::new(Mutex::new(Vec::new()));
    let total = Arc::new(AtomicUsize::new(0));
    let overflow = Arc::new(AtomicBool::new(false));
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let mut readers = 0usize;
    if let Some(stdout) = child.stdout.take() {
        read_stream(
            stdout,
            Arc::clone(&stdout_sink),
            Arc::clone(&total),
            Arc::clone(&overflow),
            limit,
            done_tx.clone(),
        );
        readers += 1;
    }
    if let Some(stderr) = child.stderr.take() {
        read_stream(
            stderr,
            Arc::clone(&stderr_sink),
            Arc::clone(&total),
            Arc::clone(&overflow),
            limit,
            done_tx.clone(),
        );
        readers += 1;
    }
    drop(done_tx);

    let status: ExitStatus = loop {
        if overflow.load(Ordering::SeqCst) {
            kill_and_reap(&mut child);
            return overflowed();
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                kill_and_reap(&mut child);
                return failure(format!("scanner wait failed: {error}"), None);
            }
        }
        if Instant::now() >= deadline {
            kill_and_reap(&mut child);
            return timed_out();
        }
        thread::sleep(POLL_INTERVAL);
    };

    // Node resolves on 'close', which waits for stdio; bound that wait by the deadline.
    for _ in 0..readers {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if done_rx.recv_timeout(remaining).is_err() {
            return timed_out();
        }
    }
    if overflow.load(Ordering::SeqCst) {
        return overflowed();
    }
    let stdout = stdout_sink
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if !status.success() {
        let code = status
            .code()
            .map_or_else(|| "null".to_string(), |code| code.to_string());
        let stderr = stderr_sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let text: String = String::from_utf8_lossy(&stderr)
            .trim()
            .chars()
            .take(STDERR_LIMIT_CHARS)
            .collect();
        let message = if text.is_empty() {
            format!("scanner exited {code}")
        } else {
            text
        };
        return failure(message, None);
    }
    match serde_json::from_slice::<Value>(&stdout) {
        Ok(value) => value,
        Err(error) => failure(format!("scanner returned invalid JSON: {error}"), None),
    }
}

/// Staged scanner root beside the admitting runtime: the packaged `ownership-scanner`
/// first, then the source `release` checkout. Requires the worker and the catalog.
/// `here` is the directory the admitting code is installed in (Node's module dir).
pub fn default_scanner_root(here: &Path) -> Option<PathBuf> {
    [
        here.join("..").join("..").join("ownership-scanner"),
        here.join("..").join("..").join("release"),
    ]
    .into_iter()
    .map(|candidate| resolve_lexical(&candidate))
    .find(|candidate| {
        candidate.join(WORKER_FILE).exists() && candidate.join(MANIFEST_FILE).exists()
    })
}

/// Lexical absolute path (Node `path.resolve`): relative paths join the current
/// directory, and `.` and `..` are collapsed without touching the filesystem.
pub fn resolve_lexical(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;

    use super::*;

    fn fake_worker(dir: &Path, body: &str) -> PathBuf {
        fs::write(dir.join(WORKER_FILE), format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::write(dir.join(MANIFEST_FILE), "{}").unwrap();
        dir.to_path_buf()
    }

    fn run_fake(body: &str, timeout: Duration, limit: usize) -> Value {
        let dir = tempfile::tempdir().unwrap();
        let root = fake_worker(dir.path(), body);
        let runner = ProcessScanRunner {
            node: PathBuf::from("/bin/sh"),
            max_output_bytes: limit,
        };
        runner.run(&ScanRequest {
            scanner_root: &root,
            scan_root: "/repo",
            timeout,
            inputs_only: true,
        })
    }

    #[test]
    fn worker_stdout_json_is_returned_verbatim() {
        let value = run_fake(
            r#"printf '%s' '{"complete":true,"args":"ok"}'"#,
            Duration::from_secs(10),
            MAX_OUTPUT_BYTES,
        );
        assert_eq!(value, json!({"complete": true, "args": "ok"}));
    }

    #[test]
    fn worker_nonzero_exit_reports_trimmed_stderr() {
        let value = run_fake(
            "echo boom >&2; exit 3",
            Duration::from_secs(10),
            MAX_OUTPUT_BYTES,
        );
        assert_eq!(value, json!({"error": "boom"}));
    }

    #[test]
    fn worker_silent_nonzero_exit_reports_code() {
        let value = run_fake("exit 7", Duration::from_secs(10), MAX_OUTPUT_BYTES);
        assert_eq!(value, json!({"error": "scanner exited 7"}));
    }

    #[test]
    fn worker_invalid_json_is_refused() {
        let value = run_fake("printf nope", Duration::from_secs(10), MAX_OUTPUT_BYTES);
        assert!(value["error"]
            .as_str()
            .unwrap()
            .starts_with("scanner returned invalid JSON"));
    }

    #[test]
    fn worker_exceeding_deadline_is_killed() {
        let started = Instant::now();
        let value = run_fake("sleep 5", Duration::from_millis(200), MAX_OUTPUT_BYTES);
        assert_eq!(value, json!({"error": "scan exceeded 200 ms"}));
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn worker_output_over_cap_is_refused() {
        let value = run_fake("printf '%0200d' 0", Duration::from_secs(10), 10);
        assert_eq!(value, json!({"error": "scanner output exceeded 10 bytes"}));
    }

    #[test]
    fn missing_node_reports_start_failure() {
        let dir = tempfile::tempdir().unwrap();
        let runner = ProcessScanRunner::new(dir.path().join("no-such-node"));
        let value = runner.run(&ScanRequest {
            scanner_root: dir.path(),
            scan_root: "/repo",
            timeout: Duration::from_secs(5),
            inputs_only: false,
        });
        assert!(value["error"]
            .as_str()
            .unwrap()
            .starts_with("scanner failed to start"));
    }
}
