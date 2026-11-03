//! Bounded cloud semantic cleanup: the L1 app-polish, L2 prompt-polish, and
//! L3 summary lanes plus their provider roster, transport, wire compiler,
//! and acceptance guardrails.
//!
//! The `pub(crate) use ...::*` re-exports below preserve a merged-namespace
//! surface the unit tests and sibling modules (`guardrail_replay`,
//! `text_polish`) resolve against.

mod acceptance;
mod lanes;
mod ops;
// Product-set cleanup policy: from the pushed runtime config, never the environment.
mod policy;
mod prompts;
mod providers;
mod state;
mod transport;
mod wire;

// Public API.
pub use lanes::{
    app_polish_outcome, app_polish_outcome_with_receipts, cleanup_enabled, prompt_polish_outcome,
    prompt_polish_outcome_with_receipts, summarize_outcome, summarize_outcome_with_receipts,
};
pub use ops::{health, record_local_fallback};
pub use state::{CleanupHealth, CleanupOutcome, PolishContext};

// Crate-internal surface: `guardrail_replay` resolves acceptance's pub(crate) fns through this glob.
pub(crate) use acceptance::*;
