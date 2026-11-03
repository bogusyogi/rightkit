//! Loudness in pure Rust: ITU-R BS.1770-4 integrated loudness (K-weighting, 400 ms blocks with
//! 75 % overlap, -70 LUFS absolute and -10 LU relative gates), EBU Tech 3342 loudness range,
//! sample peak and 4x-oversampled true peak.
//!
//! This is the validator apps assert against (GenRight: exported voice FLACs at -16 LUFS +-1 LU
//! with true peak <= -1.5 dBTP; composed narration at -16 LUFS +-3 LU). It agrees with ffmpeg's
//! `ebur128` filter, the measurement GenRight already ships, to within [`FFMPEG_AGREEMENT_LU`]
//! on integrated loudness; [`ebur128_ffmpeg`] runs that filter for cross-checks.
//!
//! Non-WAV inputs (FLAC, MP3, MP4) are decoded through an ffmpeg binary whose path the caller
//! passes in ([`measure_file`]); WAV is read directly.
use crate::media::{read_audio, Pcm};
use crate::process::{run_owned, RunOptions, Tracker};
use crate::util::{err, Result};
use serde::Serialize;
use std::path::Path;
use std::time::Duration;

/// Maximum integrated-loudness difference from ffmpeg `ebur128` this implementation is tested to.
pub const FFMPEG_AGREEMENT_LU: f64 = 0.2;

const ABS_GATE_LUFS: f64 = -70.0;
const REL_GATE_LU: f64 = -10.0;
const LRA_REL_GATE_LU: f64 = -20.0;

/// Loudness of one signal. `None` where nothing passes the gates (silence, or shorter than one
/// 400 ms block for integrated / one 3 s window for range).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Loudness {
    pub integrated_lufs: Option<f64>,
    pub loudness_range_lu: Option<f64>,
    pub sample_peak_dbfs: f64,
    pub true_peak_dbtp: f64,
}

impl Loudness {
    /// Integrated loudness within `tolerance_lu` of `target_lufs` (false when ungated/silent).
    pub fn within(&self, target_lufs: f64, tolerance_lu: f64) -> bool {
        self.integrated_lufs
            .map(|i| (i - target_lufs).abs() <= tolerance_lu)
            .unwrap_or(false)
    }
}

/// Measure decoded PCM.
pub fn measure(pcm: &Pcm) -> Loudness {
    let ch = pcm.channels.max(1) as usize;
    let rate = pcm.sample_rate as f64;
    let frames = pcm.samples.len() / ch;
    // K-weighted per-channel energy, summed per 100 ms hop so blocks are cheap sums.
    let hop = ((rate * 0.1).round() as usize).max(1);
    let hops = frames / hop;
    let weights = channel_weights(ch);
    let mut hop_energy = vec![0f64; hops];
    let (mut sample_peak, mut true_peak) = (0f64, 0f64);
    for (c, &w) in weights.iter().enumerate() {
        let mut k = KWeighting::new(rate);
        let mut tp = TruePeak::new();
        for i in 0..frames {
            let x = pcm.samples[i * ch + c] as f64;
            sample_peak = sample_peak.max(x.abs());
            true_peak = true_peak.max(tp.push(x));
            let y = k.process(x);
            let h = i / hop;
            if h < hops {
                hop_energy[h] += w * y * y;
            }
        }
    }
    let to_lufs = |mean_energy: f64| -0.691 + 10.0 * mean_energy.max(1e-30).log10();
    // Mean weighted energy of a block spanning `n` hops starting at hop `s`.
    let block = |s: usize, n: usize| hop_energy[s..s + n].iter().sum::<f64>() / (n * hop) as f64;
    let blocks: Vec<f64> = if hops >= 4 {
        (0..=hops - 4).map(|s| block(s, 4)).collect()
    } else {
        vec![]
    };
    let integrated = gated_mean(&blocks, REL_GATE_LU).map(to_lufs);
    let short_term: Vec<f64> = if hops >= 30 {
        (0..=hops - 30).map(|s| block(s, 30)).collect()
    } else {
        vec![]
    };
    let lra = loudness_range(&short_term, &to_lufs);
    let db = |v: f64| 20.0 * v.max(1e-10).log10();
    Loudness {
        integrated_lufs: integrated,
        loudness_range_lu: lra,
        sample_peak_dbfs: db(sample_peak),
        true_peak_dbtp: db(true_peak.max(sample_peak)),
    }
}

