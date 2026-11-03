//! Frame statistics sampled with the editing FFmpeg.
//!
//! `ffmpeg` decodes `frames` evenly spaced frames of a clip span, downscales, and writes raw
//! `rgb24`; statistics are then computed in Rust (mean / std / p05 / p50 / p95 per channel, luma,
//! and mid-tone channel means for white balance). Pass the SDR conversion fragment as
//! `prefilter` so HDR and Apple Log clips are analysed in the SDR space they will be graded in.

use std::path::Path;

use serde::{Deserialize, Serialize};

use rightkit_ffmpeg::FfmpegCommand;

use crate::{clamp_or, run_media_capture, ColorError};

/// A clip's source span (seconds) to sample.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClipSpan {
    pub start_s: f64,
    pub duration_s: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SampleSpec {
    /// Frames to sample, 1..=64.
    pub frames: u32,
    /// Analysis width in pixels, 32..=640.
    pub width: u32,
}

impl Default for SampleSpec {
    fn default() -> Self {
        SampleSpec {
            frames: 12,
            width: 160,
        }
    }
}

/// Distribution of one channel on a 0..1 scale.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct ChannelStats {
    pub mean: f64,
    pub std: f64,
    pub p05: f64,
    pub p50: f64,
    pub p95: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct FrameStats {
    pub pixels: u64,
    pub r: ChannelStats,
    pub g: ChannelStats,
    pub b: ChannelStats,
    /// Rec.709 luma of the gamma-encoded RGB.
    pub luma: ChannelStats,
    /// Mean R, G, B over pixels with luma in 0.15..0.85 (white-balance estimate).
    pub midtone_mean: [f64; 3],
    pub midtone_pixels: u64,
}

fn channel(hist: &[u64; 256], sum: f64, sumsq: f64, n: u64) -> ChannelStats {
    if n == 0 {
        return ChannelStats::default();
    }
    let nf = n as f64;
    let mean = sum / nf;
    let var = (sumsq / nf - mean * mean).max(0.0);
    let pct = |p: f64| {
        let target = (p * nf).ceil().max(1.0) as u64;
        let mut acc = 0;
        for (i, c) in hist.iter().enumerate() {
            acc += c;
            if acc >= target {
                return i as f64 / 255.0;
            }
        }
        1.0
    };
    ChannelStats {
        mean: mean / 255.0,
        std: var.sqrt() / 255.0,
        p05: pct(0.05),
        p50: pct(0.50),
        p95: pct(0.95),
    }
}

/// Statistics of packed `rgb24` pixels (any number of frames concatenated).
pub fn stats_from_rgb24(data: &[u8]) -> FrameStats {
    let n = (data.len() / 3) as u64;
    let mut hist = [[0u64; 256]; 4];
    let mut sum = [0f64; 4];
    let mut sumsq = [0f64; 4];
    let mut mid = [0f64; 3];
    let mut mid_n = 0u64;
    for px in data.as_chunks::<3>().0 {
        let v = [f64::from(px[0]), f64::from(px[1]), f64::from(px[2])];
        let y = 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2];
        let vals = [v[0], v[1], v[2], y];
        for (k, x) in vals.iter().enumerate() {
            hist[k][x.round().clamp(0.0, 255.0) as usize] += 1;
            sum[k] += x;
            sumsq[k] += x * x;
        }
        if (0.15 * 255.0..=0.85 * 255.0).contains(&y) {
            for k in 0..3 {
                mid[k] += v[k];
            }
            mid_n += 1;
        }
    }
    let ch = |k: usize| channel(&hist[k], sum[k], sumsq[k], n);
    let mut midtone_mean = [0.0; 3];
    if mid_n > 0 {
        for k in 0..3 {
            midtone_mean[k] = mid[k] / mid_n as f64 / 255.0;
        }
    }
    FrameStats {
        pixels: n,
        r: ch(0),
        g: ch(1),
        b: ch(2),
        luma: ch(3),
        midtone_mean,
        midtone_pixels: mid_n,
    }
}

/// Sample `input` over `span` through `prefilter` (an optional `-vf` chain, e.g. the SDR
/// conversion fragment) and return its statistics.
pub fn sample_stats(
    ffmpeg: &Path,
    input: &Path,
    span: ClipSpan,
    spec: SampleSpec,
    prefilter: Option<&str>,
) -> Result<FrameStats, ColorError> {
    if !input.is_file() {
        return Err(ColorError::Process(format!(
            "input does not exist: {}",
            input.display()
        )));
    }
    let frames = spec.frames.clamp(1, 64);
    let width = spec.width.clamp(32, 640) & !1;
    let start = clamp_or(span.start_s, 0.0, 1e7, 0.0);
    let dur = clamp_or(span.duration_s, 0.04, 1e5, 0.04);
    let mut vf = String::new();
    if let Some(p) = prefilter.filter(|p| !p.is_empty()) {
        vf.push_str(p);
        vf.push(',');
    }
    vf.push_str(&format!(
        "fps={frames}/{dur:.6},scale={width}:-2:flags=area,format=rgb24"
    ));
    let mut cmd = FfmpegCommand::new();
    cmd.texts(["-nostdin", "-v", "error", "-ss"])
        .text(format!("{start:.6}"))
        .text("-t")
        .text(format!("{dur:.6}"))
        .input(input)
        .texts(["-an", "-vf"])
        .text(vf)
        .text("-frames:v")
        .text(frames.to_string())
        .texts(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"]);
    let out = run_media_capture(ffmpeg, &cmd)?;
    if !out.status.success() {
        return Err(ColorError::Process(out.stderr.trim().to_string()));
    }
    if out.stdout.len() < 3 {
        return Err(ColorError::Process(
            "no frames decoded for the requested span".into(),
        ));
    }
    Ok(stats_from_rgb24(&out.stdout))
}
