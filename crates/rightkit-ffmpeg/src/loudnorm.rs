//! EBU R128 loudness measurement and converging normalisation.
//!
//! Measurement donors: CutRight `cutright-timeline/src/ffmpeg.rs` (`loudnorm_filter`,
//! `LoudnessTarget`, `LoudnessMeasurement`) and `cutright-project/src/loudness.rs`
//! (JSON parse, measure-file).
//!
//! Normalisation is ported from GenRight `crates/genright-studio/src/voices.rs`
//! (`transcode_normalized` + `loudnorm_measure`): render `volume` gain followed by a
//! brickwall `alimiter`, re-measure, and re-render with a secant-corrected gain until
//! the measured integrated loudness is within tolerance (default and maximum ±0.5 LU),
//! for at most [`ConvergeOptions::max_renders`] renders. The closest measured render
//! is kept, so the encoded deliverable always comes from a render that was measured.
//! One-shot `loudnorm` (linear or dynamic) undershoots on peaky speech because the
//! limiter removes crest, which is why this iterates. Every pass runs through the
//! shared runner (cancellation, timeouts, process ownership).

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::{
    runner::{run_capture, run_checked, run_with_progress, RunError, RunOptions, RunReport},
    CancellationToken, FfmpegBinary, FfmpegCommand,
};

/// Default tolerance: the kept render is within this many LU of the target.
pub const DEFAULT_TOLERANCE_LU: f64 = 0.5;
/// Tolerances above this are refused (GenRight's guarantee is the floor).
pub const MAX_TOLERANCE_LU: f64 = 0.5;
/// GenRight's render bound.
pub const DEFAULT_MAX_RENDERS: u32 = 5;
/// Hard upper bound on renders a caller may request.
pub const MAX_RENDERS_LIMIT: u32 = 16;
/// Brickwall limiter after the gain stage: sample peak 0.63 (about -4 dBFS) keeps the
/// true peak under a -1.5 dBTP ceiling on speech.
pub const LIMITER_FILTER: &str = "alimiter=limit=0.63:level=false:attack=5:release=50";
/// First-step loudness-per-gain slope GenRight observed through the limiter.
const FIRST_STEP_SLOPE: f64 = 0.45;
const MIN_SECANT_DELTA: f64 = 0.05;
const SLOPE_MIN: f64 = 0.1;
const SLOPE_MAX: f64 = 1.5;
const MAX_STEP_DB: f64 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoudnessTarget {
    pub integrated_lufs: f64,
    /// True-peak ceiling, dBTP.
    pub true_peak_db: f64,
    /// Loudness-range target, LU. Wide enough that linear mode is almost always possible.
    pub lra: f64,
}

impl LoudnessTarget {
    /// GenRight's spoken-audio export target: I=-16 LUFS, TP=-1.5 dBTP, LRA=11 LU.
    pub const SPEECH_EXPORT: Self = Self {
        integrated_lufs: -16.0,
        true_peak_db: -1.5,
        lra: 11.0,
    };
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

/// The in-graph `loudnorm` filter text: `measured` makes it the linear second pass, and
/// `print_format` (`json` for a measurement, `summary` for a human-readable report) sets
/// what `loudnorm` prints. Callers composing their own graphs want
/// [`two_pass_normalize_filter`], which has the same text without forcing a print format.
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
        f.push_str(&linear_pass_parameters(m));
    }
    f.push_str(&format!(":print_format={print_format}"));
    f
}

/// The `measured_*`/`offset`/`linear` tail that turns `loudnorm` into its linear pass.
fn linear_pass_parameters(m: &LoudnessMeasurement) -> String {
    format!(
        ":measured_I={:.2}:measured_TP={:.2}:measured_LRA={:.2}:measured_thresh={:.2}:offset={:.2}:linear=true",
        m.input_i, m.input_tp, m.input_lra, m.input_thresh, m.target_offset
    )
}

/// The in-graph second pass for callers that compose their own filter graph: a single
/// `loudnorm` with the first pass's measurement fed back, so it applies one linear gain
/// (no dynamic compression) and the output lands on `target`. `print_format` is appended
/// when given, so `two_pass_normalize_filter(m, t, Some("summary"))` equals
/// [`loudnorm_filter`]`(t, Some(m), "summary")`. The measurement only describes the audio
/// this filter receives: any filter placed before it must also have been in the graph
/// that was measured.
pub fn two_pass_normalize_filter(
    measured: &LoudnessMeasurement,
    target: &LoudnessTarget,
    print_format: Option<&str>,
) -> String {
    let mut filter = format!(
        "loudnorm=I={:.1}:TP={:.1}:LRA={:.1}{}",
        target.integrated_lufs,
        target.true_peak_db,
        target.lra,
        linear_pass_parameters(measured)
    );
    if let Some(print_format) = print_format {
        filter.push_str(&format!(":print_format={print_format}"));
    }
    filter
}

