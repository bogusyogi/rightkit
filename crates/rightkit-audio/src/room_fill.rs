//! Room-tone fill for removed gaps.
//!
//! Loops a measured quiet segment into a fill of the requested length. Each loop seam is an
//! equal-power crossfade, so the seam carries no step, and the fill is scaled so its RMS is
//! the measured segment's RMS plus `gain_db`. Pure Rust; the FFmpeg graph path for the
//! render stays in [`crate::room_tone::room_tone_fill_fragment`].

use crate::error::AudioError;

/// Fill parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FillParams {
    /// Equal-power crossfade length at each loop seam, ms (clamped to a quarter of the segment).
    pub crossfade_ms: f64,
    /// Level offset applied to the measured RMS, dB.
    pub gain_db: f64,
}

impl Default for FillParams {
    fn default() -> Self {
        Self {
            crossfade_ms: 20.0,
            gain_db: 0.0,
        }
    }
}

/// Build `len` samples of room tone from the measured `tone` segment.
///
/// The output RMS equals `rms(tone) * 10^(gain_db / 20)`. A silent `tone` yields silence.
/// Errors: `Invalid` for a zero rate or bad parameters; `NoRoomTone` when `tone` has fewer
/// than 8 samples.
pub fn room_tone_fill(
    tone: &[f32],
    rate: u32,
    len: usize,
    params: &FillParams,
) -> Result<Vec<f32>, AudioError> {
    if rate == 0 {
        return Err(AudioError::Invalid("sample rate must be positive".into()));
    }
    if !(params.crossfade_ms.is_finite() && params.crossfade_ms >= 0.0)
        || !params.gain_db.is_finite()
    {
        return Err(AudioError::Invalid("fill parameters out of range".into()));
    }
    if len == 0 {
        return Ok(Vec::new());
    }
    if tone.len() < 8 {
        return Err(AudioError::NoRoomTone(
            "room-tone segment has fewer than 8 samples".into(),
        ));
    }

    let t: Vec<f64> = tone.iter().map(|&x| x as f64).collect();
    let seg = t.len();
    let xf = ((params.crossfade_ms * rate as f64 / 1000.0).round() as usize).min(seg / 4);
    let period = seg - xf;

    // One loop cycle. Samples [xf, period) are the segment itself. The head [0, xf) fades
    // from the segment's continuation t[period + i] into its own start t[i], so the cycle
    // joins its own end with no step.
    let mut cycle = Vec::with_capacity(period);
    for i in 0..period {
        if i < xf {
            let w = std::f64::consts::FRAC_PI_2 * i as f64 / xf as f64;
            cycle.push(w.sin() * t[i] + w.cos() * t[period + i]);
        } else {
            cycle.push(t[i]);
        }
    }

    let mut out: Vec<f64> = Vec::with_capacity(len);
    while out.len() < len {
        let take = (len - out.len()).min(period);
        out.extend_from_slice(&cycle[..take]);
    }

    let target = rms(&t) * 10f64.powf(params.gain_db / 20.0);
    let got = rms(&out);
    if got <= 0.0 {
        return Ok(vec![0.0; len]);
    }
    let g = target / got;
    Ok(out.iter().map(|&x| (x * g) as f32).collect())
}

/// Root-mean-square of `x` (linear).
pub fn rms(x: &[f64]) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
}

/// One gap to fill: `len` samples starting at `start` in a bed, looped from `tone`.
#[derive(Debug, Clone, Copy)]
pub struct ToneFill<'a> {
    pub start: usize,
    pub len: usize,
    pub tone: &'a [f32],
}

/// A bed of `total` samples: each fill is looped room tone ([`room_tone_fill`]), and every
/// other sample is zero. A fill past the end is clipped. Errors as [`room_tone_fill`].
pub fn room_tone_bed(
    total: usize,
    rate: u32,
    fills: &[ToneFill<'_>],
    params: &FillParams,
) -> Result<Vec<f32>, AudioError> {
    let mut bed = vec![0.0f32; total];
    for fill in fills {
        let filled = room_tone_fill(fill.tone, rate, fill.len, params)?;
        let end = fill.start.saturating_add(filled.len()).min(total);
        if fill.start >= end {
            continue;
        }
        bed[fill.start..end].copy_from_slice(&filled[..end - fill.start]);
    }
    Ok(bed)
}

/// Encode mono f32 samples as an IEEE-float WAV (format 3, one channel, `rate` Hz).
pub fn encode_mono_f32_wav(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 4) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}
