//! Two-pass EBU R128 loudness normalisation through FFmpeg `loudnorm`.
//!
//! Donors: CutRight `cutright-timeline/src/ffmpeg.rs` (`loudnorm_filter`,
//! `LoudnessTarget`, `LoudnessMeasurement`) and `cutright-project/src/loudness.rs`
//! (JSON parse, measure-file, tolerance report). Reconciled onto the shared runner so
//! the passes honor cancellation/timeouts/process ownership, and the output sample
//! rate is restored (`loudnorm` internally resamples to 192 kHz).

use std::{path::Path, time::Duration};

use serde::{Deserialize, Serialize};

use crate::{
    runner::{run_capture, run_with_progress, RunError, RunOptions, RunReport},
    CancellationToken, FfmpegBinary, FfmpegCommand,
};

/// Delivered integrated loudness must land within this many LU of the target.
pub const TOLERANCE_LU: f64 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoudnessTarget {
    pub integrated_lufs: f64,
    /// True-peak ceiling, dBTP.
    pub true_peak_db: f64,
    /// Loudness-range target, LU. Wide enough that linear mode is almost always possible.
    pub lra: f64,
}

impl Default for LoudnessTarget {
    fn default() -> Self {
        Self {
            integrated_lufs: -14.0,
            true_peak_db: -1.5,
            lra: 20.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoudnessMeasurement {
    pub input_i: f64,
    pub input_tp: f64,
    pub input_lra: f64,
    pub input_thresh: f64,
    pub target_offset: f64,
}

impl LoudnessMeasurement {
    /// No measurable signal (`-inf` loudness): nothing to normalise.
    pub fn is_silent(&self) -> bool {
        !self.input_i.is_finite() || !self.input_thresh.is_finite()
    }
}

/// `loudnorm` filter text; with `measured` it is the linear second pass.
pub fn loudnorm_filter(
    target: &LoudnessTarget,
    measured: Option<&LoudnessMeasurement>,
    print_format: &str,
) -> String {
    let mut f = format!(
        "loudnorm=I={:.1}:TP={:.1}:LRA={:.1}",
        target.integrated_lufs, target.true_peak_db, target.lra
    );
    if let Some(m) = measured {
        f.push_str(&format!(
            ":measured_I={:.2}:measured_TP={:.2}:measured_LRA={:.2}:measured_thresh={:.2}:offset={:.2}:linear=true",
            m.input_i, m.input_tp, m.input_lra, m.input_thresh, m.target_offset
        ));
    }
    f.push_str(&format!(":print_format={print_format}"));
    f
}

#[derive(Debug)]
pub enum LoudnessError {
    Run(RunError),
    /// FFmpeg exited non-zero.
    Failed(String),
    /// `loudnorm` printed no usable JSON.
    Unparsed(String),
}

impl std::fmt::Display for LoudnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Run(e) => write!(f, "loudness pass could not run: {e}"),
            Self::Failed(m) => write!(f, "loudness pass failed: {m}"),
            Self::Unparsed(m) => write!(f, "loudness measurement unreadable: {m}"),
        }
    }
}

impl std::error::Error for LoudnessError {}

impl From<RunError> for LoudnessError {
    fn from(e: RunError) -> Self {
        Self::Run(e)
    }
}

#[derive(Deserialize)]
struct RawLoudnorm {
    input_i: String,
    input_tp: String,
    input_lra: String,
    input_thresh: String,
    target_offset: String,
}

