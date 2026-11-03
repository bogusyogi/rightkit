//! S:N2 — Post-run verifier execution contract (pure decision layer).
//!
//! A `BotProfileV1` may bind a post-run verifier (role pack + optional model
//! override + required flag). The stored binding schema lives with the bot
//! profile; this module owns the pure decision contract the daemon scheduler
//! run finalizer executes after a run reaches terminal state:
//!
//! 1. [`VerifierBindingSpec::is_configured`] — whether a verifier run must
//!    fire at all.
//! 2. [`verifier_prompt`] — deterministic verification prompt bound to the
//!    verified run's session id and a bounded result excerpt.
//! 3. [`classify_verdict`] — parse the verifier's terminal outcome into a
//!    typed [`VerifierVerdict`]. A missing or unparseable verdict fails
//!    closed as [`VerifierVerdict::Unavailable`].
//! 4. [`gate_terminal_status`] — fold the verdict into the run's terminal
//!    state. A required verifier that does not approve blocks the run from
//!    the `completed` state.
//!
//! The runtime execution (verifier session spawn, subscription wait, causal
//! receipt persistence) lives in the daemon scheduler; this crate stays a
//! pure data + pure-function domain per ADR-0021 (WP-00).

use serde::{Deserialize, Serialize};

/// Pure description of a stored verifier binding. Mirrors the wire fields of
/// the bot profile's `verifier` object without coupling this crate to the
/// daemon API surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifierBindingSpec {
    /// Role pack the verifier session runs under. `None`/blank inherits the
    /// verified run's role.
    pub role_pack: Option<String>,
    /// Model the verifier session runs under. `None`/blank inherits the
    /// verified run's model.
    pub model: Option<String>,
    /// When true, a verifier that does not approve blocks the run from the
    /// completed state. When false, the verifier is best-effort: the verdict
    /// is recorded but never blocks.
    pub required: bool,
}

impl VerifierBindingSpec {
    /// A verifier is configured only when at least one execution surface
    /// (role pack or model) is named. A binding with both blank would
    /// otherwise silently re-run the run's own configuration.
    pub fn is_configured(&self) -> bool {
        self.role_pack
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
            || self.model.as_deref().is_some_and(|v| !v.trim().is_empty())
    }
}

/// Marker the verifier prompt requires as the verifier's final line.
pub const VERDICT_PASS_MARKER: &str = "VERDICT: PASS";
/// Marker the verifier prompt requires for rejection, followed by a reason.
pub const VERDICT_REJECT_MARKER: &str = "VERDICT: REJECT:";

/// Hard cap on the run-result excerpt embedded in the verifier prompt, in
/// characters. Keeps the verifier's own context bounded regardless of the
/// verified run's output size.
pub const VERIFIER_RESULT_EXCERPT_MAX_CHARS: usize = 4_000;

/// Typed outcome of the post-run verifier execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum VerifierVerdict {
    /// The verifier completed and returned the pass marker.
    Approved,
    /// The verifier completed and returned the reject marker.
    Rejected { reason: String },
    /// The verifier could not produce a verdict (session failure, timeout,
    /// engine undelivered, or missing verdict marker). Always fails closed.
    Unavailable { reason: String },
}

/// Pure classification of a verifier session's terminal state.
///
/// - Any terminal status other than `completed` yields
///   [`VerifierVerdict::Unavailable`] — the verifier never ran to verdict.
/// - Otherwise the bounded verifier text is scanned for the required
///   markers; the last marker occurrence wins (the final statement is the
///   authoritative one). No marker fails closed.
pub fn classify_verdict(terminal_status: &str, verifier_text: &str) -> VerifierVerdict {
    if !terminal_status.eq_ignore_ascii_case("completed") {
        return VerifierVerdict::Unavailable {
            reason: format!("verifier session ended with status '{terminal_status}'"),
        };
    }
    let lowered = verifier_text.to_ascii_lowercase();
    let pass = lowered.rfind(&VERDICT_PASS_MARKER.to_ascii_lowercase());
    let reject = lowered.rfind(&VERDICT_REJECT_MARKER.to_ascii_lowercase());
    match (pass, reject) {
        (None, None) => VerifierVerdict::Unavailable {
            reason: "verifier output contains no verdict marker".into(),
        },
        (_, Some(reject_at)) if pass.is_none_or(|pass_at| reject_at > pass_at) => {
            VerifierVerdict::Rejected {
                reason: bound_reason(&verifier_text[reject_at..]),
            }
        }
        (None, Some(reject_at)) => VerifierVerdict::Rejected {
            reason: bound_reason(&verifier_text[reject_at..]),
        },
        (Some(_), _) => VerifierVerdict::Approved,
    }
}

/// Deterministic post-run verification prompt. The run result excerpt is
/// bounded so a runaway run cannot blow up the verifier's context.
pub fn verifier_prompt(
    schedule_id: &str,
    run_session_id: &str,
    run_status: &str,
    result_excerpt: &str,
) -> String {
    format!(
        "You are the post-run verifier bound to schedule '{schedule_id}'.\n\
         Verify the following completed run before it may be marked done.\n\
         Run session: {run_session_id}\n\
         Run status: {run_status}\n\
         Run result (bounded excerpt):\n\"\"\"\n{}\n\"\"\"\n\
         End your reply with exactly one final line:\n\
         {VERDICT_PASS_MARKER}\n\
         or\n\
         {VERDICT_REJECT_MARKER} <short reason>",
        bound_excerpt(result_excerpt),
    )
}

/// Fold a verifier verdict into the run's terminal gate.
///
/// - [`VerifierVerdict::Approved`] always completes.
/// - A required binding blocks on [`VerifierVerdict::Rejected`] and
///   [`VerifierVerdict::Unavailable`] (fail closed: unverifiable ≠ done).
/// - A non-required binding records the verdict without blocking.
pub fn gate_terminal_status(verdict: &VerifierVerdict, required: bool) -> TerminalGate {
    match verdict {
        VerifierVerdict::Approved => TerminalGate::Complete,
        VerifierVerdict::Rejected { reason } | VerifierVerdict::Unavailable { reason } => {
            if required {
                TerminalGate::Blocked {
                    reason: reason.clone(),
                }
            } else {
                TerminalGate::Complete
            }
        }
    }
}

/// Result of [`gate_terminal_status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalGate {
    /// The run may take its natural terminal state.
    Complete,
    /// The run is held out of the completed state; the carried reason is
    /// persisted on the terminal run event.
    Blocked { reason: String },
}

/// Char-boundary-safe truncation for the prompt excerpt.
fn bound_excerpt(text: &str) -> String {
    if text.chars().count() <= VERIFIER_RESULT_EXCERPT_MAX_CHARS {
        return text.to_string();
    }
    let truncated: String = text
        .chars()
        .take(VERIFIER_RESULT_EXCERPT_MAX_CHARS)
        .collect();
    format!("{truncated}\n[excerpt truncated]")
}

/// Bound a rejection reason to a single bounded line.
fn bound_reason(raw: &str) -> String {
    let line = raw
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .trim_start_matches(VERDICT_REJECT_MARKER)
        .trim()
        .to_string();
    let bounded: String = line.chars().take(200).collect();
    if bounded.is_empty() {
        "verifier rejected without a reason".into()
    } else {
        bounded
    }
}
