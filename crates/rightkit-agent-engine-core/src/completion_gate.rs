//! Completion-evidence vocabulary — the data the unbacked-victory gate decides on.
//!
//! FM-079 lane. This module holds the *evidence and outcome types* for the
//! completion gate: [`CompletionEvidence`] (what a turn produced and whether
//! it claims victory), [`GateOutcome`] (the three-way decision), and
//! [`VerificationIssue`] (the typed finding raised when a claim is unbacked).
//!
//! ## The gate mechanism itself is NOT here any more
//!
//! This module used to define its own `UnbackedWorkGate`. It was removed in
//! Refactor 0.5 Dispatch A (item 3) because it duplicated — in name only — the
//! live, itemized gate in `host_orchestration::unbacked_work_gate`, which
//! is already wired into production via `run_governance::GovernedArbiter`. The
//! removed copy had zero production callers. See the reconciliation comment
//! further down this file for the full reasoning and the exact live call sites.
//!
//! The decision procedure the types below encode is therefore preserved as
//! specification, not as an implementation in this module:
//! 1. A victory claim with ≥1 typed mutation is `Accepted` and resets the
//!    reenter counter (the work is proven to have landed).
//! 2. A victory claim with 0 typed mutations is `Reenter` (up to a cap) with a
//!    typed `VerificationIssue`.
//! 3. Once the reenter cap is reached, the outcome is `Unbacked`. The cap
//!    exists to prevent an infinite loop, NOT to silently accept unbacked work.
//! 4. A turn that does not signal victory is never reentered;
//!    `claims_victory` is deliberately conservative (false negatives are
//!    preferred — we err on the side of asking for proof).

/// A typed finding payload — the gate refuses to let an unbacked claim reach
/// "completed" without raising one. Mirrors `rightkit_agent_engine_protocol::VerificationIssue`;
/// the seam here keeps the gate independent so the A2 mutation contract is the
/// only consumer of these findings, and so the engine can construct one before
/// protocol wiring exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationIssue {
    pub tool: String,
    pub file_path: String,
    pub line: Option<i32>,
    pub column: Option<i32>,
    pub message: String,
    pub repair_suggestion: String,
}

/// The completion-gate decision for one victory claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    /// The completion claim is backed by typed mutations.
    Accepted,
    /// The completion claim is unbacked; re-enter once with the typed finding.
    /// `counter` is the 1-indexed reenter count.
    Reenter {
        finding: VerificationIssue,
        counter: usize,
    },
    /// The completion claim is unbacked and the reenter cap has been reached;
    /// the engine proceeds (the cap exists to prevent an infinite loop, not to
    /// silently accept unbacked work).
    Unbacked { attempts: usize },
}

/// Pure data a completion gate decides on. Constructed by the engine from the
/// A2 mutation set carried on `ToolResult` and the model's visible completion
/// claim.
///
/// The gate that consumes this shape lives in
/// `host_orchestration::unbacked_work_gate` (itemized, per-task); this
/// type is retained as the engine-side evidence vocabulary. See the module
/// docs and the reconciliation comment below.
#[derive(Debug, Default)]
pub struct CompletionEvidence<'a> {
    pub typed_mutations: u32,
    pub claim_text: Option<&'a str>,
    pub last_tool_name: Option<&'a str>,
}

impl<'a> CompletionEvidence<'a> {
    pub fn has_any_typed_mutation(&self) -> bool {
        self.typed_mutations > 0
    }

    pub fn claims_victory(&self) -> bool {
        matches!(self.claim_text, Some(s) if Self::text_signals_done(s))
    }

    pub fn last_tool_name(&self) -> Option<&'a str> {
        self.last_tool_name
    }

    /// Conservative heuristic; the agent is claiming victory. False negatives
    /// are allowed (we err on the side of asking for proof).
    fn text_signals_done(text: &str) -> bool {
        let lower = text.to_ascii_lowercase();
        [
            "all done",
            "i'm done",
            "i am done",
            "finished",
            "completed the task",
            "task is complete",
            "work is complete",
            "all set",
            "ready for review",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
    }
}

// Lane FM-079 unbacked-work verification gate. Runs BEFORE the engine emits an
// `AssistantMessageCompleted` claim of victory ("I've finished…"). The gate
// uses the typed A2 mutation set carried on the tool's `ToolResult` to confirm
// the assistant actually performed the work it claims; otherwise the gate
// forces a reenter with a typed finding.
//
// This struct is intentionally separate from `PostTurnGateState` so the
// concurrency, persistence, and reschedule knobs can evolve independently.
// ---------------------------------------------------------------------------
// Duplicate-type reconciliation (Refactor 0.5 Dispatch A, item 3)
//
// This module previously defined its OWN `UnbackedWorkGate`, structurally
// different from the live one in `host_orchestration::unbacked_work_gate`:
//
//   - this copy:  per-turn, single aggregate counter, `check(evidence) ->
//                 GateOutcome`, constructed via `new(max_force_reenter)`;
//   - the live one: per-task/itemized, `check(run_id, &[TaskEvidence]) ->
//                 Vec<PerTaskVerdict>`, keyed on stable task/job identity.
//
// Decision: KEEP THE LIVE ONE. `orchestration::unbacked_work_gate` is already
// wired into production — `run_governance::GovernedArbiter` holds it
// (`engine/crates/orchestration/src/run_governance.rs:188`, constructed at
// :200, consulted at :252 and :262) and is itself constructed into live
// session state at `engine/host-app/src/api_state_impl_04.rs:943` and
// driven from `engine/host-app/src/orchestrator/run_conductor/finalize_goal.rs:113-166`.
// This copy had zero production callers — only its own unit tests.
//
// The two are NOT drop-in replacements (different evidence types, different
// return shapes), so this is a delete-the-stale-copy decision, not a merge:
// the itemized per-task gate supersedes the aggregate per-turn one, and the
// `host_engine` crate cannot depend on `host_orchestration` to
// re-export it. This duplicate has therefore been REMOVED rather than left to
// drift. Do not reintroduce a gate here.
//
// `CompletionEvidence`, `GateOutcome` and `VerificationIssue` below are
// retained as the completion-boundary vocabulary; note that the live session
// state carries a *separate, distinct* `CompletionEvidence` at
// `engine/host-app/src/api_state_types.rs:207` (held as
// `Arc<Mutex<CompletionEvidence>>` at :196). Reconciling those two same-named
// types is an ownership decision for a later pass — see the dispatch wiring
// notes — and is deliberately NOT attempted here.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn victory_heuristic_detects_common_phrases_without_forgiving_genuine_doubt() {
        for positive in [
            "All done!",
            "Task is complete.",
            "I am done with the edits.",
            "ready for review.",
        ] {
            assert!(
                CompletionEvidence {
                    typed_mutations: 0,
                    claim_text: Some(positive),
                    last_tool_name: Some("read_file"),
                }
                .claims_victory(),
                "expected {positive:?} to be a claim"
            );
        }
        for negative in ["thinking out loud…", "I will retry once more.", ""] {
            assert!(
                !CompletionEvidence {
                    typed_mutations: 0,
                    claim_text: Some(negative),
                    last_tool_name: Some("read_file"),
                }
                .claims_victory(),
                "expected {negative:?} NOT to be a claim"
            );
        }
    }
}
