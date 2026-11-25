//! Turn-taking: has the speaker finished?
//!
//! Silence alone ends a turn too early when someone pauses mid-sentence and too late when they are
//! done. [`TurnTracker`] separates the two questions: voice activity says whether the last stretch was
//! silent, and an optional [`TurnModel`] says whether what was said sounds finished. With no model it
//! falls back to a fixed silence, which is the behaviour every build has without a download.
//!
//! A model is a download and a decision (the blueprint's candidate is smart-turn); nothing here loads
//! one. A host that has one implements [`TurnModel`] and passes it to [`TurnTracker::new`].

use std::collections::VecDeque;

/// Sample rate of the audio a tracker and a model see.
pub const TURN_RATE: u32 = 16_000;
/// RMS above which a chunk counts as voiced for [`is_voiced`].
const VOICED_RMS: f32 = 0.02;

/// A model of whether a stretch of speech is a finished turn.
pub trait TurnModel: Send {
    /// Probability from 0 to 1 that the speaker has finished, given the tail of the audio so far
    /// (mono, [`TURN_RATE`], at most [`TurnPolicy::tail_secs`] seconds). An error makes the tracker use
    /// the silence fallback for this decision.
    fn end_of_turn(&mut self, tail: &[f32]) -> Result<f32, String>;
}

/// When a turn may end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TurnPolicy {
    /// With a model: silence, in milliseconds, before the model is first asked.
    pub ask_after_ms: u32,
    /// With a model: the model is asked again each time silence grows by this much.
    pub ask_every_ms: u32,
    /// With a model: the probability at or above which the turn is over.
    pub threshold: f32,
    /// Without a model, or after a model error: silence, in milliseconds, that ends the turn.
    pub fallback_silence_ms: u32,
    /// With a model: silence that ends the turn whatever the model says.
    pub max_silence_ms: u32,
    /// Seconds of audio the model is given.
    pub tail_secs: u32,
}

impl Default for TurnPolicy {
    fn default() -> Self {
        Self {
            ask_after_ms: 250,
            ask_every_ms: 100,
            threshold: 0.5,
            fallback_silence_ms: 700,
            max_silence_ms: 1_800,
            tail_secs: 8,
        }
    }
}

/// What the tracker decided after a chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnDecision {
    /// Still speaking, or not yet spoken.
    Listening,
    /// The turn is over.
    Ended,
}

/// Whether `chunk` (mono, any rate) is loud enough to be speech. Model-free.
pub fn is_voiced(chunk: &[f32]) -> bool {
    !chunk.is_empty()
        && (chunk.iter().map(|sample| sample * sample).sum::<f32>() / chunk.len() as f32).sqrt()
            >= VOICED_RMS
}

/// Follows one utterance and says when its turn ends.
pub struct TurnTracker {
    policy: TurnPolicy,
    model: Option<Box<dyn TurnModel>>,
    tail: VecDeque<f32>,
    heard_voice: bool,
    silence_samples: usize,
    /// Silence, in samples, at which the model was last asked.
    asked_at: Option<usize>,
}

impl TurnTracker {
    pub fn new(policy: TurnPolicy, model: Option<Box<dyn TurnModel>>) -> Self {
        Self {
            policy,
            model,
            tail: VecDeque::new(),
            heard_voice: false,
            silence_samples: 0,
            asked_at: None,
        }
    }

    /// A tracker with no model: silence alone, [`TurnPolicy::fallback_silence_ms`].
    pub fn without_model(policy: TurnPolicy) -> Self {
        Self::new(policy, None)
    }

    /// Start a new utterance.
    pub fn reset(&mut self) {
        self.tail.clear();
        self.heard_voice = false;
        self.silence_samples = 0;
        self.asked_at = None;
    }

    fn ms_to_samples(ms: u32) -> usize {
        ms as usize * TURN_RATE as usize / 1_000
    }

    /// Feed the next chunk (mono, [`TURN_RATE`]) and whether voice activity found speech in it.
    pub fn push(&mut self, chunk: &[f32], voiced: bool) -> TurnDecision {
        self.tail.extend(chunk.iter().copied());
        let keep = self.policy.tail_secs as usize * TURN_RATE as usize;
        while self.tail.len() > keep {
            self.tail.pop_front();
        }
        if voiced {
            self.heard_voice = true;
            self.silence_samples = 0;
            self.asked_at = None;
            return TurnDecision::Listening;
        }
        if !self.heard_voice {
            return TurnDecision::Listening;
        }
        self.silence_samples += chunk.len();
        self.decide()
    }

    fn decide(&mut self) -> TurnDecision {
        let fallback = Self::ms_to_samples(self.policy.fallback_silence_ms);
        if self.model.is_none() {
            return Self::ended_if(self.silence_samples >= fallback);
        }
        if self.silence_samples >= Self::ms_to_samples(self.policy.max_silence_ms) {
            return TurnDecision::Ended;
        }
        let first = Self::ms_to_samples(self.policy.ask_after_ms);
        let step = Self::ms_to_samples(self.policy.ask_every_ms).max(1);
        let due = match self.asked_at {
            None => self.silence_samples >= first,
            Some(last) => self.silence_samples >= last + step,
        };
        if !due {
            return TurnDecision::Listening;
        }
        self.asked_at = Some(self.silence_samples);
        let tail: Vec<f32> = self.tail.iter().copied().collect();
        let answer = self
            .model
            .as_mut()
            .map(|model| model.end_of_turn(&tail))
            .unwrap_or_else(|| Err("no model".to_string()));
        match answer {
            Ok(probability) => Self::ended_if(probability >= self.policy.threshold),
            Err(_) => Self::ended_if(self.silence_samples >= fallback),
        }
    }

    fn ended_if(ended: bool) -> TurnDecision {
        if ended {
            TurnDecision::Ended
        } else {
            TurnDecision::Listening
        }
    }
}