#[non_exhaustive]
#[derive(Debug)]
pub enum LoudnessError {
    Run(RunError),
    /// FFmpeg exited non-zero.
    Failed(String),
    /// `loudnorm` printed no usable JSON.
    Unparsed(String),
    /// The input has no measurable signal (`-inf` LUFS); nothing to normalise.
    Silent,
    /// Tolerance outside (0, [`MAX_TOLERANCE_LU`]] or renders outside 1..=[`MAX_RENDERS_LIMIT`].
    InvalidOptions(String),
    /// The closest render missed the tolerance (see [`LoudnessReport::ensure_within_tolerance`]).
    OutOfTolerance {
        measured_i: f64,
        target_i: f64,
        tolerance_lu: f64,
    },
    /// The measured true peak is above the ceiling in a [`LoudnessAcceptance`].
    TruePeakExceeded {
        measured_tp: f64,
        ceiling_dbtp: f64,
    },
}

impl std::fmt::Display for LoudnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Run(e) => write!(f, "loudness pass could not run: {e}"),
            Self::Failed(m) => write!(f, "loudness pass failed: {m}"),
            Self::Unparsed(m) => write!(f, "loudness measurement unreadable: {m}"),
            Self::Silent => f.write_str("input is silent; nothing to normalise"),
            Self::InvalidOptions(m) => write!(f, "invalid loudness options: {m}"),
            Self::OutOfTolerance {
                measured_i,
                target_i,
                tolerance_lu,
            } => write!(
                f,
                "closest render measured {measured_i:.2} LUFS, outside {target_i:.1} ± {tolerance_lu:.2} LU"
            ),
            Self::TruePeakExceeded {
                measured_tp,
                ceiling_dbtp,
            } => write!(
                f,
                "measured true peak {measured_tp:.2} dBTP is above the {ceiling_dbtp:.2} dBTP ceiling"
            ),
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

/// The last JSON object in `stderr` (the one `loudnorm` prints at the end).
fn last_json_object(stderr: &str) -> Result<&str, LoudnessError> {
    let missing = || LoudnessError::Unparsed("no loudnorm output".into());
    let end = stderr.rfind('}').ok_or_else(missing)?;
    let start = stderr[..end].rfind('{').ok_or_else(missing)?;
    Ok(&stderr[start..=end])
}

fn number(text: &str) -> Result<f64, LoudnessError> {
    text.trim()
        .parse::<f64>()
        .map_err(|_| LoudnessError::Unparsed(format!("unreadable number {text:?}")))
}

/// Parse the last JSON object `loudnorm` printed in `stderr`.
pub fn parse_loudnorm(stderr: &str) -> Result<LoudnessMeasurement, LoudnessError> {
    let raw: RawLoudnorm = serde_json::from_str(last_json_object(stderr)?)
        .map_err(|e| LoudnessError::Unparsed(e.to_string()))?;
    Ok(LoudnessMeasurement {
        input_i: number(&raw.input_i)?,
        input_tp: number(&raw.input_tp)?,
        input_lra: number(&raw.input_lra)?,
        input_thresh: number(&raw.input_thresh)?,
        target_offset: number(&raw.target_offset)?,
    })
}

/// The `output_*` values `loudnorm` prints next to the `input_*` ones. In a measurement
/// pass they describe the dynamically normalised stream, not the input; in a linear pass
/// (one built with [`two_pass_normalize_filter`]) they describe that pass's output, so
/// `output_tp` is the delivered true peak. A file's own true peak is `input_tp` when it
/// is the thing measured.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoudnessOutput {
    pub output_i: f64,
    pub output_tp: f64,
    pub output_lra: f64,
    pub output_thresh: f64,
}

#[derive(Deserialize)]
struct RawLoudnormOutput {
    output_i: String,
    output_tp: String,
    output_lra: String,
    output_thresh: String,
}

/// Parse the `output_*` fields of the last JSON object `loudnorm` printed in `stderr`.
pub fn parse_loudnorm_output(stderr: &str) -> Result<LoudnessOutput, LoudnessError> {
    let raw: RawLoudnormOutput = serde_json::from_str(last_json_object(stderr)?)
        .map_err(|e| LoudnessError::Unparsed(e.to_string()))?;
    Ok(LoudnessOutput {
        output_i: number(&raw.output_i)?,
        output_tp: number(&raw.output_tp)?,
        output_lra: number(&raw.output_lra)?,
        output_thresh: number(&raw.output_thresh)?,
    })
}

