//! The per-frame render driver: seek a [`FrameSource`], capture a PNG, convert it to a PPM or PAM
//! frame and pipe it to one ffmpeg child, plus still and poster export.
//!
//! The frame source is a trait the app implements (headless Chrome over CDP, a webview, a test
//! generator), so this crate depends on no browser. Time is a pure function of the frame index
//! (`n / fps`), never of the wall clock, so a render is repeatable and a fallback re-run produces
//! the same frames.
//!
//! ffmpeg runs through `rightkit-ffmpeg`'s `run_with_stdin`, which owns the child as a
//! `rightkit-child-process` process tree: cancellation, a dropped call and a failure all reap it.
//! The caller resolves ffmpeg (for example `rightkit_ffmpeg::resolve_ffmpeg_handle`) and passes the
//! handle as `&dyn FfmpegBinary`; this module never builds a path or a raw `Command`.

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rightkit_ffmpeg::runner::run_with_stdin;
use rightkit_ffmpeg::{CancellationToken, FfmpegBinary, RunError, RunOptions};
use serde::Serialize;

use crate::encoder::{
    build_command, is_init_failure, AudioInput, EncodeSpec, Encoder, EncoderKind, Quality,
};
use crate::frame::{png_dimensions, png_to_pam, png_to_ppm};

/// Most frames one render may produce.
pub const MAX_FRAMES: u64 = 120_000;
/// Frame rate range a render accepts.
pub const FPS_RANGE: std::ops::RangeInclusive<u32> = 1..=120;
/// Width and height range a render accepts, pixels.
pub const SIZE_RANGE: std::ops::RangeInclusive<u32> = 16..=7680;

/// A failure from a [`FrameSource`]; the message is shown as-is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameError(pub String);

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FrameError {}

impl From<String> for FrameError {
    fn from(message: String) -> Self {
        Self(message)
    }
}

impl From<&str> for FrameError {
    fn from(message: &str) -> Self {
        Self(message.to_owned())
    }
}

/// Something that can show the composition at a time and hand back a picture of it.
///
/// The driver runs on one thread, calling [`seek`](FrameSource::seek) then
/// [`capture`](FrameSource::capture) for each frame in order, so an implementation may keep
/// mutable page state. `Send` is required because the driver runs on the writer thread that
/// `run_with_stdin` provides. A source that wraps an async session blocks inside these calls and
/// owns its timeouts (GenRight used 45 s per seek); the driver adds none.
pub trait FrameSource: Send {
    /// Make every timeline, clip and animation show time `time_s` (seconds), and wait until the
    /// paint matches. `time_s` is `frame / fps`.
    fn seek(&mut self, time_s: f64) -> Result<(), FrameError>;

    /// A PNG of the current frame at exactly the job's size. For an alpha render the PNG must keep
    /// its alpha channel (for CDP, an omit-background screenshot).
    fn capture(&mut self) -> Result<Vec<u8>, FrameError>;
}

/// Error codes are the stable prefix of each message (`code: detail`), as GenRight's strings were.
#[derive(Debug)]
pub enum RenderError {
    /// The job or request is out of range.
    InvalidInput(String),
    /// `ffmpeg -encoders` could not be read. Never an encoder finding.
    Probe(String),
    /// No usable encoder of this kind. `listed` is the build's video encoders when known;
    /// `ffmpeg_message` is ffmpeg's own output when an encoder refused to start.
    EncoderUnavailable {
        kind: EncoderKind,
        listed: Vec<String>,
        ffmpeg_message: Option<String>,
    },
    /// The chosen encoder refused to start. [`render_with_fallback`] turns this into MPEG-4 or
    /// [`RenderError::EncoderUnavailable`]; [`render`] returns it as is.
    EncoderInit { encoder: Encoder, log: String },
    /// A captured frame was not a decodable PNG.
    Decode(String),
    /// The frame source failed.
    Capture(String),
    /// A captured frame had the wrong size.
    CaptureSize {
        frame: u64,
        got: (u32, u32),
        expected: (u32, u32),
    },
    /// Writing a frame to ffmpeg failed.
    Pipe(String),
    /// ffmpeg failed to start or exited with an error unrelated to encoder start-up.
    Ffmpeg(String),
    /// The cancellation token fired.
    Cancelled,
    /// A file could not be written.
    Io(String),
}

