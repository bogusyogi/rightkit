//! Online Kaldi log-mel filterbank, ported from kaldi-native-fbank 1.22.3
//! (`OnlineGenericBaseFeature<FbankComputer>`), in the configuration Sherpa-ONNX's
//! `FeatureExtractor` gives every streaming transducer: Povey window, DC removal,
//! pre-emphasis 0.97, 25 ms / 10 ms frames, `snip_edges = false`, power spectrum,
//! natural-log mel energies floored at `f32::EPSILON`, no energy column, no dither.
//!
//! The operation order (float accumulation, double-precision window) follows the C++
//! source so the per-bin difference stays far below the 1e-4 parity gate.

use std::collections::VecDeque;
use std::sync::Arc;

use super::kissfft::{Cpx, KissFftr};
use crate::error::{Error, Result};

/// Frame and mel-bank settings. Only the knobs Sherpa exposes are configurable; the
/// rest are the kaldi-native-fbank values Sherpa never changes for streaming models.
#[derive(Debug, Clone, PartialEq)]
pub struct FbankConfig {
    pub sample_rate: u32,
    pub num_bins: usize,
    /// Lowest mel-bank frequency in Hz.
    pub low_freq: f32,
    /// Highest mel-bank frequency in Hz; zero or negative is an offset from Nyquist.
    pub high_freq: f32,
}

impl Default for FbankConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16_000,
            num_bins: 80,
            low_freq: 20.0,
            high_freq: -400.0,
        }
    }
}

const FRAME_SHIFT_MS: f32 = 10.0;
const FRAME_LENGTH_MS: f32 = 25.0;
const PREEMPH_COEFF: f32 = 0.97;

/// One mel bin: the first FFT bin it covers and its triangular weights.
struct MelBin {
    offset: usize,
    weights: Vec<f32>,
}

/// Immutable per-config tables shared by every stream of one spotter.
pub(crate) struct FbankTables {
    window_shift: i64,
    window_size: i64,
    padded_size: usize,
    window: Vec<f32>,
    bins: Vec<MelBin>,
    fft: KissFftr,
}

impl FbankTables {
    pub(crate) fn new(config: &FbankConfig) -> Result<Self> {
        let samp_freq = config.sample_rate as f32;
        // FrameExtractionOptions::WindowShift/WindowSize: float product, truncated.
        let window_shift = (samp_freq * 0.001f32 * FRAME_SHIFT_MS) as i64;
        let window_size = (samp_freq * 0.001f32 * FRAME_LENGTH_MS) as i64;
        if window_shift <= 0 || window_size <= 1 {
            return Err(Error::Config(format!(
                "fbank sample rate {} gives an empty frame",
                config.sample_rate
            )));
        }
        let padded_size = (window_size as usize).next_power_of_two();
        let window = povey_window(window_size as usize);
        let bins = kaldi_mel_bins(config, padded_size)?;
        let fft = KissFftr::new(padded_size);
        Ok(Self {
            window_shift,
            window_size,
            padded_size,
            window,
            bins,
            fft,
        })
    }

    pub(crate) fn dim(&self) -> usize {
        self.bins.len()
    }

    /// `FirstSampleOfFrame` for `snip_edges = false`.
    fn first_sample_of_frame(&self, frame: i64) -> i64 {
        let midpoint = self.window_shift * frame + self.window_shift / 2;
        midpoint - self.window_size / 2
    }

    /// `NumFrames` for `snip_edges = false`.
    fn num_frames(&self, num_samples: i64, flush: bool) -> i64 {
        let mut num_frames = (num_samples + self.window_shift / 2) / self.window_shift;
        if flush {
            return num_frames;
        }
        let mut end_of_last = self.first_sample_of_frame(num_frames - 1) + self.window_size;
        while num_frames > 0 && end_of_last > num_samples {
            num_frames -= 1;
            end_of_last -= self.window_shift;
        }
        num_frames
    }
}

/// `GetWindow("povey", n)`: computed in double, stored as float.
fn povey_window(n: usize) -> Vec<f32> {
    let a = 2.0 * std::f64::consts::PI / (n as f64 - 1.0);
    (0..n)
        .map(|i| (0.5 - 0.5 * (a * i as f64).cos()).powf(0.85) as f32)
        .collect()
}

fn mel_scale(freq: f32) -> f32 {
    1127.0f32 * (1.0f32 + freq / 700.0f32).ln()
}

