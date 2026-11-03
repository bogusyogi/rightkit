//! Whole-screen capture with step markers (rightkit#443): [`ScreenRecorder`] records one monitor or
//! the whole virtual screen at up to 120 fps into a lossless [`crate::rkv`] file, stamps every
//! frame with the capture clock, and lets the caller drop labelled markers on that same clock to
//! line a take up with the steps it was driving.
//!
//! This is the sibling of the window [`crate::Recorder`]. It has no scenario, redaction or bundle
//! manifest: a take is a directory that holds
//!
//! | file | content |
//! |---|---|
//! | `screen.rkv` | the frames, each with its `hostNs` ([`crate::rkv`]) |
//! | `markers.ndjson` | one `{"seq","label","hostNs","utcMs"}` line per [`ScreenRecorder::mark`] (or the path in [`ScreenSpec::markers`]) |
//! | `screen.json` | what was captured and how it ended |
//!
//! `hostNs` is on the clock named by `screen.json` `clock`: QPC nanoseconds on Windows
//! (`qpc_ns`), `CLOCK_UPTIME_RAW` nanoseconds on macOS (`uptime_ns`). Frames and markers share it,
//! so "the frames of step 3" is the frames whose `hostNs` lies between two markers
//! ([`Marker::active_at`]). [`ScreenReader`] yields the frames with their timestamps and the
//! markers back; nothing needs ffmpeg.
//!
//! Capture delivers a frame when the screen changes, so a static screen produces no frames and an
//! arrival gap is not a drop. Frames the capture received and then lost (a full queue, a size
//! change mid-take) are counted in `dropped`.
//!
//! The recorder never sends input and needs the operating system's screen capture consent: Screen
//! Recording permission on macOS, none on Windows (a yellow capture border is shown unless
//! borderless access is granted).

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::backend::Failure;
use crate::error::{io_err, Code, RecordError, Result};
use crate::rkv::{LosslessReader, RawFrame};

pub const VIDEO_FILE: &str = "screen.rkv";
pub const SUMMARY_FILE: &str = "screen.json";
pub const MARKERS_FILE: &str = "markers.ndjson";
/// `screen.json` `schema`.
pub const SUMMARY_SCHEMA: &str = "rightkit-record.screen.v1";
/// Longest marker label, in bytes.
pub const LABEL_MAX: usize = 512;
const MAX_FPS: u32 = 120;

/// What to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenTarget {
    /// One monitor by index: 0 is the primary monitor, the rest follow left to right, then top to bottom.
    Monitor(u32),
    /// Every monitor composited at its place on the virtual desktop; gaps are black.
    Virtual,
}

impl ScreenTarget {
    /// `virtual`, or `monitor:N` (a bare `N` is also a monitor index).
    pub fn parse(text: &str) -> Option<ScreenTarget> {
        let text = text.trim();
        if text == "virtual" {
            return Some(ScreenTarget::Virtual);
        }
        let index = text.strip_prefix("monitor:").unwrap_or(text);
        index.parse().ok().map(ScreenTarget::Monitor)
    }

    /// The text [`ScreenTarget::parse`] reads back.
    pub fn to_arg(self) -> String {
        match self {
            ScreenTarget::Virtual => "virtual".to_string(),
            ScreenTarget::Monitor(index) => format!("monitor:{index}"),
        }
    }
}

/// A monitor the backend can capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    pub index: u32,
    /// Position of the top left corner on the virtual desktop, in pixels.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

impl MonitorInfo {
    pub fn to_json(&self) -> Value {
        json!({
            "index": self.index,
            "x": self.x,
            "y": self.y,
            "width": self.width,
            "height": self.height,
            "primary": self.primary,
        })
    }
}

/// The request for one whole-screen take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenSpec {
    pub target: ScreenTarget,
    /// Frames per second to keep at most, 1 to 120.
    pub fps: u32,
    /// Must be true: whole-screen capture is lossless (a compressed whole-screen take is not offered).
    pub lossless: bool,
    /// The take directory. It is created; it must not exist or must be empty.
    pub out: PathBuf,
    /// Where markers are written. Default: `markers.ndjson` in `out`. Must not exist.
    pub markers: Option<PathBuf>,
}

