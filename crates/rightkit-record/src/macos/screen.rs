//! Whole-screen capture on macOS (rightkit#443): [`MacScreen`] runs the Swift helper's
//! `capture-screen` mode, which captures a display (or every display composited) with
//! ScreenCaptureKit and writes the lossless `screen.rkv` itself (`ScreenCapture.swift`, the same
//! `RKV1` format as [`crate::rkv`]). The helper is launched and reaped through
//! `rightkit-child-process`, like the window backend's helper.
//!
//! The helper's stdout carries one JSON object per line, nothing else:
//!
//! - `{"event":"started","hostNsAtStart":N,"width":W,"height":H,"osBuild":"...","monitors":[...]}`
//! - `{"event":"dropped","hostNs":N,"count":C}` when frames were received and lost
//! - `{"event":"failed","code":"permission_denied|...","message":"..."}`, after which it exits
//! - `{"event":"finished","clean":true,"frames":N,"dropped":D}`, after the stop line
//!
//! Its stdin takes one line, `stop`, which finalizes the video; closing stdin does the same.
//! Frames are stamped with `CLOCK_UPTIME_RAW` nanoseconds, the clock of [`uptime_ns`], which is what
//! [`crate::ScreenRecorder::mark`] stamps markers with. Screen Recording permission belongs to the
//! helper identity (see the module documentation of `macos`); the helper fails closed with
//! `permission_denied` before writing anything.
//!
//! Not built or run: written without compiling; the next gate run on a Mac is the first compile.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rightkit_child_process::{OwnedChild, OwnedCommand, StderrTail};
use serde_json::{json, Value};

use super::{uptime_ns, EXIT_GRACE, HELPER_BINARY, HELPER_ENV, STDERR_TAIL_BYTES};
use crate::backend::Failure;
use crate::error::{Code, RecordError, Result};
use crate::macos::MacBackend;
use crate::screen::{
    MonitorInfo, ScreenBackend, ScreenEvent, ScreenFinished, ScreenSpec, ScreenStart,
};

/// How long the helper gets to report `started` (permission, display discovery, stream start).
const START_WAIT: Duration = Duration::from_secs(30);
/// How long the helper gets to finalize the video after `stop`.
const FINISH_WAIT: Duration = Duration::from_secs(90);

struct Running {
    child: OwnedChild,
    stderr: StderrTail,
    stdin: Option<ChildStdin>,
    events: Receiver<Value>,
    reader: Option<JoinHandle<()>>,
}

/// The macOS side of [`ScreenBackend`]. One take at a time; the helper is launched by `start`.
pub struct MacScreen {
    helper: PathBuf,
    running: Option<Running>,
    /// The helper's stream ended and the loss was already reported by `poll`.
    exit_reported: bool,
}

impl MacScreen {
    /// The helper at [`MacBackend::helper_path`] (`RIGHTKIT_RECORD_MACOS_HELPER`, or next to the
    /// running executable).
    pub fn launch_default() -> Result<MacScreen> {
        let helper = MacBackend::helper_path().ok_or_else(|| {
            RecordError::new(
                Code::NotFound,
                format!("{HELPER_BINARY} not found; set {HELPER_ENV} or build helpers/macos"),
            )
        })?;
        Ok(MacScreen::with_helper(helper))
    }

    pub fn with_helper(helper: impl Into<PathBuf>) -> MacScreen {
        MacScreen {
            helper: helper.into(),
            running: None,
            exit_reported: false,
        }
    }

