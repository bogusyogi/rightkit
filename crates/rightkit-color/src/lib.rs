//! Colour for CutRight (media plan M2-P6).
//!
//! The render path asks for a [`ColorPlan`] (a JSON [`presets`] entry or hand-built), calls
//! [`build_fragment`] with the detected [`ColorSpaceKind`] and gets back a [`ColorFragment`]:
//! one linear `-vf` chain for the editing FFmpeg, the files it needs (generated LUTs), the
//! filters it requires and the colour tags the encoder must write.
//!
//! - [`space`]: ffprobe colour tags -> [`ColorSpaceKind`] (PQ / HLG / Apple Log / Rec.709).
//! - [`sdr`]: HDR and Apple Log -> Rec.709 SDR (`zscale` + `tonemap`, generated Apple Log LUT).
//! - [`apple_log`]: the published Apple Log curve and the 3D LUT generated from it.
//! - [`cube`]: `.cube` parsing, validation (size <= 65, finite values) and intensity mixing.
//! - [`sample`]: frame statistics from the editing FFmpeg (per-channel mean/std/percentiles).
//! - [`correct`]: bounded auto exposure + white balance with manual overrides.
//! - [`shot_match`]: match clip B to reference clip A (per-channel mean/std).
//! - [`grade`]: the user grade (curves, lift/gamma/gain wheels, HSL secondaries, saturation).
//! - [`plan`]: [`ColorPlan`], fragment assembly, presets and output-tag verification.

pub mod apple_log;
pub mod correct;
pub mod cube;
pub mod grade;
pub mod plan;
pub mod sample;
pub mod sdr;
pub mod shot_match;
pub mod space;

pub use correct::{auto_correction, ColorCorrection, CorrectionLimits, CorrectionOverrides};
pub use cube::{Cube3d, LutError, MAX_LUT_SIZE};
pub use grade::{Curves, Grade, HslRange, HslSecondary, Point, Wheels, MAX_CURVE_POINTS, MAX_HSL};
pub use plan::{
    build_fragment, build_master_stage, preset, presets, verify_output_tags, BuildEnv,
    ColorFragment, ColorPlan, FilterCapabilities, LutRef, MasterStage, Preset, EXPECTED_SDR_TAGS,
};
pub use sample::{sample_stats, ChannelStats, ClipSpan, FrameStats, SampleSpec};
pub use sdr::{SdrConversion, Tonemap};
pub use shot_match::{match_clip_to_reference, ShotMatch, ShotMatchLimits};
pub use space::{detect_color_space, probe_color_tags, ColorSpaceKind, SourceColorTags};

use std::time::Duration;

use rightkit_ffmpeg::runner::{run_capture_with, Captured, RunError, RunOptions};
use rightkit_ffmpeg::{CancellationToken, FfmpegCommand};

/// Errors from colour analysis and fragment building.
#[derive(Debug, thiserror::Error)]
pub enum ColorError {
    #[error("{0}")]
    Lut(#[from] LutError),
    /// The user's `.cube` file could not be read (missing, a directory, not UTF-8, ...).
    #[error("the colour LUT {path} cannot be read: {reason}")]
    LutUnreadable {
        path: String,
        reason: std::io::Error,
    },
    /// The user's `.cube` file was read but is not a valid 3D LUT.
    #[error("the colour LUT {path} is not a valid .cube file: {reason}")]
    LutInvalid { path: String, reason: LutError },
    #[error("ffmpeg/ffprobe failed: {0}")]
    Process(String),
    #[error("invalid ffprobe/ffmpeg output: {0}")]
    Parse(String),
    #[error("the editing FFmpeg lacks required filter(s): {0}")]
    MissingFilters(String),
    #[error("invalid colour parameter: {0}")]
    Invalid(String),
    #[error("output colour tags do not match: expected {expected}, got {actual}")]
    TagMismatch { expected: String, actual: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Clamp `v` into `lo..=hi`; NaN/inf become `default`.
pub(crate) fn clamp_or(v: f64, lo: f64, hi: f64, default: f64) -> f64 {
    if v.is_finite() {
        v.clamp(lo, hi)
    } else {
        default
    }
}

fn run_error(err: RunError) -> ColorError {
    match err {
        RunError::Spawn(e) | RunError::Io(e) => ColorError::Io(e),
        other => ColorError::Io(std::io::Error::other(other.to_string())),
    }
}

/// Run a probe or sample to completion and capture its output. A non-zero exit is not an
/// error here; the caller decides, as with `Command::output`. No deadline and no cancel flag
/// (the old spawn had neither); kill grace zero.
pub(crate) fn run_media_capture(
    exe: &std::path::Path,
    cmd: &FfmpegCommand,
) -> Result<Captured, ColorError> {
    let opts = RunOptions::unbounded(CancellationToken::new()).with_kill_grace(Duration::ZERO);
    use rightkit_ffmpeg as rk;
    let timeout = Duration::from_secs(30);
    if exe.file_stem().is_some_and(|stem| stem == "ffprobe") {
        let binary = rk::verify_ffprobe_handle(exe, rk::DECODE_VARIANT, timeout)
            .map_err(|e| ColorError::Process(e.to_string()))?;
        run_capture_with(&binary, cmd, opts).map_err(run_error)
    } else {
        let binary = rk::verify_ffmpeg_handle(exe, rk::EXPORT_ASS_ZIMG_VARIANT, timeout)
            .or_else(|_| rk::verify_ffmpeg_handle(exe, rk::EXPORT_ASS_VARIANT, timeout))
            .map_err(|e| ColorError::Process(e.to_string()))?;
        run_capture_with(&binary, cmd, opts).map_err(run_error)
    }
}
