use std::path::{Path, PathBuf};

// ── Whisper log-mel front-end (canonical openai/whisper spec) ───────────────
// Replaces the WhisperKit MelSpectrogram CoreML stage. Spec (pinned): n_fft=400,
// hop=160, periodic Hann, reflect-pad n_fft/2, 128 slaney mel filters, log10
// clamped to (max-8), then (x+4)/4. The filterbank bytes ARE whisper's shipped
// mel_128 (assets/mel_filters.npz). Verified by numerical diff against the numpy
// oracle (scripts/whisper_mel.py) to ≤1e-4/element BEFORE trusting CER — a subtly
// wrong mel transcribes fine then degrades silently (root cause on both platforms).
const MEL_N_FFT: usize = 400;
const MEL_HOP: usize = 160;
pub(crate) const MEL_N_MELS: usize = 128;
const MEL_N_FREQ: usize = 201; // n_fft/2 + 1
static MEL_FILTERS_BIN: &[u8] = include_bytes!("../whisper_mel_128.bin"); // 128*201 f32 LE

fn mel_filters() -> Vec<f32> {
    MEL_FILTERS_BIN
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn mel_spectrum(power: &[f32], frames: usize) -> Vec<f32> {
    let filt = mel_filters();
    // The FFT power is nonnegative. For finite power, multiplying by a zero
    // filter coefficient adds only signed zero to the nonnegative sum. Keep
    // every nonzero product in its original increasing-frequency order.
    // Nonfinite power must retain 0 * NaN/Inf, so it uses the full dense range.
    let finite_power = power.iter().all(|value| value.is_finite());
    let mut mel = vec![0f32; MEL_N_MELS * frames];
    for m in 0..MEL_N_MELS {
        let frow = &filt[m * MEL_N_FREQ..(m + 1) * MEL_N_FREQ];
        let frequencies = if finite_power {
            let first = frow
                .iter()
                .position(|&weight| weight != 0.0)
                .unwrap_or(MEL_N_FREQ);
            let end = frow
                .iter()
                .rposition(|&weight| weight != 0.0)
                .map_or(first, |k| k + 1);
            first..end
        } else {
            0..MEL_N_FREQ
        };
        for f in 0..frames {
            let mut s = 0f32;
            for k in frequencies.clone() {
                s += frow[k] * power[k * frames + f];
            }
            mel[m * frames + f] = s;
        }
    }
    mel
}

/// Canonical whisper log-mel. `audio` = 16 kHz mono f32; returns `[128 × frames]`
/// C-order (mel-major), `frames = audio_len/hop` (3000 for a 30 s window) — the exact
/// layout the AudioEncoder's `melspectrogram_features [1,128,1,3000]` input expects.
pub fn log_mel(audio: &[f32]) -> Vec<f32> {
    use realfft::RealFftPlanner;
    let pad = MEL_N_FFT / 2; // 200
    let n = audio.len();
    let mut a = vec![0f32; n + 2 * pad];
    for i in 0..pad {
        a[i] = audio[pad - i]; // numpy 'reflect' (no edge repeat)
    }
    a[pad..pad + n].copy_from_slice(audio);
    for i in 0..pad {
        a[pad + n + i] = audio[n - 2 - i];
    }
    // periodic Hann (== torch.hann_window(400) == np.hanning(401)[:-1])
    let win: Vec<f32> = (0..MEL_N_FFT)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / MEL_N_FFT as f32).cos())
        .collect();
    let mut planner = RealFftPlanner::<f32>::new();
    let r2c = planner.plan_fft_forward(MEL_N_FFT);
    let mut inbuf = r2c.make_input_vec();
    let mut outbuf = r2c.make_output_vec();
    let frames = (a.len() - MEL_N_FFT) / MEL_HOP; // 1 + ... then drop last → audio_len/hop
    let mut power = vec![0f32; MEL_N_FREQ * frames];
    for f in 0..frames {
        for i in 0..MEL_N_FFT {
            inbuf[i] = a[f * MEL_HOP + i] * win[i];
        }
        r2c.process(&mut inbuf, &mut outbuf).expect("rfft");
        for k in 0..MEL_N_FREQ {
            power[k * frames + f] = outbuf[k].norm_sqr(); // |stft|^2
        }
    }
    let mut mel = mel_spectrum(&power, frames);
    let mut mx = f32::NEG_INFINITY;
    for v in mel.iter_mut() {
        *v = v.max(1e-10).log10();
        if *v > mx {
            mx = *v;
        }
    }
    let floor = mx - 8.0;
    for v in mel.iter_mut() {
        *v = (v.max(floor) + 4.0) / 4.0;
    }
    mel
}

