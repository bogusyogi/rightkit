//! The app text pipeline: deterministic polish, vocabulary, screen context,
//! bounded remote cleanup, and canonical polish evaluation.

#![allow(
    clippy::result_large_err,
    clippy::large_enum_variant,
    clippy::too_many_arguments,
    clippy::doc_lazy_continuation,
    clippy::manual_clamp
)]

pub mod canonical_polish_harness;
pub mod guardrail_replay;
pub mod l1c1_serialize;
pub mod l3_cleanup;
pub mod provider_keys;
pub mod screen_vocab;
pub mod settings;
pub mod text_polish;
pub mod vocabulary;

// Shared size bound used by focus capture and cleanup sanitization. Keeping it
// here prevents UI/focus implementation details from leaking into this crate.
pub mod focus {
    pub const FIELD_CONTEXT_MAX_CHARS: usize = 1_500;
}
