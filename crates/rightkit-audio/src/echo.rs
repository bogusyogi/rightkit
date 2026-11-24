//! Echo reference: what the speakers are playing, kept so the capture side can take the agent's own
//! voice out of the microphone before it listens (barge-in, `docs/contracts/voice.md` VO-5).
//!
//! Two pieces, both pure Rust and model-free:
//! - [`ReferenceTap`]: a time-indexed 16 kHz mono history of played audio. Whoever plays speech
//!   [`push`](ReferenceTap::push)es what it queues; whoever captures reads the window that was playing
//!   one echo-path delay ago.
//! - [`EchoCanceller`]: a normalised-LMS adaptive filter that subtracts the part of the microphone
//!   that is a filtered copy of the reference.
//!
//! The tap and the canceller share a process: playback and capture must run in the same process, or the
//! host feeds the tap from the process that plays. The echo-path delay (queue depth plus device latency)
//! is the host's to supply; the filter spans a short window around it.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Sample rate of the reference history and of the audio the canceller works on.
pub const REFERENCE_RATE: u32 = 16_000;
/// Seconds of reference kept.
const HISTORY_SECS: usize = 2;
/// Filter length: 64 ms of echo path around the supplied delay.
pub const DEFAULT_TAPS: usize = 1024;
/// NLMS step size.
const DEFAULT_MU: f32 = 0.3;
/// Added to the reference power so silence never divides by zero.
const POWER_FLOOR: f32 = 1e-4;
/// The filter stops adapting when the microphone is louder than this fraction of the reference peak:
/// someone else is talking over the reference and adapting to them would cancel their words.
const DOUBLE_TALK_RATIO: f32 = 0.5;

struct TapState {
    origin: Instant,
    /// Samples of the history, oldest first; `end_index` is the absolute index just past the last one.
    ring: VecDeque<f32>,
    end_index: u64,
}

/// A time-indexed history of the audio being played, at [`REFERENCE_RATE`], mono.
pub struct ReferenceTap {
    state: Mutex<TapState>,
}

impl Default for ReferenceTap {
    fn default() -> Self {
        Self::new()
    }
}

impl ReferenceTap {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(TapState {
                origin: Instant::now(),
                ring: VecDeque::with_capacity(HISTORY_SECS * REFERENCE_RATE as usize),
                end_index: 0,
            }),
        }
    }

    fn now_index(state: &TapState) -> u64 {
        (state.origin.elapsed().as_secs_f64() * f64::from(REFERENCE_RATE)) as u64
    }

    /// Record `samples` (mono, [`REFERENCE_RATE`]) as being queued for playback now. Audio queued
    /// behind earlier audio follows it; after a gap, silence fills the gap, so the history stays
    /// aligned with the wall clock.
    pub fn push(&self, samples: &[f32]) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let now = Self::now_index(&state);
        if now > state.end_index {
            let gap =
                ((now - state.end_index) as usize).min(HISTORY_SECS * REFERENCE_RATE as usize);
            let filled = state.ring.len() + gap;
            state.ring.resize(filled, 0.0);
            state.end_index = now;
        }
        state.ring.extend(samples.iter().copied());
        state.end_index += samples.len() as u64;
        let keep = HISTORY_SECS * REFERENCE_RATE as usize;
        while state.ring.len() > keep {
            state.ring.pop_front();
        }
    }

    /// Whether anything queued is still ahead of, or within `slack` behind, the wall clock: the
    /// speakers are playing (or just were), so the microphone may hold an echo.
    pub fn is_active(&self, slack: Duration) -> bool {
        let Ok(state) = self.state.lock() else {
            return false;
        };
        let slack_samples = (slack.as_secs_f64() * f64::from(REFERENCE_RATE)) as u64;
        state.end_index + slack_samples >= Self::now_index(&state) && state.end_index > 0
    }

    /// `len` samples of reference ending `delay` before now; zeros where the history has none.
    pub fn window_ending_before_now(&self, delay: Duration, len: usize) -> Vec<f32> {
        let Ok(state) = self.state.lock() else {
            return vec![0.0; len];
        };
        let delay_samples = (delay.as_secs_f64() * f64::from(REFERENCE_RATE)) as u64;
        let end = Self::now_index(&state).saturating_sub(delay_samples);
        let start = end.saturating_sub(len as u64);
        let first_kept = state.end_index - state.ring.len() as u64;
        let mut out = vec![0.0; len];
        let offset = len - (end - start) as usize;
        for (i, slot) in out.iter_mut().enumerate().skip(offset) {
            let index = start + (i - offset) as u64;
            if index >= first_kept && index < state.end_index {
                *slot = state.ring[(index - first_kept) as usize];
            }
        }
        out
    }
}

