//! Shared front end (plan arm C6), default off: the wake stream's encoder output, kept in a
//! bounded ring of encoder frames on the WAKE clock, feeds a confirm BEAM that owns no
//! filterbank and no encoder state ([`BeamStream`]). Detection-changing by design: the confirm
//! beam scores frames from the wake stream's encoder run, not from a second run that starts at
//! the 2.0 s anchor. Nothing here exists on a stream unless
//! [`super::KeywordSpotterConfig::shared_front_end`] is set.
//!
//! Frozen rules (see `docs/KWS-ONSET.md`, "Shared front end C6 (default off)"):
//! - The clock is the wake stream's integer encoder-frame clock (40 ms, 640 samples a frame,
//!   counted from the last reset). The caller supplies the requested start as an integer frame,
//!   rounded by FLOOR toward minus infinity ([`i64::div_euclid`]), so the beam never starts
//!   later than the requested context.
//! - A start before the stream's origin is CLAMPED ([`BeamStart::origin_clamped`]); a start the
//!   ring has already evicted is TRUNCATED ([`BeamStart::ring_truncated`]). They are separate
//!   states and either one means the context is not the requested one.
//! - Missing onset history yields an explicit `onset_unavailable` reason, never a boundary
//!   inferred from a shorter window.

use std::collections::VecDeque;

use super::decoder::DecoderResult;
use super::model::EncoderOut;
use super::onset::{OnsetConfig, OnsetHistory};
use super::trace::TraceState;

/// Encoder frames in the ring when [`FrontEndConfig::ring_frames`] is not chosen: 160 frames,
/// 6.4 s. The arithmetic (all constants from the cascade, in encoder frames of 40 ms):
/// requested context 2.0 s = 50; candidate span at most 2.0 s (`MAX_KEYWORD_SPAN_SECONDS`) = 50;
/// confirm timeout 1.5 s (`CONFIRM_TIMEOUT_SAMPLES`), after which the candidate expires, = 38
/// rounded up, because the beam can be recreated up to then; detection latency one encoder chunk
/// (8) plus one trailing blank plus a probe-cadence allowance, budgeted at 16. Newest back to the
/// requested start: 50 + 50 + 38 + 16 = 154, rounded to 160. A proposal derived from those
/// constants; a start older than the ring is reported as `ring_truncated`, never hidden.
pub const DEFAULT_RING_FRAMES: usize = 160;

/// Shared front end settings for the stream that owns the encoder (the wake stream).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrontEndConfig {
    /// Encoder frames kept (at least 16, at most 4096).
    pub ring_frames: usize,
}

impl Default for FrontEndConfig {
    fn default() -> Self {
        Self {
            ring_frames: DEFAULT_RING_FRAMES,
        }
    }
}

impl FrontEndConfig {
    pub(crate) fn valid(&self) -> bool {
        (16..=4096).contains(&self.ring_frames)
    }
}

/// Onset-history sizes for the confirm beam's estimator, derived from the ring: encoder frames
/// `ring + align_lead + posterior_max` (the estimator reads that far before the first token, and
/// the first token can be as old as the ring), feature frames `4 * ring + lookback` where the
/// lookback is the larger of the acoustic search reach and, when D4 is on, its window plus floor.
/// The wake stream's own history keeps the configured sizes, so wake estimates do not change.
pub(crate) fn shared_onset_retention(ring_frames: usize, config: &OnsetConfig) -> (usize, usize) {
    let encoder = ring_frames + config.align_lead_frames + config.posterior_max_frames;
    let d4 = if config.fricative.enabled {
        (config.fricative.window_ms + config.fricative.floor_ms) as usize / 10
    } else {
        0
    };
    let feature = 4 * ring_frames + config.acoustic_max_frames.max(d4);
    (encoder, feature)
}

/// A bounded ring of encoder frames on the wake clock.
pub(crate) struct FrameRing {
    capacity: usize,
    frames: VecDeque<Vec<f32>>,
    /// Wake-clock encoder frame of the front of the ring.
    first: i32,
    dim: usize,
}

