// Iteration budget for the session turn loop.
//
// Extracted from `engine.rs` as part of FEATURE-MATRIX par §#141
// (iteration-budget refund for programmatic sub-calls + graceful
// exhaustion summary).
//
// Design notes
// ============
//
// `IterationBudget::charge` costs 1.0 for a non-programmatic turn
// (user-visible assistant text or any mutating tool call) and 0.25 for
// a programmatic turn (the model is purely chaining read-only tool
// calls — e.g. read_file -> grep -> read_file — to gather context for
// itself, with no user-visible output). A programmatic turn therefore
// consumes 1/4 of a full turn: four read-only lookups cost one
// user-visible turn. Rationale:
//
// - "Free" (cost 0) read-only chaining lets the model loop forever on
//   lookups; the fractional cost keeps a ceiling while still letting a
//   model do 5–10 read-only lookups to answer one question without
//   burning 5–10 of the user's 32 configured turns.
// - 0.25 is the simplest fractional value that both prevents
//   unbounded read-only chaining AND gives the "free lookups"
//   behaviour the FEATURE-MATRIX row calls for.
// - The MOP loop detector still runs; runaway read-only loops are
//   caught by `ToolLoopDetector` before the budget matters.
//
// Graceful exhaustion summary
// ===========================
//
// When the budget is exhausted, the engine attempts ONE more turn
// with a system-level nudge injected ("you are out of turns —
// summarize what you found and stop"). For that final turn the
// model is told tools are unavailable; if it still emits tool calls
// they are stripped (same post-processing pattern as
// `strip_safety_terminated_tool_calls` in `tool_dispatch_plumbing.rs`).
// If the stripped response has user-visible text, the run ends
// successfully with that text as the final output. Only if the
// summary turn also fails or produces no text does the engine fall
// through to the existing `RunFailed` path — preserving the
// regression guarantee that the abrupt-failure path still exists.
//
// NOTE: this file is pulled in via `#[path] mod iteration_budget;`
// from `engine.rs`. Per the crate's `include!`/`#[path] mod`
// conventions it uses `//` (not `//!`) for the file-level header,
// relies on `use super::*` to inherit the parent's imports, and
// declares its test module as `iteration_budget_tests` (not `tests`,
// which the parent already declares via `engine_tests.rs`).

use super::*;
use std::sync::Arc;

// `LlmResponse` is NOT imported by the parent, so we add it here.
// (Everything else we need — `LlmClient`, `Transcript`, `ToolCall`,
// `ToolRegistry`, `TranscriptMessage` — comes from `use super::*`.)
use crate::llm::LlmResponse;

/// Cost of a single non-programmatic (user-visible / mutating) turn.
const COST_FULL_TURN: f64 = 1.0;
/// Cost of a single programmatic (read-only tool chaining) turn.
/// Four programmatic turns cost one full turn.
const COST_PROGRAMMATIC_TURN: f64 = 0.25;

/// One entry in the budget's audit trail. Every successful `charge`
/// and every successful `refund` (non-zero amount) records an entry.
/// Refund events always carry a free-form reason string supplied by
/// the caller (e.g. `"no-op programmatic turn"`,
/// `"internal reset"`, `"crash recovery"`). Charge events optionally
/// include a reason; the default is `None`.
// The `Refund` variant + `refund()` implement the P-3 budget refund: the engine
// loop refunds a no-op turn's charge (no tool calls, no assistant text) so it does
// not burn the user's turn budget (wired at engine_loop.rs). `remaining()` and
// `audit_trail()` are the read side of the same primitive, retained for UI/
// diagnostics surfacing and kept `#[allow(dead_code)]` until consumed.
#[derive(Debug, Clone, PartialEq)]
pub enum BudgetEvent {
    Charge {
        amount: f64,
        programmatic: bool,
        remaining_after: f64,
        reason: Option<String>,
    },
    Refund {
        amount: f64,
        remaining_after: f64,
        reason: String,
    },
}

/// Nudge injected as a user turn on budget exhaustion. The model is
/// told to summarize and stop calling tools; any tool calls it still
/// emits are stripped before the response is recorded.
pub const MAX_ITERATION_SUMMARY_PROMPT: &str =
    "you are out of turns — summarize what you found and stop. Do not call more tools.";

