//! Bounded sandbox-mode escalation ladder (ENG-90, dispatch B lane B9).
//!
//! Execution always starts at the least-privilege [`SandboxTier`] and may
//! escalate to a weaker (more permissive) tier only when a tool call was
//! just blocked by a *recognized* sandbox denial — never speculatively, and
//! never silently. Each step is meant to be recorded in the
//! [`crate::decision_log::DecisionLog`] by the caller using the
//! [`EscalationOutcome`] this module returns.
//!
//! This module classifies nothing itself: `rightkit-agent-policy` has no
//! dependency on `host_tools` (and must not gain one, to avoid a
//! dependency cycle — `host_tools` depends on `rightkit-agent-policy`).
//! Denial classification happens in
//! `rightkit_sandbox::classify_denial` (adapted by
//! `host_tools::sandbox::denial_report`), whose
//! `DenialSignature::category` `Display`s to the exact lowercase labels
//! this module parses via [`RecognizedDenialCategory::from_label`]
//! (`"filesystem"`, `"network"`, `"process-spawn"`, `"other"`,
//! `"unrecognized"`). The two crates share a vocabulary, not a type.
//!
//! # Escalation ladder
//!
//! Three rungs, most restrictive first:
//! 1. [`SandboxTier::Strict`] — filesystem isolation plus syscall/network
//!    restrictions. Every call starts here.
//! 2. [`SandboxTier::WorkspaceWrite`] — filesystem isolation only; escalate
//!    here from `Strict` on a recognized `network` or `process-spawn`
//!    denial (the restriction `Strict` adds on top of `WorkspaceWrite`).
//! 3. [`SandboxTier::Disabled`] — no OS-level restriction, last resort;
//!    escalate here from `WorkspaceWrite` on a recognized `filesystem`
//!    denial.
//!
//! Escalation never proceeds on `other` or `unrecognized` denials (fail
//! closed: unclassified failures are not evidence of a sandbox
//! false-positive, they might be a real bug or a real block that should
//! hold), never proceeds past [`SandboxTier::Disabled`], and never proceeds
//! past the caller-supplied `ceiling` — which the caller must derive from
//! [`crate::workspace_trust_gate::WorkspaceTrustGate`] so an untrusted or
//! not-yet-decided workspace can never escalate past its trust verdict.

use crate::sandbox_policy::SandboxTier;
use crate::workspace_trust_gate::TrustGateState;

/// Denial categories the escalation ladder recognizes as evidence.
///
/// Parsed from the string label `host_tools`'s `DenialSignature`
/// produces (see module docs) rather than a shared type, to keep
/// `rightkit-agent-policy` free of a dependency on `host_tools`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecognizedDenialCategory {
    Filesystem,
    Network,
    ProcessSpawn,
    /// Recognized as a real sandbox denial, but not one of the categories
    /// above — never sufficient evidence to escalate on its own.
    Other,
}

impl RecognizedDenialCategory {
    /// Parse the label produced by `DenialCategory`'s `Display` impl of
    /// `rightkit_sandbox::DenialCategory`. Returns `None` for
    /// `"unrecognized"` and for any unknown label — both must fail closed.
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "filesystem" => Some(Self::Filesystem),
            "network" => Some(Self::Network),
            "process-spawn" => Some(Self::ProcessSpawn),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

/// Result of one escalation-ladder evaluation.
///
/// Shaped for the caller to hand straight to
/// [`crate::decision_log::DecisionLog::append`]: `rule_id` becomes
/// `rule_attribution`, `reason` becomes the human-readable explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationOutcome {
    /// The tier execution should use for the retry. Equal to the tier that
    /// was passed in when `escalated` is `false`.
    pub tier: SandboxTier,
    /// Whether this evaluation actually loosened the sandbox tier.
    pub escalated: bool,
    /// Machine-stable identifier, e.g. `"sandbox-escalation:network->workspace-write"`
    /// or `"sandbox-escalation:blocked-by-trust-gate"`.
    pub rule_id: String,
    /// Human-readable reason, safe to surface to the user and to log.
    pub reason: String,
}

