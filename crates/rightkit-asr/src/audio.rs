use crate::config::PreprocessorConfig;
use crate::error::{Error, Result};
use crate::whisper::WHISPER_SAMPLE_RATE;
use hound::{WavReader, WavSpec};
use ndarray::Array2;
use std::f32::consts::PI;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Why [`load_wav_mono_16k_head`] refused a file. Each variant says what the file
/// holds, so a host can tell the user what to convert.
#[derive(Debug)]
pub enum WavLoadError {
    /// The file could not be opened or read.
    Io(std::io::Error),
    /// The bytes are not a WAV file this reader can parse, or the samples are truncated.
    Malformed(String),
    /// A sample format other than 16, 24 or 32-bit integer PCM and 32-bit float.
    /// `float` is true for a float file.
    UnsupportedFormat { float: bool, bits_per_sample: u16 },
    /// A sample rate other than 16 kHz. This crate does not resample; convert the file first.
    UnsupportedSampleRate(u32),
    /// The header declares zero channels.
    NoChannels,
    /// A float sample is NaN or infinite.
    NonFiniteSample,
}

impl std::fmt::Display for WavLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WavLoadError::Io(e) => write!(f, "could not read the WAV file: {e}"),
            WavLoadError::Malformed(m) => write!(f, "malformed WAV file: {m}"),
            WavLoadError::UnsupportedFormat { float, bits_per_sample } => write!(
                f,
                "unsupported WAV sample format: {bits_per_sample}-bit {}; expected 16, 24 or 32-bit integer PCM or 32-bit float",
                if *float { "float" } else { "integer" }
            ),
            WavLoadError::UnsupportedSampleRate(rate) => {
                write!(f, "unsupported WAV sample rate {rate} Hz; expected 16000 Hz (no resampling)")
            }
            WavLoadError::NoChannels => write!(f, "WAV file declares zero channels"),
            WavLoadError::NonFiniteSample => write!(f, "WAV file contains a NaN or infinite sample"),
        }
    }
}

impl std::error::Error for WavLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WavLoadError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<WavLoadError> for Error {
    fn from(e: WavLoadError) -> Self {
        Error::Audio(e.to_string())
    }
}

/// Read at most the first `max_duration` of a WAV file as mono 16 kHz `f32` in `[-1, 1]`,
/// for example to identify the language before decoding the whole recording.
///
/// Reading stops once the requested samples are in hand, so a long file costs no more
/// than its head. Accepts 16, 24 or 32-bit integer PCM and 32-bit float, mono or
/// multi-channel (channels are averaged). The file must already be 16 kHz: this crate
/// has no resampler, and any other rate is [`WavLoadError::UnsupportedSampleRate`].
/// A file shorter than `max_duration` yields all of its samples.
pub fn load_wav_mono_16k_head<P: AsRef<Path>>(
    path: P,
    max_duration: Duration,
) -> std::result::Result<Vec<f32>, WavLoadError> {
    let file = File::open(path).map_err(WavLoadError::Io)?;
    let mut reader =
        WavReader::new(BufReader::new(file)).map_err(|e| WavLoadError::Malformed(e.to_string()))?;
    let spec = reader.spec();
    let float = spec.sample_format == hound::SampleFormat::Float;
    let supported = match spec.sample_format {
        hound::SampleFormat::Int => matches!(spec.bits_per_sample, 16 | 24 | 32),
        hound::SampleFormat::Float => spec.bits_per_sample == 32,
    };
    if !supported {
        return Err(WavLoadError::UnsupportedFormat { float, bits_per_sample: spec.bits_per_sample });
    }
    if spec.sample_rate != WHISPER_SAMPLE_RATE {
        return Err(WavLoadError::UnsupportedSampleRate(spec.sample_rate));
    }
    let channels = spec.channels as usize;
    if channels == 0 {
        return Err(WavLoadError::NoChannels);
    }
    // Frames wanted from the head, capped by what the file holds.
    let wanted = usize::try_from(max_duration.as_nanos() * WHISPER_SAMPLE_RATE as u128 / 1_000_000_000)
        .unwrap_or(usize::MAX);
    let frames = wanted.min(reader.duration() as usize);
    let take = frames.saturating_mul(channels);
    let malformed = |e: hound::Error| WavLoadError::Malformed(e.to_string());
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            // hound returns integer samples in the file's own bit range, so scale by 2^(bits-1).
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .take(take)
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<std::result::Result<Vec<f32>, hound::Error>>()
                .map_err(malformed)?
        }
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .take(take)
            .collect::<std::result::Result<Vec<f32>, hound::Error>>()
            .map_err(malformed)?,
    };
    if samples.iter().any(|v| !v.is_finite()) {
        return Err(WavLoadError::NonFiniteSample);
    }
    Ok(samples
        .chunks(channels)
        .map(|c| c.iter().sum::<f32>() / channels as f32)
        .collect())
}