/// `MelBanks::InitKaldiMelBanks` without VTLN warping or HTK mode.
fn kaldi_mel_bins(config: &FbankConfig, padded_size: usize) -> Result<Vec<MelBin>> {
    if config.num_bins < 3 {
        return Err(Error::Config("fbank needs at least 3 mel bins".into()));
    }
    let sample_freq = config.sample_rate as f32;
    let num_fft_bins = padded_size / 2;
    let nyquist = 0.5f32 * sample_freq;
    let low_freq = config.low_freq;
    let high_freq = if config.high_freq > 0.0 {
        config.high_freq
    } else {
        nyquist + config.high_freq
    };
    if low_freq < 0.0
        || low_freq >= nyquist
        || high_freq <= 0.0
        || high_freq > nyquist
        || high_freq <= low_freq
    {
        return Err(Error::Config(format!(
            "fbank low-freq {low_freq} and high-freq {high_freq} are invalid for nyquist {nyquist}"
        )));
    }
    let fft_bin_width = sample_freq / padded_size as f32;
    let mel_low = mel_scale(low_freq);
    let mel_high = mel_scale(high_freq);
    let mel_delta = (mel_high - mel_low) / (config.num_bins as f32 + 1.0);

    let mut bins = Vec::with_capacity(config.num_bins);
    for bin in 0..config.num_bins {
        let left = mel_low + bin as f32 * mel_delta;
        let center = mel_low + (bin as f32 + 1.0) * mel_delta;
        let right = mel_low + (bin as f32 + 2.0) * mel_delta;
        let mut this_bin = vec![0.0f32; num_fft_bins];
        let mut first = None;
        let mut last = 0usize;
        for (i, slot) in this_bin.iter_mut().enumerate() {
            let mel = mel_scale(fft_bin_width * i as f32);
            if mel > left && mel < right {
                *slot = if mel <= center {
                    (mel - left) / (center - left)
                } else {
                    (right - mel) / (right - center)
                };
                first.get_or_insert(i);
                last = i;
            }
        }
        let Some(first) = first else {
            return Err(Error::Config(format!(
                "mel bin {bin} is empty; num_bins {} is too large",
                config.num_bins
            )));
        };
        bins.push(MelBin {
            offset: first,
            weights: this_bin[first..=last].to_vec(),
        });
    }
    Ok(bins)
}

/// Streaming feature extractor: accepts PCM in any chunking and yields the same frames.
pub(crate) struct OnlineFbank {
    tables: Arc<FbankTables>,
    remainder: Vec<f32>,
    waveform_offset: i64,
    /// Frames still held, starting at absolute index `first_available`.
    frames: VecDeque<Vec<f32>>,
    first_available: usize,
    input_finished: bool,
    frame_buf: Vec<f32>,
    spectrum: Vec<Cpx>,
    scratch: Vec<Cpx>,
    power: Vec<f32>,
}

impl OnlineFbank {
    pub(crate) fn new(tables: Arc<FbankTables>) -> Self {
        let padded = tables.padded_size;
        let spectrum = vec![Cpx::default(); padded / 2 + 1];
        let scratch = vec![Cpx::default(); padded / 2];
        Self {
            tables,
            remainder: Vec::new(),
            waveform_offset: 0,
            frames: VecDeque::new(),
            first_available: 0,
            input_finished: false,
            frame_buf: vec![0.0; padded],
            spectrum,
            scratch,
            power: vec![0.0; padded / 2 + 1],
        }
    }

    /// Append samples already in `[-1, 1]` (Sherpa's `normalize_samples = true`).
    pub(crate) fn accept_waveform(&mut self, samples: &[f32]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        if self.input_finished {
            return Err(Error::Audio(
                "accept_waveform called after input_finished".into(),
            ));
        }
        self.remainder.extend_from_slice(samples);
        self.compute_features();
        Ok(())
    }

    /// Append externally computed frames (parity tests feed a reference fbank).
    pub(crate) fn push_frames(&mut self, frames: &[f32]) {
        for frame in frames.chunks_exact(self.tables.dim()) {
            self.frames.push_back(frame.to_vec());
        }
    }

    pub(crate) fn input_finished(&mut self) {
        self.input_finished = true;
        self.compute_features();
    }

    /// Total frames computed so far, counting frames already released.
    pub(crate) fn num_frames_ready(&self) -> usize {
        self.first_available + self.frames.len()
    }

    /// Frames `[start, start + n)` flattened row-major. Frames before `start` are
    /// released, as Sherpa's `FeatureExtractor::GetFrames` does.
    pub(crate) fn frames(&mut self, start: usize, n: usize) -> Result<Vec<f32>> {
        if start < self.first_available || start + n > self.num_frames_ready() {
            return Err(Error::Model(format!(
                "fbank frames [{start}, {}) requested; held [{}, {})",
                start + n,
                self.first_available,
                self.num_frames_ready()
            )));
        }
        while self.first_available < start {
            self.frames.pop_front();
            self.first_available += 1;
        }
        let dim = self.tables.dim();
        let mut out = Vec::with_capacity(n * dim);
        for frame in self.frames.iter().take(n) {
            out.extend_from_slice(frame);
        }
        Ok(out)
    }