/// The process-wide tap that playback pushes to and capture reads.
pub fn reference_tap() -> &'static ReferenceTap {
    static TAP: OnceLock<ReferenceTap> = OnceLock::new();
    TAP.get_or_init(ReferenceTap::new)
}

/// Normalised-LMS echo canceller for mono audio at [`REFERENCE_RATE`].
pub struct EchoCanceller {
    weights: Vec<f32>,
    mu: f32,
}

impl Default for EchoCanceller {
    fn default() -> Self {
        Self::new(DEFAULT_TAPS)
    }
}

impl EchoCanceller {
    /// A canceller with `taps` filter taps (at least 1).
    pub fn new(taps: usize) -> Self {
        Self {
            weights: vec![0.0; taps.max(1)],
            mu: DEFAULT_MU,
        }
    }

    /// Filter length in samples.
    pub fn taps(&self) -> usize {
        self.weights.len()
    }

    /// Forget what was learned (a new device or route).
    pub fn reset(&mut self) {
        self.weights.iter_mut().for_each(|w| *w = 0.0);
    }

    /// Subtract the echo of `reference` from `mic`. `reference` must hold `taps() - 1` samples of
    /// history followed by one sample per `mic` sample, all at the speaker side of the echo path (the
    /// reference as it was playing when each mic sample was heard); otherwise the mic is returned as is.
    pub fn process(&mut self, mic: &[f32], reference: &[f32]) -> Vec<f32> {
        let taps = self.weights.len();
        if mic.is_empty() || reference.len() != mic.len() + taps - 1 {
            return mic.to_vec();
        }
        let peak = reference
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        let mic_rms = (mic.iter().map(|s| s * s).sum::<f32>() / mic.len() as f32).sqrt();
        let adapt = peak > 0.0 && mic_rms <= DOUBLE_TALK_RATIO * peak;
        let mut out = Vec::with_capacity(mic.len());
        for (n, &heard) in mic.iter().enumerate() {
            // Newest reference sample first, to line up with weights[0].
            let history = &reference[n..n + taps];
            let mut estimate = 0.0_f32;
            let mut power = POWER_FLOOR;
            for (k, weight) in self.weights.iter().enumerate() {
                let x = history[taps - 1 - k];
                estimate += weight * x;
                power += x * x;
            }
            let error = heard - estimate;
            if adapt {
                let gain = self.mu * error / power;
                for (k, weight) in self.weights.iter_mut().enumerate() {
                    *weight += gain * history[taps - 1 - k];
                }
            }
            out.push(error);
        }
        out
    }

    /// [`process`](Self::process) against the live tap: the reference is the window that was playing
    /// `path_delay` ago. When nothing is playing the mic is returned untouched and nothing is learned.
    pub fn process_with_tap(
        &mut self,
        tap: &ReferenceTap,
        path_delay: Duration,
        mic: &[f32],
    ) -> Vec<f32> {
        if mic.is_empty() || !tap.is_active(path_delay + Duration::from_millis(100)) {
            return mic.to_vec();
        }
        let reference = tap.window_ending_before_now(path_delay, mic.len() + self.taps() - 1);
        self.process(mic, &reference)
    }
}
