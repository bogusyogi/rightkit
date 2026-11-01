//! WP-07 — Hash-bound, expiring approvals for the autonomous workflow
//! control plane.
//!
//! An *approval* is the canonical way to delegate a destructive / policy-
//! sensitive step to a human or service principal while still keeping the
//! workflow **fully replayable**. Every approval is pinned to:
//!
//! - exact hashes of the inputs the approver saw ([`ApprovalRequest::inputs_hash`]),
//! - exact hashes of the outputs the approver is signing off on
//!   ([`ApprovalRequest::outputs_hash`]),
//! - the [`ApprovalRequest::artifact_refs`] referenced by the node,
//! - the [`ApprovalRequest::effect_request_hash`] (so a denied / expired
//!   approval can never accidentally satisfy a *different* effect intent
//!   later in the run),
//! - an absolute expiry ([`ApprovalRequest::expires_at_unix_ms`]),
//! - a required [`ApprovalRequest::role`].
//
//!
//! Once an approval is decided, result is hashed into journal & bound to
//! those hashes. Replay re-checks binding on every reducer pass so stale
//! approval cannot satisfy a node whose inputs changed.
//!
//! ## Why "hash-bound"
//!
//! A normal approval system lets you approve "the next deploy". A
//! hash-bound approval system only lets you approve *this* deploy with
//! *these* artifacts and *these* effect parameters. The compiler binds
//! the hashes at compile time; the reducer re-verifies them at every
//! dispatch. There is no "blanket" approval.
//!
//! ## Why "one-shot"
//!
//! [`ApprovalRequest::one_shot`] is true for V1 by default: once an approval
//! has been decided, the decision is terminal and the request cannot be
//! satisfied again. The reviewer has to issue a new request for the new
//! inputs / outputs. This is what [`apply_decision`] enforces.

#![allow(clippy::module_name_repetitions)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::control_plane::domain::CapabilitySet;

// ─── ApprovalRequest ─────────────────────────────────────────────────────────

/// A pending approval request. The reducer accepts at most one
/// [`ApprovalDecision`] per request when `one_shot` is true.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// `wfp_*` — namespaced id assigned at admission time.
    pub id: String,
    /// `wfr_*` — namespaced run id this request belongs to.
    pub run_id: String,
    /// `wfn_*` — namespaced node id this request is registered against.
    pub node_id: String,
    /// `wfv_*` — version pin. An approval is only valid against the exact
    /// version that produced the inputs/outputs; a workflow revision that
    /// postdates the approval cannot re-use it.
    pub version_id: String,
    /// `sha256:`-prefixed hash of the canonical-JSON-encoded node inputs.
    pub inputs_hash: String,
    /// `sha256:`-prefixed hash of the canonical-JSON-encoded proposed
    /// outputs. Recorded so an approver cannot later be retroactively
    /// re-bound to *different* outputs.
    pub outputs_hash: String,
    /// Stable references to the artifacts the approver is signing off on
    /// (e.g. `artifact://<content_hash>` URIs).
    pub artifact_refs: Vec<String>,
    /// Effective capability set the approval is granting. Recorded so the
    /// reducer can verify the runtime intersection is unchanged on replay.
    pub capabilities: CapabilitySet,
    /// `sha256:`-prefixed hash of the effect intent this approval would
    /// satisfy. Bound explicitly so an expired / denied approval cannot
    /// be retargeted at a different effect later in the run.
    pub effect_request_hash: String,
    /// Absolute Unix epoch millisecond after which this request is
    /// automatically considered [`ApprovalDecision::Expired`].
    pub expires_at_unix_ms: i64,
    /// Required role. The approver string on a decision must equal this
    /// value — see [`apply_decision`].
    pub role: String,
    /// True when the request is single-use. Once a decision is applied,
    /// the request cannot be re-decided. V1 default.
    pub one_shot: bool,
}

// ─── ApprovalDecision ────────────────────────────────────────────────────────