impl ScreenSpec {
    pub fn new(
        target: ScreenTarget,
        fps: u32,
        lossless: bool,
        out: impl Into<PathBuf>,
    ) -> ScreenSpec {
        ScreenSpec {
            target,
            fps,
            lossless,
            out: out.into(),
            markers: None,
        }
    }

    /// The path markers are written to.
    pub fn markers_path(&self) -> PathBuf {
        self.markers
            .clone()
            .unwrap_or_else(|| self.out.join(MARKERS_FILE))
    }

    fn validate(&self) -> Result<()> {
        if !(1..=MAX_FPS).contains(&self.fps) {
            return Err(RecordError::new(
                Code::ArgInvalid,
                format!("fps must be between 1 and {MAX_FPS}, got {}", self.fps),
            ));
        }
        if !self.lossless {
            return Err(RecordError::new(
                Code::Unsupported,
                "whole-screen capture is lossless only; a compressed take of one window is the window recorder's job",
            ));
        }
        if self.out.as_os_str().is_empty() {
            return Err(RecordError::new(Code::ArgInvalid, "out must not be empty"));
        }
        Ok(())
    }
}

/// What a backend reports once capture is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenStart {
    /// The capture clock just before the first frame could arrive.
    pub host_ns_at_start: u64,
    /// Frame size in pixels.
    pub width: u32,
    pub height: u32,
    /// `qpc_ns` or `uptime_ns`.
    pub clock: &'static str,
    pub os_build: String,
    /// The monitors that make up the frame, with their place in it (origin of the frame is the
    /// top left of their union).
    pub monitors: Vec<MonitorInfo>,
}

/// Something that happened during a take.
#[derive(Debug, Clone, PartialEq)]
pub enum ScreenEvent {
    /// Frames received and then lost.
    Dropped { host_ns: u64, count: u32 },
    /// The take cannot continue and ends not clean.
    Failed(Failure),
    /// The helper process ended without being asked to stop.
    Exited,
}

/// How the capture ended.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenFinished {
    /// False when the video could not be finalized cleanly.
    pub clean: bool,
    /// Frames written to `screen.rkv`.
    pub frames: u64,
    /// Frames received and lost.
    pub dropped: u64,
    /// Backend figures for `screen.json` (arrival intervals, losses, border state).
    pub summary: Value,
}

/// The per-OS side: Windows.Graphics.Capture monitor items in `windows`, ScreenCaptureKit displays
/// through the Swift helper in `macos`. A backend writes `screen.rkv` into the spec's directory.
pub trait ScreenBackend {
    /// The monitors that can be captured, in [`ScreenTarget::Monitor`] index order.
    fn monitors(&self) -> Result<Vec<MonitorInfo>>;
    /// Starts capture and writes `screen.rkv` under `spec.out`. Refuses with `Code::NotFound` for a
    /// monitor index that does not exist and `Code::PermissionDenied` when the OS refuses capture.
    fn start(&mut self, spec: &ScreenSpec) -> Result<ScreenStart>;
    /// The current time on the clock frames are stamped with, once started.
    fn host_ns(&self) -> Option<u64>;
    /// Drains events since the last call. Never blocks.
    fn poll(&mut self) -> Vec<ScreenEvent>;
    /// Stops capture and finalizes the video.
    fn stop(&mut self) -> Result<ScreenFinished>;
    /// Ends capture without a clean finish; frames already on disk are kept.
    fn abort(&mut self);
}

/// The platform's backend, started fresh.
pub fn default_backend() -> Result<Box<dyn ScreenBackend>> {
    #[cfg(windows)]
    {
        Ok(Box::new(crate::windows::WindowsScreen::new()))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(crate::macos::MacScreen::launch_default()?))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Err(RecordError::new(
            Code::Unsupported,
            "whole-screen capture is implemented for Windows and macOS only",
        ))
    }
}

/// The monitors of this machine, through the platform's backend.
pub fn list_monitors() -> Result<Vec<MonitorInfo>> {
    default_backend()?.monitors()
}

