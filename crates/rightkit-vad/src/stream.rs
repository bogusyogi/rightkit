use crate::{FrameScorer, FRAME_SAMPLES};

/// Cap on buffered-but-unscored samples (30 s), matching HeardRight.
const MAX_PENDING_SAMPLES: usize = 16_000 * 30;

/// One entry per complete frame consumed by [`FrameStream::push`].
#[derive(Debug, Clone, PartialEq)]
pub struct FrameVerdicts {
    /// Raw probabilities, one per frame.
    pub scores: Vec<f32>,
    /// `score >= threshold`, one per frame.
    pub speech: Vec<bool>,
}

/// Lossless streaming adapter: arbitrary-size capture callbacks in, whole
/// frames scored. The trailing partial frame is carried to the next push so
/// neither the waveform nor the recurrent state ever has a gap. If the scorer
/// fails, `push` returns `None` (unknown), which callers must not treat as
/// silence; the stream stays failed until [`FrameStream::reset`].
pub struct FrameStream<S: FrameScorer> {
    scorer: S,
    threshold: f32,
    pending: Vec<f32>,
    failed: bool,
    observed: u64,
    speech: u64,
}

impl<S: FrameScorer> FrameStream<S> {
    pub fn new(scorer: S, threshold: f32) -> Self {
        Self {
            scorer,
            threshold,
            pending: Vec::new(),
            failed: false,
            observed: 0,
            speech: 0,
        }
    }

    pub fn reset(&mut self) {
        self.scorer.reset();
        self.pending.clear();
        self.failed = false;
        self.observed = 0;
        self.speech = 0;
    }

    pub fn is_failed(&self) -> bool {
        self.failed
    }

    /// (frames observed, frames judged speech) since the last reset.
    pub fn aggregate(&self) -> Option<(u64, u64)> {
        (self.observed > 0).then_some((self.observed, self.speech))
    }

    pub fn threshold(&self) -> f32 {
        self.threshold
    }

    pub fn set_threshold(&mut self, threshold: f32) {
        self.threshold = threshold;
    }

    pub fn push(&mut self, samples: &[f32]) -> Option<FrameVerdicts> {
        if self.failed {
            return None;
        }
        self.pending.extend_from_slice(samples);
        if self.pending.len() > MAX_PENDING_SAMPLES {
            let extra = self.pending.len() - MAX_PENDING_SAMPLES;
            self.pending.drain(..extra);
        }
        let complete = self.pending.len() / FRAME_SAMPLES * FRAME_SAMPLES;
        let tail = self.pending.split_off(complete);
        let frames = std::mem::replace(&mut self.pending, tail);
        let mut out = FrameVerdicts {
            scores: Vec::new(),
            speech: Vec::new(),
        };
        for frame in frames.as_chunks::<FRAME_SAMPLES>().0 {
            match self.scorer.score(frame) {
                Ok(score) => {
                    self.observed += 1;
                    let is_speech = score >= self.threshold;
                    self.speech += is_speech as u64;
                    out.scores.push(score);
                    out.speech.push(is_speech);
                }
                Err(_) => {
                    self.failed = true;
                    self.pending.clear();
                    return None;
                }
            }
        }
        Some(out)
    }
}