impl RenderError {
    /// The stable machine code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "invalid_input",
            Self::Probe(_) | Self::Ffmpeg(_) => "ffmpeg_failed",
            Self::EncoderUnavailable { kind, .. } => match kind {
                EncoderKind::H264 | EncoderKind::Mpeg4 => "h264_encoder_unavailable",
                EncoderKind::ProRes4444 | EncoderKind::Vp9Alpha => "alpha_encoder_unavailable",
            },
            Self::EncoderInit { .. } => "encoder_init_failed",
            Self::Decode(_) => "capture_decode",
            Self::Capture(_) => "capture_failed",
            Self::CaptureSize { .. } => "capture_size_mismatch",
            Self::Pipe(_) => "ffmpeg_pipe",
            Self::Cancelled => "cancelled",
            Self::Io(_) => "io_error",
        }
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self.code();
        match self {
            Self::InvalidInput(m)
            | Self::Probe(m)
            | Self::Decode(m)
            | Self::Capture(m)
            | Self::Pipe(m)
            | Self::Ffmpeg(m)
            | Self::Io(m) => write!(f, "{code}: {m}"),
            Self::EncoderUnavailable {
                kind,
                listed,
                ffmpeg_message,
            } => {
                write!(
                    f,
                    "{code}: no usable {kind:?} encoder in the resolved ffmpeg"
                )?;
                if *kind == EncoderKind::H264 {
                    f.write_str("; allow the MPEG-4 fallback to accept MPEG-4 video")?;
                }
                if !listed.is_empty() {
                    write!(f, "; video encoders listed: {}", listed.join(", "))?;
                }
                if let Some(message) = ffmpeg_message {
                    write!(f, "; ffmpeg said: {}", tail(message, 600))?;
                }
                Ok(())
            }
            Self::EncoderInit { encoder, log } => write!(
                f,
                "{code}: {} refused to start: {}",
                encoder.name,
                tail(log, 600)
            ),
            Self::CaptureSize {
                frame,
                got,
                expected,
            } => write!(
                f,
                "{code}: frame {frame} is {}x{}, expected {}x{}",
                got.0, got.1, expected.0, expected.1
            ),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for RenderError {}

/// The last `max` characters, trimmed.
fn tail(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    let count = trimmed.chars().count();
    if count <= max {
        trimmed.to_owned()
    } else {
        trimmed.chars().skip(count - max).collect()
    }
}

/// One render.
#[derive(Clone, Debug)]
pub struct RenderJob {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Timeline length, seconds; the frame count is `ceil(duration_s * fps)`.
    pub duration_s: f64,
    pub quality: Quality,
    pub encoder: Encoder,
    pub audio: Vec<AudioInput>,
    pub output: PathBuf,
    /// Where to write the poster frame (a PNG), if wanted.
    pub poster: Option<PathBuf>,
}

impl RenderJob {
    /// Check ranges and return the frame count.
    pub fn validate(&self) -> Result<u64, RenderError> {
        let bad = |m: String| Err(RenderError::InvalidInput(m));
        if !FPS_RANGE.contains(&self.fps) {
            return bad("fps must be between 1 and 120".into());
        }
        if !SIZE_RANGE.contains(&self.width) || !SIZE_RANGE.contains(&self.height) {
            return bad("width and height must be between 16 and 7680".into());
        }
        if !(self.duration_s.is_finite() && self.duration_s > 0.0) {
            return bad("duration must be a positive number of seconds".into());
        }
        let total = (self.duration_s * self.fps as f64).ceil() as u64;
        if total == 0 || total > MAX_FRAMES {
            return bad(format!("{total} frames is outside 1..={MAX_FRAMES}"));
        }
        Ok(total)
    }
}

/// The frame saved as the poster: two fifths of the way in (GenRight's choice).
pub fn poster_frame(total_frames: u64) -> u64 {
    total_frames * 2 / 5
}

/// Where a render is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderPhase {
    Capturing,
    Encoding,
    Done,
}

/// A progress report; `frame` counts frames handed to ffmpeg.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RenderProgress {
    pub phase: RenderPhase,
    pub frame: u64,
    pub total: u64,
}

/// What a finished render produced.
#[derive(Clone, Debug, Serialize)]
pub struct RenderOutcome {
    pub output: PathBuf,
    /// The poster PNG, if one was requested and written.
    pub poster: Option<PathBuf>,
    pub frames: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub duration_s: f64,
    pub encoder: Encoder,
    pub elapsed_ms: u128,
}