/// Default tokenizer path is the bundled `tokenizer.json` beside the CoreML
/// bundle. `RIGHTKIT_WHISPER_TOKENIZER` remains as a QA override only.
pub fn tokenizer_path(model_dir: &Path) -> PathBuf {
    if let Ok(p) = rightkit_dictation_core::dev_var!("RIGHTKIT_WHISPER_TOKENIZER") {
        return PathBuf::from(p);
    }
    model_dir.join("tokenizer.json")
}

#[cfg(test)]
mod tests {
    use super::{mel_filters, mel_spectrum, MEL_N_FREQ, MEL_N_MELS};

    fn dense_reference(power: &[f32], frames: usize) -> Vec<f32> {
        let filt = mel_filters();
        let mut mel = vec![0f32; MEL_N_MELS * frames];
        for m in 0..MEL_N_MELS {
            let frow = &filt[m * MEL_N_FREQ..(m + 1) * MEL_N_FREQ];
            for f in 0..frames {
                let mut s = 0f32;
                for k in 0..MEL_N_FREQ {
                    s += frow[k] * power[k * frames + f];
                }
                mel[m * frames + f] = s;
            }
        }
        mel
    }

    fn assert_power_bits(power: &[f32], frames: usize) {
        let expected = dense_reference(power, frames);
        let actual = mel_spectrum(power, frames);
        assert_eq!(actual.len(), expected.len());
        for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
            assert_eq!(actual.to_bits(), expected.to_bits(), "power output {index}");
        }
    }

    #[test]
    fn pinned_filterbank_shape_values_and_signed_zero() {
        let filters = mel_filters();
        assert_eq!((MEL_N_MELS, MEL_N_FREQ), (128, 201));
        assert_eq!(filters.len(), 25_728);
        assert_eq!(filters.iter().filter(|&&value| value != 0.0).count(), 394);
        assert!(filters
            .iter()
            .all(|value| value.is_finite() && *value >= 0.0));
        assert_eq!(filters[0].to_bits(), (-0.0_f32).to_bits());
        assert_eq!(
            filters
                .iter()
                .filter(|value| value.to_bits() == (-0.0_f32).to_bits())
                .count(),
            1
        );
    }

    #[test]
    fn sparse_finite_power_preserves_every_dense_output_bit() {
        let frames = 7;
        let power: Vec<f32> = (0..MEL_N_FREQ * frames)
            .map(|index| match index % 7 {
                0 => 0.0,
                1 => -0.0,
                2 => f32::from_bits(1),
                3 => f32::MIN_POSITIVE,
                4 => 1.0,
                5 => 1.0e20,
                _ => f32::MAX,
            })
            .collect();
        assert_power_bits(&power, frames);
    }

    #[test]
    fn nonfinite_power_retains_zero_coefficient_products() {
        let frames = 3;
        let filters = mel_filters();
        assert!(filters
            .as_chunks::<MEL_N_FREQ>()
            .0
            .iter()
            .all(|row| row[0] == 0.0));
        for special in [
            f32::from_bits(0x7fc0_1234),
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            for frame in 0..frames {
                let mut power = vec![1.0; MEL_N_FREQ * frames];
                power[frame] = special; // Frequency bin 0 has zero weight in every row.
                let expected = dense_reference(&power, frames);
                assert!(expected.chunks_exact(frames).all(|row| row[frame].is_nan()));
                assert_power_bits(&power, frames);
            }
        }
    }
}
