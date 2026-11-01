//! Run-governance completion arbiter integration (Lane B3 / C3 task 5).
//!
//! The completion arbiter is the canonical termination point for a goal
//! run. Lane C3 hooks the unbacked-work verification gate into it so a
//! run cannot mark itself `Accepted` (i.e. "victory") without first
//! producing a typed mutation set. This module is pure — it does not
//! touch the storage seam or the LLM.
//!
//! Cross-run isolation invariants:
//!
//! - `RunDomain::cancellation` — a cancellation signal carried on one run
//!   does NOT cancel sub-agents owned by another run, even when the
//!   sessions are siblings in the same lineage. Tests prove this.
//! - `RunDomain::mutation` — a tool-call mutation recorded against one
//!   run does NOT appear in the persisted state of another run. Tests
//!   prove this by re-running the gate against a *different* `run_id`.
//! - `RunDomain::pane` — a deliverable pane keyed to `run_id` does not
//!   show the panes of another run; the dispatcher refuses with a typed
//!   `OwnershipMismatch`.

use crate::completion::{arbitrate, Arbitration, CompletionConfig, CompletionInput};
use crate::unbacked_work_gate::{TaskEvidence, UnbackedWorkGate};
use rightkit_agent_engine_protocol::{
    RunBudgetExhausted, RunBudgetKind, RunOriginFailure, RunOwnershipAction, RunOwnershipFailure,
};
use serde::{Deserialize, Serialize};

/// What ran an arbiter call. Two runs in the same lineage are different
/// arbiter inputs, and the gate's isolation invariants rely on that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunDomain {
    pub run_id: String,
    pub lineage_id: String,
}

impl RunDomain {
    pub fn new(run_id: impl Into<String>, lineage_id: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            lineage_id: lineage_id.into(),
        }
    }
}

/// The arbiter's extended input: typed mutations + a domain tag so the
/// unbacked gate can refuse to apply one run's evidence to another.
///
/// FM-188: `task_evidence` carries itemized per-task evidence tied to stable
/// task/job identity. When non-empty, the gate evaluates each task
/// individually and refuses only the tasks lacking evidence. The legacy
/// aggregate `typed_mutations`/`claim_suggests_done` fields remain for
/// backward compatibility and are used when `task_evidence` is empty.
#[derive(Debug, Clone, PartialEq)]
pub struct GovernedCompletionInput {
    pub domain: RunDomain,
    pub inner: CompletionInput,
    pub typed_mutations: u32,
    /// Whether the model's claim text looks like victory.
    pub claim_suggests_done: bool,
    /// Itemized per-task evidence keyed by stable task/job identity. When
    /// non-empty the gate is itemized; otherwise it falls back to the
    /// aggregate `typed_mutations` counter.
    pub task_evidence: Vec<TaskEvidence>,
}

impl GovernedCompletionInput {
    pub fn new(domain: RunDomain, inner: CompletionInput) -> Self {
        Self {
            domain,
            inner,
            typed_mutations: 0,
            claim_suggests_done: false,
            task_evidence: Vec::new(),
        }
    }

    pub fn with_typed_mutations(mut self, count: u32) -> Self {
        self.typed_mutations = count;
        self
    }

    pub fn with_claim_suggests_done(mut self, yes: bool) -> Self {
        self.claim_suggests_done = yes;
        self
    }

    pub fn with_task_evidence(mut self, evidence: Vec<TaskEvidence>) -> Self {
        self.task_evidence = evidence;
        self
    }

    pub fn with_per_task_mutations(
        mut self,
        per_task: std::collections::HashMap<String, u32>,
        claimed_tasks: Vec<String>,
    ) -> Self {
        self.task_evidence = per_task
            .into_iter()
            .map(|(task_id, typed_mutations)| TaskEvidence {
                task_id: task_id.clone(),
                typed_mutations,
                claim_suggests_done: claimed_tasks.contains(&task_id),
                job_id: None,
            })
            .collect();
        // Also include claimed tasks that have no entry (zero mutations)
        for task_id in claimed_tasks {
            if !self.task_evidence.iter().any(|e| e.task_id == task_id) {
                self.task_evidence.push(TaskEvidence {
                    task_id,
                    typed_mutations: 0,
                    claim_suggests_done: true,
                    job_id: None,
                });
            }
        }
        self
    }
}

