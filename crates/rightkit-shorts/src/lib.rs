//! CutRight shorts finder (M2 parity: `shorts.rs` + `shorts_scoring.rs`).
//!
//! - [`config`]: the versioned scoring profile (weights, thresholds, cue lists as JSON).
//! - [`score`]: the scoring stages, ported formula for formula.
//! - [`propose`]: candidate windows from transcript sentences and pauses, the full
//!   pipeline, and the word-aligned [`Proposals`] (status always `proposed`).
//! - [`llm`] / [`ranking`]: LLM moment ranking (Anthropic by default, key from the OS
//!   secret store) validated onto word boundaries, with the heuristic scorer as fallback.
//! - [`judge`]: the typed LLM verdict; [`budget`]: the prompt token budget and its report.
//! - [`audio`]: measured loudness and laughter passed in as typed ranking inputs.
//! - [`short`]: an accepted proposal -> variant timeline actions, cut plan, 9:16
//!   centre-crop placeholder, caption titles and a render request.
//!
//! Pure computation; portable.

pub mod audio;
pub mod budget;
pub mod config;
pub mod error;
pub mod judge;
pub mod llm;
pub mod propose;
pub mod ranking;
pub mod score;
pub mod short;

pub use audio::{
    audio_adjustment, window_audio, AudioAdjustment, AudioFeatures, LaughterSpan, LoudnessSample,
    WindowAudio,
};
pub use budget::{plan_requests, BudgetAction, BudgetReport, RequestPlan};
pub use config::{AudioRules, PlatformWindow, ShortsProfile};
pub use error::ShortsError;
pub use judge::JudgeVerdict;
pub use llm::{
    AnthropicProvider, LlmMoment, MomentProvider, MomentRequest, ProviderError, SentenceItem,
    DEFAULT_MODEL, SECRET_ACCOUNT, SECRET_SERVICE,
};
pub use propose::{
    propose_shorts, BeatHint, LlmEvidence, PlatformFit, Preview, Proposal, Proposals, ProposeInput,
    RankingInfo, RejectedWindow, TruthfulnessNote, PROPOSALS_SCHEMA,
};
pub use ranking::propose_shorts_ranked;
pub use score::{Confidence, Score, ScoreBreakdown, StandaloneCheck, VisualPoint};
pub use short::{build_short, ReframeState, ShortOptions, ShortPlan, ShortRenderRequest};