/// One marker: a label stamped on the frames' clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub seq: u64,
    pub label: String,
    /// On the same clock as the frames' `host_ns`.
    pub host_ns: u64,
    /// Wall clock, milliseconds since the Unix epoch.
    pub utc_ms: u64,
}

impl Marker {
    pub fn to_json(&self) -> Value {
        json!({
            "seq": self.seq,
            "label": self.label,
            "hostNs": self.host_ns,
            "utcMs": self.utc_ms,
        })
    }

    fn from_json(value: &Value) -> Option<Marker> {
        Some(Marker {
            seq: value.get("seq")?.as_u64()?,
            label: value.get("label")?.as_str()?.to_string(),
            host_ns: value.get("hostNs")?.as_u64()?,
            utc_ms: value.get("utcMs")?.as_u64()?,
        })
    }

    /// The marker in force at `host_ns`: the last one stamped at or before it. `markers` must be
    /// in file order (as [`ScreenReader::markers`] returns them).
    pub fn active_at(markers: &[Marker], host_ns: u64) -> Option<&Marker> {
        markers
            .iter()
            .rev()
            .find(|marker| marker.host_ns <= host_ns)
    }
}

fn utc_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// How a take ended.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenTake {
    pub out: PathBuf,
    pub frames: u64,
    pub dropped: u64,
    pub markers: u64,
    pub width: u32,
    pub height: u32,
    pub clean: bool,
    /// Why the take could not be clean, when it could not.
    pub failure: Option<String>,
    /// `screen.json` as written.
    pub summary: Value,
}

/// A running whole-screen take. Dropping it without [`ScreenRecorder::stop`] aborts the capture.
pub struct ScreenRecorder {
    backend: Option<Box<dyn ScreenBackend>>,
    spec: ScreenSpec,
    started: ScreenStart,
    markers: BufWriter<File>,
    marker_count: u64,
    dropped: u64,
    failure: Option<Failure>,
}

impl ScreenRecorder {
    /// The platform's backend ([`default_backend`]).
    pub fn start_default(spec: ScreenSpec) -> Result<ScreenRecorder> {
        ScreenRecorder::start(spec, default_backend()?)
    }

