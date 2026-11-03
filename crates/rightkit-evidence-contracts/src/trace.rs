//! Route/outcome telemetry trace (tracker P1.6).
//!
//! Automatic, content-light, structured trace emitted once per routed
//! request. It carries no agent-authored prose — every field is a
//! mechanically recorded classification, count, or flag — so the four
//! Bounded Falsification metrics defined in
//! `docs/provenance/migrations/2026-08-29-pending/ARCANE-COGNITIVE-CONTROL-PLANE-2026-08-29-REV3.md` §30
//! ("Telemetry (feeds Section 29 / tracker P1.6)") are computable as pure
//! functions over recorded traces, with no human judgment required at
//! measurement time. See `schemas/route-outcome-trace.v1.md` for the exact
//! metric formulas over this schema.
//!
//! v0 is schema-only: this module defines and validates the record shape.
//! No collector/emitter pipeline is implemented here; recording sites are
//! wired in a later change.

use serde::{Deserialize, Serialize};

use crate::{
    canonical_digest,
    id::{RequestId, TaskId, TraceId},
    ContractError,
};

/// Cognition axis (Arcane §5): direct answer, deliberate thinking, or
/// grounding. Decomposition is represented via multiple child traces
/// rather than a fourth route value, matching tracker P1.6's field list.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    Direct,
    Deliberate,
    Grounded,
}

/// Semantic requirement tri-state (Arcane §7). Governs model vs no-model
/// execution and is a shared concept between the router and the executor.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[allow(non_camel_case_types)]
pub enum SemanticRequirement {
    FORBIDDEN,
    CONDITIONAL,
    REQUIRED,
}

/// Which shared authority role attached to this request (Arcane §13), if
/// any. Absence is represented by `None` on the containing field, not by a
/// variant, so "no authority attached" never needs a placeholder string.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityKind {
    Sage,
    Alchemist,
    Oracle,
}

/// Host/lead supplied observation of role adoption for this route.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleDecisionState {
    Selected,
    Bound,
    Launched,
    Skipped,
}

/// Typed observation emitted by the native hook for a host subagent lifecycle
/// event. It is deliberately distinct from caller-declared `RoleDecision`s.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ChildLifecycleEvent {
    #[serde(rename = "SubagentStart")]
    SubagentStart,
    #[serde(rename = "SubagentStop")]
    SubagentStop,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildLifecycleObservation {
    pub schema_version: u32,
    pub kind: String,
    pub event: ChildLifecycleEvent,
    pub session_id: String,
    pub child_id: String,
    pub role: AuthorityKind,
    /// True means the hook received this host event; it does not mean the
    /// child launch or task succeeded.
    pub event_observed: bool,
    /// Hook response status only; it is not launch approval.
    pub hook_acknowledged: bool,
    pub observed_at_unix_ms: u64,
}