/// Audio handed to a transcriber. WAV stays supported in-crate; every other
/// container or codec is the caller's job: decode it to mono 16 kHz `f32` in
/// `[-1, 1]` and pass [`AudioSource::Pcm16k`]. This crate does not depend on
/// ffmpeg or any other decoder.
#[derive(Debug, Clone)]
pub enum AudioSource {
    /// A WAV file (16-bit PCM or 32-bit float) read by this crate. Multi-channel
    /// input is downmixed; sample rate must be 16 kHz.
    Wav(PathBuf),
    /// Caller-decoded mono PCM at 16 kHz, samples in `[-1, 1]`. Non-finite
    /// samples are rejected.
    Pcm16k(Vec<f32>),
}

/// Read a WAV file into interleaved `f32` samples in `[-1, 1]` plus its
/// [`WavSpec`] (sample rate, channel count, format).
///
/// Accepts 16-bit integer PCM and 32-bit float WAV; other formats are an
/// `Error::Audio`. Nothing is resampled or downmixed: for Whisper input use
/// [`load_wav_mono_16k`](crate::load_wav_mono_16k), which also requires 16 kHz
/// and averages channels to mono. This is the function [`AudioSource::Wav`] uses.
pub fn load_audio<P: AsRef<Path>>(path: P) -> Result<(Vec<f32>, WavSpec)> {
    let mut reader = WavReader::open(path)?;
    let spec = reader.spec();

    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Audio(format!("Failed to read float samples: {e}")))?,
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.map(|s| s as f32 / 32768.0))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Audio(format!("Failed to read int samples: {e}")))?,
    };

    Ok((samples, spec))
}

pub fn apply_preemphasis(audio: &[f32], coef: f32) -> Vec<f32> {
    if audio.is_empty() {
        return Vec::new();
    }

    let mut result = Vec::with_capacity(audio.len());
    result.push(audio[0]);

    for i in 1..audio.len() {
        result.push(audio[i] - coef * audio[i - 1]);
    }

    result
}

fn hann_window(window_length: usize) -> Vec<f32> {
    (0..window_length)
        .map(|i| 0.5 - 0.5 * ((2.0 * PI * i as f32) / (window_length as f32 - 1.0)).cos())
        .collect()
}

// We use proper FFT here instead of naive DFT because the model was trained
// on correctly computed spectrograms. Naive DFT produces wrong frequency bins
// and the model outputs all blank tokens. realfft (real-valued FFT wrapper around
// RustFFT) gives us O(n log n) performance and numerically correct results.
pub fn stft(
    audio: &[f32],
    n_fft: usize,
    hop_length: usize,
    win_length: usize,
) -> Result<Array2<f32>> {
    use realfft::RealFftPlanner;

    let pad_amount = n_fft / 2;
    let mut padded = vec![0.0f32; pad_amount];
    padded.extend_from_slice(audio);
    padded.resize(padded.len() + pad_amount, 0.0);

    let window = hann_window(win_length);
    let num_frames = (padded.len() - n_fft) / hop_length + 1;
    let freq_bins = n_fft / 2 + 1;
    let mut spectrogram = Array2::<f32>::zeros((freq_bins, num_frames));

    let mut planner = RealFftPlanner::<f32>::new();
    let r2c = planner.plan_fft_forward(n_fft);
    let mut input = vec![0.0f32; n_fft];
    let mut output = r2c.make_output_vec();
    let mut scratch = r2c.make_scratch_vec();

    for frame_idx in 0..num_frames {
        let start = frame_idx * hop_length;

        input.fill(0.0);
        for i in 0..win_length.min(padded.len() - start) {
            input[i] = padded[start + i] * window[i];
        }

        r2c.process_with_scratch(&mut input, &mut output, &mut scratch)
            .map_err(|e| Error::Audio(format!("FFT failed: {e}")))?;

        for k in 0..freq_bins {
            spectrogram[[k, frame_idx]] = output[k].norm_sqr();
        }
    }

    Ok(spectrogram)
}

