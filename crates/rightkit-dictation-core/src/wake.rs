//! Wake-word listener scaffolding (Phase F seam).
//!
//! This is the **structural** half of the always-on wake listener: the rolling
//! window state machine, threshold/cooldown gating, and config validation —
//! all pure and unit-tested without a model.
//!
//! The two pieces that carry Adrian's 7-week wake investment are **injected
//! traits**, NOT implemented here, and MUST be ported faithfully in Phase F:
//!
//! - [`WakeFrontend`] — the mandatory conditioning. The model
//!   is trained on audio passed through `condition_i16` /
//!   `train_serve_frontend.frontend_block` (2nd-order Butterworth HPF @ 80 Hz +
//!   rolling-RMS AGC, target −22 dBFS). **Feeding raw audio mis-scores to
//!   ~0.000.** The Phase F impl MUST source the exact frontend from
//!   `model_registry/final/wake/repro_scripts/train_serve_frontend.py`, not a
//!   re-derivation.
//! - [`WakeScorer`] — the ONNX cascade (gate → SSL confirmer). Output is a
//!   2-class softmax; `score` is `softmax[1]`. The shipped model is
//!   `wake_rddp_zephyr.onnx`.
//!
//! Wiring this into the live capture loop + flipping `start_wake_listen` on is
//! the Phase F activation; until a real [`WakeScorer`] exists the engine still
//! reports wake unavailable.

/// Inference-contract constants (CLAUDE.md §6A). 16 kHz mono throughout.
pub const SAMPLE_RATE: u32 = 16_000;
/// Scoring window: 1.5 s.
pub const WINDOW_SAMPLES: usize = 24_000;
/// Hop between scores: 80 ms.
pub const HOP_SAMPLES: usize = 1_280;
/// Pre-buffer the listener keeps so a window is always available: 2 s.
pub const PREBUFFER_SAMPLES: usize = 32_000;

/// Documented θ range. rddp scores are bimodal (~0 / ~1, well separated), so a
/// usable threshold sits between these — never 0.0 (fires on silence) or 1.0
/// (never fires). Addresses audit A3 (`WakeDebounce` accepted 0.0 unvalidated).
pub const MIN_THRESHOLD: f32 = 0.05;
pub const MAX_THRESHOLD: f32 = 0.99;

/// The mandatory wake conditioning frontend (§6A). Implemented in Phase F by
/// porting `train_serve_frontend.frontend_block` exactly.
pub trait WakeFrontend {
    /// Condition a raw 16 kHz mono window in place of scoring's expectations.
    fn condition(&self, raw_window: &[f32]) -> Vec<f32>;
}

/// The wake acoustic scorer (§6A ONNX cascade). Returns `softmax[1]` ∈ [0, 1].
pub trait WakeScorer {
    fn score(&mut self, conditioned_window: &[f32]) -> f32;
}

/// Validated listener config. Build via [`WakeConfig::new`] so the threshold is
/// always clamped into the documented range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WakeConfig {
    threshold: f32,
    /// Min hops to suppress re-fires after a detection (cooldown). Stops one
    /// utterance firing repeatedly while the wake word is still in the window.
    cooldown_hops: u32,
}

impl WakeConfig {
    /// Clamp `threshold` into `[MIN_THRESHOLD, MAX_THRESHOLD]`. Returns the
    /// config plus whether the input was out of range (so callers can log it).
    pub fn new(threshold: f32, cooldown_hops: u32) -> (Self, bool) {
        let clamped = if threshold.is_finite() {
            threshold.clamp(MIN_THRESHOLD, MAX_THRESHOLD)
        } else {
            // NaN/inf → safe default mid-band.
            0.5
        };
        let was_clamped = (clamped - threshold).abs() > f32::EPSILON || !threshold.is_finite();
        (
            Self {
                threshold: clamped,
                cooldown_hops: cooldown_hops.max(1),
            },
            was_clamped,
        )
    }

    pub fn threshold(&self) -> f32 {
        self.threshold
    }
}

/// One detection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WakeFire {
    /// `softmax[1]` at the firing window.
    pub score: f32,
    /// Monotonic fire count since the listener started.
    pub fire_count: u64,
}

/// Always-on wake listener state machine. Generic over the injected frontend +
/// scorer so it is fully testable with fakes and the Phase F port is a drop-in.
pub struct WakeListener<F: WakeFrontend, S: WakeScorer> {
    frontend: F,
    scorer: S,
    config: WakeConfig,
    buffer: Vec<f32>,
    samples_since_hop: usize,
    cooldown_remaining: u32,
    fire_count: u64,
}

impl<F: WakeFrontend, S: WakeScorer> WakeListener<F, S> {
    pub fn new(frontend: F, scorer: S, config: WakeConfig) -> Self {
        Self {
            frontend,
            scorer,
            config,
            buffer: Vec::with_capacity(PREBUFFER_SAMPLES + HOP_SAMPLES),
            samples_since_hop: 0,
            cooldown_remaining: 0,
            fire_count: 0,
        }
    }