/// Seek to `time_s`, capture, and check the PNG is `expected` size when its header can be read.
fn capture_frame(
    source: &mut dyn FrameSource,
    frame: u64,
    time_s: f64,
    expected: (u32, u32),
) -> Result<Vec<u8>, RenderError> {
    source
        .seek(time_s)
        .map_err(|e| RenderError::Capture(format!("seek to frame {frame}: {e}")))?;
    let png = source
        .capture()
        .map_err(|e| RenderError::Capture(format!("capture frame {frame}: {e}")))?;
    if let Some(got) = png_dimensions(&png) {
        if got != expected {
            return Err(RenderError::CaptureSize {
                frame,
                got,
                expected,
            });
        }
    }
    Ok(png)
}

/// Render `job` once: for each frame seek `source` to `n / fps`, capture a PNG, convert it
/// (PPM, or PAM for an alpha encoder) and write it to ffmpeg's stdin.
///
/// `on_progress` is called from the writer thread. The token cancels the render; ffmpeg's process
/// tree is stopped and the result is [`RenderError::Cancelled`]. On any error the partial output
/// file is removed.
///
/// Error precedence matches GenRight: an encoder that refused to start
/// ([`RenderError::EncoderInit`], from ffmpeg's own output) wins over the capture error its broken
/// pipe causes; then a capture error; then other ffmpeg failures. Use [`render_with_fallback`] to
/// apply the MPEG-4 rule.
pub fn render(
    ffmpeg: &dyn FfmpegBinary,
    source: &mut dyn FrameSource,
    job: &RenderJob,
    cancel: &CancellationToken,
    on_progress: &mut (dyn FnMut(RenderProgress) + Send),
) -> Result<RenderOutcome, RenderError> {
    let started = Instant::now();
    let total = job.validate()?;
    let command = build_command(&EncodeSpec {
        encoder: job.encoder.clone(),
        width: job.width,
        height: job.height,
        fps: job.fps,
        duration: job.duration_s,
        quality: job.quality,
        audio: job.audio.clone(),
        output: job.output.clone(),
    })?;
    let alpha = job.encoder.kind.has_alpha();
    let poster_at = poster_frame(total);
    let expected = (job.width, job.height);
    let mut failure: Option<RenderError> = None;
    let mut poster_written = false;

    let run = run_with_stdin(
        ffmpeg,
        &command,
        RunOptions::unbounded(cancel.clone()),
        |mut stdin| {
            for n in 0..total {
                if cancel.is_cancelled() {
                    failure = Some(RenderError::Cancelled);
                    break;
                }
                let png = match capture_frame(source, n, n as f64 / job.fps as f64, expected) {
                    Ok(png) => png,
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                };
                if n == poster_at {
                    if let Some(path) = &job.poster {
                        poster_written = std::fs::write(path, &png).is_ok();
                    }
                }
                let frame = if alpha {
                    png_to_pam(&png)
                } else {
                    png_to_ppm(&png)
                };
                let wrote = frame.and_then(|bytes| {
                    stdin
                        .write_all(&bytes)
                        .map_err(|e| RenderError::Pipe(e.to_string()))
                });
                if let Err(e) = wrote {
                    failure = Some(e);
                    break;
                }
                on_progress(RenderProgress {
                    phase: RenderPhase::Capturing,
                    frame: n + 1,
                    total,
                });
            }
            on_progress(RenderProgress {
                phase: RenderPhase::Encoding,
                frame: total,
                total,
            });
            // Dropping stdin sends EOF; ffmpeg finishes whatever it was given.
            Ok(())
        },
    );

    let result = match run {
        Err(RunError::Cancelled) => Err(RenderError::Cancelled),
        Err(RunError::Failed { stderr_tail, .. })
            if job.encoder.kind != EncoderKind::Mpeg4 && is_init_failure(&stderr_tail) =>
        {
            Err(RenderError::EncoderInit {
                encoder: job.encoder.clone(),
                log: stderr_tail,
            })
        }
        Err(other) => Err(failure
            .take()
            .unwrap_or_else(|| RenderError::Ffmpeg(other.to_string()))),
        Ok(_) => match failure.take() {
            Some(e) => Err(e),
            None if job.output.is_file() => Ok(()),
            None => Err(RenderError::Ffmpeg(
                "ffmpeg exited cleanly but wrote no output file".into(),
            )),
        },
    };
    if let Err(e) = result {
        let _ = std::fs::remove_file(&job.output);
        return Err(e);
    }
    on_progress(RenderProgress {
        phase: RenderPhase::Done,
        frame: total,
        total,
    });
    Ok(RenderOutcome {
        output: job.output.clone(),
        poster: (poster_written).then(|| job.poster.clone()).flatten(),
        frames: total,
        width: job.width,
        height: job.height,
        fps: job.fps,
        duration_s: job.duration_s,
        encoder: job.encoder.clone(),
        elapsed_ms: started.elapsed().as_millis(),
    })
}