// Slaney mel scale (again librosa)
const F_SP: f64 = 200.0 / 3.0;
const MIN_LOG_HZ: f64 = 1000.0;
const MIN_LOG_MEL: f64 = MIN_LOG_HZ / F_SP;
const LOG_STEP: f64 = 0.06875177742094912;

fn hz_to_mel_slaney(hz: f64) -> f64 {
    if hz < MIN_LOG_HZ {
        hz / F_SP
    } else {
        MIN_LOG_MEL + (hz / MIN_LOG_HZ).ln() / LOG_STEP
    }
}

fn mel_to_hz_slaney(mel: f64) -> f64 {
    if mel < MIN_LOG_MEL {
        mel * F_SP
    } else {
        MIN_LOG_HZ * ((mel - MIN_LOG_MEL) * LOG_STEP).exp()
    }
}

pub fn create_mel_filterbank(n_fft: usize, n_mels: usize, sample_rate: usize) -> Array2<f32> {
    let freq_bins = n_fft / 2 + 1;
    let mut filterbank = Array2::<f32>::zeros((n_mels, freq_bins));

    let fmax = sample_rate as f64 / 2.0;
    let mel_min = hz_to_mel_slaney(0.0);
    let mel_max = hz_to_mel_slaney(fmax);

    // Mel cent freq
    let mel_points: Vec<f64> = (0..=n_mels + 1)
        .map(|i| mel_to_hz_slaney(mel_min + (mel_max - mel_min) * i as f64 / (n_mels + 1) as f64))
        .collect();

    // FFT bin freq
    let fft_freqs: Vec<f64> = (0..freq_bins)
        .map(|i| i as f64 * sample_rate as f64 / n_fft as f64)
        .collect();

    // librosa's ramp
    let fdiff: Vec<f64> = mel_points.windows(2).map(|w| w[1] - w[0]).collect();

    for i in 0..n_mels {
        for (k, &freq) in fft_freqs.iter().enumerate() {
            let lower = (freq - mel_points[i]) / fdiff[i];
            let upper = (mel_points[i + 2] - freq) / fdiff[i + 1];
            filterbank[[i, k]] = 0.0f64.max(lower.min(upper)) as f32;
        }
    }

    // Slaney norm
    for i in 0..n_mels {
        let enorm = 2.0 / (mel_points[i + 2] - mel_points[i]);
        for k in 0..freq_bins {
            filterbank[[i, k]] *= enorm as f32;
        }
    }

    filterbank
}