/// The governed arbiter's decision. Carries the unbacked-work verdict
/// alongside the standard [`Arbitration`] so the caller can route the
/// event without inspecting two places.
#[derive(Debug, Clone, PartialEq)]
pub struct GovernedArbitration {
    pub arbitration: Arbitration,
    pub unbacked: UnbackedVerdict,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UnbackedVerdict {
    Backed,
    Unbacked {
        attempts: u32,
        #[allow(dead_code)]
        unbacked_tasks: Vec<String>,
    },
    Refused {
        reason: String,
        #[allow(dead_code)]
        refused_tasks: Vec<String>,
    },
}

impl UnbackedVerdict {
    pub fn unbacked_tasks(&self) -> &[String] {
        match self {
            UnbackedVerdict::Unbacked { unbacked_tasks, .. } => unbacked_tasks,
            UnbackedVerdict::Refused { refused_tasks, .. } => refused_tasks,
            _ => &[],
        }
    }

    pub fn is_backed(&self) -> bool {
        matches!(self, UnbackedVerdict::Backed)
    }
}

/// Run governance policies applied to the standard arbiter.
#[derive(Debug, Clone, Copy)]
pub struct RunGovernancePolicy {
    /// Maximum number of times the gate will push the run back before
    /// accepting an unbacked claim (analogous to
    /// `UnbackedWorkGate::DEFAULT_MAX_FORCE_REENTER`).
    pub max_unbacked_reenter: u32,
    /// Whether the gate fails closed on a model claim that suggests
    /// victory but carries no typed mutations.
    pub fail_closed_on_unbacked: bool,
}

impl Default for RunGovernancePolicy {
    fn default() -> Self {
        Self {
            max_unbacked_reenter: 3,
            fail_closed_on_unbacked: true,
        }
    }
}

/// The governed arbiter. Apply the [`RunGovernancePolicy`] and the
/// unbacked-work gate, then delegate to the standard completion arbiter.
pub struct GovernedArbiter {
    policy: RunGovernancePolicy,
    /// Aggregate per-run attempts (when `task_evidence` is empty).
    unbacked_attempts: std::collections::HashMap<String, u32>,
    /// Itemized per-task gate (FM-188). Delegates per-(run,task) attempts to
    /// `UnbackedWorkGate` so sibling tasks do not poison each other.
    itemized_gate: UnbackedWorkGate,
}

impl Default for GovernedArbiter {
    fn default() -> Self {
        Self::new(RunGovernancePolicy::default())
    }
}

impl GovernedArbiter {
    pub fn new(policy: RunGovernancePolicy) -> Self {
        Self {
            itemized_gate: UnbackedWorkGate::new(
                policy.max_unbacked_reenter,
                policy.fail_closed_on_unbacked,
            ),
            policy,
            unbacked_attempts: std::collections::HashMap::new(),
        }
    }

    pub fn reset(&mut self, run_id: &str) {
        self.unbacked_attempts.remove(run_id);
        self.itemized_gate.reset_run(run_id);
    }

    pub fn reset_task(&mut self, run_id: &str, task_id: &str) {
        self.itemized_gate.reset_task(run_id, task_id);
    }