/// What a measured file must meet. The default is the tolerance GenRight ships
/// (±0.5 LU of the target integrated loudness) with no true-peak ceiling. Unlike
/// [`ConvergeOptions`], the tolerance is not capped at 0.5 LU here: a verifier may be
/// looser than the render loop, but it must be positive and finite.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct LoudnessAcceptance {
    /// Allowed distance, in LU, between the measured integrated loudness and the target.
    pub lufs_tolerance_lu: f64,
    /// Highest allowed true peak, dBTP. `None` enforces no ceiling.
    pub true_peak_max_dbtp: Option<f64>,
}

impl Default for LoudnessAcceptance {
    fn default() -> Self {
        Self {
            lufs_tolerance_lu: DEFAULT_TOLERANCE_LU,
            true_peak_max_dbtp: None,
        }
    }
}

impl LoudnessAcceptance {
    /// Check one measurement (a file's own, as measured) against `target`. Silent input
    /// is [`LoudnessError::Silent`]; invalid bounds are [`LoudnessError::InvalidOptions`].
    pub fn verify(
        &self,
        measured: &LoudnessMeasurement,
        target: &LoudnessTarget,
    ) -> Result<(), LoudnessError> {
        if !(self.lufs_tolerance_lu > 0.0 && self.lufs_tolerance_lu.is_finite()) {
            return Err(LoudnessError::InvalidOptions(format!(
                "tolerance {} LU must be a positive, finite number",
                self.lufs_tolerance_lu
            )));
        }
        if let Some(ceiling) = self.true_peak_max_dbtp {
            if !ceiling.is_finite() {
                return Err(LoudnessError::InvalidOptions(format!(
                    "true-peak ceiling {ceiling} dBTP must be finite"
                )));
            }
        }
        if measured.is_silent() {
            return Err(LoudnessError::Silent);
        }
        // `input_i` is finite here (`is_silent` rejected the rest), so this is a plain test.
        let error = (measured.input_i - target.integrated_lufs).abs();
        if error > self.lufs_tolerance_lu {
            return Err(LoudnessError::OutOfTolerance {
                measured_i: measured.input_i,
                target_i: target.integrated_lufs,
                tolerance_lu: self.lufs_tolerance_lu,
            });
        }
        if let Some(ceiling) = self.true_peak_max_dbtp {
            if measured.input_tp.is_nan() || measured.input_tp > ceiling {
                return Err(LoudnessError::TruePeakExceeded {
                    measured_tp: measured.input_tp,
                    ceiling_dbtp: ceiling,
                });
            }
        }
        Ok(())
    }
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
    cmd.input(file)
        .text("-vn")
        .texts(["-af".to_string(), loudnorm_filter(target, None, "json")]);
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

/// [`measure_loudness`], then [`LoudnessAcceptance::verify`] on that same measurement.
/// [`measure_loudness`] itself never judges the result, so callers that only want the
/// numbers keep using it.
pub fn measure_loudness_verified(
    ffmpeg: &dyn FfmpegBinary,
    file: &Path,
    target: &LoudnessTarget,
    acceptance: &LoudnessAcceptance,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<LoudnessMeasurement, LoudnessError> {
    let measured = measure_loudness(ffmpeg, file, target, timeout, cancel)?;
    acceptance.verify(&measured, target)?;
    Ok(measured)
}

/// Iteration policy for [`normalize_file`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ConvergeOptions {
    tolerance_lu: f64,
    max_renders: u32,
}

impl ConvergeOptions {
    pub fn new(tolerance_lu: f64, max_renders: u32) -> Result<Self, LoudnessError> {
        if !(tolerance_lu > 0.0 && tolerance_lu <= MAX_TOLERANCE_LU) {
            return Err(LoudnessError::InvalidOptions(format!(
                "tolerance {tolerance_lu} LU must be in (0, {MAX_TOLERANCE_LU}]"
            )));
        }
        if !(1..=MAX_RENDERS_LIMIT).contains(&max_renders) {
            return Err(LoudnessError::InvalidOptions(format!(
                "max renders {max_renders} must be in 1..={MAX_RENDERS_LIMIT}"
            )));
        }
        Ok(Self {
            tolerance_lu,
            max_renders,
        })
    }

