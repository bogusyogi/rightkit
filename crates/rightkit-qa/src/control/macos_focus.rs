//! NSWorkspace lives on a managed helper's main runloop; Rust only reads immutable frames.
use rightkit_child_process::{OwnedChild, OwnedCommand, StderrTail};
use serde::Deserialize;
use std::io::{BufRead, BufReader, Read};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

const SCRIPT: &str = include_str!("macos_focus.js");
const MAX_FRAME: usize = 16 * 1024;
const MAX_EVENTS: u64 = 65536;
const START_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Frame {
    version: u32,
    sequence: u64,
    kind: String,
    pub(super) pid: Option<u32>,
    pub(super) name: Option<String>,
    pub(super) executable: Option<String>,
}

pub(super) fn owned_frame(frame: &Frame, primary: &[u32], executable: Option<&str>) -> bool {
    frame.pid.is_some_and(|pid| primary.contains(&pid))
        || executable.is_some_and(|path| frame.executable.as_deref() == Some(path))
}

#[derive(Default)]
struct Protocol {
    next: u64,
    snapshot: bool,
    ready: bool,
    stopped: bool,
    final_snapshot: bool,
}
impl Protocol {
    fn accept(&mut self, frame: &Frame) -> bool {
        if self.stopped
            || frame.version != 1
            || frame.sequence != self.next
            || self.next >= MAX_EVENTS
        {
            return false;
        }
        self.next += 1;
        match frame.kind.as_str() {
            "focus" => {
                if frame.pid.unwrap_or(0) == 0
                    || frame.name.as_ref().is_none_or(|v| v.is_empty())
                    || frame
                        .executable
                        .as_ref()
                        .is_none_or(|v| !v.starts_with('/'))
                {
                    return false;
                }
                self.snapshot = true;
                self.final_snapshot = self.ready;
            }
            "ready"
                if self.snapshot
                    && !self.ready
                    && frame.pid.is_none()
                    && frame.name.is_none()
                    && frame.executable.is_none() =>
            {
                self.ready = true
            }
            "stopped"
                if self.ready
                    && self.final_snapshot
                    && frame.pid.is_none()
                    && frame.name.is_none()
                    && frame.executable.is_none() =>
            {
                self.stopped = true
            }
            _ => return false,
        }
        true
    }
}

pub(super) struct Monitor {
    child: OwnedChild,
    input: Option<ChildStdin>,
    reader: Option<JoinHandle<()>>,
    done: mpsc::Receiver<bool>,
    failed: Arc<dyn Fn() + Send + Sync>,
    stopping: Arc<std::sync::atomic::AtomicBool>,
    stderr: StderrTail,
}
impl Monitor {
    pub(super) fn start(
        record: impl Fn(Frame) + Send + 'static,
        failed: impl Fn() + Send + Sync + 'static,
    ) -> Option<Self> {
        let failed: Arc<dyn Fn() + Send + Sync> = Arc::new(failed);
        let mut command = Command::new("/usr/bin/osascript");
        command
            .args(["-l", "JavaScript", "-e", SCRIPT])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut command = OwnedCommand::from_command(command);
        command.kill_on_parent_exit(true);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => {
                failed();
                return None;
            }
        };
        let input = child.take_stdin();
        let Some(output) = child.take_stdout() else {
            failed();
            return None;
        };
        if input.is_none() {
            failed();
            return None;
        }
        let Some(stderr) = child.take_stderr() else {
            failed();
            return None;
        };
        let stderr = match StderrTail::try_capture(stderr, 4096, |line| Some(line.into()), |_| {}) {
            Ok(stderr) => stderr,
            Err(_) => {
                failed();
                return None;
            }
        };
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (done_tx, done) = mpsc::sync_channel(1);
        let thread_failed = failed.clone();
        let stopping = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader_stopping = stopping.clone();
        let reader = std::thread::spawn(move || {
            let mut output = BufReader::new(output);
            let mut protocol = Protocol::default();
            let mut valid = true;
            loop {
                let mut line = Vec::new();
                // Bound allocation before JSON parsing, including a malicious endless line.
                let count = output
                    .by_ref()
                    .take((MAX_FRAME + 1) as u64)
                    .read_until(b'\n', &mut line);
                match count {
                    Ok(0) => break,
                    Ok(n) if n <= MAX_FRAME && line.last() == Some(&b'\n') => {}
                    _ => {
                        valid = false;
                        break;
                    }
                }
                let frame = match serde_json::from_slice::<Frame>(&line) {
                    Ok(frame) if protocol.accept(&frame) => frame,
                    _ => {
                        valid = false;
                        break;
                    }
                };
                if frame.kind == "stopped"
                    && !reader_stopping.load(std::sync::atomic::Ordering::SeqCst)
                {
                    valid = false;
                    break;
                }
                match frame.kind.as_str() {
                    "focus" => record(frame),
                    "ready" => {
                        let _ = ready_tx.try_send(());
                    }
                    _ => {}
                }
            }
            let success = valid && protocol.stopped;
            if !success {
                thread_failed();
            }
            let _ = done_tx.send(success);
        });
        let monitor = Self {
            child,
            input,
            reader: Some(reader),
            done,
            failed,
            stopping,
            stderr,
        };
        if ready_rx.recv_timeout(START_TIMEOUT).is_err() {
            (monitor.failed)();
            drop(monitor);
            return None;
        }
        Some(monitor)
    }
}
impl Drop for Monitor {
    fn drop(&mut self) {
        // EOF is a native async file-handle notification, not a timer or JS worker.
        self.stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        drop(self.input.take());
        let drained = self.done.recv_timeout(STOP_TIMEOUT).unwrap_or(false);
        if !drained {
            (self.failed)();
        }
        match self.child.wait_timeout(STOP_TIMEOUT) {
            Ok(Some(status)) if status.success() => {}
            _ => {
                (self.failed)();
                let _ = self.child.terminate_tree_bounded(STOP_TIMEOUT);
            }
        }
        // A reader only joins after EOF/protocol termination. Child Drop owns final reap.
        if self
            .reader
            .as_ref()
            .is_some_and(|reader| reader.is_finished())
        {
            let _ = self.reader.take().unwrap().join();
        }
        let _ = self.stderr.finish_within(STOP_TIMEOUT);
    }
}