/// [`render`] with GenRight's retry rule: when an H.264 encoder refuses to start and
/// `allow_mpeg4_fallback` is set, render once more with MPEG-4 (`fallback: true` in the outcome's
/// encoder). Without the flag, or for an alpha encoder, the refusal is
/// [`RenderError::EncoderUnavailable`] carrying ffmpeg's output. The re-run replays the same
/// frames, since the source is seeked by time.
pub fn render_with_fallback(
    ffmpeg: &dyn FfmpegBinary,
    source: &mut dyn FrameSource,
    job: &RenderJob,
    allow_mpeg4_fallback: bool,
    cancel: &CancellationToken,
    on_progress: &mut (dyn FnMut(RenderProgress) + Send),
) -> Result<RenderOutcome, RenderError> {
    match render(ffmpeg, source, job, cancel, on_progress) {
        Err(RenderError::EncoderInit { encoder, log }) => {
            if allow_mpeg4_fallback && encoder.kind == EncoderKind::H264 {
                let mut retry = job.clone();
                retry.encoder = Encoder::mpeg4_fallback();
                render(ffmpeg, source, &retry, cancel, on_progress)
            } else {
                Err(RenderError::EncoderUnavailable {
                    kind: encoder.kind,
                    listed: Vec::new(),
                    ffmpeg_message: Some(log),
                })
            }
        }
        other => other,
    }
}

/// A still: seek `source` to `time_s`, capture, and return the PNG bytes. When `expected` is
/// given, a PNG of another size is [`RenderError::CaptureSize`] (reported as frame 0).
pub fn capture_still(
    source: &mut dyn FrameSource,
    time_s: f64,
    expected: Option<(u32, u32)>,
) -> Result<Vec<u8>, RenderError> {
    if !(time_s.is_finite() && time_s >= 0.0) {
        return Err(RenderError::InvalidInput(
            "still time must be a non-negative number of seconds".into(),
        ));
    }
    source
        .seek(time_s)
        .map_err(|e| RenderError::Capture(format!("seek to {time_s:.3}s: {e}")))?;
    let png = source
        .capture()
        .map_err(|e| RenderError::Capture(format!("capture at {time_s:.3}s: {e}")))?;
    if let (Some(expected), Some(got)) = (expected, png_dimensions(&png)) {
        if got != expected {
            return Err(RenderError::CaptureSize {
                frame: 0,
                got,
                expected,
            });
        }
    }
    Ok(png)
}

/// [`capture_still`], written to `path` as a PNG (the parent folder is created).
pub fn export_still(
    source: &mut dyn FrameSource,
    time_s: f64,
    expected: Option<(u32, u32)>,
    path: &Path,
) -> Result<(), RenderError> {
    let png = capture_still(source, time_s, expected)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| RenderError::Io(e.to_string()))?;
    }
    std::fs::write(path, png).map_err(|e| RenderError::Io(format!("{}: {e}", path.display())))
}

/// The poster for a timeline of `duration_s` at `fps`: the still at the poster frame's time.
pub fn export_poster(
    source: &mut dyn FrameSource,
    duration_s: f64,
    fps: u32,
    expected: Option<(u32, u32)>,
    path: &Path,
) -> Result<(), RenderError> {
    if !FPS_RANGE.contains(&fps) || !(duration_s.is_finite() && duration_s > 0.0) {
        return Err(RenderError::InvalidInput(
            "poster needs a positive duration and fps 1..=120".into(),
        ));
    }
    let total = (duration_s * fps as f64).ceil() as u64;
    export_still(
        source,
        poster_frame(total) as f64 / fps as f64,
        expected,
        path,
    )
}