impl FrameRing {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            frames: VecDeque::new(),
            first: 0,
            dim: 0,
        }
    }

    /// Append the chunk whose first frame is wake-clock frame `first`. A chunk that does not
    /// follow the ring starts it over. Once full the oldest frame's buffer is reused.
    pub(crate) fn push_chunk(&mut self, first: i32, out: &EncoderOut) {
        if self.frames.is_empty() || first != self.first + self.frames.len() as i32 {
            self.frames.clear();
            self.first = first;
        }
        self.dim = out.dim;
        for t in 0..out.frames {
            let mut frame = if self.frames.len() >= self.capacity {
                self.first += 1;
                self.frames.pop_front().unwrap_or_default()
            } else {
                Vec::with_capacity(out.dim)
            };
            frame.clear();
            frame.extend_from_slice(out.frame(t));
            self.frames.push_back(frame);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.frames.clear();
        self.first = 0;
    }

    pub(crate) fn first(&self) -> i32 {
        self.first
    }

    /// One past the newest retained frame.
    pub(crate) fn end(&self) -> i32 {
        self.first + self.frames.len() as i32
    }

    pub(crate) fn len(&self) -> usize {
        self.frames.len()
    }

    /// Bytes of retained frame values.
    pub(crate) fn bytes(&self) -> usize {
        self.frames.len() * self.dim * std::mem::size_of::<f32>()
    }

    /// Frames `from..to` as one encoder output, or `None` when any of them is outside the ring.
    pub(crate) fn slice(&self, from: i32, to: i32) -> Option<EncoderOut> {
        if from >= to || from < self.first || to > self.end() {
            return None;
        }
        let mut data = Vec::with_capacity((to - from) as usize * self.dim);
        for frame in self.frames.range((from - self.first) as usize..(to - self.first) as usize) {
            data.extend_from_slice(frame);
        }
        Some(EncoderOut {
            frames: (to - from) as usize,
            dim: self.dim,
            data,
        })
    }
}

/// What the stream that owns the encoder keeps for shared beams.
pub(crate) struct SharedFrontEnd {
    pub(crate) ring: FrameRing,
    /// The confirm beam's onset history, sized by [`shared_onset_retention`]; `None` when onset
    /// estimation is off.
    pub(crate) onset: Option<OnsetHistory>,
    /// Resets of the owner; a beam from an earlier epoch is dead.
    pub(crate) epoch: u64,
}

/// Where a confirm beam started on the wake clock, and why. Frame numbers are integer wake
/// encoder frames; the two states are independent flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeamStart {
    /// The requested start frame (floor of the requested sample), negative when it precedes
    /// the stream's origin.
    pub requested_frame: i64,
    /// The frame the beam actually started on.
    pub start_frame: i32,
    /// The request preceded the stream's origin (frame 0) and was moved to it.
    pub origin_clamped: bool,
    /// The (clamped) request was older than the ring's oldest frame and was moved to it.
    pub ring_truncated: bool,
    /// Frames from the requested start to the newest retained frame.
    pub requested_context_frames: i64,
    /// Frames from the actual start to the newest retained frame.
    pub actual_context_frames: i64,
}

impl BeamStart {
    /// `full`, `origin_clamped`, `ring_truncated` or `origin_clamped_and_ring_truncated`:
    /// only `full` is the requested context.
    pub fn state(&self) -> &'static str {
        match (self.origin_clamped, self.ring_truncated) {
            (false, false) => "full",
            (true, false) => "origin_clamped",
            (false, true) => "ring_truncated",
            (true, true) => "origin_clamped_and_ring_truncated",
        }
    }
}

/// The start of a confirm beam: `requested_frame` clamped to the origin, then to the ring.
pub(crate) fn plan_beam_start(requested_frame: i64, ring_first: i32, ring_end: i32) -> BeamStart {
    let origin_clamped = requested_frame < 0;
    let wanted = requested_frame.max(0);
    let ring_truncated = wanted < i64::from(ring_first);
    let start = wanted
        .max(i64::from(ring_first))
        .min(i64::from(ring_end));
    BeamStart {
        requested_frame,
        start_frame: start as i32,
        origin_clamped,
        ring_truncated,
        requested_context_frames: i64::from(ring_end) - requested_frame,
        actual_context_frames: i64::from(ring_end) - start,
    }
}

/// A confirm beam with no front end of its own: it decodes frames the wake stream's ring holds
/// ([`super::KeywordSpotter::decode_beam`]).
pub struct BeamStream {
    pub(crate) spotter_id: u64,
    pub(crate) result: DecoderResult,
    pub(crate) reported_last_frame: Option<i32>,
    /// Next wake-clock frame to decode.
    pub(crate) next_frame: i32,
    pub(crate) start: BeamStart,
    pub(crate) epoch: u64,
    /// Ring frames and bytes held by the source when the beam was created.
    pub(crate) ring_frames: usize,
    pub(crate) ring_bytes: usize,
    pub(crate) trace: Option<TraceState>,
}

impl BeamStream {
    /// Where this beam started and why.
    pub fn start(&self) -> BeamStart {
        self.start
    }

    /// Ring frames and bytes the source held when this beam was created.
    pub fn ring_size(&self) -> (usize, usize) {
        (self.ring_frames, self.ring_bytes)
    }