    pub fn tolerance_lu(&self) -> f64 {
        self.tolerance_lu
    }

    pub fn max_renders(&self) -> u32 {
        self.max_renders
    }
}

impl Default for ConvergeOptions {
    /// GenRight's policy: ±0.5 LU, at most five renders.
    fn default() -> Self {
        Self {
            tolerance_lu: DEFAULT_TOLERANCE_LU,
            max_renders: DEFAULT_MAX_RENDERS,
        }
    }
}

/// One rendered gain and the integrated loudness measured on that render.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RenderAttempt {
    pub gain_db: f64,
    pub measured_i: f64,
}

/// Outcome of [`converge_gain`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Convergence {
    /// Every render, in order.
    pub attempts: Vec<RenderAttempt>,
    /// Index into `attempts` of the render closest to the target.
    pub best: usize,
    pub within_tolerance: bool,
}

impl Convergence {
    pub fn kept(&self) -> RenderAttempt {
        self.attempts[self.best]
    }
}

/// `volume=<gain>dB,alimiter=...` exactly as GenRight renders it.
pub fn gain_limiter_filter(gain_db: f64) -> String {
    format!("volume={gain_db:.2}dB,{LIMITER_FILTER}")
}

/// GenRight's convergence loop, independent of FFmpeg so it can be tested exactly.
///
/// `render_and_measure(index, gain_db)` renders `gain_db` and returns the measured
/// integrated loudness of that render. Starts at `target - input_i`; stops as soon as a
/// render is within tolerance; otherwise steps by the secant through the last two
/// measurements (slope clamped to [0.1, 1.5], first step 0.45 dB per dB) with each step
/// clamped to ±8 dB. The closest render (smallest |error|, earliest on ties) is kept.
pub fn converge_gain<E>(
    target_i: f64,
    input_i: f64,
    options: &ConvergeOptions,
    mut render_and_measure: impl FnMut(usize, f64) -> Result<f64, E>,
) -> Result<Convergence, E> {
    let mut gain = target_i - input_i;
    let mut attempts = Vec::new();
    let mut best: Option<(f64, usize)> = None;
    let mut prev: Option<(f64, f64)> = None;
    for index in 0..options.max_renders as usize {
        let got = render_and_measure(index, gain)?;
        attempts.push(RenderAttempt {
            gain_db: gain,
            measured_i: got,
        });
        let err = target_i - got;
        if best.is_none_or(|(e, _)| err.abs() < e) {
            best = Some((err.abs(), index));
        }
        if err.abs() <= options.tolerance_lu {
            break;
        }
        let slope = match prev {
            Some((g0, m0))
                if (gain - g0).abs() > MIN_SECANT_DELTA && (got - m0).abs() > MIN_SECANT_DELTA =>
            {
                ((got - m0) / (gain - g0)).clamp(SLOPE_MIN, SLOPE_MAX)
            }
            _ => FIRST_STEP_SLOPE,
        };
        prev = Some((gain, got));
        gain += (err / slope).clamp(-MAX_STEP_DB, MAX_STEP_DB);
    }
    let (best_err, best) = best.expect("max_renders >= 1 guarantees one render");
    Ok(Convergence {
        attempts,
        best,
        within_tolerance: best_err <= options.tolerance_lu,
    })
}

/// Encoder for the delivered file. Renders are always `pcm_s16le` WAV; this is the
/// final encode from the kept render.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportEncoding {
    /// Arguments after `-i <kept render>`, e.g. `["-codec:a", "aac", "-b:a", "192k"]`.
    pub args: Vec<String>,
}

impl ExportEncoding {
    pub fn pcm_s16le() -> Self {
        Self::codec(&["-codec:a", "pcm_s16le"])
    }

    pub fn flac() -> Self {
        Self::codec(&["-codec:a", "flac"])
    }

    /// AAC-LC at 192 kb/s through FFmpeg's built-in `aac` encoder (no external library,
    /// so every catalog variant can produce it). Deliver as `.m4a`.
    pub fn aac_192k() -> Self {
        Self::codec(&["-codec:a", "aac", "-b:a", "192k"])
    }

    /// GenRight's format mapping: `m4a`/`aac` use the built-in AAC encoder, `flac` is
    /// FLAC, anything else is 16-bit PCM. MP3 is not encoded.
    pub fn for_format(format: &str) -> Self {
        match format {
            "m4a" | "aac" => Self::aac_192k(),
            "flac" => Self::flac(),
            _ => Self::pcm_s16le(),
        }
    }