    /// Creates the take directory and the markers file, then starts `backend`. Nothing is left
    /// behind when the backend refuses: a directory this call created is removed again.
    pub fn start(spec: ScreenSpec, mut backend: Box<dyn ScreenBackend>) -> Result<ScreenRecorder> {
        spec.validate()?;
        let existed = spec.out.exists();
        if existed {
            let empty = fs::read_dir(&spec.out)
                .map_err(|e| io_err("read the output directory", &e))?
                .next()
                .is_none();
            if !empty {
                return Err(RecordError::new(
                    Code::OutExists,
                    format!("output directory is not empty: {}", spec.out.display()),
                ));
            }
        }
        fs::create_dir_all(&spec.out).map_err(|e| io_err("create the output directory", &e))?;
        let cleanup = |spec: &ScreenSpec| {
            if !existed {
                let _ = fs::remove_dir_all(&spec.out);
            }
        };
        let markers_path = spec.markers_path();
        let markers = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&markers_path)
        {
            Ok(file) => BufWriter::new(file),
            Err(e) => {
                cleanup(&spec);
                return Err(if e.kind() == io::ErrorKind::AlreadyExists {
                    RecordError::new(
                        Code::OutExists,
                        format!("{} already exists", markers_path.display()),
                    )
                } else {
                    io_err("create the markers file", &e)
                });
            }
        };
        let started = match backend.start(&spec) {
            Ok(started) => started,
            Err(e) => {
                drop(markers);
                let _ = fs::remove_file(&markers_path);
                cleanup(&spec);
                return Err(e);
            }
        };
        Ok(ScreenRecorder {
            backend: Some(backend),
            spec,
            started,
            markers,
            marker_count: 0,
            dropped: 0,
            failure: None,
        })
    }

    /// What the backend reported at start.
    pub fn started(&self) -> &ScreenStart {
        &self.started
    }

    /// Stamps `label` on the frames' clock and appends it to the markers file (flushed at once, so
    /// a killed take keeps it). Labels are at most [`LABEL_MAX`] bytes.
    pub fn mark(&mut self, label: &str) -> Result<Marker> {
        if label.len() > LABEL_MAX {
            return Err(RecordError::new(
                Code::ArgInvalid,
                format!("a marker label is at most {LABEL_MAX} bytes"),
            ));
        }
        let host_ns = self
            .backend
            .as_ref()
            .and_then(|backend| backend.host_ns())
            .ok_or_else(|| {
                RecordError::new(Code::NotRecording, "the capture clock cannot be read")
            })?;
        let marker = Marker {
            seq: self.marker_count,
            label: label.to_string(),
            host_ns,
            utc_ms: utc_ms(),
        };
        writeln!(self.markers, "{}", marker.to_json())
            .and_then(|()| self.markers.flush())
            .map_err(|e| io_err("write a marker", &e))?;
        self.marker_count += 1;
        Ok(marker)
    }

    /// Drains what the backend has reported. Returns the failure that ended the take, if one has.
    pub fn pump(&mut self) -> Option<&Failure> {
        if let Some(backend) = self.backend.as_mut() {
            for event in backend.poll() {
                match event {
                    ScreenEvent::Dropped { count, .. } => {
                        self.dropped = self.dropped.saturating_add(u64::from(count));
                    }
                    ScreenEvent::Failed(failure) => {
                        self.failure.get_or_insert(failure);
                    }
                    ScreenEvent::Exited => {
                        self.failure.get_or_insert(Failure::HelperLost);
                    }
                }
            }
        }
        self.failure.as_ref()
    }

    /// Stops capture and writes `screen.json`. The take is not clean when the backend failed
    /// mid-take or could not finalize the video; the frames written so far are kept either way.
    pub fn stop(mut self) -> Result<ScreenTake> {
        self.pump();
        let mut backend = self
            .backend
            .take()
            .ok_or_else(|| RecordError::new(Code::NotRecording, "the take has ended"))?;
        let finished = if self.failure.is_some() {
            backend.abort();
            None
        } else {
            Some(backend.stop()?)
        };
        // Events produced while stopping (a late drop count) land here.
        for event in backend.poll() {
            if let ScreenEvent::Dropped { count, .. } = event {
                self.dropped = self.dropped.saturating_add(u64::from(count));
            }
        }
        let (clean, frames, summary) = match finished {
            Some(done) => {
                self.dropped = self.dropped.max(done.dropped);
                (done.clean, done.frames, done.summary)
            }
            None => (
                false,
                count_frames(&self.spec.out.join(VIDEO_FILE)),
                Value::Null,
            ),
        };
        self.markers
            .flush()
            .map_err(|e| io_err("flush the markers file", &e))?;
        let failure = self.failure.as_ref().map(Failure::reason);
        let markers_name = match self.spec.markers.as_deref() {
            Some(path) => path.to_string_lossy().into_owned(),
            None => MARKERS_FILE.to_string(),
        };
        let monitors: Vec<Value> = self
            .started
            .monitors
            .iter()
            .map(MonitorInfo::to_json)
            .collect();
        let document = json!({
            "schema": SUMMARY_SCHEMA,
            "clock": self.started.clock,
            "hostNsAtStart": self.started.host_ns_at_start,
            "osBuild": self.started.os_build,
            "target": self.spec.target.to_arg(),
            "fps": self.spec.fps,
            "lossless": true,
            "width": self.started.width,
            "height": self.started.height,
            "monitors": monitors,
            "video": VIDEO_FILE,
            "markersFile": markers_name,
            "markers": self.marker_count,
            "frames": frames,
            "dropped": self.dropped,
            "clean": clean && failure.is_none(),
            "failure": failure,
            "capture": summary,
        });
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|e| RecordError::new(Code::Io, format!("serialize {SUMMARY_FILE}: {e}")))?;
        fs::write(self.spec.out.join(SUMMARY_FILE), bytes)
            .map_err(|e| io_err(&format!("write {SUMMARY_FILE}"), &e))?;
        Ok(ScreenTake {
            out: self.spec.out.clone(),
            frames,
            dropped: self.dropped,
            markers: self.marker_count,
            width: self.started.width,
            height: self.started.height,
            clean: clean && failure.is_none(),
            failure,
            summary: document,
        })
    }

    /// Ends the take without a clean finish. Frames and markers already on disk are kept.
    pub fn abort(mut self) {
        if let Some(mut backend) = self.backend.take() {
            backend.abort();
        }
        let _ = self.markers.flush();
    }
}

