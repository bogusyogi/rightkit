//! Bounded cloud semantic cleanup: the L1 app-polish, L2 prompt-polish, and
//! L3 summary lanes plus their provider roster, transport, wire compiler,
//! acceptance guardrails, and the debug-only live-evaluation harness.
//!
//! The `pub(crate) use ...::*` re-exports below preserve a merged-namespace
//! surface the unit tests and sibling modules (`guardrail_replay`,
//! `text_polish`) resolve against.

mod acceptance;
mod lanes;
// The harness reads evaluation selectors and provider-key variables, so it exists only in test
// and `test-mode` builds. A release build gets the inert stand-in with the same public surface.
#[path = "l3_cleanup/live_eval_release.rs"]
mod live_eval;
mod ops;
// Environment overrides (endpoints, models, routing, tuning, provider keys): test-mode only.
mod overrides;
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
pub use live_eval::{
    live_eval_enabled, live_eval_suppresses_delivery, render_prompt_convergence_payloads_json,
    run_live_evaluation, LiveEvalLane,
};
pub use ops::{health, record_local_fallback};
pub use state::{CleanupHealth, CleanupOutcome, PolishContext};

// Crate-internal surface: names the test subtree and sibling modules resolve
// against. Explicit `pub use`s above win over these globs for public items.
//
// `acceptance` stays ungated: `guardrail_replay` resolves its pub(crate) fns
// through this glob in normal builds. The rest are referenced only by the
// `#[cfg(test)]` subtree (`tests.rs` / `tests/more.rs` via `use super::*`),
// so they are test-only and would warn as unused in the lib target.
pub(crate) use acceptance::*;
