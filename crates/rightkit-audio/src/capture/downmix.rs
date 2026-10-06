// Realtime-callback mono downmix + ring-buffer push helpers.
//
// Everything here runs inside the cpal data callback: no allocation, no
// logging — the channel-lock event is surfaced through `Metrics` by the push
// helpers.

use cpal::{FromSample, Sample};
use ringbuf::traits::Producer;

use super::metrics::Metrics;

/// Adaptive multichannel→mono downmix. Plain channel averaging silently
/// destroys speech on devices whose extra channels are echo-cancellation
/// references (phase-inverted → destructive cancellation) or dead (÷N
/// dilution). Strategy: emit the average while
/// accumulating per-channel and mixed energy over ~0.5s windows; when the
/// mixed RMS collapses below `RMS_RATIO_FLOOR` of the strongest channel's
/// RMS on a window with real signal, lock onto that channel for the rest of
/// the session. Runs inside the realtime callback: no allocation, no logging
/// — the lock event is surfaced through `Metrics` by the push helpers.
pub struct MonoDownmix {
    chans: usize,
    window_frames: u64,
    frames: u64,
    mix_energy: f64,
    chan_energy: Vec<f64>,
    selected: Option<usize>,
    pending_lock: Option<usize>,
}

impl MonoDownmix {
    /// Mixed RMS below this fraction of the best channel's RMS marks the
    /// average as destructive/diluted. A healthy correlated array averages
    /// within ~3dB of its best channel (ratio ≈ 1); one-live-of-four dilutes
    /// to 0.25; inverted reference pairs land near 0.
    const RMS_RATIO_FLOOR: f64 = 0.35;
    /// The best channel must carry at least this RMS over the window before
    /// a lock is considered — never lock on noise-floor silence.
    const MIN_SIGNAL_RMS: f64 = 0.003;

    pub fn new(chans: usize, input_rate: u32) -> Self {
        Self {
            chans,
            window_frames: (input_rate as u64 / 2).max(1),
            frames: 0,
            mix_energy: 0.0,
            chan_energy: vec![0.0; chans],
            selected: None,
            pending_lock: None,
        }
    }

    #[inline(always)]
    pub fn mix<F: FnMut(usize) -> f32>(&mut self, mut sample_at: F) -> f32 {
        let mut sum = 0.0f32;
        let mut selected_value = 0.0f32;
        for ch in 0..self.chans {
            let v = sample_at(ch);
            sum += v;
            self.chan_energy[ch] += f64::from(v) * f64::from(v);
            if Some(ch) == self.selected {
                selected_value = v;
            }
        }
        let avg = sum / self.chans as f32;
        self.mix_energy += f64::from(avg) * f64::from(avg);
        self.frames += 1;
        if self.frames >= self.window_frames {
            self.evaluate_window();
        }
        if self.selected.is_some() {
            selected_value
        } else {
            avg
        }
    }

    fn evaluate_window(&mut self) {
        if self.selected.is_none() {
            let (best_ch, best_energy) = self
                .chan_energy
                .iter()
                .copied()
                .enumerate()
                .fold((0, 0.0f64), |acc, item| if item.1 > acc.1 { item } else { acc });
            let min_energy = Self::MIN_SIGNAL_RMS * Self::MIN_SIGNAL_RMS * self.frames as f64;
            let ratio_energy_floor = Self::RMS_RATIO_FLOOR * Self::RMS_RATIO_FLOOR;
            if best_energy > min_energy && self.mix_energy < best_energy * ratio_energy_floor {
                self.selected = Some(best_ch);
                self.pending_lock = Some(best_ch);
            }
        }
        self.frames = 0;
        self.mix_energy = 0.0;
        self.chan_energy.fill(0.0);
    }

    /// One-shot: the channel just locked by `evaluate_window`, for the push
    /// helpers to report through `Metrics` outside the per-frame loop.
    pub fn take_pending_lock(&mut self) -> Option<usize> {
        self.pending_lock.take()
    }
}

#[inline(always)]
pub(super) fn push_mono_f32(
    data: &[f32],
    in_chans: usize,
    downmix: &mut MonoDownmix,
    producer: &mut ringbuf::HeapProd<f32>,
    metrics: &Metrics,
) {
    if in_chans <= 1 {
        let pushed = producer.push_slice(data);
        if pushed < data.len() {
            metrics.record_overflow((data.len() - pushed) as u64);
        }
    } else {
        let frames = data.len() / in_chans;
        for f in 0..frames {
            let start = f * in_chans;
            let s = downmix.mix(|ch| data[start + ch]);
            if producer.try_push(s).is_err() {
                metrics.record_overflow(1);
                break;
            }
        }
        if let Some(ch) = downmix.take_pending_lock() {
            metrics.record_downmix_lock(ch);
        }
    }
}

#[inline(always)]
pub(super) fn push_mono_pcm<T>(
    data: &[T],
    in_chans: usize,
    downmix: &mut MonoDownmix,
    producer: &mut ringbuf::HeapProd<f32>,
    metrics: &Metrics,
) where
    T: Sample,
    f32: FromSample<T>,
{
    if in_chans <= 1 {
        for &s in data {
            if producer.try_push(f32::from_sample(s)).is_err() {
                metrics.record_overflow(1);
                break;
            }
        }
    } else {
        let frames = data.len() / in_chans;
        for f in 0..frames {
            let start = f * in_chans;
            let s = downmix.mix(|ch| f32::from_sample(data[start + ch]));
            if producer.try_push(s).is_err() {
                metrics.record_overflow(1);
                break;
            }
        }
        if let Some(ch) = downmix.take_pending_lock() {
            metrics.record_downmix_lock(ch);
        }
    }
}