    fn command(&self, args: &[&str]) -> OwnedCommand {
        let mut command = OwnedCommand::new(&self.helper);
        command
            .command_mut()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}

fn helper_error(code: &str, message: &str) -> RecordError {
    RecordError::new(Code::parse(code).unwrap_or(Code::Helper), message)
}

/// The failure a `failed` event names.
fn failure_of(event: &Value) -> Failure {
    let message = event["message"].as_str().unwrap_or("").to_string();
    match event["code"].as_str().unwrap_or("") {
        "permission_revoked" => Failure::PermissionRevoked,
        "disk_full" => Failure::DiskFull,
        _ => Failure::Io(message),
    }
}

fn monitors_of(value: &Value) -> Vec<MonitorInfo> {
    value
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|m| {
                    Some(MonitorInfo {
                        index: u32::try_from(m["index"].as_u64()?).ok()?,
                        x: i32::try_from(m["x"].as_i64()?).ok()?,
                        y: i32::try_from(m["y"].as_i64()?).ok()?,
                        width: u32::try_from(m["width"].as_u64()?).ok()?,
                        height: u32::try_from(m["height"].as_u64()?).ok()?,
                        primary: m["primary"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Reads the helper's stdout line by line into the channel until it closes.
fn read_events(stdout: impl Read + Send + 'static, tx: mpsc::Sender<Value>) -> JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if tx.send(value).is_err() {
                    break;
                }
            }
        }
    })
}

impl Running {
    /// Gives a helper that has finished a moment to exit, then terminates it, and joins the reader.
    fn reap(mut self) {
        drop(self.stdin.take());
        if !matches!(self.child.wait_timeout(EXIT_GRACE), Ok(Some(_))) {
            let _ = self.child.terminate_tree();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl ScreenBackend for MacScreen {
    fn monitors(&self) -> Result<Vec<MonitorInfo>> {
        let mut child = self.command(&["screens"]).spawn().map_err(|e| {
            RecordError::new(
                Code::Helper,
                format!("the helper {} could not start: {e}", self.helper.display()),
            )
        })?;
        let (Some(stdin), Some(mut stdout), Some(stderr)) =
            (child.take_stdin(), child.take_stdout(), child.take_stderr())
        else {
            return Err(RecordError::new(Code::Helper, "the helper has no stdio"));
        };
        drop(stdin);
        let mut stderr = StderrTail::plain(stderr, STDERR_TAIL_BYTES);
        let mut text = String::new();
        let read = stdout.read_to_string(&mut text);
        if !matches!(child.wait_timeout(EXIT_GRACE), Ok(Some(_))) {
            let _ = child.terminate_tree();
        }
        let tail = stderr.finish_within(Duration::from_secs(1));
        read.map_err(|e| RecordError::new(Code::Helper, format!("reading the helper: {e}")))?;
        let value: Value = serde_json::from_str(text.trim()).map_err(|_| {
            RecordError::new(
                Code::Helper,
                format!("the helper's screen list is not JSON (stderr: {tail})"),
            )
        })?;
        if value["event"] == "failed" {
            return Err(helper_error(
                value["code"].as_str().unwrap_or(""),
                value["message"].as_str().unwrap_or("the helper failed"),
            ));
        }
        Ok(monitors_of(&value["monitors"]))
    }

    fn start(&mut self, spec: &ScreenSpec) -> Result<ScreenStart> {
        if self.running.is_some() {
            return Err(RecordError::new(
                Code::AlreadyRecording,
                "the macOS screen backend already has a take",
            ));
        }
        let fps = spec.fps.to_string();
        let target = spec.target.to_arg();
        let out = spec.out.to_string_lossy().into_owned();
        let mut child = self
            .command(&[
                "capture-screen",
                "--out",
                &out,
                "--fps",
                &fps,
                "--target",
                &target,
            ])
            .spawn()
            .map_err(|e| {
                RecordError::new(
                    Code::Helper,
                    format!("the helper {} could not start: {e}", self.helper.display()),
                )
            })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.take_stdin(), child.take_stdout(), child.take_stderr())
        else {
            let _ = child.terminate_tree();
            return Err(RecordError::new(Code::Helper, "the helper has no stdio"));
        };
        let stderr = StderrTail::plain(stderr, STDERR_TAIL_BYTES);
        let (tx, events) = mpsc::channel::<Value>();
        let reader = read_events(stdout, tx);
        let mut running = Running {
            child,
            stderr,
            stdin: Some(stdin),
            events,
            reader: Some(reader),
        };
        let first = running.events.recv_timeout(START_WAIT);
        let started = match first {
            Ok(value) if value["event"] == "started" => value,
            Ok(value) => {
                let error = helper_error(
                    value["code"].as_str().unwrap_or(""),
                    value["message"]
                        .as_str()
                        .unwrap_or("the helper did not start capture"),
                );
                let _ = running.child.terminate_tree();
                running.reap();
                return Err(error);
            }
            Err(failure) => {
                let _ = running.child.terminate_tree();
                let tail = running.stderr.finish_within(Duration::from_secs(1));
                running.reap();
                return Err(RecordError::new(
                    Code::Helper,
                    match failure {
                        RecvTimeoutError::Timeout => {
                            format!("the helper did not report a start in time (stderr: {tail})")
                        }
                        RecvTimeoutError::Disconnected => {
                            format!("the helper ended before it started (stderr: {tail})")
                        }
                    },
                ));
            }
        };
        let (Some(width), Some(height), Some(host_ns_at_start)) = (
            started["width"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok()),
            started["height"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok()),
            started["hostNsAtStart"].as_u64(),
        ) else {
            let _ = running.child.terminate_tree();
            running.reap();
            return Err(RecordError::new(
                Code::Helper,
                "the helper's started event is missing its size or clock",
            ));
        };
        self.exit_reported = false;
        self.running = Some(running);
        Ok(ScreenStart {
            host_ns_at_start,
            width,
            height,
            clock: "uptime_ns",
            os_build: started["osBuild"].as_str().unwrap_or("unknown").to_string(),
            monitors: monitors_of(&started["monitors"]),
        })
    }

    /// `CLOCK_UPTIME_RAW`, the clock the helper stamps frames with.
    fn host_ns(&self) -> Option<u64> {
        Some(uptime_ns())
    }

    fn poll(&mut self) -> Vec<ScreenEvent> {
        let Some(running) = self.running.as_mut() else {
            return Vec::new();
        };
        let mut batch = Vec::new();
        loop {
            match running.events.try_recv() {
                Ok(value) => match value["event"].as_str().unwrap_or("") {
                    "dropped" => batch.push(ScreenEvent::Dropped {
                        host_ns: value["hostNs"].as_u64().unwrap_or(0),
                        count: value["count"]
                            .as_u64()
                            .and_then(|c| u32::try_from(c).ok())
                            .unwrap_or(1),
                    }),
                    "failed" => batch.push(ScreenEvent::Failed(failure_of(&value))),
                    _ => {}
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.exit_reported {
                        self.exit_reported = true;
                        batch.push(ScreenEvent::Exited);
                    }
                    break;
                }
            }
        }
        batch
    }

    fn stop(&mut self) -> Result<ScreenFinished> {
        let mut running = self.running.take().ok_or_else(|| {
            RecordError::new(
                Code::NotRecording,
                "the macOS screen backend has no take to stop",
            )
        })?;
        if let Some(mut stdin) = running.stdin.take() {
            // A helper that already ended has closed its end; the finished event (or its absence)
            // below says how it went.
            let _ = stdin.write_all(b"stop\n").and_then(|()| stdin.flush());
        }
        let mut dropped_seen = 0u64;
        let mut finished: Option<Value> = None;
        let mut failure: Option<Value> = None;
        while let Ok(value) = running.events.recv_timeout(FINISH_WAIT) {
            match value["event"].as_str().unwrap_or("") {
                "finished" => {
                    finished = Some(value);
                    break;
                }
                "failed" => failure = Some(value),
                "dropped" => {
                    dropped_seen += value["count"].as_u64().unwrap_or(0);
                }
                _ => {}
            }
        }
        let tail = running.stderr.snapshot();
        running.reap();
        let Some(done) = finished else {
            let detail = failure
                .as_ref()
                .and_then(|f| f["message"].as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("no finished event (stderr: {tail})"));
            return Err(RecordError::new(
                Code::Helper,
                format!("the helper did not finish the video: {detail}"),
            ));
        };
        Ok(ScreenFinished {
            clean: done["clean"].as_bool().unwrap_or(false) && failure.is_none(),
            frames: done["frames"].as_u64().unwrap_or(0),
            dropped: done["dropped"].as_u64().unwrap_or(dropped_seen),
            summary: json!({ "helper": done }),
        })
    }

    fn abort(&mut self) {
        if let Some(mut running) = self.running.take() {
            let _ = running.child.terminate_tree();
            running.reap();
        }
    }
}
