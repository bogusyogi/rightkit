//! Pending required-verification state for one user turn.
//!
//! A required verifier that produced no verdict (Unavailable) or failed
//! must not silently let the turn finish as if verified — but the turn also
//! must not force-stop while the agent can still do useful diagnostic work.
//! [`PendingVerification`] is the bounded middle: the dispatch gate arms it
//! with the receipt-bound mutated results whenever a required gap exists,
//! and the completion path re-runs the same fan-out over the latest on-disk
//! bytes at most `MAX_RECHECKS` times before the loop-safety gate stops the
//! turn without a verified-clean claim.
//!
//! The state is turn-scoped (the engine loop constructs a fresh one per user
//! turn, like `PostTurnGateState`), so it resets correctly per session/turn
//! by construction; receipt binding re-runs through `ReceiptBinding` on the
//! recheck, and staleness stays governed by the content-hash leg.

use std::collections::HashMap;

use rightkit_agent_tools::coding_tools::ToolResult;

/// Accumulated required-verification debt for the current turn.
#[derive(Debug, Default)]
pub struct PendingVerification {
    /// Mutated tool results that still owe a required verdict. Kept as the
    /// original receipt-bound `ToolResult`s so the completion recheck feeds
    /// them through `run_verify_fanout` unchanged — the same units, budgets,
    /// binding, and staleness rules apply to the latest bytes.
    results: HashMap<String, ToolResult>,
    /// Human-readable gap lines from the most recent fan-out, reused when
    /// the recheck bound is spent and the gate must report what stayed
    /// unverified.
    gaps: Vec<String>,
    /// Completion-time rechecks already spent this turn.
    rechecks: usize,
}

impl PendingVerification {
    /// Bounded completion rechecks per turn. After the cap, the loop-safety
    /// gate ends the turn with an explicit unverified warning instead of
    /// ping-ponging completion attempts forever.
    pub const MAX_RECHECKS: usize = 2;

    pub fn new() -> Self {
        Self::default()
    }

    /// True while any mutation still owes a required verification verdict.
    pub fn is_pending(&self) -> bool {
        !self.results.is_empty()
    }

    /// Fold a dispatch-time fan-out gap into the pending set. Mutation-
    /// bearing results accumulate (call ids are unique per turn); `gaps`
    /// is replaced with the newest report's lines so the eventual stop
    /// message describes current, not historical, debt.
    pub fn arm(&mut self, turn_results: &HashMap<String, ToolResult>, gaps: Vec<String>) {
        for (call_id, result) in turn_results {
            if !result.mutations.is_empty() {
                self.results.insert(call_id.clone(), result.clone());
            }
        }
        self.gaps = gaps;
    }

    /// The accumulated results to recheck at completion.
    pub fn results(&self) -> &HashMap<String, ToolResult> {
        &self.results
    }

    /// The most recent human-readable gap description.
    pub fn gaps(&self) -> &[String] {
        &self.gaps
    }

    pub(crate) fn replace_gaps(&mut self, gaps: Vec<String>) {
        self.gaps = gaps;
    }

    pub fn rechecks(&self) -> usize {
        self.rechecks
    }

    /// Spend one completion recheck. Returns `false` once the bound is
    /// exhausted — the caller must then end the turn honestly rather than
    /// rechecking again.
    pub fn try_recheck(&mut self) -> bool {
        if self.rechecks >= Self::MAX_RECHECKS {
            return false;
        }
        self.rechecks += 1;
        true
    }

    /// Drop all pending debt (a recheck passed, or the turn reset).
    pub fn clear(&mut self) {
        self.results.clear();
        self.gaps.clear();
        self.rechecks = 0;
    }
}