    fn compute_features(&mut self) {
        let tables = Arc::clone(&self.tables);
        let total = self.waveform_offset + self.remainder.len() as i64;
        let old = self.num_frames_ready() as i64;
        let new = tables.num_frames(total, self.input_finished);
        for frame in old..new {
            let feature = self.compute_frame(&tables, frame);
            self.frames.push_back(feature);
        }
        let first_of_next = tables.first_sample_of_frame(new);
        let discard = first_of_next - self.waveform_offset;
        if discard > 0 {
            let keep = self.remainder.len() as i64 - discard;
            if keep <= 0 {
                self.waveform_offset += self.remainder.len() as i64;
                self.remainder.clear();
            } else {
                self.remainder.drain(..discard as usize);
                self.waveform_offset += discard;
            }
        }
    }

    fn compute_frame(&mut self, tables: &FbankTables, frame: i64) -> Vec<f32> {
        let size = tables.window_size as usize;
        let wave = &self.remainder;
        let buf = &mut self.frame_buf;
        buf.fill(0.0);

        // ExtractWindow: copy, reflecting at either end of the available wave.
        let wave_start = tables.first_sample_of_frame(frame) - self.waveform_offset;
        let wave_len = wave.len() as i64;
        if wave_start >= 0 && wave_start + size as i64 <= wave_len {
            let s = wave_start as usize;
            buf[..size].copy_from_slice(&wave[s..s + size]);
        } else {
            for (s, slot) in buf[..size].iter_mut().enumerate() {
                let mut i = s as i64 + wave_start;
                while i < 0 || i >= wave_len {
                    i = if i < 0 { -i - 1 } else { 2 * wave_len - 1 - i };
                }
                *slot = wave[i as usize];
            }
        }

        // ProcessWindow: DC removal, pre-emphasis, window.
        let window = &mut buf[..size];
        let mut sum = 0.0f32;
        for &x in window.iter() {
            sum += x;
        }
        let mean = sum / size as f32;
        for x in window.iter_mut() {
            *x -= mean;
        }
        for i in (1..size).rev() {
            window[i] -= PREEMPH_COEFF * window[i - 1];
        }
        window[0] -= PREEMPH_COEFF * window[0];
        for (x, w) in window.iter_mut().zip(&tables.window) {
            *x *= *w;
        }

        tables
            .fft
            .process(buf, &mut self.scratch, &mut self.spectrum);

        // ComputePowerSpectrum (bins 0..=N/2).
        for (p, c) in self.power.iter_mut().zip(&self.spectrum) {
            *p = c.r * c.r + c.i * c.i;
        }
        self.power[0] = self.spectrum[0].r * self.spectrum[0].r;

        tables
            .bins
            .iter()
            .map(|bin| {
                let mut energy = 0.0f32;
                for (k, w) in bin.weights.iter().enumerate() {
                    energy += *w * self.power[k + bin.offset];
                }
                energy.max(f32::EPSILON).ln()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fbank() -> OnlineFbank {
        OnlineFbank::new(Arc::new(FbankTables::new(&FbankConfig::default()).unwrap()))
    }

    #[test]
    fn frame_geometry_matches_kaldi_for_16k() {
        let t = FbankTables::new(&FbankConfig::default()).unwrap();
        assert_eq!((t.window_shift, t.window_size, t.padded_size), (160, 400, 512));
        // Frame f covers [160f - 120, 160f + 280); it is ready once that end is buffered.
        assert_eq!(t.num_frames(279, false), 0);
        assert_eq!(t.num_frames(280, false), 1);
        assert_eq!(t.num_frames(16_000, false), 99);
        assert_eq!(t.num_frames(16_000, true), 100);
    }

    #[test]
    fn chunking_does_not_change_frames() {
        let audio: Vec<f32> = (0..16_000)
            .map(|i| ((i as f32) * 0.031).sin() * 0.3 + ((i * 7919) % 97) as f32 * 1e-3)
            .collect();
        let mut whole = fbank();
        whole.accept_waveform(&audio).unwrap();
        let mut chunked = fbank();
        for chunk in audio.chunks(320) {
            chunked.accept_waveform(chunk).unwrap();
        }
        let n = whole.num_frames_ready();
        assert_eq!(n, chunked.num_frames_ready());
        assert_eq!(whole.frames(0, n).unwrap(), chunked.frames(0, n).unwrap());
    }

    #[test]
    fn digital_silence_floors_at_log_epsilon() {
        let mut f = fbank();
        f.accept_waveform(&[0.0; 1_600]).unwrap();
        let frames = f.frames(0, f.num_frames_ready()).unwrap();
        assert!(frames.iter().all(|v| *v == f32::EPSILON.ln()));
    }

    #[test]
    fn released_frames_cannot_be_read_again() {
        let mut f = fbank();
        f.accept_waveform(&[0.1; 3_200]).unwrap();
        f.frames(5, 2).unwrap();
        assert!(f.frames(4, 1).is_err());
        assert!(f.frames(5, 1).is_ok());
    }
}