/// Fractional iteration budget. A non-programmatic turn costs
/// `COST_FULL_TURN`; a programmatic turn costs `COST_PROGRAMMATIC_TURN`.
/// `is_exhausted()` flips to `true` when the remaining balance reaches
/// zero. The budget never goes negative — overspends are clamped to
/// `0.0`.
#[derive(Debug, Clone)]
pub struct IterationBudget {
    /// Configured ceiling (in full-turn units).
    max: f64,
    /// Remaining budget in fractional units. Starts equal to `max`.
    /// Refunds can lift this value back toward `max`, but the budget
    /// is capped at `max` — refunds cannot inflate the ceiling.
    remaining: f64,
    /// Cached exhaustion flag. `true` once the balance reaches `0.0`.
    /// Lifted back to `false` by a `refund` that lifts the remaining
    /// balance strictly above zero (see [`Self::refund`]).
    exhausted: bool,
    /// Audit trail of every charge/refund the engine applied. Useful
    /// for unit tests and for surfacing the budget accounting to the
    /// UI (parity P-3 exposes `audit_trail()`).
    audit_trail: Vec<BudgetEvent>,
}

impl IterationBudget {
    /// Build a budget from the configured `max_iterations`.
    pub fn new(max_iterations: usize) -> Self {
        let max = max_iterations as f64;
        Self {
            max,
            remaining: max,
            exhausted: false,
            audit_trail: Vec::new(),
        }
    }

    /// Maximum number of full turns the budget was configured for.
    pub fn max(&self) -> usize {
        self.max as usize
    }

    /// `true` once a charge has driven the remaining balance to `0.0`
    /// (or below — clamped). Refunds can lift this back to `false`
    /// if they restore the remaining balance strictly above `0.0`.
    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// Current remaining balance in fractional turn units. Useful for
    /// surfacing budget headroom to the UI; reads do not mutate state.
    #[allow(dead_code)]
    pub fn remaining(&self) -> f64 {
        self.remaining
    }

    /// Read-only view of the budget's audit trail (every charge and
    /// successful refund). Returned slice is in chronological order.
    #[allow(dead_code)]
    pub fn audit_trail(&self) -> &[BudgetEvent] {
        &self.audit_trail
    }

    /// Charge a turn against the budget. Returns the cost applied
    /// (`COST_FULL_TURN` for non-programmatic, `COST_PROGRAMMATIC_TURN`
    /// for programmatic). The caller passes `programmatic = true`
    /// when the turn was a read-only tool-chaining turn with no
    /// user-visible assistant text.
    pub fn charge(&mut self, programmatic: bool) -> f64 {
        let cost = if programmatic {
            COST_PROGRAMMATIC_TURN
        } else {
            COST_FULL_TURN
        };
        self.remaining = (self.remaining - cost).max(0.0);
        if self.remaining <= 0.0 {
            self.exhausted = true;
        }
        self.audit_trail.push(BudgetEvent::Charge {
            amount: cost,
            programmatic,
            remaining_after: self.remaining,
            reason: None,
        });
        cost
    }

    /// Refund a previously charged amount (parity P-3). The typical
    /// caller is a programmatic / internal turn that made no model
    /// call and no tool mutation; refunding the fractional charge
    /// restores headroom and prevents the agent from "burning" the
    /// budget on housekeeping.
    ///
    /// Rules:
    /// - The refund is **clamped** to the configured ceiling — a
    ///   refund cannot inflate the remaining balance above `max`.
    /// - A refund that lifts the remaining balance strictly above
    ///   `0.0` clears the exhaustion latch (so a `B → refund → C`
    ///   sequence re-enables the engine loop).
    /// - `amount <= 0` is a no-op; nothing is recorded in the
    ///   audit trail (matches the "no event for a no-op refund"
    ///   call-site contract).
    /// - The audit trail records every successful refund with the
    ///   amount applied (post-clamp), the remaining balance after
    ///   the refund, and the caller-supplied reason string.
    ///
    /// Returns the actual amount refunded (post-clamp; equal to
    /// `amount` unless it was clamped against the ceiling or was
    /// `<= 0`).
    // FM-064: retain current refund semantics — mechanism site for the
    // ledger's "no superior adoption source proven" disposition. Live and
    // wired at the real no-op-turn call site in engine_loop.rs (see the
    // `FEATURE-MATRIX par §#141` comment there); covered by the
    // `IterationBudget.refund()` unit tests below.
    pub fn refund(&mut self, amount: f64, reason: impl Into<String>) -> f64 {
        if amount <= 0.0 {
            return 0.0;
        }
        let headroom = (self.max - self.remaining).max(0.0);
        let applied = amount.min(headroom);
        if applied <= 0.0 {
            // Already at the ceiling — nothing to refund.
            return 0.0;
        }
        self.remaining += applied;
        if self.remaining > 0.0 {
            self.exhausted = false;
        }
        self.audit_trail.push(BudgetEvent::Refund {
            amount: applied,
            remaining_after: self.remaining,
            reason: reason.into(),
        });
        applied
    }
}