    /// Record per-step window traces only for wake-clock frames in `from_frame..to_frame`.
    pub fn set_trace_window(&mut self, from_frame: i32, to_frame: i32) {
        if let Some(trace) = self.trace.as_mut() {
            trace.set_window(from_frame, to_frame);
        }
    }

    /// Drain this beam's trace records. `None` when tracing is off.
    pub fn take_trace(&mut self) -> Option<super::trace::TraceRecords> {
        self.trace.as_mut().map(TraceState::take)
    }

    /// The once-only gate of [`super::KeywordStream`], for this beam's result.
    pub(crate) fn take_new_result(&mut self) -> Option<&DecoderResult> {
        let first = self.result.timestamps.first().copied();
        if let (Some(reported), Some(first)) = (self.reported_last_frame, first) {
            if first <= reported {
                return None;
            }
        }
        self.reported_last_frame = self.result.timestamps.last().copied();
        Some(&self.result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(first_value: f32, frames: usize, dim: usize) -> EncoderOut {
        EncoderOut {
            frames,
            dim,
            data: (0..frames * dim).map(|i| first_value + i as f32).collect(),
        }
    }

    #[test]
    fn start_is_full_origin_clamped_ring_truncated_or_both() {
        // Ring holds frames 40..200; the request is 150 frames: full.
        let start = plan_beam_start(150, 40, 200);
        assert_eq!((start.start_frame, start.state()), (150, "full"));
        assert_eq!(start.requested_context_frames, 50);
        assert_eq!(start.actual_context_frames, 50);
        // Before the origin with an unevicted ring: only the origin clamp.
        let start = plan_beam_start(-30, 0, 100);
        assert_eq!((start.start_frame, start.state()), (0, "origin_clamped"));
        assert_eq!(start.requested_context_frames, 130);
        assert_eq!(start.actual_context_frames, 100);
        // Inside the stream but evicted from the ring: only the ring truncation.
        let start = plan_beam_start(10, 40, 200);
        assert_eq!((start.start_frame, start.state()), (40, "ring_truncated"));
        // Before the origin and the ring has moved on: both are reported.
        let start = plan_beam_start(-5, 40, 200);
        assert_eq!(start.state(), "origin_clamped_and_ring_truncated");
        // A request past the newest frame starts at the next frame to arrive.
        assert_eq!(plan_beam_start(500, 40, 200).start_frame, 200);
        // An empty ring at the origin.
        assert_eq!(plan_beam_start(0, 0, 0).start_frame, 0);
    }

    #[test]
    fn requested_sample_rounds_by_floor_in_integer_frames() {
        // The cascade's rule: floor((sample - origin) / 640), toward minus infinity.
        let frame = |offset: i64| offset.div_euclid(640);
        assert_eq!(frame(0), 0);
        assert_eq!(frame(639), 0);
        assert_eq!(frame(640), 1);
        assert_eq!(frame(-1), -1);
        assert_eq!(frame(-640), -1);
        assert_eq!(frame(-641), -2);
    }

    #[test]
    fn ring_keeps_the_newest_frames_and_slices_only_what_it_holds() {
        let mut ring = FrameRing::new(4);
        ring.push_chunk(0, &chunk(0.0, 3, 2));
        assert_eq!((ring.first(), ring.end()), (0, 3));
        ring.push_chunk(3, &chunk(100.0, 3, 2));
        // Six pushed into four: frames 2..6 remain.
        assert_eq!((ring.first(), ring.end(), ring.len()), (2, 6, 4));
        assert!(ring.slice(1, 3).is_none());
        assert!(ring.slice(2, 7).is_none());
        let out = ring.slice(4, 6).expect("held");
        assert_eq!((out.frames, out.dim), (2, 2));
        assert_eq!(out.data, vec![102.0, 103.0, 104.0, 105.0]);
        assert_eq!(ring.bytes(), 4 * 2 * 4);
        // A chunk that does not follow starts the ring over.
        ring.push_chunk(50, &chunk(7.0, 1, 2));
        assert_eq!((ring.first(), ring.end()), (50, 51));
    }

    #[test]
    fn onset_retention_follows_the_ring_and_the_estimator_reach() {
        let config = OnsetConfig::default();
        // 160 + align_lead 10 + posterior_max 15; 4 * 160 + acoustic reach 30.
        assert_eq!(shared_onset_retention(160, &config), (185, 670));
        let mut d4 = config.clone();
        d4.fricative.enabled = true;
        // D4 window 250 ms + floor 500 ms = 75 feature frames beats the acoustic reach.
        assert_eq!(shared_onset_retention(160, &d4), (185, 715));
    }
}