/// Parse the last JSON object `loudnorm` printed in `stderr`.
pub fn parse_loudnorm(stderr: &str) -> Result<LoudnessMeasurement, LoudnessError> {
    let bad = |why: String| LoudnessError::Unparsed(why);
    let end = stderr.rfind('}').ok_or_else(|| bad("no loudnorm output".into()))?;
    let start = stderr[..end]
        .rfind('{')
        .ok_or_else(|| bad("no loudnorm output".into()))?;
    let raw: RawLoudnorm =
        serde_json::from_str(&stderr[start..=end]).map_err(|e| bad(e.to_string()))?;
    let num = |s: &str| -> Result<f64, LoudnessError> {
        s.trim()
            .parse::<f64>()
            .map_err(|_| bad(format!("unreadable number {s:?}")))
    };
    Ok(LoudnessMeasurement {
        input_i: num(&raw.input_i)?,
        input_tp: num(&raw.input_tp)?,
        input_lra: num(&raw.input_lra)?,
        input_thresh: num(&raw.input_thresh)?,
        target_offset: num(&raw.target_offset)?,
    })
}

/// First-pass measurement of `file`'s audio (integrated, true peak, LRA).
pub fn measure_loudness(
    ffmpeg: &dyn FfmpegBinary,
    file: &Path,
    target: &LoudnessTarget,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<LoudnessMeasurement, LoudnessError> {
    let mut cmd = FfmpegCommand::base("info");
    cmd.input(file).text("-vn").texts(["-af".to_string(), loudnorm_filter(target, None, "json")]);
    cmd.texts(["-f", "null", "-"]);
    let out = run_capture(ffmpeg, &cmd, timeout, cancel)?;
    if !out.status.success() {
        let tail: Vec<&str> = out.stderr.lines().rev().take(6).collect();
        return Err(LoudnessError::Failed(
            tail.into_iter().rev().collect::<Vec<_>>().join(" | "),
        ));
    }
    parse_loudnorm(&out.stderr)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LoudnessReport {
    pub target: LoudnessTarget,
    pub measured: LoudnessMeasurement,
    /// The delivered file, measured after encoding.
    pub after: LoudnessMeasurement,
    /// Delivered integrated loudness is within [`TOLERANCE_LU`] of the target and the
    /// true peak does not exceed the target ceiling by more than 0.5 dB.
    pub within_tolerance: bool,
}

/// Measure, apply the linear second pass, and measure the delivered file.
///
/// `output_codec` is an FFmpeg audio encoder (`pcm_s16le`, `pcm_f32le`, `flac`...) and
/// `output_rate` the delivered sample rate. Silent input is rejected with
/// [`LoudnessError::Unparsed`] rather than normalised to garbage.
#[allow(clippy::too_many_arguments)]
pub fn normalize_file(
    ffmpeg: &dyn FfmpegBinary,
    input: &Path,
    output: &Path,
    target: &LoudnessTarget,
    output_codec: &str,
    output_rate: u32,
    timeout: Duration,
    cancel: &CancellationToken,
    on_progress: Option<&mut dyn FnMut(&crate::ProgressEvent)>,
) -> Result<(LoudnessReport, RunReport), LoudnessError> {
    let measured = measure_loudness(ffmpeg, input, target, timeout, cancel)?;
    if measured.is_silent() {
        return Err(LoudnessError::Unparsed("input is silent".into()));
    }
    let filter = format!(
        "{},aresample={output_rate}",
        loudnorm_filter(target, Some(&measured), "summary")
    );
    let mut cmd = FfmpegCommand::base("error");
    cmd.progress_pipe()
        .input(input)
        .text("-vn")
        .texts(["-af".to_string(), filter])
        .texts(["-ar".to_string(), output_rate.to_string()])
        .texts(["-c:a", output_codec])
        .path(output);
    let mut opts = RunOptions::new(timeout, cancel.clone());
    opts.on_progress = on_progress;
    let run = run_with_progress(ffmpeg, &cmd, opts)?;
    let after = measure_loudness(ffmpeg, output, target, timeout, cancel)?;
    let within_tolerance = (after.input_i - target.integrated_lufs).abs() <= TOLERANCE_LU
        && after.input_tp <= target.true_peak_db + 0.5;
    Ok((
        LoudnessReport {
            target: *target,
            measured,
            after,
            within_tolerance,
        },
        run,
    ))
}