/// Extract mel spectrogram features from raw audio samples.
///
/// # Arguments
///
/// * `audio` - Audio samples as f32 values
/// * `sample_rate` - Sample rate in Hz
/// * `channels` - Number of audio channels
/// * `config` - Preprocessor configuration
///
/// # Returns
///
/// 2D array of mel spectrogram features (time_steps x feature_size)
pub fn extract_features_raw(
    mut audio: Vec<f32>,
    sample_rate: u32,
    channels: u16,
    config: &PreprocessorConfig,
) -> Result<Array2<f32>> {
    if sample_rate != config.sampling_rate as u32 {
        return Err(Error::Audio(format!(
            "Audio sample rate {} doesn't match expected {}. Please resample your audio first.",
            sample_rate, config.sampling_rate
        )));
    }

    if channels > 1 {
        let mono: Vec<f32> = audio
            .chunks(channels as usize)
            .map(|chunk| chunk.iter().sum::<f32>() / channels as f32)
            .collect();
        audio = mono;
    }

    audio = apply_preemphasis(&audio, config.preemphasis);

    let spectrogram = stft(&audio, config.n_fft, config.hop_length, config.win_length)?;

    let mel_filterbank =
        create_mel_filterbank(config.n_fft, config.feature_size, config.sampling_rate);
    let mel_spectrogram = mel_filterbank.dot(&spectrogram);
    // Log with additive guard (NeMo: log_zero_guard_type="add", value=2^-24)
    let log_zero_guard: f32 = 2.0f32.powi(-24);
    let mel_spectrogram = mel_spectrogram.mapv(|x| (x + log_zero_guard).ln());

    let mut mel_spectrogram = mel_spectrogram.t().to_owned();

    // Normalize per_feature: mean=0, std=1 with Bessel's correction (N-1)
    let num_frames = mel_spectrogram.shape()[0];
    let num_features = mel_spectrogram.shape()[1];

    if num_frames <= 1 {
        return Ok(mel_spectrogram);
    }

    for feat_idx in 0..num_features {
        let mut column = mel_spectrogram.column_mut(feat_idx);
        let mean: f32 = column.iter().sum::<f32>() / num_frames as f32;
        let variance: f32 =
            column.iter().map(|&x| (x - mean).powi(2)).sum::<f32>() / (num_frames as f32 - 1.0);
        let std = variance.sqrt() + 1e-5;

        for val in column.iter_mut() {
            *val = (*val - mean) / std;
        }
    }

    Ok(mel_spectrogram)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate a pure sine wave at the given frequency and sample rate.
    fn sine_wave(freq_hz: f32, sample_rate: usize, num_samples: usize) -> Vec<f32> {
        (0..num_samples)
            .map(|i| (2.0 * PI * freq_hz * i as f32 / sample_rate as f32).sin())
            .collect()
    }

    #[test]
    fn stft_concentrates_power_at_expected_bin() {
        // 1kHz sine at 16kHz sample rate, 1 second
        let n_fft = 512;
        let hop_length = 160;
        let win_length = 400;
        let sample_rate = 16000;
        let audio = sine_wave(1000.0, sample_rate, sample_rate);

        let spec = stft(&audio, n_fft, hop_length, win_length).unwrap();

        // Expected bin for 1kHz: freq_hz * n_fft / sample_rate = 1000 * 512 / 16000 = 32
        let expected_bin = 32;
        let freq_bins = n_fft / 2 + 1;
        let num_frames = spec.shape()[1];

        // Check that bin 32 has the highest power in most frames (skip edge frames)
        let mut correct_frames = 0;
        for frame in 2..num_frames.saturating_sub(2) {
            let mut max_bin = 0;
            let mut max_power = 0.0f32;
            for bin in 0..freq_bins {
                if spec[[bin, frame]] > max_power {
                    max_power = spec[[bin, frame]];
                    max_bin = bin;
                }
            }
            if max_bin == expected_bin {
                correct_frames += 1;
            }
        }

        let interior_frames = num_frames.saturating_sub(4);
        assert!(
            correct_frames > interior_frames / 2,
            "Expected bin {expected_bin} to dominate in most frames, but only {correct_frames}/{interior_frames}"
        );
    }

    #[test]
    fn stft_output_shape_is_correct() {
        let n_fft = 512;
        let hop_length = 160;
        let win_length = 400;
        let audio = vec![0.0f32; 16000]; // 1 second of silence

        let spec = stft(&audio, n_fft, hop_length, win_length).unwrap();

        let freq_bins = n_fft / 2 + 1;
        assert_eq!(spec.shape()[0], freq_bins);
        // num_frames = (audio_len + n_fft - n_fft) / hop_length + 1 = 16000 / 160 + 1 = 101
        assert!(spec.shape()[1] > 0);
    }
}