impl ChildLifecycleObservation {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema_version != 2 {
            return Err(ContractError::UnsupportedVersion(self.schema_version));
        }
        if self.kind != "legion-child-lifecycle" {
            return Err(ContractError::InvalidContract {
                path: "kind".into(),
                reason: "must identify a child lifecycle observation".into(),
            });
        }
        for (path, value) in [
            ("session_id", self.session_id.as_str()),
            ("child_id", self.child_id.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ContractError::InvalidContract {
                    path: path.into(),
                    reason: "must be non-empty for lifecycle identity matching".into(),
                });
            }
        }
        if !self.event_observed {
            return Err(ContractError::InvalidContract {
                path: "event_observed".into(),
                reason: "typed lifecycle record must represent a received host event".into(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleDecision {
    pub role: AuthorityKind,
    /// None means eligibility was not labelled. It is distinct from false.
    pub eligible: Option<bool>,
    pub state: RoleDecisionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl RoleDecision {
    fn validate(&self) -> Result<(), ContractError> {
        if self.state == RoleDecisionState::Skipped
            && self
                .reason
                .as_deref()
                .is_none_or(|reason| reason.trim().is_empty())
        {
            return Err(ContractError::InvalidContract {
                path: "role_decisions.reason".into(),
                reason: "skipped role decision must include a reason".into(),
            });
        }
        Ok(())
    }
}

/// Compute posture (Arcane §5/§9/§18): whether a model runs at all, and if
/// so which tier.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputePosture {
    NoModel,
    Tiny,
    Strong,
}

/// Terminal outcome of the routed request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeResult {
    Success,
    Repair,
    UserCorrection,
    Blocked,
}

/// Bounded Falsification level (Arcane §30 "Three levels").
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ChallengeLevel {
    L0,
    L1,
    L2,
}

/// L1 trigger vocabulary, verbatim from Arcane §30 "L1 triggers".
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChallengeTrigger {
    /// Recommendation resting on assumed rather than inspected implementation.
    AssumedImplementation,
    /// Diagnosis from symptoms while decisive evidence is cheaply available.
    DiagnosisFromAvailableEvidence,
    /// Architectural recommendation materially dependent on checkable
    /// implementation assumptions (conceptual design work alone does not
    /// trigger).
    ArchitecturalAssumptionDependency,
    /// Consequential extrapolation in the answer.
    ConsequentialExtrapolation,
    /// About to contradict a canonical source.
    ContradictsCanonicalSource,
    /// Confidence materially dependent on 1-3 checkable assumptions.
    CheckableAssumptionConfidence,
    /// Explicit user challenge ("are you sure?", "check that").
    ExplicitUserChallenge,
    /// The previous answer was challenged or corrected.
    PriorAnswerChallenged,
}

/// KEEP / NARROW / REVISE (Arcane §30 "The primitive"). Only present when
/// a challenge pass was invoked.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChallengeOutcome {
    Keep,
    Narrow,
    Revise,
}

/// Context sources and total retrieved size for this request (Attention
/// axis, Arcane §5).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextUsage {
    pub sources: Vec<String>,
    pub size_bytes: u64,
}

/// Capabilities/skills considered vs the ones actually selected (Expertise
/// axis, Arcane §5).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityUsage {
    pub considered: Vec<String>,
    pub selected: Vec<String>,
}

/// Token and cost accounting. Cost is recorded in integer micro-USD
/// (1 unit = 1e-6 USD) to keep the record float-free, matching the
/// integer-units convention already used for cost/risk/rework in
/// `schemas/goal-route.v1.schema.json`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd_micros: u64,
}

/// Bounded Falsification (Challenge Pass) fields. These, plus
/// `assumption_dependent_conclusion`, `evidence_available_at_first_answer`,
/// and `user_challenge_event`, are exactly the recorded fields the four
/// Arcane §30 telemetry formulas are built from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChallengePass {
    /// Whether a challenge pass (L1 self-challenge or L2 independent
    /// review) was invoked for this request at all.
    pub invoked: bool,
    /// Level of the pass. `L0` when `invoked` is false.
    pub level: ChallengeLevel,
    /// Which L1 trigger (if any) fired the pass. `None` when `invoked` is
    /// false, and may be `None` even when invoked (e.g. an L2 pass invoked
    /// for reasons outside the L1 trigger vocabulary).
    pub trigger: Option<ChallengeTrigger>,
    /// KEEP / NARROW / REVISE. `None` exactly when `invoked` is false.
    pub outcome: Option<ChallengeOutcome>,
    /// Whether the committed conclusion was materially assumption-dependent
    /// (denominator of `user_challenge_rate` and
    /// `avoidable_user_challenge_rate`), independent of whether a pass ran.
    pub assumption_dependent_conclusion: bool,
    /// Whether decisive evidence for the checked assumptions was already
    /// available at the time of the first answer, before any challenge.
    /// Used by `avoidable_user_challenge_rate`.
    pub evidence_available_at_first_answer: bool,
    /// Distinct from `assumption_dependent_conclusion`: whether the user
    /// explicitly challenged a prior answer in this turn (e.g. "are you
    /// sure?"). Numerator of `user_challenge_rate`.
    pub user_challenge_event: bool,
}