    /// Feed a block of raw 16 kHz mono samples. Scores at most once per
    /// [`HOP_SAMPLES`] of accumulated audio and returns a [`WakeFire`] if a
    /// score crosses the threshold and the cooldown has elapsed.
    pub fn push(&mut self, block: &[f32]) -> Option<WakeFire> {
        self.buffer.extend_from_slice(block);
        // Keep the tail at prebuffer + one window. Compact only after one
        // extra `cap` of slack accumulates, so the front shift is amortized
        // O(1) per sample and memory stays bounded at 2 * cap.
        let cap = PREBUFFER_SAMPLES.max(WINDOW_SAMPLES);
        if self.buffer.len() > cap * 2 {
            let drop = self.buffer.len() - cap;
            self.buffer.drain(0..drop);
        }
        self.samples_since_hop += block.len();
        if self.samples_since_hop < HOP_SAMPLES || self.buffer.len() < WINDOW_SAMPLES {
            return None;
        }
        self.samples_since_hop = 0;
        if self.cooldown_remaining > 0 {
            self.cooldown_remaining -= 1;
            return None;
        }
        let window = &self.buffer[self.buffer.len() - WINDOW_SAMPLES..];
        let conditioned = self.frontend.condition(window);
        let score = self.scorer.score(&conditioned);
        if score > self.config.threshold {
            self.cooldown_remaining = self.config.cooldown_hops;
            self.fire_count += 1;
            Some(WakeFire {
                score,
                fire_count: self.fire_count,
            })
        } else {
            None
        }
    }

    pub fn fire_count(&self) -> u64 {
        self.fire_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct IdentityFrontend;
    impl WakeFrontend for IdentityFrontend {
        fn condition(&self, raw: &[f32]) -> Vec<f32> {
            raw.to_vec()
        }
    }

    /// Scores the mean absolute amplitude of the window — a deterministic stand
    /// in for the real ONNX cascade so the state machine is exercised.
    struct MeanAbsScorer;
    impl WakeScorer for MeanAbsScorer {
        fn score(&mut self, w: &[f32]) -> f32 {
            (w.iter().map(|s| s.abs()).sum::<f32>() / w.len().max(1) as f32).min(1.0)
        }
    }

    fn listener(threshold: f32) -> WakeListener<IdentityFrontend, MeanAbsScorer> {
        let (cfg, _) = WakeConfig::new(threshold, 4);
        WakeListener::new(IdentityFrontend, MeanAbsScorer, cfg)
    }

    #[test]
    fn threshold_is_clamped_out_of_range() {
        let (lo, c0) = WakeConfig::new(0.0, 4);
        assert!(c0);
        assert_eq!(lo.threshold(), MIN_THRESHOLD);
        let (hi, c1) = WakeConfig::new(2.0, 4);
        assert!(c1);
        assert_eq!(hi.threshold(), MAX_THRESHOLD);
        let (nan, c2) = WakeConfig::new(f32::NAN, 4);
        assert!(c2);
        assert_eq!(nan.threshold(), 0.5);
        let (ok, c3) = WakeConfig::new(0.4, 4);
        assert!(!c3);
        assert_eq!(ok.threshold(), 0.4);
    }

    #[test]
    fn buffer_stays_bounded_and_scores_the_newest_window() {
        let mut l = listener(0.5);
        let cap = PREBUFFER_SAMPLES.max(WINDOW_SAMPLES);
        for _ in 0..(cap * 6 / HOP_SAMPLES + 1) {
            l.push(&vec![0.0; HOP_SAMPLES]);
            assert!(l.buffer.len() <= cap * 2 + HOP_SAMPLES);
        }
        let mut l2 = listener(0.5);
        for _ in 0..(cap * 3 / HOP_SAMPLES) {
            l2.push(&vec![0.0; HOP_SAMPLES]);
        }
        assert!(l2.push(&vec![1.0; WINDOW_SAMPLES]).is_some());
    }

    #[test]
    fn does_not_score_before_a_full_window() {
        let mut l = listener(0.5);
        // Less than one window of loud audio — no fire yet (warming up).
        assert_eq!(l.push(&vec![1.0; HOP_SAMPLES]), None);
    }

    #[test]
    fn fires_when_score_exceeds_threshold_then_respects_cooldown() {
        let mut l = listener(0.5);
        // Fill a full window of max-amplitude audio (score ~1.0 > 0.5).
        let fire = l.push(&vec![1.0; WINDOW_SAMPLES]);
        assert!(fire.is_some());
        let f = fire.unwrap();
        assert!(f.score > 0.5);
        assert_eq!(f.fire_count, 1);
        // Next hops are suppressed by cooldown even though audio is still loud.
        let mut suppressed = 0;
        for _ in 0..3 {
            if l.push(&vec![1.0; HOP_SAMPLES]).is_none() {
                suppressed += 1;
            }
        }
        assert_eq!(suppressed, 3, "cooldown must suppress immediate re-fires");
    }

    #[test]
    fn silence_never_fires() {
        let mut l = listener(0.5);
        for _ in 0..40 {
            assert_eq!(l.push(&vec![0.0; HOP_SAMPLES]), None);
        }
        assert_eq!(l.fire_count(), 0);
    }
}