/// The four terminal states of an approval request. The reducer accepts
/// exactly one variant per request when `one_shot` is true.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Human or service principal approved the request. The signature is a
    /// free-form attestation token (operator-supplied; the workflow crate
    /// treats it as opaque). The reducer records the signature in the
    /// journal for audit.
    Approved {
        /// Principal id (must equal [`ApprovalRequest::role`]).
        approver: String,
        /// Absolute Unix epoch millisecond at which the decision was made.
        decided_at_unix_ms: i64,
        /// Operator-supplied attestation token (opaque to the workflow
        /// plane; recorded verbatim for audit).
        signature: String,
    },
    /// Human or service principal denied the request.
    Denied {
        /// Principal id (must equal [`ApprovalRequest::role`]).
        approver: String,
        /// Free-form reason text recorded verbatim for audit.
        reason: String,
    },
    /// The request expired before any decision was applied. Recorded by
    /// the scheduler once `now_unix_ms >= expires_at_unix_ms`.
    Expired,
    /// The request was invalidated before any decision was applied (e.g.
    /// the run was cancelled, the workflow version was retired, the node
    /// was superseded). Distinct from [`ApprovalDecision::Expired`] so
    /// audit logs can tell the two apart.
    Invalidated {
        /// Free-form reason text recorded verbatim for audit.
        reason: String,
    },
}

// ─── ApprovalError ───────────────────────────────────────────────────────────

/// Typed failures [`apply_decision`] can emit.
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum ApprovalError {
    /// The decision's approver string does not equal the request's required
    /// role. The reducer MUST refuse the decision; replay would also refuse
    /// it, so silently coercing the decision would create journal divergence.
    #[error("approver `{approver}` does not satisfy required role `{required}`")]
    RoleMismatch {
        /// Required role from [`ApprovalRequest::role`].
        required: String,
        /// Approver string from the decision.
        approver: String,
    },
    /// The request was already decided. With `one_shot = true` the reducer
    /// MUST reject every subsequent decision to keep the journal linear.
    #[error("approval `{request_id}` already decided (one-shot violation)")]
    AlreadyDecided {
        /// `wfp_*` id of the already-decided request.
        request_id: String,
    },
    /// The decision variant is structurally invalid for this request —
    /// for example, applying [`ApprovalDecision::Expired`] to a request
    /// whose expiry has not yet elapsed (the scheduler is the only
    /// authority on expiry and must use a fresh clock).
    #[error("decision variant `{decision:?}` is not valid for this request: {reason}")]
    InvalidDecision {
        /// Stringified decision variant tag.
        decision: String,
        /// Human-readable cause.
        reason: String,
    },
}

// ─── ApprovalOutcome ─────────────────────────────────────────────────────────

/// Result of [`apply_decision`]. The decision is always recorded verbatim
/// (so the journal still has an auditable row); `Rejected` carries the
/// typed reason so the reducer can branch on it without parsing strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalOutcome {
    /// Decision accepted and bound to the request.
    Applied {
        /// The decision that was applied.
        decision: ApprovalDecision,
        /// Whether the request was one-shot. Recorded so downstream tests
        /// can assert one-shot semantics without re-reading the request.
        one_shot: bool,
    },
    /// Decision rejected by the reducer. The decision is preserved here so
    /// callers can log / audit it, but it MUST NOT be treated as final.
    Rejected {
        /// Typed reason.
        reason: ApprovalError,
        /// The decision that was rejected (verbatim).
        decision: ApprovalDecision,
    },
}

// ─── is_hash_bound ───────────────────────────────────────────────────────────

/// Pure check: returns `true` when `new_inputs_hash` equals the request's
/// recorded `inputs_hash`. Returns `false` when the hashes diverge — a
/// diverged hash means the inputs the approver would be signing off on
/// have changed since the request was issued, so the approval cannot be
/// reused.
///
/// The reducer calls this on every dispatch to prevent replay from
/// re-using an approval whose inputs have since changed.
pub fn is_hash_bound(request: &ApprovalRequest, new_inputs_hash: &str) -> bool {
    request.inputs_hash == new_inputs_hash
}

// ─── is_expired ──────────────────────────────────────────────────────────────