    fn codec(args: &[&str]) -> Self {
        Self {
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LoudnessReport {
    pub target: LoudnessTarget,
    pub options: ConvergeOptions,
    /// Measurement of the input.
    pub source: LoudnessMeasurement,
    pub convergence: Convergence,
    /// Measurement of the delivered (encoded) file.
    pub delivered: LoudnessMeasurement,
    /// The kept render measured within tolerance of the target.
    pub within_tolerance: bool,
}

impl LoudnessReport {
    /// Integrated loudness of the kept, measured render.
    pub fn kept_measured_i(&self) -> f64 {
        self.convergence.kept().measured_i
    }

    /// Check the file as delivered (`delivered`, measured after encoding) against
    /// `acceptance`. Unlike [`LoudnessReport::ensure_within_tolerance`], which judges the
    /// kept render against the convergence tolerance, this is what a receiver sees.
    pub fn verify(&self, acceptance: &LoudnessAcceptance) -> Result<(), LoudnessError> {
        acceptance.verify(&self.delivered, &self.target)
    }

    /// Turn a best-effort result into a hard guarantee.
    pub fn ensure_within_tolerance(&self) -> Result<(), LoudnessError> {
        if self.within_tolerance {
            Ok(())
        } else {
            Err(LoudnessError::OutOfTolerance {
                measured_i: self.kept_measured_i(),
                target_i: self.target.integrated_lufs,
                tolerance_lu: self.options.tolerance_lu,
            })
        }
    }
}

/// Removes every temporary render on drop (success, error, or cancellation).
struct TempRenders(Vec<PathBuf>);

impl Drop for TempRenders {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

fn render_path(output: &Path, index: usize) -> PathBuf {
    let stem = output
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    output.with_file_name(format!(".{stem}.norm-{}-{index}.wav", std::process::id()))
}

/// Normalise `input` to `target` and encode it to `output`.
///
/// Measures the input, then renders `volume`+`alimiter` to temporary 16-bit WAVs next to
/// `output` and re-measures each one until a render is within
/// `options.tolerance_lu()` (at most `options.max_renders()` renders). The closest
/// render is encoded to `output` with `encoding` (progress reported through
/// `on_progress`), and the delivered file is measured again for the report. When no
/// render reaches the tolerance the closest one is still delivered and
/// `within_tolerance` is false; call [`LoudnessReport::ensure_within_tolerance`] to
/// make that an error. Silent input is rejected with [`LoudnessError::Silent`].
#[allow(clippy::too_many_arguments)]
pub fn normalize_file(
    ffmpeg: &dyn FfmpegBinary,
    input: &Path,
    output: &Path,
    target: &LoudnessTarget,
    encoding: &ExportEncoding,
    options: &ConvergeOptions,
    timeout: Duration,
    cancel: &CancellationToken,
    on_progress: Option<&mut dyn FnMut(&crate::ProgressEvent)>,
) -> Result<(LoudnessReport, RunReport), LoudnessError> {
    let source = measure_loudness(ffmpeg, input, target, timeout, cancel)?;
    if source.is_silent() {
        return Err(LoudnessError::Silent);
    }
    let mut temps = TempRenders(Vec::new());
    let convergence = converge_gain(
        target.integrated_lufs,
        source.input_i,
        options,
        |index, gain| -> Result<f64, LoudnessError> {
            let path = render_path(output, index);
            temps.0.push(path.clone());
            let mut cmd = FfmpegCommand::base("error");
            cmd.input(input)
                .text("-vn")
                .texts(["-af".to_string(), gain_limiter_filter(gain)])
                .texts(["-codec:a", "pcm_s16le"])
                .path(&path);
            run_checked(ffmpeg, &cmd, timeout, cancel)?;
            Ok(measure_loudness(ffmpeg, &path, target, timeout, cancel)?.input_i)
        },
    )?;
    let kept = render_path(output, convergence.best);
    let mut cmd = FfmpegCommand::base("error");
    cmd.progress_pipe()
        .input(&kept)
        .texts(encoding.args.iter().cloned())
        .path(output);
    let mut opts = RunOptions::new(timeout, cancel.clone());
    opts.on_progress = on_progress;
    let run = run_with_progress(ffmpeg, &cmd, opts)?;
    drop(temps);
    let delivered = measure_loudness(ffmpeg, output, target, timeout, cancel)?;
    let within_tolerance = convergence.within_tolerance;
    Ok((
        LoudnessReport {
            target: *target,
            options: *options,
            source,
            convergence,
            delivered,
            within_tolerance,
        },
        run,
    ))
}
