//! Release stand-in for the live-evaluation harness (`live_eval.rs`).
//!
//! The harness is a developer tool: it reads evaluation selectors and provider-key variables from
//! the environment and sends dictated text to candidate endpoints. A release build must contain
//! none of that (release switch rule), so the real module exists only in test and `test-mode`
//! builds and this file keeps the public surface: evaluation is never enabled and never runs.

use std::path::PathBuf;

use super::state::PolishContext;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveEvalLane {
    L1,
    L2,
    L3,
}

pub fn live_eval_enabled() -> bool {
    false
}

pub fn live_eval_suppresses_delivery() -> bool {
    false
}

pub fn run_live_evaluation(
    _input: &str,
    _context: &PolishContext,
    _lane: LiveEvalLane,
) -> Result<PathBuf, String> {
    Err("live evaluation is available only in a test-mode build".to_string())
}

#[doc(hidden)]
pub fn render_prompt_convergence_payloads_json(_input: &str) -> Result<String, String> {
    Err("prompt convergence rendering is available only in a test-mode build".to_string())
}