impl ChallengePass {
    fn validate(&self) -> Result<(), ContractError> {
        if self.invoked {
            if self.level == ChallengeLevel::L0 {
                return Err(ContractError::InvalidContract {
                    path: "challenge.level".into(),
                    reason: "invoked challenge pass must be L1 or L2, not L0".into(),
                });
            }
            if self.outcome.is_none() {
                return Err(ContractError::InvalidContract {
                    path: "challenge.outcome".into(),
                    reason: "invoked challenge pass must record KEEP/NARROW/REVISE".into(),
                });
            }
        } else {
            if self.level != ChallengeLevel::L0 {
                return Err(ContractError::InvalidContract {
                    path: "challenge.level".into(),
                    reason: "level must be L0 when no challenge pass was invoked".into(),
                });
            }
            if self.outcome.is_some() {
                return Err(ContractError::InvalidContract {
                    path: "challenge.outcome".into(),
                    reason: "outcome must be absent when no challenge pass was invoked".into(),
                });
            }
            if self.trigger.is_some() {
                return Err(ContractError::InvalidContract {
                    path: "challenge.trigger".into(),
                    reason: "trigger must be absent when no challenge pass was invoked".into(),
                });
            }
        }
        Ok(())
    }
}

/// One automatic, content-light route/outcome trace record (tracker P1.6).
///
/// Emitted per routed request. Carries no agent-authored artifact content —
/// only mechanical classifications, counts, and flags — so it can be
/// recorded on every request without a review step, and so the Arcane §30
/// metrics can be computed over a corpus of these traces without
/// re-deriving their definitions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteOutcomeTrace {
    #[serde(deserialize_with = "deserialize_trace_schema_version")]
    pub schema_version: u32,
    pub trace_id: TraceId,
    pub request_id: RequestId,
    pub task_id: Option<TaskId>,
    /// Content-addressed identity of the Arcane cognitive profile that
    /// produced this route, when provenance was available.
    #[serde(default, rename = "arcaneProfileDigest")]
    pub cognitive_profile_digest: Option<String>,
    /// Content-addressed identity of the canonical rules that were
    /// in force when this route was produced, when provenance was available.
    #[serde(default, rename = "legionCanonDigest")]
    pub canon_digest: Option<String>,
    /// Content-addressed identity of the installed skill catalog used for
    /// this route, when provenance was available.
    #[serde(default, rename = "skillCatalogDigest")]
    pub skill_catalog_digest: Option<String>,
    /// Content-addressed identity of the Guard policy used for this route or
    /// verdict, when provenance was available.
    #[serde(default, rename = "guardPolicyDigest")]
    pub guard_policy_digest: Option<String>,
    pub route: Route,
    pub semantic_requirement: SemanticRequirement,
    pub context: ContextUsage,
    pub capabilities: CapabilityUsage,
    pub authority_attached: Option<AuthorityKind>,
    pub compute_posture: ComputePosture,
    pub result: OutcomeResult,
    pub latency_ms: u64,
    pub cost: CostUsage,
    pub challenge: ChallengePass,
    /// Present only for schema v2. Omitted from v1 serialization so all v1
    /// fields and canonical digests remain unchanged.
    #[serde(
        default,
        rename = "roleDecisions",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_role_decisions"
    )]
    pub role_decisions: Option<Vec<RoleDecision>>,
}

fn deserialize_role_decisions<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<RoleDecision>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Omission denotes v1; explicit null is malformed, not legacy omission.
    Vec::<RoleDecision>::deserialize(deserializer).map(Some)
}

/// V2 keeps the v1 flat record shape and adds labelled role observations.
/// This reader accepts both v1 and v2; strict v1-only readers remain v1-only.
pub type RouteOutcomeTraceV2 = RouteOutcomeTrace;

/// Counts latest caller-declared role decision states per request/role.
/// These values are not evidence that a child launched or completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default, Serialize, Deserialize)]
pub struct RoleDecisionDeclarationMetrics {
    pub labelled_eligible: usize,
    pub declared_eligible_launches: usize,
    pub declared_unnecessary_launches: usize,
    pub declared_unknown_launches: usize,
    pub eligible_skips: usize,
    pub pending_selected: usize,
    pub pending_bound: usize,
}