#[cfg(test)]
mod wav_head_tests {
    use super::*;
    use hound::{SampleFormat, WavSpec, WavWriter};

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rightkit-asr-{}-{name}.wav", std::process::id()))
    }

    fn spec(channels: u16, sample_rate: u32, bits_per_sample: u16, sample_format: SampleFormat) -> WavSpec {
        WavSpec { channels, sample_rate, bits_per_sample, sample_format }
    }

    #[test]
    fn head_stops_at_the_requested_duration_for_16_bit_mono() {
        let path = temp("head16");
        let mut w = WavWriter::create(&path, spec(1, 16_000, 16, SampleFormat::Int)).unwrap();
        for i in 0..(3 * 16_000) {
            w.write_sample(if i < 16_000 { 16_384i16 } else { 0i16 }).unwrap();
        }
        w.finalize().unwrap();
        let head = load_wav_mono_16k_head(&path, Duration::from_secs(1)).unwrap();
        assert_eq!(head.len(), 16_000);
        assert!((head[0] - 0.5).abs() < 1e-6, "16-bit scale is 2^15");
        let whole = load_wav_mono_16k_head(&path, Duration::from_secs(60)).unwrap();
        assert_eq!(whole.len(), 48_000, "a request longer than the file returns the file");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn head_downmixes_24_bit_stereo() {
        let path = temp("head24");
        let mut w = WavWriter::create(&path, spec(2, 16_000, 24, SampleFormat::Int)).unwrap();
        for _ in 0..16_000 {
            w.write_sample(4_194_304i32).unwrap(); // 2^22 = half of 2^23
            w.write_sample(0i32).unwrap();
        }
        w.finalize().unwrap();
        let head = load_wav_mono_16k_head(&path, Duration::from_millis(250)).unwrap();
        assert_eq!(head.len(), 4_000);
        assert!((head[0] - 0.25).abs() < 1e-6, "mean of 0.5 and 0.0");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn head_reads_32_bit_int_and_32_bit_float() {
        let path = temp("head32i");
        let mut w = WavWriter::create(&path, spec(1, 16_000, 32, SampleFormat::Int)).unwrap();
        w.write_sample(1_073_741_824i32).unwrap(); // 2^30 = half of 2^31
        w.write_sample(-1_073_741_824i32).unwrap();
        w.finalize().unwrap();
        let v = load_wav_mono_16k_head(&path, Duration::from_secs(1)).unwrap();
        assert_eq!(v.len(), 2);
        assert!((v[0] - 0.5).abs() < 1e-6 && (v[1] + 0.5).abs() < 1e-6);
        let _ = std::fs::remove_file(&path);

        let path = temp("head32f");
        let mut w = WavWriter::create(&path, spec(1, 16_000, 32, SampleFormat::Float)).unwrap();
        w.write_sample(0.25f32).unwrap();
        w.write_sample(-0.5f32).unwrap();
        w.finalize().unwrap();
        let v = load_wav_mono_16k_head(&path, Duration::from_secs(1)).unwrap();
        assert_eq!(v, vec![0.25, -0.5]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unsupported_rate_and_missing_file_are_typed() {
        let path = temp("rate");
        let mut w = WavWriter::create(&path, spec(1, 44_100, 16, SampleFormat::Int)).unwrap();
        w.write_sample(0i16).unwrap();
        w.finalize().unwrap();
        let err = load_wav_mono_16k_head(&path, Duration::from_secs(1)).unwrap_err();
        assert!(matches!(err, WavLoadError::UnsupportedSampleRate(44_100)), "{err:?}");
        let as_crate: Error = err.into();
        assert!(matches!(as_crate, Error::Audio(_)));
        let _ = std::fs::remove_file(&path);

        let err = load_wav_mono_16k_head("/definitely/not/here.wav", Duration::from_secs(1)).unwrap_err();
        assert!(matches!(err, WavLoadError::Io(_)), "{err:?}");
    }
}