/// The fixed three-rung ladder, most restrictive first.
const LADDER: [SandboxTier; 3] = [
    SandboxTier::Strict,
    SandboxTier::WorkspaceWrite,
    SandboxTier::Disabled,
];

/// Evaluate whether a sandbox call that was just denied at `current_tier`
/// may retry one rung weaker.
///
/// `denial_label` is the `Display` output of the denial signature that was
/// just observed (see module docs); pass the raw label, not a parsed
/// variant, so callers do not need to reimplement the parse. `trust`
/// and `ceiling` together bound how far the ladder may ever climb for this
/// workspace: `trust` must be [`TrustGateState::Trusted`] for any
/// escalation at all, and the resulting tier is never weaker than
/// `ceiling` even when the ladder and the denial category would otherwise
/// allow it.
///
/// Returns `escalated: false` (with `tier` unchanged) whenever:
/// - the denial category is not recognized as escalation evidence
///   (`other`, `unrecognized`, or an unparseable label),
/// - `current_tier` is already the last rung (`Disabled`),
/// - the workspace is not `Trusted`, or
/// - the ladder's next rung would be weaker than `ceiling`.
pub fn evaluate_escalation(
    current_tier: SandboxTier,
    denial_label: &str,
    trust: TrustGateState,
    ceiling: SandboxTier,
) -> EscalationOutcome {
    let category = match RecognizedDenialCategory::from_label(denial_label) {
        Some(category) => category,
        None => {
            return EscalationOutcome {
                tier: current_tier,
                escalated: false,
                rule_id: "sandbox-escalation:unrecognized-denial".to_string(),
                reason: format!(
                    "denial label '{denial_label}' is not recognized escalation evidence; \
                     staying at {current_tier:?}"
                ),
            };
        }
    };

    if !trust.permits_tools() {
        return EscalationOutcome {
            tier: current_tier,
            escalated: false,
            rule_id: "sandbox-escalation:blocked-by-trust-gate".to_string(),
            reason: "workspace trust gate does not permit tool execution; \
                     sandbox tier cannot escalate"
                .to_string(),
        };
    }

    let Some(next_tier) = next_rung(current_tier) else {
        return EscalationOutcome {
            tier: current_tier,
            escalated: false,
            rule_id: "sandbox-escalation:already-at-floor".to_string(),
            reason: format!("{current_tier:?} is already the least-restrictive ladder rung"),
        };
    };

    if category == RecognizedDenialCategory::Other {
        return EscalationOutcome {
            tier: current_tier,
            escalated: false,
            rule_id: "sandbox-escalation:category-insufficient".to_string(),
            reason: "recognized denial, but category 'other' is not sufficient evidence \
                     to escalate the sandbox tier"
                .to_string(),
        };
    }

    if next_tier < ceiling {
        return EscalationOutcome {
            tier: current_tier,
            escalated: false,
            rule_id: "sandbox-escalation:capped-by-ceiling".to_string(),
            reason: format!(
                "escalation to {next_tier:?} exceeds the workspace trust ceiling \
                 ({ceiling:?}); staying at {current_tier:?}"
            ),
        };
    }

    EscalationOutcome {
        tier: next_tier,
        escalated: true,
        rule_id: format!(
            "sandbox-escalation:{denial_label}->{}",
            next_tier.persisted_value()
        ),
        reason: format!(
            "recognized {denial_label} denial at {current_tier:?}; escalating to {next_tier:?}"
        ),
    }
}

/// The next weaker (more permissive) rung after `tier`, or `None` if
/// `tier` is already the last rung on the ladder.
fn next_rung(tier: SandboxTier) -> Option<SandboxTier> {
    let index = LADDER.iter().position(|rung| *rung == tier)?;
    LADDER.get(index + 1).copied()
}