/// Measure a file: WAV directly, anything else decoded through `ffmpeg`.
pub fn measure_file(path: &Path, ffmpeg: Option<&Path>, tracker: &Tracker) -> Result<Loudness> {
    Ok(measure(&read_audio(path, ffmpeg, tracker)?))
}

/// BS.1770 channel weights: 1.0 for L/R/C, 1.41 for surrounds, LFE excluded (5.0 and 5.1 order).
fn channel_weights(ch: usize) -> Vec<f64> {
    match ch {
        5 => vec![1.0, 1.0, 1.0, 1.41, 1.41],
        6 => vec![1.0, 1.0, 1.0, 0.0, 1.41, 1.41],
        n => vec![1.0; n],
    }
}

/// Absolute gate, then a relative gate `rel_lu` below the abs-gated mean; mean energy of survivors.
fn gated_mean(energies: &[f64], rel_lu: f64) -> Option<f64> {
    let to_lufs = |e: f64| -0.691 + 10.0 * e.max(1e-30).log10();
    let abs: Vec<f64> = energies
        .iter()
        .copied()
        .filter(|&e| to_lufs(e) > ABS_GATE_LUFS)
        .collect();
    if abs.is_empty() {
        return None;
    }
    let rel_gate = to_lufs(abs.iter().sum::<f64>() / abs.len() as f64) + rel_lu;
    let kept: Vec<f64> = abs.into_iter().filter(|&e| to_lufs(e) > rel_gate).collect();
    (!kept.is_empty()).then(|| kept.iter().sum::<f64>() / kept.len() as f64)
}

/// EBU Tech 3342: gated short-term loudness, 95th minus 10th percentile.
fn loudness_range(short_term: &[f64], to_lufs: &dyn Fn(f64) -> f64) -> Option<f64> {
    let abs: Vec<f64> = short_term
        .iter()
        .copied()
        .filter(|&e| to_lufs(e) > ABS_GATE_LUFS)
        .collect();
    if abs.is_empty() {
        return None;
    }
    let gate = to_lufs(abs.iter().sum::<f64>() / abs.len() as f64) + LRA_REL_GATE_LU;
    let mut l: Vec<f64> = abs.into_iter().map(to_lufs).filter(|&v| v > gate).collect();
    if l.is_empty() {
        return None;
    }
    l.sort_by(f64::total_cmp);
    let pct = |p: f64| l[(((l.len() - 1) as f64) * p).round() as usize];
    Some(pct(0.95) - pct(0.10))
}

/// Two-stage K-weighting (high shelf + RLB high pass), coefficients derived for any rate.
struct KWeighting {
    s1: Biquad,
    s2: Biquad,
}

