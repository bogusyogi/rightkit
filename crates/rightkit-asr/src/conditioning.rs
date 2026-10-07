//! ASR-facing audio conditioning, moved from HeardRight (`heardright_core::audio_conditioning`)
//! so every app feeds Parakeet the same front end. HeardRight consumes this copy once it adopts
//! rightkit-asr; until then the two must stay byte-identical (the Phase 6 parity gate checks it).

/// Apply the app's ASR-facing audio conditioning policy to a mono f32 buffer.
///
/// `raw` returns the input unchanged. The default (`asr_simple_gain_hpf`) is the
/// deliberately-shipped policy: remove DC, light one-pole 80 Hz high-pass, boost
/// quiet speech (< -31 dBFS) toward -20 dBFS (gain clamped 1-12x), tanh soft-limit.
/// The Parakeet TDT ASR path is level-robust and gains ~no WER from heavier
/// conditioning, so this simple gain+HPF is the deliberate choice, not a stopgap.
/// The WAKE model has its OWN mandatory conditioning (HPF + rolling-RMS AGC) —
/// separate from this; do not conflate. Pure fn so engine callers can test it
/// without audio devices or model state.
///
/// Legacy alias: `adaptive_hot_v2_5` still reaches this same implementation for
/// old scripts/env overrides, but new reports should use `asr_simple_gain_hpf`.
pub fn condition_for_asr(samples: &[f32], sample_rate: u32, policy: &str) -> Vec<f32> {
    if samples.is_empty() || policy.eq_ignore_ascii_case("raw") {
        return samples.to_vec();
    }

    let finite: Vec<f32> = samples
        .iter()
        .map(|s| if s.is_finite() { *s } else { 0.0 })
        .collect();
    let mean = finite.iter().sum::<f32>() / finite.len() as f32;
    let centered: Vec<f32> = finite.into_iter().map(|s| s - mean).collect();

    let mut conditioned = high_pass_one_pole(&centered, sample_rate.max(1), 80.0);
    recenter(&mut conditioned);
    let level = rms(&conditioned);
    if level > 0.0 && level <= db_to_amp(-31.0) {
        let target = db_to_amp(-20.0);
        let gain = (target / level).clamp(1.0, 12.0);
        for sample in &mut conditioned {
            *sample = soft_limit(*sample * gain);
        }
    } else {
        for sample in &mut conditioned {
            *sample = sample.clamp(-0.99, 0.99);
        }
    }
    conditioned
}

pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

fn db_to_amp(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

fn high_pass_one_pole(samples: &[f32], sample_rate: u32, cutoff_hz: f32) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }
    let dt = 1.0 / sample_rate as f32;
    let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz.max(1.0));
    let alpha = rc / (rc + dt);
    let mut out = Vec::with_capacity(samples.len());
    let mut prev_y = 0.0;
    let mut prev_x = samples[0];
    for &x in samples {
        let y = alpha * (prev_y + x - prev_x);
        out.push(y);
        prev_y = y;
        prev_x = x;
    }
    out
}

fn recenter(samples: &mut [f32]) {
    if samples.is_empty() {
        return;
    }
    let mean = samples.iter().sum::<f32>() / samples.len() as f32;
    for sample in samples {
        *sample -= mean;
    }
}

fn soft_limit(sample: f32) -> f32 {
    if sample.abs() <= 0.99 {
        sample
    } else {
        sample.tanh().clamp(-0.99, 0.99)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_policy_leaves_audio_unchanged() {
        let input = vec![0.10, -0.20, 0.30, -0.40];

        let output = condition_for_asr(&input, 16_000, "raw");

        assert_eq!(output, input);
    }

    #[test]
    fn adaptive_policy_removes_dc_offset() {
        let input = vec![0.20, 0.22, 0.18, 0.20];

        let output = condition_for_asr(&input, 16_000, "asr_simple_gain_hpf");
        let mean = output.iter().sum::<f32>() / output.len() as f32;

        assert!(mean.abs() < 0.001, "mean should be near zero, got {mean}");
    }

    #[test]
    fn adaptive_policy_boosts_quiet_speech_without_clipping() {
        let input: Vec<f32> = (0..1600)
            .map(|i| if i % 2 == 0 { 0.003 } else { -0.003 })
            .collect();

        let output = condition_for_asr(&input, 16_000, "asr_simple_gain_hpf");
        let input_rms = rms(&input);
        let output_rms = rms(&output);
        let peak = output.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()));

        assert!(output_rms > input_rms * 2.0);
        assert!(peak <= 0.99);
    }

    #[test]
    fn adaptive_policy_keeps_silence_silent() {
        // The gain gate must NOT boost true silence into noise, and must never
        // emit NaN/inf (level == 0 → division-by-zero trap in a naive AGC).
        let input = vec![0.0_f32; 1600];

        let output = condition_for_asr(&input, 16_000, "asr_simple_gain_hpf");

        assert_eq!(output.len(), input.len());
        assert!(output.iter().all(|s| s.is_finite()), "no NaN/inf");
        assert!(
            rms(&output) < 1e-6,
            "silence must stay silent, got {}",
            rms(&output)
        );
    }

    #[test]
    fn adaptive_policy_does_not_boost_loud_audio_and_caps_peaks() {
        // Above the quiet threshold the gain branch must NOT fire (no runaway
        // boost), and the soft-limiter must keep peaks below full scale.
        let input: Vec<f32> = (0..1600)
            .map(|i| if i % 2 == 0 { 0.8 } else { -0.8 })
            .collect();

        let output = condition_for_asr(&input, 16_000, "asr_simple_gain_hpf");
        let peak = output.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()));

        assert!(output.iter().all(|s| s.is_finite()));
        // not boosted beyond the input level (loud stays ~loud, not amplified)
        assert!(
            rms(&output) <= rms(&input) * 1.05,
            "loud audio must not be boosted"
        );
        assert!(peak <= 0.99, "peaks must be soft-limited below full scale");
    }
}