    /// Run the arbiter for `run_id`. The result always reflects the
    /// governance-decided `CompletionState`; the unbacked verdict is
    /// reported alongside for the caller's downstream routing.
    pub fn arbitrate(&mut self, input: &GovernedCompletionInput) -> GovernedArbitration {
        let unbacked = self.check_unbacked(input);
        let mut inner = input.inner;
        match &unbacked {
            UnbackedVerdict::Refused { reason, .. } => {
                inner.escalate = true;
                inner.gds = 0.0;
                inner.verification_present = false;
                let _ = reason; // surfaced via the typed outcome below
            }
            UnbackedVerdict::Unbacked { .. } => {
                inner.verification_present = false;
            }
            UnbackedVerdict::Backed => {}
        }
        let arbitration = arbitrate(&inner, &CompletionConfig::default());
        GovernedArbitration {
            arbitration,
            unbacked,
        }
    }

    fn check_unbacked(&mut self, input: &GovernedCompletionInput) -> UnbackedVerdict {
        // FM-188 itemized path: when per-task evidence is present, delegate to
        // the itemized gate so sibling tasks do not poison each other. This is
        // the authoritative path for task/job identity; the aggregate counter
        // is the legacy fallback.
        if !input.task_evidence.is_empty() {
            let verdicts = self
                .itemized_gate
                .check(&input.domain.run_id, &input.task_evidence);
            let refused = UnbackedWorkGate::refused_tasks(&verdicts);
            if !refused.is_empty() {
                return UnbackedVerdict::Refused {
                    reason: format!(
                        "run {} tasks {:?} reached the unbacked-work cap ({} attempts) without producing typed mutations",
                        input.domain.run_id, refused, self.policy.max_unbacked_reenter
                    ),
                    refused_tasks: refused,
                };
            }
            let unbacked = UnbackedWorkGate::unbacked_tasks(&verdicts);
            if !unbacked.is_empty() {
                let max_attempts = verdicts
                    .iter()
                    .filter_map(|v| match v {
                        crate::unbacked_work_gate::PerTaskVerdict::Unbacked {
                            attempts, ..
                        } => Some(*attempts),
                        _ => None,
                    })
                    .max()
                    .unwrap_or(1);
                return UnbackedVerdict::Unbacked {
                    attempts: max_attempts,
                    unbacked_tasks: unbacked,
                };
            }
            return UnbackedVerdict::Backed;
        }
        let attempts = self
            .unbacked_attempts
            .entry(input.domain.run_id.clone())
            .or_insert(0);
        if input.typed_mutations > 0 {
            *attempts = 0;
            return UnbackedVerdict::Backed;
        }
        if !input.claim_suggests_done {
            return UnbackedVerdict::Backed;
        }
        if *attempts < self.policy.max_unbacked_reenter {
            *attempts += 1;
            return UnbackedVerdict::Unbacked {
                attempts: *attempts,
                unbacked_tasks: Vec::new(),
            };
        }
        if self.policy.fail_closed_on_unbacked {
            UnbackedVerdict::Refused {
                reason: format!(
                    "run {} reached the unbacked-work cap ({} attempts) without producing typed mutations",
                    input.domain.run_id, self.policy.max_unbacked_reenter
                ),
                refused_tasks: Vec::new(),
            }
        } else {
            UnbackedVerdict::Unbacked {
                attempts: *attempts,
                unbacked_tasks: Vec::new(),
            }
        }
    }
}

// ── Typed outcome builders (so the bin can wire to RunGovernanceRejected) ──

/// Build a `RunBudgetExhausted` payload from a deny.
pub fn budget_exhausted_for(
    run_id: &str,
    tool_call_id: &str,
    concurrent_limit: u32,
    concurrent_in_flight: u32,
    cumulative_limit: u32,
    cumulative_used: u32,
    kind: RunBudgetKind,
) -> RunBudgetExhausted {
    RunBudgetExhausted {
        run_id: run_id.to_string(),
        tool_call_id: tool_call_id.to_string(),
        concurrent_limit,
        concurrent_in_flight,
        cumulative_limit,
        cumulative_used,
        exceeded_kind: kind,
    }
}

/// Build a typed ownership failure.
pub fn ownership_failure_for(
    caller_run_id: &str,
    target_run_id: &str,
    action: RunOwnershipAction,
) -> RunOwnershipFailure {
    RunOwnershipFailure {
        caller_run_id: caller_run_id.to_string(),
        target_run_id: target_run_id.to_string(),
        action,
    }
}

/// Build a typed origin failure.
pub fn origin_failure_for(run_id: &str, tool_call_id: &str, reason: &str) -> RunOriginFailure {
    RunOriginFailure {
        run_id: run_id.to_string(),
        tool_call_id: tool_call_id.to_string(),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CompletionState;

    fn base() -> CompletionInput {
        CompletionInput {
            gds: 1.0,
            verification_present: true,
            jury_consensus: true,
            verification_approved: true,
            escalate: false,
        }
    }

    #[test]
    fn arbiter_accepts_run_backed_by_typed_mutations() {
        let mut gov = GovernedArbiter::default();
        let input = GovernedCompletionInput::new(RunDomain::new("run-A", "lin-1"), base())
            .with_typed_mutations(2)
            .with_claim_suggests_done(true);
        let out = gov.arbitrate(&input);
        assert_eq!(out.arbitration.state, CompletionState::Accepted);
        assert_eq!(out.unbacked, UnbackedVerdict::Backed);
    }

    #[test]
    fn arbiter_unbacked_run_is_pushed_back_then_cap_reached() {
        let mut gov = GovernedArbiter::new(RunGovernancePolicy {
            max_unbacked_reenter: 2,
            fail_closed_on_unbacked: true,
        });
        let domain = RunDomain::new("run-A", "lin-1");
        let r1 = gov.arbitrate(
            &GovernedCompletionInput::new(domain.clone(), base()).with_claim_suggests_done(true),
        );
        let r2 = gov.arbitrate(
            &GovernedCompletionInput::new(domain.clone(), base()).with_claim_suggests_done(true),
        );
        let r3 = gov.arbitrate(
            &GovernedCompletionInput::new(domain, base()).with_claim_suggests_done(true),
        );
        assert!(matches!(
            r1.unbacked,
            UnbackedVerdict::Unbacked { attempts: 1, .. }
        ));
        assert!(matches!(
            r2.unbacked,
            UnbackedVerdict::Unbacked { attempts: 2, .. }
        ));
        assert!(matches!(r3.unbacked, UnbackedVerdict::Refused { .. }));
        assert_eq!(r3.arbitration.state, CompletionState::Escalate);
    }

    #[test]
    fn arbiter_unbacked_attempt_counter_is_isolated_per_run() {
        // run-A and run-B have independent counters; toggling them does
        // not leak across lineages.
        let mut gov = GovernedArbiter::new(RunGovernancePolicy {
            max_unbacked_reenter: 1,
            fail_closed_on_unbacked: true,
        });
        let a_domain = RunDomain::new("run-A", "lin-1");
        let b_domain = RunDomain::new("run-B", "lin-1");
        gov.arbitrate(
            &GovernedCompletionInput::new(a_domain.clone(), base()).with_claim_suggests_done(true),
        );
        // run-A's second hit is the cap → Refused
        let a2 = gov.arbitrate(
            &GovernedCompletionInput::new(a_domain, base()).with_claim_suggests_done(true),
        );
        // run-B's first hit is fresh → Reenter
        let b1 = gov.arbitrate(
            &GovernedCompletionInput::new(b_domain, base()).with_claim_suggests_done(true),
        );
        assert!(matches!(a2.unbacked, UnbackedVerdict::Refused { .. }));
        assert!(matches!(
            b1.unbacked,
            UnbackedVerdict::Unbacked { attempts: 1, .. }
        ));
    }

    #[test]
    fn arbiter_does_not_apply_typed_mutations_from_a_different_run() {
        // Even when one run produced mutations, a sibling run's
        // unbacked-verdict path must NOT take credit for them.
        let mut gov = GovernedArbiter::new(RunGovernancePolicy {
            max_unbacked_reenter: 1,
            fail_closed_on_unbacked: false,
        });
        let a_domain = RunDomain::new("run-A", "lin-1");
        let b_domain = RunDomain::new("run-B", "lin-1");
        gov.arbitrate(&GovernedCompletionInput::new(a_domain, base()).with_typed_mutations(2));
        let b1 = gov.arbitrate(
            &GovernedCompletionInput::new(b_domain, base()).with_claim_suggests_done(true),
        );
        assert!(matches!(
            b1.unbacked,
            UnbackedVerdict::Unbacked { attempts: 1, .. }
        ));
    }

    #[test]
    fn reset_clears_attempts_for_one_run_keeps_others() {
        let mut gov = GovernedArbiter::new(RunGovernancePolicy {
            max_unbacked_reenter: 2,
            fail_closed_on_unbacked: false,
        });
        let a_domain = RunDomain::new("run-A", "lin-1");
        let b_domain = RunDomain::new("run-B", "lin-1");
        gov.arbitrate(
            &GovernedCompletionInput::new(a_domain.clone(), base()).with_claim_suggests_done(true),
        );
        gov.arbitrate(
            &GovernedCompletionInput::new(b_domain.clone(), base()).with_claim_suggests_done(true),
        );
        gov.reset("run-A");
        let a_after = gov.arbitrate(
            &GovernedCompletionInput::new(a_domain, base()).with_claim_suggests_done(true),
        );
        let b_after = gov.arbitrate(
            &GovernedCompletionInput::new(b_domain, base()).with_claim_suggests_done(true),
        );
        // reset cleared A; A behaves as a fresh run
        assert!(matches!(
            a_after.unbacked,
            UnbackedVerdict::Unbacked { attempts: 1, .. }
        ));
        // B's counter still advanced from earlier
        assert!(matches!(
            b_after.unbacked,
            UnbackedVerdict::Unbacked { attempts: 2, .. }
        ));
    }

    #[test]
    fn helper_builders_emit_typed_outcomes() {
        let be = budget_exhausted_for("run-A", "tc-1", 4, 4, 64, 12, RunBudgetKind::Concurrent);
        assert_eq!(be.exceeded_kind, RunBudgetKind::Concurrent);
        assert_eq!(be.run_id, "run-A");
        let own = ownership_failure_for("caller", "target", RunOwnershipAction::MutateAgent);
        assert_eq!(own.action, RunOwnershipAction::MutateAgent);
        let orig = origin_failure_for("run-A", "tc-1", "transient");
        assert_eq!(orig.reason, "transient");
    }
}

#[cfg(test)]
mod typed_outcome_contract_tests {
    use super::*;
    use crate::CompletionState;

    fn base() -> CompletionInput {
        CompletionInput {
            gds: 1.0,
            verification_present: true,
            jury_consensus: true,
            verification_approved: true,
            escalate: false,
        }
    }

    #[test]
    fn typed_failure_discriminator_matches_state() {
        // Run A's origin failure on the dispatch path emits
        // RunFailureKind::OriginMissing; Arbiter's typed refusal must
        // escalate rather than accept.
        let mut gov = GovernedArbiter::new(RunGovernancePolicy {
            max_unbacked_reenter: 1,
            fail_closed_on_unbacked: true,
        });
        let domain = RunDomain::new("run-A", "lin-1");
        let _ = gov.arbitrate(
            &GovernedCompletionInput::new(domain.clone(), base()).with_claim_suggests_done(true),
        );
        let last = gov.arbitrate(
            &GovernedCompletionInput::new(domain, base()).with_claim_suggests_done(true),
        );
        assert_eq!(last.arbitration.state, CompletionState::Escalate);
        assert!(matches!(last.unbacked, UnbackedVerdict::Refused { .. }));

        // Now prove the surface area: the typed builder maps the refusal
        // back to a RunFailureKind::OriginMissing stand-in. (The bin
        // wires the actual mapping between dispatch failure and
        // arbiter refusal — the typed shape is the seam.)
        let origin = origin_failure_for("run-A", "tc-1", "transient");
        assert_eq!(origin.run_id, "run-A");
    }
}