impl Drop for ScreenRecorder {
    fn drop(&mut self) {
        if let Some(mut backend) = self.backend.take() {
            backend.abort();
        }
    }
}

/// Whole frames in a video, for a take that ended without a clean finish. 0 when it cannot be read.
fn count_frames(path: &Path) -> u64 {
    let Ok(mut reader) = LosslessReader::open(path) else {
        return 0;
    };
    let mut frames = 0;
    while let Ok(Some(_)) = reader.next_frame() {
        frames += 1;
    }
    frames
}

/// One frame of a take with its timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenFrame {
    pub index: u64,
    /// On the take's clock ([`ScreenReader::clock`]); markers are on the same one.
    pub host_ns: u64,
    pub width: u32,
    pub height: u32,
    /// Packed BGRA, `width * height * 4` bytes.
    pub bgra: Vec<u8>,
}

/// Reads a take back: its frames in order with their timestamps, and its markers.
pub struct ScreenReader {
    dir: PathBuf,
    summary: Value,
    video: LosslessReader,
}

impl ScreenReader {
    pub fn open(dir: &Path) -> Result<ScreenReader> {
        let summary_path = dir.join(SUMMARY_FILE);
        let bytes = fs::read(&summary_path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                RecordError::new(
                    Code::NotFound,
                    format!(
                        "{} not found: not a whole-screen take",
                        summary_path.display()
                    ),
                )
            } else {
                io_err(&format!("read {SUMMARY_FILE}"), &e)
            }
        })?;
        let summary: Value = serde_json::from_slice(&bytes)
            .map_err(|e| RecordError::new(Code::Io, format!("{SUMMARY_FILE} is not JSON: {e}")))?;
        if summary["schema"] != SUMMARY_SCHEMA {
            return Err(RecordError::new(
                Code::NotBundle,
                format!("{SUMMARY_FILE} is not a {SUMMARY_SCHEMA} document"),
            ));
        }
        let video = LosslessReader::open(&dir.join(VIDEO_FILE))?;
        Ok(ScreenReader {
            dir: dir.to_path_buf(),
            summary,
            video,
        })
    }

    /// `screen.json`.
    pub fn summary(&self) -> &Value {
        &self.summary
    }

    /// `qpc_ns` or `uptime_ns`: the clock of frame and marker timestamps.
    pub fn clock(&self) -> &str {
        self.summary["clock"].as_str().unwrap_or("")
    }

    pub fn width(&self) -> u32 {
        self.video.width()
    }

    pub fn height(&self) -> u32 {
        self.video.height()
    }

    /// The take's markers in the order they were made. A cut-off last line (a killed take) is ignored.
    pub fn markers(&self) -> Result<Vec<Marker>> {
        let name = self.summary["markersFile"].as_str().unwrap_or(MARKERS_FILE);
        let path = self.dir.join(name);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err("read the markers file", &e)),
        };
        Ok(text
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|value| Marker::from_json(&value))
            .collect())
    }

    /// The next frame, or `None` at the end.
    pub fn next_frame(&mut self) -> Result<Option<ScreenFrame>> {
        let (width, height) = (self.video.width(), self.video.height());
        Ok(self.video.next_frame()?.map(
            |RawFrame {
                 index,
                 host_ns,
                 bgra,
             }| ScreenFrame {
                index,
                host_ns,
                width,
                height,
                bgra,
            },
        ))
    }
}

impl Iterator for ScreenReader {
    type Item = Result<ScreenFrame>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_frame().transpose()
    }
}