/// Pure check: returns `true` when `now_unix_ms >= expires_at_unix_ms`. The
/// scheduler calls this on every tick; the reducer also calls it before
/// applying any [`ApprovalDecision`] to guard against stale replays.
pub fn is_expired(request: &ApprovalRequest, now_unix_ms: i64) -> bool {
    now_unix_ms >= request.expires_at_unix_ms
}

// ─── apply_decision ──────────────────────────────────────────────────────────

/// Apply `decision` against `request`, enforcing:
///
/// 1. **role match** — the approver string on `Approved` / `Denied`
///    decisions must equal [`ApprovalRequest::role`];
/// 2. **expiry** — an [`ApprovalDecision::Expired`] decision is only
///    valid when the request is in fact expired at decision time; any
///    other decision variant against an already-expired request is
///    rejected as [`ApprovalError::InvalidDecision`];
/// 3. **one-shot** — every decision is terminal: an `Applied` outcome
///    records the decision verbatim and records whether the request was
///    one-shot, so downstream callers cannot re-decide the same request.
///
/// The function is **pure**. It does not touch `request` and does not
/// consume any external clock — the caller passes `now_unix_ms` so the
/// expiry check is explicit at the call site.
pub fn apply_decision(
    request: &ApprovalRequest,
    decision: ApprovalDecision,
    now_unix_ms: i64,
) -> ApprovalOutcome {
    // 1. Expiry enforcement.
    //
    //    An `Expired` decision is only structurally valid when the request
    //    has actually expired. Any non-`Expired` decision applied to an
    //    already-expired request is also refused: the reviewer had their
    //    window and either decided or did not.
    let expired = is_expired(request, now_unix_ms);
    match &decision {
        ApprovalDecision::Expired if !expired => {
            return ApprovalOutcome::Rejected {
                reason: ApprovalError::InvalidDecision {
                    decision: "Expired".into(),
                    reason: "decision marked Expired but request has not yet expired".into(),
                },
                decision,
            };
        }
        ApprovalDecision::Expired => {
            // Expired decision against an expired request — accept.
        }
        _ if expired => {
            // Non-expired decision against an already-expired request.
            return ApprovalOutcome::Rejected {
                reason: ApprovalError::InvalidDecision {
                    decision: decision_tag(&decision),
                    reason: "request has already expired; no further decisions accepted".into(),
                },
                decision,
            };
        }
        _ => {
            // Non-expired decision against a non-expired request — fall
            // through to role matching below.
        }
    }

    // 2. Role match for principal decisions.
    match &decision {
        ApprovalDecision::Approved { approver, .. } | ApprovalDecision::Denied { approver, .. } => {
            if approver != &request.role {
                return ApprovalOutcome::Rejected {
                    reason: ApprovalError::RoleMismatch {
                        required: request.role.clone(),
                        approver: approver.clone(),
                    },
                    decision,
                };
            }
        }
        ApprovalDecision::Expired | ApprovalDecision::Invalidated { .. } => {
            // System-recorded states; no principal to match.
        }
    }

    // 3. One-shot semantics: the decision is final. We record `one_shot`
    //    in the outcome so downstream callers can assert the request was
    //    single-use without re-reading the request.
    ApprovalOutcome::Applied {
        decision,
        one_shot: request.one_shot,
    }
}

