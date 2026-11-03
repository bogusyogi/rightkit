//! Host application authority (PL-011).
//!
//! The proposal is evidence; **the host app's own policy is the authority** that
//! applies it. This module is that decision point. The gate receives the
//! validated proposal and returns a typed decision carrying a policy receipt
//! that the application receipt records — so every applied mutation is
//! traceable both to its evidence (the proposal + approval) and to its
//! authority (this gate's receipt).
//!
//! The default implementation is [`DenyAllAuthority`]: fail-closed. A host
//! that wires nothing applies nothing. [`StaticPolicyAuthority`] is the
//! minimal configurable grant (surface + subject-prefix + payload-size bound)
//! used by tests and as a template for the daemon's real policy integration.

use super::guidance::covers;
use super::proposal::SealedRemediationProposalV1;

/// The host authority's decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorityDecision {
    /// the host app policy authorizes application. `policy_receipt` is recorded
    /// verbatim on the application receipt.
    Granted { policy_receipt: String },
    /// the host app policy refuses. The proposal remains evidence; it is never
    /// applied behind the policy's back.
    Denied { reason: String },
}

impl AuthorityDecision {
    pub fn is_granted(&self) -> bool {
        matches!(self, AuthorityDecision::Granted { .. })
    }
}

/// the host app's own authority/policy gate. Deliberately a separate object from
/// the proposal: an approved proposal can never grant itself authority.
pub trait ApplicationAuthorityGate: Send + Sync {
    fn authorize(&self, proposal: &SealedRemediationProposalV1) -> AuthorityDecision;
}

/// Fail-closed default: no configured authority means no application, ever.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DenyAllAuthority;

impl ApplicationAuthorityGate for DenyAllAuthority {
    fn authorize(&self, _proposal: &SealedRemediationProposalV1) -> AuthorityDecision {
        AuthorityDecision::Denied {
            reason: "fail-closed default: no application authority is configured; a proposal (however approved) is evidence, never authority".into(),
        }
    }
}

/// One grant rule: a guidance surface + subject prefix (dot-path semantics via
/// [`covers`]) and a payload-size bound (integration doc §10 gate 5: artifact
/// and token-size budgets). The bound applies to the delta payload — the
/// replacement body or the appended content — not the resulting artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRule {
    pub kind: super::proposal::GuidanceKind,
    pub subject_prefix: String,
    pub max_delta_bytes: usize,
}

/// Minimal configurable authority: grants when any rule covers the target and
/// the delta payload fits the bound; denies everything else. Intended as the
/// template the daemon's real policy layer replaces.
#[derive(Debug, Clone, Default)]
pub struct StaticPolicyAuthority {
    pub rules: Vec<PolicyRule>,
}

impl StaticPolicyAuthority {
    pub fn new(rules: Vec<PolicyRule>) -> Self {
        Self { rules }
    }
}

impl ApplicationAuthorityGate for StaticPolicyAuthority {
    fn authorize(&self, proposal: &SealedRemediationProposalV1) -> AuthorityDecision {
        let target = &proposal.intervention_target;
        let payload_bytes = proposal.delta.payload_bytes();
        for rule in &self.rules {
            if rule.kind != target.surface || !covers(&rule.subject_prefix, &target.subject) {
                continue;
            }
            return if payload_bytes <= rule.max_delta_bytes {
                AuthorityDecision::Granted {
                    policy_receipt: format!(
                        "static-policy: kind={} subject_prefix={} max_delta_bytes={} (payload {payload_bytes} bytes)",
                        rule.kind.as_str(),
                        rule.subject_prefix,
                        rule.max_delta_bytes
                    ),
                }
            } else {
                AuthorityDecision::Denied {
                    reason: format!(
                        "delta payload of {payload_bytes} bytes exceeds the policy bound of {} bytes for kind={} subject_prefix={}",
                        rule.max_delta_bytes,
                        rule.kind.as_str(),
                        rule.subject_prefix
                    ),
                }
            };
        }
        AuthorityDecision::Denied {
            reason: format!(
                "no static policy rule covers kind={} subject={}",
                target.surface.as_str(),
                target.subject
            ),
        }
    }
}
