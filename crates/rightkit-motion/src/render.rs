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
                write!(f, "{code}: no usable {kind:?} encoder in the resolved ffmpeg")?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::Encoder;

    /// A source that returns a solid PNG of a chosen size and records the seeks.
    struct Solid {
        size: (u32, u32),
        alpha: bool,
        seeks: Vec<f64>,
        fail_capture_at: Option<usize>,
        cancel_after: Option<(usize, CancellationToken)>,
    }

    impl Solid {
        fn new(w: u32, h: u32) -> Self {
            Self {
                size: (w, h),
                alpha: false,
                seeks: Vec::new(),
                fail_capture_at: None,
                cancel_after: None,
            }
        }
    }

    impl FrameSource for Solid {
        fn seek(&mut self, time_s: f64) -> Result<(), FrameError> {
            self.seeks.push(time_s);
            Ok(())
        }
        fn capture(&mut self) -> Result<Vec<u8>, FrameError> {
            let done = self.seeks.len();
            if self.fail_capture_at == Some(done) {
                return Err("page went away".into());
            }
            if let Some((at, token)) = &self.cancel_after {
                if done == *at {
                    token.cancel();
                }
            }
            let img: image::DynamicImage = if self.alpha {
                image::RgbaImage::from_pixel(self.size.0, self.size.1, image::Rgba([1, 2, 3, 0])).into()
            } else {
                image::RgbImage::from_pixel(self.size.0, self.size.1, image::Rgb([9, 9, 9])).into()
            };
            let mut png = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            Ok(png)
        }
    }

    fn job(dir: &Path, encoder: Encoder) -> RenderJob {
        RenderJob {
            width: 16,
            height: 16,
            fps: 10,
            duration_s: 0.5,
            quality: Quality::Draft,
            encoder,
            audio: vec![],
            output: dir.join("out.mp4"),
            poster: Some(dir.join("poster.png")),
        }
    }

    #[test]
    fn validation_covers_every_range_and_counts_frames() {
        let dir = Path::new("x");
        let mut j = job(dir, Encoder::mpeg4_fallback());
        assert_eq!(j.validate().unwrap(), 5);
        j.duration_s = 0.51;
        assert_eq!(j.validate().unwrap(), 6, "frames round up");
        for mutate in [
            (|j: &mut RenderJob| j.fps = 0) as fn(&mut RenderJob),
            |j| j.fps = 121,
            |j| j.width = 8,
            |j| j.height = 7681,
            |j| j.duration_s = 0.0,
            |j| j.duration_s = f64::NAN,
            |j| j.duration_s = 100_000.0,
        ] {
            let mut bad = job(dir, Encoder::mpeg4_fallback());
            mutate(&mut bad);
            assert_eq!(bad.validate().unwrap_err().code(), "invalid_input");
        }
    }

    #[test]
    fn the_poster_is_two_fifths_in() {
        assert_eq!(poster_frame(5), 2);
        assert_eq!(poster_frame(100), 40);
        assert_eq!(poster_frame(1), 0);
    }

    #[test]
    fn error_messages_lead_with_the_code() {
        let e = RenderError::EncoderUnavailable {
            kind: EncoderKind::H264,
            listed: vec!["mpeg4".into()],
            ffmpeg_message: Some("init failed".into()),
        };
        assert!(e.to_string().starts_with("h264_encoder_unavailable: "), "{e}");
        assert!(e.to_string().contains("init failed") && e.to_string().contains("mpeg4"));
        let size = RenderError::CaptureSize { frame: 3, got: (1, 2), expected: (3, 4) };
        assert_eq!(size.to_string(), "capture_size_mismatch: frame 3 is 1x2, expected 3x4");
        assert_eq!(RenderError::Cancelled.to_string(), "cancelled");
    }

    #[test]
    fn a_still_is_the_png_at_that_time_and_checks_its_size() {
        let mut source = Solid::new(16, 16);
        let png = capture_still(&mut source, 1.25, Some((16, 16))).unwrap();
        assert_eq!(png_dimensions(&png), Some((16, 16)));
        assert_eq!(source.seeks, [1.25]);
        let wrong = capture_still(&mut source, 0.0, Some((32, 16))).unwrap_err();
        assert_eq!(wrong.code(), "capture_size_mismatch");
        assert_eq!(capture_still(&mut source, -1.0, None).unwrap_err().code(), "invalid_input");
        assert_eq!(capture_still(&mut source, f64::NAN, None).unwrap_err().code(), "invalid_input");
    }

    #[test]
    fn export_still_and_poster_write_png_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut source = Solid::new(16, 16);
        let still = dir.path().join("nested/still.png");
        export_still(&mut source, 0.5, None, &still).unwrap();
        assert_eq!(png_dimensions(&std::fs::read(&still).unwrap()), Some((16, 16)));
        let poster = dir.path().join("poster.png");
        let mut source = Solid::new(16, 16);
        // 2 s at 10 fps = 20 frames; the poster is frame 8, at 0.8 s.
        export_poster(&mut source, 2.0, 10, Some((16, 16)), &poster).unwrap();
        assert_eq!(source.seeks, [0.8]);
        assert!(poster.is_file());
        assert_eq!(
            export_poster(&mut source, 0.0, 10, None, &poster).unwrap_err().code(),
            "invalid_input"
        );
    }

    #[test]
    fn a_failing_source_is_a_capture_error_with_the_frame_number() {
        let mut source = Solid::new(16, 16);
        source.fail_capture_at = Some(1);
        let e = capture_still(&mut source, 0.0, None).unwrap_err();
        assert_eq!(e.code(), "capture_failed");
        assert!(e.to_string().contains("page went away"), "{e}");
    }

    #[cfg(unix)]
    mod with_a_fake_ffmpeg {
        use super::*;
        use rightkit_ffmpeg::DevFfmpeg;

        /// A shell stand-in for ffmpeg: `body` runs with the arguments in `$@`.
        fn fake(dir: &Path, body: &str) -> DevFfmpeg {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.join("fake-ffmpeg");
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            DevFfmpeg::unverified(path)
        }

        /// Writes the number of stdin bytes it received to the output file (the last argument).
        const COUNT_BYTES: &str = r#"out=""; for a in "$@"; do out="$a"; done; cat | wc -c | tr -d ' ' > "$out""#;

        fn noop() -> impl FnMut(RenderProgress) + Send {
            |_| {}
        }

        #[test]
        fn every_frame_is_seeked_by_time_converted_and_piped() {
            let dir = tempfile::tempdir().unwrap();
            let ffmpeg = fake(dir.path(), COUNT_BYTES);
            let j = job(dir.path(), Encoder::mpeg4_fallback());
            let mut source = Solid::new(16, 16);
            let mut seen = Vec::new();
            let mut progress = |p: RenderProgress| seen.push(p);
            let outcome = render(&ffmpeg, &mut source, &j, &CancellationToken::new(), &mut progress).unwrap();
            assert_eq!(outcome.frames, 5);
            assert_eq!(source.seeks, [0.0, 0.1, 0.2, 0.3, 0.4]);
            // Five P6 frames: "P6\n16 16\n255\n" is 13 bytes, plus 16*16*3 pixel bytes.
            let bytes: u64 = std::fs::read_to_string(&j.output).unwrap().trim().parse().unwrap();
            assert_eq!(bytes, 5 * (13 + 16 * 16 * 3));
            assert!(outcome.poster.as_deref() == Some(j.poster.as_deref().unwrap()));
            assert!(j.poster.unwrap().is_file());
            let phases: Vec<_> = seen.iter().map(|p| p.phase).collect();
            assert_eq!(phases.iter().filter(|p| **p == RenderPhase::Capturing).count(), 5);
            assert_eq!(phases[phases.len() - 2..], [RenderPhase::Encoding, RenderPhase::Done]);
            assert_eq!(seen[0], RenderProgress { phase: RenderPhase::Capturing, frame: 1, total: 5 });
        }

        #[test]
        fn alpha_encoders_receive_pam_frames() {
            let dir = tempfile::tempdir().unwrap();
            let ffmpeg = fake(dir.path(), COUNT_BYTES);
            let mut j = job(dir.path(), Encoder::mpeg4_fallback());
            j.encoder = crate::encoder::pick_alpha_encoder(
                &rightkit_ffmpeg::EncoderInventory::parse(" V....D prores_ks x\n"),
                EncoderKind::ProRes4444,
            )
            .unwrap();
            j.output = dir.path().join("out.mov");
            let mut source = Solid::new(16, 16);
            source.alpha = true;
            render(&ffmpeg, &mut source, &j, &CancellationToken::new(), &mut noop()).unwrap();
            let header = "P7\nWIDTH 16\nHEIGHT 16\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n".len() as u64;
            let bytes: u64 = std::fs::read_to_string(&j.output).unwrap().trim().parse().unwrap();
            assert_eq!(bytes, 5 * (header + 16 * 16 * 4));
        }

        #[test]
        fn a_capture_error_stops_the_render_and_removes_the_output() {
            let dir = tempfile::tempdir().unwrap();
            let ffmpeg = fake(dir.path(), COUNT_BYTES);
            let j = job(dir.path(), Encoder::mpeg4_fallback());
            let mut source = Solid::new(16, 16);
            source.fail_capture_at = Some(3);
            let e = render(&ffmpeg, &mut source, &j, &CancellationToken::new(), &mut noop()).unwrap_err();
            assert_eq!(e.code(), "capture_failed");
            assert!(e.to_string().contains("frame 2"), "{e}");
            assert!(!j.output.exists(), "partial output must not survive");
        }

        #[test]
        fn a_wrong_sized_capture_is_a_typed_error() {
            let dir = tempfile::tempdir().unwrap();
            let ffmpeg = fake(dir.path(), COUNT_BYTES);
            let j = job(dir.path(), Encoder::mpeg4_fallback());
            let mut source = Solid::new(32, 16);
            let e = render(&ffmpeg, &mut source, &j, &CancellationToken::new(), &mut noop()).unwrap_err();
            assert_eq!(e.code(), "capture_size_mismatch");
        }

        #[test]
        fn cancelling_mid_render_is_cancelled_and_cleans_up() {
            let dir = tempfile::tempdir().unwrap();
            let ffmpeg = fake(dir.path(), COUNT_BYTES);
            let j = job(dir.path(), Encoder::mpeg4_fallback());
            let cancel = CancellationToken::new();
            let mut source = Solid::new(16, 16);
            source.cancel_after = Some((2, cancel.clone()));
            let e = render(&ffmpeg, &mut source, &j, &cancel, &mut noop()).unwrap_err();
            assert_eq!(e.code(), "cancelled");
            assert!(!j.output.exists());
        }

        const REFUSE: &str =
            r#"cat > /dev/null; echo "[h264_videotoolbox] Error while opening encoder for output stream" >&2; exit 1"#;

        #[test]
        fn an_encoder_that_refuses_to_start_is_classified_before_the_pipe_error() {
            let dir = tempfile::tempdir().unwrap();
            // Exit without reading stdin so the writer sees a broken pipe too.
            let ffmpeg = fake(dir.path(), r#"echo "Error while opening encoder" >&2; exit 1"#);
            let j = job(dir.path(), Encoder::mpeg4_fallback());
            let mut h264 = j.clone();
            h264.encoder = crate::encoder::pick_encoder(
                &rightkit_ffmpeg::EncoderInventory::parse(" V....D h264_videotoolbox x\n"),
                "macos",
                false,
                &mut |_| true,
            )
            .unwrap();
            let mut source = Solid::new(16, 16);
            let e = render(&ffmpeg, &mut source, &h264, &CancellationToken::new(), &mut noop()).unwrap_err();
            assert_eq!(e.code(), "encoder_init_failed", "{e}");
        }

        #[test]
        fn without_the_flag_a_refused_h264_encoder_is_unavailable_with_ffmpegs_words() {
            let dir = tempfile::tempdir().unwrap();
            let ffmpeg = fake(dir.path(), REFUSE);
            let mut j = job(dir.path(), Encoder::mpeg4_fallback());
            j.encoder = crate::encoder::pick_encoder(
                &rightkit_ffmpeg::EncoderInventory::parse(" V....D h264_videotoolbox x\n"),
                "macos",
                false,
                &mut |_| true,
            )
            .unwrap();
            let mut source = Solid::new(16, 16);
            let e = render_with_fallback(&ffmpeg, &mut source, &j, false, &CancellationToken::new(), &mut noop())
                .unwrap_err();
            assert_eq!(e.code(), "h264_encoder_unavailable");
            assert!(e.to_string().contains("Error while opening encoder"), "{e}");
        }

        #[test]
        fn with_the_flag_a_refused_h264_encoder_retries_once_as_mpeg4() {
            let dir = tempfile::tempdir().unwrap();
            // Refuses any run that is not mpeg4; counts bytes for mpeg4.
            let ffmpeg = fake(
                dir.path(),
                &format!(
                    r#"case " $* " in *" -c:v mpeg4 "*) {COUNT_BYTES};; *) {REFUSE};; esac"#
                ),
            );
            let mut j = job(dir.path(), Encoder::mpeg4_fallback());
            j.encoder = crate::encoder::pick_encoder(
                &rightkit_ffmpeg::EncoderInventory::parse(" V....D h264_videotoolbox x\n"),
                "macos",
                false,
                &mut |_| true,
            )
            .unwrap();
            let mut source = Solid::new(16, 16);
            let outcome =
                render_with_fallback(&ffmpeg, &mut source, &j, true, &CancellationToken::new(), &mut noop())
                    .unwrap();
            assert!(outcome.encoder.fallback && outcome.encoder.name == "mpeg4");
            assert_eq!(outcome.frames, 5);
            assert_eq!(source.seeks.len(), 10, "the failed attempt streamed 5 frames, the retry 5");
        }

        #[test]
        fn an_unrelated_ffmpeg_failure_passes_through_unchanged() {
            let dir = tempfile::tempdir().unwrap();
            let ffmpeg = fake(dir.path(), r#"cat > /dev/null; echo "disk full" >&2; exit 1"#);
            let j = job(dir.path(), Encoder::mpeg4_fallback());
            let mut source = Solid::new(16, 16);
            let e = render_with_fallback(&ffmpeg, &mut source, &j, true, &CancellationToken::new(), &mut noop())
                .unwrap_err();
            assert_eq!(e.code(), "ffmpeg_failed");
            assert!(e.to_string().contains("disk full"), "{e}");
        }
    }
}