impl KWeighting {
    fn new(rate: f64) -> Self {
        use std::f64::consts::PI;
        let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
        let k = (PI * f0 / rate).tan();
        let vh = 10f64.powf(g / 20.0);
        let vb = vh.powf(0.4996667741545416);
        let a0 = 1.0 + k / q + k * k;
        let s1 = Biquad::new(
            [
                (vh + vb * k / q + k * k) / a0,
                2.0 * (k * k - vh) / a0,
                (vh - vb * k / q + k * k) / a0,
            ],
            [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        );
        let (f0, q) = (38.13547087602444, 0.5003270373238773);
        let k = (PI * f0 / rate).tan();
        let d = 1.0 + k / q + k * k;
        let s2 = Biquad::new(
            [1.0, -2.0, 1.0],
            [2.0 * (k * k - 1.0) / d, (1.0 - k / q + k * k) / d],
        );
        Self { s1, s2 }
    }
    fn process(&mut self, x: f64) -> f64 {
        self.s2.process(self.s1.process(x))
    }
}

struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn new(b: [f64; 3], a: [f64; 2]) -> Self {
        Self { b, a, z: [0.0; 2] }
    }
    // Transposed direct form II.
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// 4x oversampling interpolator (BS.1770-4 Annex 2): 4 phases x 12 taps windowed sinc.
struct TruePeak {
    phases: [[f64; TAPS]; 4],
    hist: [f64; TAPS],
    pos: usize,
}

const TAPS: usize = 12;

impl TruePeak {
    fn new() -> Self {
        use std::f64::consts::PI;
        let mut phases = [[0f64; TAPS]; 4];
        let half = TAPS as f64 / 2.0;
        for (p, phase) in phases.iter_mut().enumerate() {
            let frac = p as f64 / 4.0;
            for (t, c) in phase.iter_mut().enumerate() {
                // Offset of tap t from the interpolated point, in input samples.
                let x = t as f64 - (half - 1.0) - frac;
                let sinc = if x.abs() < 1e-12 {
                    1.0
                } else {
                    (PI * x).sin() / (PI * x)
                };
                let w = 0.5 * (1.0 + (PI * x / (half + 0.5)).cos()); // Hann over the support
                *c = sinc * w.max(0.0);
            }
            let sum: f64 = phase.iter().sum();
            phase.iter_mut().for_each(|c| *c /= sum);
        }
        Self {
            phases,
            hist: [0.0; TAPS],
            pos: 0,
        }
    }
    /// Push one sample, return the largest absolute interpolated value around it.
    fn push(&mut self, x: f64) -> f64 {
        self.hist[self.pos] = x;
        self.pos = (self.pos + 1) % TAPS;
        let mut peak = 0f64;
        for phase in &self.phases {
            let mut acc = 0.0;
            for (t, c) in phase.iter().enumerate() {
                acc += c * self.hist[(self.pos + t) % TAPS];
            }
            peak = peak.max(acc.abs());
        }
        peak
    }
}

/// ffmpeg `ebur128=peak=true` summary (integrated, LRA, true peak), the measurement GenRight's
/// engine uses. `ffmpeg` is the binary path; nothing is looked up on `PATH`.
pub fn ebur128_ffmpeg(ffmpeg: &Path, input: &Path, tracker: &Tracker) -> Result<Loudness> {
    let args: Vec<String> = [
        "-hide_banner",
        "-nostats",
        "-nostdin",
        "-i",
        &input.to_string_lossy(),
        "-filter_complex",
        "ebur128=peak=true",
        "-f",
        "null",
        "-",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let out = run_owned(
        &ffmpeg.to_string_lossy(),
        &args,
        &RunOptions {
            timeout: Some(Duration::from_secs(300)),
            label: "ffmpeg ebur128".into(),
            ..Default::default()
        },
        tracker,
    )?;
    if out.code != Some(0) {
        return err(format!(
            "ffmpeg ebur128 failed ({:?}): {}",
            out.code,
            crate::util::tail(&out.stderr, 800)
        ));
    }
    parse_ebur128_summary(&out.stderr)
}

/// Parse the `Summary:` block ffmpeg's `ebur128` filter prints at the end of a run.
pub fn parse_ebur128_summary(text: &str) -> Result<Loudness> {
    let summary = match text.rfind("Summary:") {
        Some(i) => &text[i..],
        None => return err("ffmpeg ebur128 printed no Summary block"),
    };
    let value = |key: &str| -> Option<f64> {
        summary.lines().find_map(|l| {
            let rest = l.trim().strip_prefix(key)?;
            rest.split_whitespace().next()?.parse::<f64>().ok()
        })
    };
    let integrated = value("I:");
    let Some(i) = integrated else {
        return err("ffmpeg ebur128 summary has no integrated loudness (no audio stream?)");
    };
    let peak = value("Peak:").unwrap_or(f64::NEG_INFINITY);
    Ok(Loudness {
        // ffmpeg prints -70.0 for an all-gated signal.
        integrated_lufs: (i > ABS_GATE_LUFS).then_some(i),
        loudness_range_lu: value("LRA:"),
        sample_peak_dbfs: peak,
        true_peak_dbtp: peak,
    })
}
