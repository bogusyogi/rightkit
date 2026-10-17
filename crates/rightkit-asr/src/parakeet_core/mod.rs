//! Runtime-agnostic Parakeet TDT decoding primitives: beam search, context
//! bias, detokenization (incl. N0 filler policy), score arithmetic and
//! per-platform decode policy.
//!
//! Donor: HeardRight `heardright-parakeet-core` (production semantics; HR wins
//! on decode semantics for the suite). Ported verbatim except: no product
//! environment-variable reads (`HR_ASR_BEAM_EMIT_REWARD` became an explicit
//! `emit_reward` argument / `DecodePolicy::emit_reward`), the terminal
//! command-locator helper (`command_alignment`) is HeardRight-specific and
//! stays in that app's adapter, and the unit tests moved to end-to-end tests.
//! Contains no CoreML, ORT, audio or model-loading code.

pub mod backend;
pub mod beam;
pub mod bias;
pub mod detok;
pub mod policy;
pub mod scoring;
pub mod tdt_beam;
pub mod types;

pub use backend::{BackendStep, RecurrentTdtBackend};
pub use beam::{prune_global_stable, prune_time_synchronous_stable, BeamHypothesis};
pub use bias::{BiasScorer, ContextBias, NoBias};
pub use detok::{
    assemble_pieces, assemble_timed_pieces, is_skippable_special, strip_mandatory_fillers,
    strip_mandatory_fillers_text, DetokPolicy, Detokenized,
};
pub use policy::{
    DecodePolicy, ModelSemantics, PruningMode, MACOS_POST_C0239763_EMIT_REWARD, WINDOWS_EMIT_REWARD,
    WINDOWS_VENDOR_DURATION_CAP_INDEX,
};
pub use scoring::{
    log_prob_at, log_softmax, logsumexp, score_blank, score_emission, top_k_indices,
};
pub use types::{DecodedPiece, TimedToken, TranscriptionResult};
