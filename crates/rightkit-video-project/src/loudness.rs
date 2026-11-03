//! Two-pass loudness for final renders: measure the composed mix, apply a linear
//! `loudnorm` with the measured values, then measure the delivered file.
//!
//! The filters live in `rightkit-timeline` (`build_ffmpeg_measure`,
//! `AudioFinish::Normalize`). The composed-mix pass runs that graph on RightKit's checked
//! runner; measuring a finished file is `rightkit-ffmpeg`'s `measure_loudness`; parsing and
//! acceptance are its loudnorm types. Only the graph policy and the error mapping are CutRight's.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use rightkit_ffmpeg as rk;
use rightkit_ffmpeg::loudnorm as media_loudnorm;
use rightkit_ffmpeg::loudnorm::{LoudnessMeasurement, LoudnessTarget};
use rightkit_ffmpeg::FfmpegCommand;
use serde::{Deserialize, Serialize};

use crate::error::ProjectError;
use crate::worker::map_run_error;

/// Delivered integrated loudness must land within this many LU of the target.
pub const TOLERANCE_LU: f64 = 1.0;

/// The delivered true peak must be at or below this ceiling, dBTP.
const TRUE_PEAK_CEILING_DBTP: f64 = -1.0;

/// The acceptance the report applies: within [`TOLERANCE_LU`] of the target integrated
/// loudness, and a true peak at or below the ceiling.
fn acceptance() -> media_loudnorm::LoudnessAcceptance {
    media_loudnorm::LoudnessAcceptance {
        lufs_tolerance_lu: TOLERANCE_LU,
        true_peak_max_dbtp: Some(TRUE_PEAK_CEILING_DBTP),
    }
}

/// Loudness handling for a render.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoudnessSetting {
    Off,
    Normalize(LoudnessTarget),
}

impl Default for LoudnessSetting {
    /// -14 LUFS integrated, -1.5 dBTP ceiling.
    fn default() -> Self {
        LoudnessSetting::Normalize(LoudnessTarget::default())
    }
}

/// What the render report records about loudness.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoudnessReport {
    pub target: LoudnessTarget,
    /// Composed mix before normalisation (first pass). `None` when the mix was silent.
    pub measured: Option<LoudnessMeasurement>,
    /// The delivered file, measured after encoding. `None` when normalisation was skipped.
    pub after: Option<LoudnessMeasurement>,
    /// `true` when the first pass found no signal, so nothing was applied.
    pub skipped_silent: bool,
    /// Delivered integrated loudness is within [`TOLERANCE_LU`] of the target and the
    /// true peak is at or below -1 dBTP.
    pub within_tolerance: bool,
}

/// A loudness failure as a project error: a runner failure keeps its runner mapping, anything
/// else is a failed measurement.
fn loudness_error(
    ffmpeg: &Path,
    timeout: Duration,
    error: media_loudnorm::LoudnessError,
) -> ProjectError {
    match error {
        media_loudnorm::LoudnessError::Run(run) => map_run_error(ffmpeg, "ffmpeg", timeout, run),
        media_loudnorm::LoudnessError::Failed(tail) => ProjectError::RenderFailed {
            message: format!("ffmpeg measurement failed: {tail}"),
        },
        media_loudnorm::LoudnessError::Unparsed(why) => ProjectError::RenderFailed {
            message: format!("loudness measurement: {why}"),
        },
        other => ProjectError::RenderFailed {
            message: format!("loudness measurement: {other}"),
        },
    }
}

/// The editing ffmpeg for a measuring pass: the zimg build if this host has it, else the ASS one.
fn measure_binary(ffmpeg: &Path) -> Result<rk::FfmpegHandle, ProjectError> {
    rk::verify_ffmpeg_handle(
        ffmpeg,
        rk::EXPORT_ASS_ZIMG_VARIANT,
        crate::tools::VERIFY_TIMEOUT,
    )
    .or_else(|_| {
        rk::verify_ffmpeg_handle(ffmpeg, rk::EXPORT_ASS_VARIANT, crate::tools::VERIFY_TIMEOUT)
    })
    .map_err(|e| ProjectError::MediaToolMissing(e.to_string()))
}

/// First pass: runs the measure graph from `build_ffmpeg_measure` (the graph is CutRight's
/// policy) through RightKit's checked runner and reads `loudnorm`'s JSON with its parser.
pub fn measure_command(
    ffmpeg: &Path,
    cmd: &FfmpegCommand,
    timeout: Duration,
    cancel: &Arc<AtomicBool>,
) -> Result<LoudnessMeasurement, ProjectError> {
    let token = rk::CancellationToken::from_flag(Arc::clone(cancel));
    let out = rk::run_checked(&measure_binary(ffmpeg)?, cmd, timeout, &token)
        .map_err(|e| map_run_error(ffmpeg, "ffmpeg", timeout, e))?;
    media_loudnorm::parse_loudnorm(&out.stderr).map_err(|e| loudness_error(ffmpeg, timeout, e))
}

/// Measures a finished media file's audio (integrated, true peak, LRA).
pub fn measure_file(
    ffmpeg: &Path,
    file: &Path,
    target: &LoudnessTarget,
    timeout: Duration,
    cancel: &Arc<AtomicBool>,
) -> Result<LoudnessMeasurement, ProjectError> {
    let token = rk::CancellationToken::from_flag(Arc::clone(cancel));
    media_loudnorm::measure_loudness(&measure_binary(ffmpeg)?, file, target, timeout, &token)
        .map_err(|e| loudness_error(ffmpeg, timeout, e))
}

/// Assembles the report from the passes. The delivered file is accepted when it is within
/// [`TOLERANCE_LU`] of the target and at or below the true-peak ceiling.
pub fn report(
    target: LoudnessTarget,
    measured: Option<LoudnessMeasurement>,
    after: Option<LoudnessMeasurement>,
) -> LoudnessReport {
    let within_tolerance = after.is_some_and(|a| acceptance().verify(&a, &target).is_ok());
    LoudnessReport {
        target,
        skipped_silent: measured.is_none(),
        measured,
        after,
        within_tolerance,
    }
}
