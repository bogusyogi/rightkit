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

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(sequence: u64, kind: &str) -> Frame {
        let focus = kind == "focus";
        Frame {
            version: 1,
            sequence,
            kind: kind.into(),
            pid: focus.then_some(42),
            name: focus.then(|| "Fixture".into()),
            executable: focus.then(|| "/fixture/app".into()),
        }
    }
    #[test]
    fn protocol_preserves_transient_order_and_requires_snapshot_before_ready() {
        let mut protocol = Protocol::default();
        assert!(!protocol.accept(&frame(0, "ready")));
        let mut protocol = Protocol::default();
        for (seq, kind) in ["focus", "ready", "focus", "focus", "stopped"]
            .iter()
            .enumerate()
        {
            assert!(protocol.accept(&frame(seq as u64, kind)));
        }
        assert!(!protocol.accept(&frame(5, "focus")));
        assert!(protocol.stopped);
    }
    #[test]
    fn protocol_rejects_gaps_versions_missing_identity_and_early_stop() {
        for bad in [frame(1, "focus"), frame(0, "stopped"), frame(0, "invalid")] {
            assert!(!Protocol::default().accept(&bad));
        }
        let mut bad = frame(0, "focus");
        bad.version = 2;
        assert!(!Protocol::default().accept(&bad));
        bad.version = 1;
        bad.executable = None;
        assert!(!Protocol::default().accept(&bad));
    }
    #[test]
    fn startup_history_can_be_reconciled_after_executable_registration() {
        let event = frame(0, "focus");
        assert!(!owned_frame(&event, &[], None));
        assert!(owned_frame(&event, &[], Some("/fixture/app")));
        assert!(owned_frame(&event, &[42], None));
        assert!(!owned_frame(&event, &[], Some("/fixture/ap")));
    }
    #[test]
    fn immediate_stop_requires_a_post_ready_snapshot() {
        let mut protocol = Protocol::default();
        assert!(protocol.accept(&frame(0, "focus")));
        assert!(protocol.accept(&frame(1, "ready")));
        assert!(!protocol.accept(&frame(2, "stopped")));
        let mut protocol = Protocol::default();
        for (sequence, kind) in ["focus", "ready", "focus", "stopped"].iter().enumerate() {
            assert!(protocol.accept(&frame(sequence as u64, kind)));
        }
        assert!(protocol.stopped);
    }
    #[test]
    fn eof_without_stopped_never_establishes_coverage() {
        for kinds in [
            vec![],
            vec!["focus"],
            vec!["focus", "ready"],
            vec!["focus", "ready", "focus"],
        ] {
            let mut protocol = Protocol::default();
            for (sequence, kind) in kinds.iter().enumerate() {
                assert!(protocol.accept(&frame(sequence as u64, kind)));
            }
            assert!(!protocol.stopped);
        }
    }
    // Producer supplies two distinct, already-running disposable GUI fixture PIDs.
    // No Finder/System Events AppleEvents or user applications are used as fixtures.
    // Serialize native probes in an active login session; run under an external deadline.
    #[test]
    #[ignore = "native foreground fixture: requires two disposable GUI apps in an active login session"]
    fn native_helper_records_rapid_activate_and_return() {
        let target: u32 = std::env::var("RIGHTKIT_QA_NATIVE_FOCUS_TARGET_PID")
            .expect("target fixture PID")
            .parse()
            .expect("numeric target PID");
        let return_pid: u32 = std::env::var("RIGHTKIT_QA_NATIVE_FOCUS_RETURN_PID")
            .expect("return fixture PID")
            .parse()
            .expect("numeric return PID");
        let target_executable = std::env::var("RIGHTKIT_QA_NATIVE_FOCUS_TARGET_EXECUTABLE")
            .expect("exact target fixture executable");
        let return_executable = std::env::var("RIGHTKIT_QA_NATIVE_FOCUS_RETURN_EXECUTABLE")
            .expect("exact return fixture executable");
        assert!(target > 0 && return_pid > 0 && target != return_pid);
        assert!(target_executable.starts_with('/') && return_executable.starts_with('/'));
        let (tx, rx) = mpsc::channel();
        let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let failures = failed.clone();
        let monitor = Monitor::start(
            move |frame| {
                let _ = tx.send(frame);
            },
            move || {
                failures.store(true, std::sync::atomic::Ordering::SeqCst);
            },
        )
        .expect("native observer ready");
        // Activate return first, then target, then return with no dwell or sample timer.
        // NSWorkspace confirmation establishes every transition actually happened.
        const ACTIVATE: &str = r#"
ObjC.import('Cocoa');
function run(args) {
    var workspace = $.NSWorkspace.sharedWorkspace;
    [[Number(args[1]), args[3]], [Number(args[0]), args[2]], [Number(args[1]), args[3]]].forEach(function(fixture) {
        var pid = fixture[0];
        var app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(pid);
        if (!app || app.isNil() || ObjC.unwrap(app.executableURL.path) !== fixture[1])
            throw new Error('fixture identity mismatch');
        if (!app.activateWithOptions($.NSApplicationActivateIgnoringOtherApps))
            throw new Error('fixture activation refused');
        var deadline = Date.now() + 3000;
        while (Number(workspace.frontmostApplication.processIdentifier) !== pid) {
            if (Date.now() >= deadline) throw new Error('fixture activation deadline');
            $.NSRunLoop.currentRunLoop.runUntilDate($.NSDate.dateWithTimeIntervalSinceNow(0.001));
        }
    });
}
"#;
        let mut command = Command::new("/usr/bin/osascript");
        command
            .args([
                "-l",
                "JavaScript",
                "-e",
                ACTIVATE,
                "--",
                &target.to_string(),
                &return_pid.to_string(),
                &target_executable,
                &return_executable,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut command = OwnedCommand::from_command(command);
        command.kill_on_parent_exit(true);
        let mut activator = command.spawn().expect("native fixture activator");
        let status = activator
            .wait_timeout(Duration::from_secs(10))
            .expect("fixture wait");
        if status.is_none() {
            let _ = activator.terminate_tree_bounded(STOP_TIMEOUT);
        }
        assert!(
            status.is_some_and(|status| status.success()),
            "fixture activation failed"
        );
        drop(monitor); // EOF must drain pending transitions before stopped.
        assert!(!failed.load(std::sync::atomic::Ordering::SeqCst));
        let frames: Vec<Frame> = rx.try_iter().collect();
        let target_index = frames
            .iter()
            .position(|frame| {
                frame.pid == Some(target)
                    && frame.executable.as_deref() == Some(target_executable.as_str())
            })
            .expect("transient target activation retained");
        let is_return = |frame: &Frame| {
            frame.pid == Some(return_pid)
                && frame.executable.as_deref() == Some(return_executable.as_str())
        };
        assert!(
            frames[..target_index].iter().any(is_return),
            "initial return activation retained"
        );
        assert!(
            frames[target_index + 1..].iter().any(is_return),
            "return activation retained"
        );
    }
    #[test]
    #[ignore = "native helper loss qualification: requires macOS foreground desktop"]
    fn native_helper_loss_fails_closed() {
        let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let failures = failed.clone();
        let mut monitor = Monitor::start(
            |_| {},
            move || {
                failures.store(true, std::sync::atomic::Ordering::SeqCst);
            },
        )
        .expect("native observer ready");
        monitor
            .child
            .terminate_tree_bounded(STOP_TIMEOUT)
            .expect("kill owned helper");
        assert!(!monitor
            .done
            .recv_timeout(STOP_TIMEOUT)
            .expect("reader observes EOF"));
        assert!(failed.load(std::sync::atomic::Ordering::SeqCst));
        drop(monitor);
    }
    #[test]
    #[ignore = "native JXA bridge qualification: run on macOS with a foreground desktop"]
    fn native_helper_starts_and_stops_without_focus_activity() {
        let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let failures = failed.clone();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let samples = count.clone();
        let monitor = Monitor::start(
            move |_| {
                samples.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
            move || {
                failures.store(true, std::sync::atomic::Ordering::SeqCst);
            },
        )
        .expect("native helper ready after runloop entry");
        drop(monitor);
        assert!(!failed.load(std::sync::atomic::Ordering::SeqCst));
        assert!(count.load(std::sync::atomic::Ordering::SeqCst) >= 2);
    }
}
