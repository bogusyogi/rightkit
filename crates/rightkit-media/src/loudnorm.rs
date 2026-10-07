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
    let end = stderr
        .rfind('}')
        .ok_or_else(|| bad("no loudnorm output".into()))?;
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
    /// Arguments after `-i <kept render>`, e.g. `["-codec:a", "libmp3lame", "-q:a", "2"]`.
    pub args: Vec<String>,
}

impl ExportEncoding {
    pub fn pcm_s16le() -> Self {
        Self::codec(&["-codec:a", "pcm_s16le"])
    }

    pub fn flac() -> Self {
        Self::codec(&["-codec:a", "flac"])
    }

    /// LAME VBR quality 2. Needs an FFmpeg with libmp3lame (the `export-lame-ass`
    /// catalog variant, or an unverified development build).
    pub fn mp3_lame_vbr2() -> Self {
        Self::codec(&["-codec:a", "libmp3lame", "-q:a", "2"])
    }

    /// GenRight's format mapping: `mp3`, `flac`, anything else is 16-bit PCM.
    pub fn for_format(format: &str) -> Self {
        match format {
            "mp3" => Self::mp3_lame_vbr2(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Loudness through a limiter: linear below the knee, compressed above it.
    fn limiter_model(input_i: f64, knee: f64, above_slope: f64) -> impl Fn(f64) -> f64 {
        move |gain| {
            let linear = input_i + gain;
            if linear <= knee {
                linear
            } else {
                knee + (linear - knee) * above_slope
            }
        }
    }

    #[test]
    fn options_bound_tolerance_to_half_an_lu_and_renders_to_the_limit() {
        assert!(ConvergeOptions::new(0.5, 5).is_ok());
        assert!(ConvergeOptions::new(0.2, 1).is_ok());
        for (t, n) in [
            (0.0, 5),
            (0.51, 5),
            (1.0, 5),
            (f64::NAN, 5),
            (0.5, 0),
            (0.5, 17),
        ] {
            assert!(
                matches!(
                    ConvergeOptions::new(t, n),
                    Err(LoudnessError::InvalidOptions(_))
                ),
                "{t} {n}"
            );
        }
        let d = ConvergeOptions::default();
        assert_eq!((d.tolerance_lu(), d.max_renders()), (0.5, 5));
    }

    #[test]
    fn linear_response_converges_on_the_first_render() {
        let model = limiter_model(-30.0, 0.0, 1.0);
        let c = converge_gain(-16.0, -30.0, &ConvergeOptions::default(), |_, g| {
            Ok::<_, ()>(model(g))
        })
        .unwrap();
        assert_eq!(c.attempts.len(), 1);
        assert!(c.within_tolerance);
        assert!((c.kept().gain_db - 14.0).abs() < 1e-9);
    }

    #[test]
    fn limiter_compression_is_corrected_by_secant_steps_within_half_an_lu() {
        // Above -22 LUFS every dB of gain yields only 0.4 LU: one-shot gain undershoots.
        let model = limiter_model(-30.0, -22.0, 0.4);
        let c = converge_gain(-16.0, -30.0, &ConvergeOptions::default(), |_, g| {
            Ok::<_, ()>(model(g))
        })
        .unwrap();
        assert!(c.within_tolerance, "{c:?}");
        assert!(c.attempts.len() > 1 && c.attempts.len() <= 5);
        assert!((c.kept().measured_i - -16.0).abs() <= 0.5);
        // The first correction has no secant yet, so it uses GenRight's 0.45 slope.
        let first = c.attempts[0];
        assert!((first.gain_db - 14.0).abs() < 1e-9);
        let err0 = -16.0 - first.measured_i;
        assert!((c.attempts[1].gain_db - (14.0 + (err0 / 0.45).clamp(-8.0, 8.0))).abs() < 1e-9);
    }

    #[test]
    fn iterations_are_bounded_and_the_closest_measured_render_is_kept() {
        // A hard ceiling: loudness can never exceed -18 LUFS, so -16 is unreachable.
        let model = |g: f64| (-30.0 + g).min(-18.0);
        let mut calls = 0;
        let c = converge_gain(
            -16.0,
            -30.0,
            &ConvergeOptions::new(0.5, 4).unwrap(),
            |_, g| {
                calls += 1;
                Ok::<_, ()>(model(g))
            },
        )
        .unwrap();
        assert_eq!(calls, 4);
        assert_eq!(c.attempts.len(), 4);
        assert!(!c.within_tolerance);
        let best_err = c
            .attempts
            .iter()
            .map(|a| (-16.0 - a.measured_i).abs())
            .fold(f64::INFINITY, f64::min);
        assert_eq!((-16.0 - c.kept().measured_i).abs(), best_err);
        // Earliest render wins ties: every render measured -18.
        assert_eq!(c.best, 0);
    }

    #[test]
    fn step_size_is_clamped_to_eight_db() {
        let c = converge_gain(
            -16.0,
            -16.0,
            &ConvergeOptions::new(0.5, 2).unwrap(),
            |i, g| Ok::<_, ()>(if i == 0 { -60.0 } else { g }),
        )
        .unwrap();
        assert!((c.attempts[0].gain_db - 0.0).abs() < 1e-9);
        assert!((c.attempts[1].gain_db - 8.0).abs() < 1e-9);
    }

    #[test]
    fn render_errors_abort_the_loop() {
        let r = converge_gain(-16.0, -30.0, &ConvergeOptions::default(), |i, _| {
            if i == 1 {
                Err("boom")
            } else {
                Ok(-20.0)
            }
        });
        assert_eq!(r.unwrap_err(), "boom");
    }

    #[test]
    fn filter_and_encodings_match_genright_text() {
        assert_eq!(
            gain_limiter_filter(4.236),
            "volume=4.24dB,alimiter=limit=0.63:level=false:attack=5:release=50"
        );
        assert_eq!(
            ExportEncoding::for_format("mp3").args,
            ["-codec:a", "libmp3lame", "-q:a", "2"]
        );
        assert_eq!(
            ExportEncoding::for_format("flac").args,
            ["-codec:a", "flac"]
        );
        assert_eq!(
            ExportEncoding::for_format("wav").args,
            ["-codec:a", "pcm_s16le"]
        );
        assert_eq!(
            LoudnessTarget::SPEECH_EXPORT,
            LoudnessTarget {
                integrated_lufs: -16.0,
                true_peak_db: -1.5,
                lra: 11.0
            }
        );
    }
}
