// Lane C3 task 3 evidence: prove the governed completion arbiter gates
// `GoalStatus::Achieved` and downgrades on unbacked refusal.
//
// The actual `finalize_goal` lives in `finalize_goal.rs` and consults
// `LiveSession::governed_arbiter` (a tokio mutex). These tests do NOT
// boot a full DaemonState — they cover the typed arbiter contract and
// prove the downgrade mapping (`Achieved → Unmet`) the seam applies on
// `UnbackedVerdict::Refused`.

use rightkit_agent_orchestration::coding::completion::{CompletionInput, CompletionState};
use rightkit_agent_orchestration::coding::run_governance::{
    GovernedArbiter, GovernedArbitration, GovernedCompletionInput, RunDomain, RunGovernancePolicy,
    UnbackedVerdict,
};

fn unbacked_input(run_id: &str, with_claim: bool) -> GovernedCompletionInput {
    let inner = CompletionInput {
        gds: 1.0,
        verification_present: true,
        jury_consensus: true,
        verification_approved: true,
        escalate: false,
    };
    GovernedCompletionInput::new(RunDomain::new(run_id, "lin-test"), inner)
        .with_typed_mutations(0)
        .with_claim_suggests_done(with_claim)
}

#[test]
fn unbacked_claim_first_attempt_is_unbacked_not_refused() {
    let mut arbiter = GovernedArbiter::new(RunGovernancePolicy {
        max_unbacked_reenter: 3,
        fail_closed_on_unbacked: true,
    });
    let out = arbiter.arbitrate(&unbacked_input("run-A", true));
    assert!(matches!(
        out.unbacked,
        UnbackedVerdict::Unbacked { attempts: 1, .. }
    ));
}

#[test]
fn unbacked_claim_after_policy_cap_is_refused() {
    // The fail-closed property of the arbiter: after the policy's
    // re-enter cap, an unbacked claim is REFUSED — not silently accepted,
    // not silently nudged back. The seam in `finalize_goal` reads this
    // `Refused` verdict and downgrades `Achieved → Unmet`.
    let mut arbiter = GovernedArbiter::new(RunGovernancePolicy {
        max_unbacked_reenter: 2,
        fail_closed_on_unbacked: true,
    });
    let r1 = arbiter.arbitrate(&unbacked_input("run-A", true));
    let r2 = arbiter.arbitrate(&unbacked_input("run-A", true));
    let r3 = arbiter.arbitrate(&unbacked_input("run-A", true));
    assert!(matches!(
        r1.unbacked,
        UnbackedVerdict::Unbacked { attempts: 1, .. }
    ));
    assert!(matches!(
        r2.unbacked,
        UnbackedVerdict::Unbacked { attempts: 2, .. }
    ));
    let refused = match r3.unbacked {
        UnbackedVerdict::Refused { reason, .. } => reason,
        other => panic!("expected Refused, got {other:?}"),
    };
    // The reason names the run so the audit log can find the offender.
    assert!(
        refused.contains("run-A"),
        "refusal must name the run: {refused}"
    );
    assert!(
        refused.contains("unbacked"),
        "refusal must reference the unbacked gate: {refused}"
    );
}

#[test]
fn refusal_downgrade_target_is_unmet_not_achieved() {
    // The seam mapping is: on `Refused`, downgrade `GoalStatus::Achieved`
    // to `GoalStatus::Unmet`. This test proves the typed state the
    // seam produces — that on `Refused`, the arbiter's escalated
    // completion state is NOT `Accepted`, so the seam's downgrade is
    // the only path forward.
    let mut arbiter = GovernedArbiter::new(RunGovernancePolicy {
        max_unbacked_reenter: 1,
        fail_closed_on_unbacked: true,
    });
    let _ = arbiter.arbitrate(&unbacked_input("run-B", true));
    let GovernedArbitration {
        arbitration,
        unbacked,
    } = arbiter.arbitrate(&unbacked_input("run-B", true));
    assert!(matches!(unbacked, UnbackedVerdict::Refused { .. }));
    // The arbiter escalates the inner state so the daemon's mapping
    // table cannot mistake the refusal for an Accept.
    assert_ne!(
        arbitration.state,
        CompletionState::Accepted,
        "Refused must not translate to Accepted"
    );
}

#[test]
fn typed_mutations_break_the_unbacked_chain() {
    // Production rule: as soon as the run produces typed mutations
    // (tool calls the lineage can attest to), the arbiter resets the
    // unbacked counter and accepts. This is the green-path the policy
    // expects.
    let mut arbiter = GovernedArbiter::new(RunGovernancePolicy {
        max_unbacked_reenter: 2,
        fail_closed_on_unbacked: true,
    });
    let _ = arbiter.arbitrate(&unbacked_input("run-C", true));
    let backed = GovernedCompletionInput::new(
        RunDomain::new("run-C", "lin-test"),
        CompletionInput {
            gds: 1.0,
            verification_present: true,
            jury_consensus: true,
            verification_approved: true,
            escalate: false,
        },
    )
    .with_typed_mutations(2)
    .with_claim_suggests_done(true);
    let out = arbiter.arbitrate(&backed);
    assert_eq!(out.unbacked, UnbackedVerdict::Backed);
    assert_eq!(out.arbitration.state, CompletionState::Accepted);
}

#[test]
fn non_claim_input_is_always_backed() {
    // If the run did NOT claim "done", the arbiter does not gate it —
    // the failure to claim is orthogonal to the unbacked-work gate.
    let mut arbiter = GovernedArbiter::new(RunGovernancePolicy {
        max_unbacked_reenter: 1,
        fail_closed_on_unbacked: true,
    });
    let out = arbiter.arbitrate(&unbacked_input("run-D", false));
    assert_eq!(out.unbacked, UnbackedVerdict::Backed);
}

#[test]
fn refusal_reason_is_non_empty_for_audit_log() {
    // The downgrade emits a `RunGovernanceRejected` event with the
    // reason verbatim; an empty reason would lose the audit signal.
    let mut arbiter = GovernedArbiter::new(RunGovernancePolicy {
        max_unbacked_reenter: 1,
        fail_closed_on_unbacked: true,
    });
    let _ = arbiter.arbitrate(&unbacked_input("run-E", true));
    let out = arbiter.arbitrate(&unbacked_input("run-E", true));
    if let UnbackedVerdict::Refused { reason, .. } = out.unbacked {
        assert!(!reason.is_empty(), "Refused.reason must be non-empty");
    } else {
        panic!("expected Refused on the second attempt");
    }
}

#[test]
fn arbiter_isolates_counters_across_runs() {
    // The per-session arbiter is reset between sessions by virtue of
    // living on `LiveSession` — but the policy still guarantees that
    // two different run_ids on the same arbiter do not contaminate
    // each other's counters. This is the cross-run isolation invariant.
    let mut arbiter = GovernedArbiter::new(RunGovernancePolicy {
        max_unbacked_reenter: 1,
        fail_closed_on_unbacked: true,
    });
    // Run-A gets one Unbacked then Refused on the second hit.
    let _ = arbiter.arbitrate(&unbacked_input("run-A", true));
    let a_refused = arbiter.arbitrate(&unbacked_input("run-A", true));
    assert!(matches!(
        a_refused.unbacked,
        UnbackedVerdict::Refused { .. }
    ));
    // Run-B's first hit must still be Unbacked (counter fresh) — the
    // refusal on run-A did not leak.
    let b_first = arbiter.arbitrate(&unbacked_input("run-B", true));
    assert!(matches!(
        b_first.unbacked,
        UnbackedVerdict::Unbacked { attempts: 1, .. }
    ));
}