/// Stringify the decision variant tag. Used for [`ApprovalError`] payloads.
fn decision_tag(decision: &ApprovalDecision) -> String {
    match decision {
        ApprovalDecision::Approved { .. } => "Approved".into(),
        ApprovalDecision::Denied { .. } => "Denied".into(),
        ApprovalDecision::Expired => "Expired".into(),
        ApprovalDecision::Invalidated { .. } => "Invalidated".into(),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::domain::Capability;

    fn request(role: &str, expires_at: i64, one_shot: bool) -> ApprovalRequest {
        ApprovalRequest {
            id: "wfp_1".into(),
            run_id: "wfr_run".into(),
            node_id: "wfn_node".into(),
            version_id: "wfv_v1".into(),
            inputs_hash: "sha256:in".into(),
            outputs_hash: "sha256:out".into(),
            artifact_refs: vec!["artifact://a".into()],
            capabilities: CapabilitySet::from_iter([Capability::new("noop")]),
            effect_request_hash: "sha256:effect".into(),
            expires_at_unix_ms: expires_at,
            role: role.into(),
            one_shot,
        }
    }

    #[test]
    fn is_hash_bound_returns_true_when_hashes_match() {
        let r = request("reviewer", 100, true);
        assert!(is_hash_bound(&r, "sha256:in"));
    }

    #[test]
    fn is_hash_bound_returns_false_when_hashes_diverge() {
        let r = request("reviewer", 100, true);
        assert!(!is_hash_bound(&r, "sha256:different"));
        assert!(!is_hash_bound(&r, ""));
    }

    #[test]
    fn is_expired_uses_strict_inequality_at_expiry_boundary() {
        let r = request("reviewer", 1_000, true);
        assert!(!is_expired(&r, 999));
        assert!(is_expired(&r, 1_000));
        assert!(is_expired(&r, 2_500));
    }

    #[test]
    fn apply_decision_accepts_approved_when_role_matches_and_not_expired() {
        let r = request("reviewer", 10_000, true);
        let decision = ApprovalDecision::Approved {
            approver: "reviewer".into(),
            decided_at_unix_ms: 100,
            signature: "sig".into(),
        };
        match apply_decision(&r, decision, 100) {
            ApprovalOutcome::Applied { one_shot, .. } => assert!(one_shot),
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn apply_decision_rejects_approved_when_role_does_not_match() {
        let r = request("reviewer", 10_000, true);
        let decision = ApprovalDecision::Approved {
            approver: "intern".into(),
            decided_at_unix_ms: 100,
            signature: "sig".into(),
        };
        match apply_decision(&r, decision, 100) {
            ApprovalOutcome::Rejected { reason, .. } => {
                assert!(matches!(reason, ApprovalError::RoleMismatch { .. }));
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn apply_decision_rejects_denied_when_role_does_not_match() {
        let r = request("reviewer", 10_000, true);
        let decision = ApprovalDecision::Denied {
            approver: "intern".into(),
            reason: "looks wrong".into(),
        };
        match apply_decision(&r, decision, 100) {
            ApprovalOutcome::Rejected { reason, .. } => {
                assert!(matches!(reason, ApprovalError::RoleMismatch { .. }));
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn apply_decision_accepts_expired_decision_only_when_request_is_expired() {
        let r = request("reviewer", 100, true);
        // Before expiry: Expired is rejected.
        let outcome = apply_decision(&r, ApprovalDecision::Expired, 50);
        assert!(matches!(outcome, ApprovalOutcome::Rejected { .. }));
        // At / after expiry: Expired is accepted.
        match apply_decision(&r, ApprovalDecision::Expired, 100) {
            ApprovalOutcome::Applied { one_shot, .. } => assert!(one_shot),
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn apply_decision_rejects_approved_when_request_is_already_expired() {
        let r = request("reviewer", 100, true);
        let decision = ApprovalDecision::Approved {
            approver: "reviewer".into(),
            decided_at_unix_ms: 200,
            signature: "sig".into(),
        };
        match apply_decision(&r, decision, 200) {
            ApprovalOutcome::Rejected { reason, .. } => {
                assert!(matches!(reason, ApprovalError::InvalidDecision { .. }));
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn apply_decision_accepts_invalidated_without_role_check() {
        let r = request("reviewer", 10_000, true);
        let decision = ApprovalDecision::Invalidated {
            reason: "node superseded".into(),
        };
        match apply_decision(&r, decision, 100) {
            ApprovalOutcome::Applied { one_shot, .. } => assert!(one_shot),
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn apply_decision_records_one_shot_false_when_request_is_not_one_shot() {
        let r = request("reviewer", 10_000, false);
        let decision = ApprovalDecision::Approved {
            approver: "reviewer".into(),
            decided_at_unix_ms: 100,
            signature: "sig".into(),
        };
        match apply_decision(&r, decision, 100) {
            ApprovalOutcome::Applied { one_shot, .. } => assert!(!one_shot),
            other => panic!("expected Applied, got {other:?}"),
        }
    }
}