pub fn fold_role_decision_declarations(
    traces: &[RouteOutcomeTrace],
) -> RoleDecisionDeclarationMetrics {
    let mut latest: Vec<(&RequestId, AuthorityKind, &RoleDecision)> = Vec::new();
    for trace in traces {
        if trace.validate().is_err() {
            continue;
        }
        let Some(decisions) = trace.role_decisions.as_ref() else {
            continue;
        };
        for decision in decisions {
            if let Some(existing) = latest.iter_mut().find(|(request, role, _)| {
                request.as_str() == trace.request_id.as_str() && *role == decision.role
            }) {
                *existing = (&trace.request_id, decision.role, decision);
            } else {
                latest.push((&trace.request_id, decision.role, decision));
            }
        }
    }
    let mut metrics = RoleDecisionDeclarationMetrics::default();
    for (_, _, decision) in latest {
        if decision.eligible == Some(true) {
            metrics.labelled_eligible += 1;
        }
        match (decision.state, decision.eligible) {
            (RoleDecisionState::Launched, Some(true)) => metrics.declared_eligible_launches += 1,
            (RoleDecisionState::Launched, Some(false)) => {
                metrics.declared_unnecessary_launches += 1
            }
            (RoleDecisionState::Launched, None) => metrics.declared_unknown_launches += 1,
            (RoleDecisionState::Skipped, Some(true)) => metrics.eligible_skips += 1,
            (RoleDecisionState::Selected, _) => metrics.pending_selected += 1,
            (RoleDecisionState::Bound, _) => metrics.pending_bound += 1,
            _ => {}
        }
    }
    metrics
}

/// Compatibility adapter retaining the old metric field names. Values still
/// come only from caller declarations; use `fold_role_decision_declarations`
/// for outputs that label their provenance explicitly.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default, Serialize, Deserialize)]
pub struct RoleAdoptionMetrics {
    pub labelled_eligible: usize,
    pub eligible_launches: usize,
    pub unnecessary_launches: usize,
    pub unknown_launches: usize,
    pub eligible_skips: usize,
    pub pending_selected: usize,
    pub pending_bound: usize,
}

pub fn fold_role_adoption(traces: &[RouteOutcomeTrace]) -> RoleAdoptionMetrics {
    let declarations = fold_role_decision_declarations(traces);
    RoleAdoptionMetrics {
        labelled_eligible: declarations.labelled_eligible,
        eligible_launches: declarations.declared_eligible_launches,
        unnecessary_launches: declarations.declared_unnecessary_launches,
        unknown_launches: declarations.declared_unknown_launches,
        eligible_skips: declarations.eligible_skips,
        pending_selected: declarations.pending_selected,
        pending_bound: declarations.pending_bound,
    }
}

fn deserialize_trace_schema_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if matches!(version, 1 | 2) {
        Ok(version)
    } else {
        Err(serde::de::Error::custom(format!(
            "unsupported schema major version {version}"
        )))
    }
}

impl RouteOutcomeTrace {
    pub fn validate(&self) -> Result<(), ContractError> {
        if !matches!(self.schema_version, 1 | 2) {
            return Err(ContractError::UnsupportedVersion(self.schema_version));
        }
        if self.schema_version == 1 && self.role_decisions.is_some() {
            return Err(ContractError::InvalidContract {
                path: "role_decisions".into(),
                reason: "v1 trace cannot contain role decisions".into(),
            });
        }
        if self.schema_version == 2 {
            let decisions =
                self.role_decisions
                    .as_ref()
                    .ok_or_else(|| ContractError::InvalidContract {
                        path: "role_decisions".into(),
                        reason: "v2 trace must contain role decisions".into(),
                    })?;
            let mut roles = Vec::new();
            for decision in decisions {
                decision.validate()?;
                if roles.contains(&decision.role) {
                    return Err(ContractError::InvalidContract {
                        path: "role_decisions.role".into(),
                        reason: "role may occur only once per trace".into(),
                    });
                }
                roles.push(decision.role);
            }
        }
        self.challenge.validate()?;
        Ok(())
    }

    pub fn digest(&self) -> Result<String, crate::canonical::CanonicalError> {
        canonical_digest(self)
    }
}
