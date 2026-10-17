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

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic white noise in [-amp, amp).
    fn noise(n: usize, amp: f32, seed: &mut u32) -> Vec<f32> {
        (0..n)
            .map(|_| {
                *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                ((*seed >> 8) as f32 / 8_388_608.0 - 1.0) * amp
            })
            .collect()
    }

    fn rms_db(x: &[f32]) -> f64 {
        let p = x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / x.len() as f64;
        10.0 * p.log10()
    }

    fn max_step(x: &[f32]) -> f32 {
        x.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    }

    const RATE: u32 = 16_000;

    #[test]
    fn fill_rms_matches_the_measured_room_tone_for_each_gap_length() {
        let mut seed = 42;
        // One second of room tone at about -54 dBFS.
        let tone = noise(RATE as usize, 0.002, &mut seed);
        let want = rms_db(&tone);
        // Gaps shorter than, equal to, and several times longer than the tone segment.
        for gap_ms in [80u64, 333, 1000, 2500, 7321] {
            let len = (RATE as u64 * gap_ms / 1000) as usize;
            let fill = room_tone_fill(&tone, RATE, len, &FillParams::default()).expect("fill");
            assert_eq!(fill.len(), len);
            let got = rms_db(&fill);
            assert!(
                (got - want).abs() < 0.1,
                "gap {gap_ms} ms: fill {got:.3} dB vs tone {want:.3} dB"
            );
        }
    }

    #[test]
    fn gain_offset_shifts_the_fill_level_exactly() {
        let mut seed = 7;
        let tone = noise(RATE as usize, 0.002, &mut seed);
        let want = rms_db(&tone);
        let params = FillParams {
            crossfade_ms: 20.0,
            gain_db: -6.0,
        };
        let fill = room_tone_fill(&tone, RATE, 3 * RATE as usize, &params).expect("fill");
        assert!((rms_db(&fill) - (want - 6.0)).abs() < 0.1);
    }

    #[test]
    fn loop_seams_have_no_step_larger_than_the_tone_itself() {
        let mut seed = 99;
        let tone = noise(RATE as usize, 0.002, &mut seed);
        let params = FillParams::default();
        let fill = room_tone_fill(&tone, RATE, 3 * RATE as usize, &params).expect("fill");
        // Seams sit where one loop cycle ends. The cycle length is the segment minus the
        // crossfade (20 ms at this rate, clamped to a quarter of the segment).
        let xf =
            ((params.crossfade_ms * RATE as f64 / 1000.0).round() as usize).min(tone.len() / 4);
        let period = tone.len() - xf;
        let limit = 1.5 * max_step(&tone);
        for k in 1..3 {
            let at = k * period;
            let step = (fill[at] - fill[at - 1]).abs();
            assert!(step <= limit, "seam {k} step {step} exceeds {limit}");
        }
    }

    #[test]
    fn silent_tone_fills_with_silence_and_bad_input_is_refused() {
        let silent = vec![0.0f32; RATE as usize];
        let fill = room_tone_fill(&silent, RATE, 1000, &FillParams::default()).expect("fill");
        assert!(fill.iter().all(|&v| v == 0.0));

        let mut seed = 5;
        let tone = noise(RATE as usize, 0.002, &mut seed);
        assert!(room_tone_fill(&tone[..4], RATE, 100, &FillParams::default()).is_err());
        assert!(room_tone_fill(&tone, 0, 100, &FillParams::default()).is_err());
        assert!(matches!(
            room_tone_fill(
                &tone,
                RATE,
                100,
                &FillParams {
                    crossfade_ms: -1.0,
                    gain_db: 0.0
                }
            ),
            Err(AudioError::Invalid(_))
        ));
        assert!(room_tone_fill(&tone, RATE, 0, &FillParams::default())
            .expect("empty")
            .is_empty());
    }
}