/// Classify a turn as `programmatic` (read-only tool chaining with no
/// user-visible assistant text) or `non-programmatic` (user-visible
/// assistant text, or any mutating tool call).
///
/// A turn is `programmatic` iff ALL of the following hold:
///
/// 1. The response has at least one tool call.
/// 2. The response has no user-visible assistant text (empty after
///    trimming). Reasoning text does NOT count as user-visible — the
///    `is_reasoning` field on `AssistantDelta` already partitions it
///    out for UIs.
/// 3. Every tool call is read-only, as reported by the tool
///    registry's `is_read_only()` method. Tools that are not
///    registered (e.g. a hallucinated tool name) are conservatively
///    classified as mutating.
///
/// Mutating tool calls (WriteFile, shell, apply_patch, multi_edit,
/// edit_file, etc.) ALWAYS make the turn non-programmatic regardless
/// of text content — even a turn that says "I'll write this now" and
/// then calls `write_file` is non-programmatic, because the mutation
/// is a user-visible side effect.
pub fn classify_programmatic(response: &LlmResponse, tools: &ToolRegistry) -> bool {
    if response.tool_calls.is_empty() {
        return false;
    }
    if !response.text.trim().is_empty() {
        return false;
    }
    is_every_tool_read_only(&response.tool_calls, tools)
}

fn is_every_tool_read_only(calls: &[ToolCall], tools: &ToolRegistry) -> bool {
    calls.iter().all(|call| {
        tools
            .get(&call.name)
            .map(|tool| tool.is_read_only())
            .unwrap_or(false)
    })
}

/// Outcome of the graceful exhaustion summary attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GracefulSummaryOutcome {
    /// The summary turn produced user-visible text after tool
    /// stripping. The run ends successfully with this text.
    Completed(String),
    /// The summary turn failed (LLM error) or produced no text. The
    /// engine should fall through to `RunFailed`.
    Failed { reason: &'static str },
}

/// Attempt the graceful exhaustion summary turn.
///
/// The model is told (via a user-turn nudge) that it is out of turns
/// and must respond with text only. Any tool calls the model still
/// emits are stripped (same pattern as
/// `strip_safety_terminated_tool_calls`). If the stripped response
/// has non-empty text, the run completes successfully with that
/// text. If the LLM errors or the stripped response is empty, the
/// caller should fall through to `RunFailed` — the abrupt-failure
/// regression path is preserved.
///
/// This function is the genuinely-reachable code path the FEATURE-
/// MATRIX row calls for. It is wired into the real turn loop (see
/// `engine_loop.rs`) and exercised by the graceful-exhaustion tests
/// in `iteration_budget_tests` below.
pub async fn attempt_graceful_summary(
    transcript: &mut Transcript,
    llm: &Arc<dyn LlmClient>,
) -> GracefulSummaryOutcome {
    transcript.push_user(MAX_ITERATION_SUMMARY_PROMPT);
    let mut response = match llm.complete(transcript.messages()).await {
        Ok(r) => r,
        Err(_) => {
            return GracefulSummaryOutcome::Failed {
                reason: "llm_error",
            }
        }
    };
    // Strip any tool calls the model still emitted despite the
    // nudge. The summary turn is a tools-stripped turn — the model
    // was told to respond with text only, and any tool calls it
    // still produced are dropped so the run ends with a clean
    // text-only assistant message.
    response.tool_calls.clear();
    let text = response.text.trim().to_string();
    if text.is_empty() {
        return GracefulSummaryOutcome::Failed {
            reason: "empty_summary",
        };
    }
    GracefulSummaryOutcome::Completed(text)
}

#[cfg(test)]
mod iteration_budget_tests {
    use super::*;
    use crate::llm::{LlmClient, LlmResponse, ScriptedLlm};
    use rightkit_agent_engine_protocol::TranscriptMessage;
    include!("iteration_budget_tests.rs");
}
