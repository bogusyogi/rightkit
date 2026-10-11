
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
static MEL_FILTERS_BIN: &[u8] = include_bytes!("../../whisper_mel_128.bin"); // 128*201 f32 LE

fn mel_filters() -> Vec<f32> {
    MEL_FILTERS_BIN
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
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

