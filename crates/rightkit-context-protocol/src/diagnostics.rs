//! Live Diagnostics operational contracts — mutation-bound diagnostic evidence
//! and Semantic Edit Fence gate decisions.
//!
//! Implements the wire shapes of
//! `docs/architecture/live-diagnostics.md` (§4 identity
//! model, §5 contracts, §8 coverage/convergence, §9 correlation). These are
//! *operational* schemas: they never mutate Membrane's frozen public V1 context
//! shapes, the Membrane planner remains the sole gate-policy owner, and
//! [`evaluate_gate`] is the pure deterministic evaluator over planner-owned
//! policy — it invents nothing and performs no I/O.
//!
//! Serde conventions follow the rest of the crate: `camelCase` struct fields,
//! `snake_case` closed enum vocabularies, `#[serde(default)]` on optional and
//! repeated fields.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Schema version stamped on every [`WorkspaceEpochV1`].
pub const WORKSPACE_EPOCH_SCHEMA_VERSION: &str = "workspace-epoch.v1";
/// Schema version stamped on every [`DiagnosticEvidenceSnapshotV1`].
pub const DIAGNOSTIC_EVIDENCE_SNAPSHOT_SCHEMA_VERSION: &str = "diagnostics-evidence-snapshot.v1";
/// Schema version stamped on every [`DiagnosticGateDecisionV1`].
pub const DIAGNOSTIC_GATE_DECISION_SCHEMA_VERSION: &str = "diagnostics-gate-decision.v1";

// ============================================================================
// Shared value types
// ============================================================================

/// Exact content hash of one changed file within a workspace epoch (§4).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFileHashV1 {
    /// Repo-relative path of the changed file.
    pub path: String,
    /// Content digest of the exact resulting bytes.
    pub hash: String,
}

/// One typed, machine-readable degradation recorded instead of silent data loss.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypedOmission {
    /// Stable omission code (e.g. `provider_unavailable`).
    pub code: String,
    /// Human-readable detail; never load-bearing for decisions.
    pub detail: String,
}

/// Half-open normalized source range; lines and columns are 1-based.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRange {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

// ============================================================================
// Closed vocabularies
// ============================================================================

/// Economic class of one diagnostic producer or acquisition request (§6).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostClass {
    #[default]
    Instant,
    Interactive,
    Verification,
    Build,
    Test,
}

/// How a workspace epoch came to exist (§4.1 host modes).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceEpochOrigin {
    #[default]
    Transactional,
    ObservedHook,
    Reconciliation,
}

/// Semantic capability a coverage obligation may demand (§8).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityVocabulary {
    #[default]
    Syntax,
    RepositoryModuleResolution,
    ImportExportBinding,
    NameResolution,
    TypeSemantics,
    ConfiguredStaticPolicy,
    CompilerProjectSemantics,
    GeneratedSourceAwareness,
}

impl CapabilityVocabulary {
    /// Stable lowercase-snake name used in gate reason codes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Syntax => "syntax",
            Self::RepositoryModuleResolution => "repository_module_resolution",
            Self::ImportExportBinding => "import_export_binding",
            Self::NameResolution => "name_resolution",
            Self::TypeSemantics => "type_semantics",
            Self::ConfiguredStaticPolicy => "configured_static_policy",
            Self::CompilerProjectSemantics => "compiler_project_semantics",
            Self::GeneratedSourceAwareness => "generated_source_awareness",
        }
    }
}

/// How exactly a lane's convergence was proven (§8).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExactnessRequirement {
    Exact,
    #[default]
    Advisory,
}

/// Satisfaction state of one coverage obligation.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObligationState {
    #[default]
    Unsatisfied,
    SatisfiedExact,
    SatisfiedAdvisory,
    Unavailable,
    TimedOut,
    Unsupported,
}

/// Authority class of the producer that emitted an observation (§5.1).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceClass {
    #[default]
    Parser,
    RepositoryFinding,
    NativeLanguageService,
    StaticAnalyzer,
    CompilerCheck,
}

/// Policy-facing hint for how an observation should gate (never enforcement).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeverityHint {
    Blocking,
    #[default]
    Advisory,
}

/// Aggregate delta classification across exact workspace epochs (§8.1).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaClassification {
    New,
    Persistent,
    Resolved,
    Moved,
    Changed,
    #[default]
    UnknownBaseline,
}

/// Qualified convergence classes; only the `*_exact` trio can clear the fence.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConvergenceClass {
    PullExact,
    PushVersionedExact,
    SnapshotCheckerExact,
    PushUnversionedAdvisory,
    #[default]
    Unsupported,
}

/// Completion state of one coverage lane (§8).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneState {
    Complete,
    Partial,
    Unavailable,
    TimedOut,
    #[default]
    Unsupported,
}

/// Freshness of the Blueprint generation bound to a snapshot (§4).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlueprintFreshness {
    Current,
    Stale,
    #[default]
    Unknown,
}

/// Closed gate outcome vocabulary (§5.2). There is deliberately no
/// `clean_partial`, `clean_stale`, or `probably_clean`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateOutcome {
    CleanExact,
    DirtyExact,
    #[default]
    UnknownIncomplete,
    UnknownUnavailable,
    UnknownTimedOut,
    UnknownConflict,
    Superseded,
}

// ============================================================================
// Envelopes
// ============================================================================

/// Monotonic operational identity for exact current worktree bytes in one
/// diagnostics session (§4). Derived from exact host bytes; never competes with
/// Blueprint source identity.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEpochV1 {
    pub schema_version: String,
    pub repo_id: String,
    pub worktree_id: String,
    pub epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_epoch: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_id: Option<String>,
    pub source_manifest_digest: String,
    #[serde(default)]
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub changed_file_hashes: Vec<ChangedFileHashV1>,
    pub project_config_digest: String,
    pub toolchain_digest: String,
    pub sandbox_policy_digest: String,
    pub origin: WorkspaceEpochOrigin,
}

/// Paths a coverage obligation must be satisfied over (§8).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequiredScope {
    #[serde(default)]
    pub paths: Vec<String>,
}

/// One planner-derived capability requirement supplied with a snapshot (§8).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageObligationV1 {
    pub capability: CapabilityVocabulary,
    pub language_dialect: String,
    pub project_identity: String,
    pub required_scope: RequiredScope,
    pub exactness_requirement: ExactnessRequirement,
    #[serde(default)]
    pub acceptable_provider_alternatives: Vec<String>,
    pub maximum_cost: CostClass,
    pub state: ObligationState,
    #[serde(default)]
    pub omissions: Vec<TypedOmission>,
}

/// One normalized diagnostic observation from one producer (§5.1, §9).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationV1 {
    pub observation_id: String,
    pub provider_id: String,
    pub provider_version: String,
    pub code: String,
    pub path: String,
    pub range: SourceRange,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_anchor: Option<String>,
    pub source_class: SourceClass,
    pub cost_class: CostClass,
    pub severity_hint: SeverityHint,
}

/// Confidently equivalent observations grouped under one repair-packet issue
/// (§9). Uncertain matches remain separate issues.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticIssueV1 {
    pub issue_id: String,
    pub correlation_key: String,
    #[serde(default)]
    pub observations: Vec<ObservationV1>,
    pub classification: DeltaClassification,
}

/// One producer's reported coverage over one invocation (§8).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageLaneV1 {
    pub provider_id: String,
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default)]
    pub capabilities_covered: Vec<CapabilityVocabulary>,
    pub convergence_class: ConvergenceClass,
    pub bound_workspace_epoch: u64,
    pub state: LaneState,
    #[serde(default)]
    pub omissions: Vec<TypedOmission>,
}

/// Blueprint's Tier-0 finding delta carried without recomputation (§8.1).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlueprintDeltaV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_generation: Option<String>,
    #[serde(default)]
    pub findings_delta: Vec<DiagnosticIssueV1>,
}

/// Aggregate observation/issue delta across exact workspace epochs (§8.1).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregateDeltaV1 {
    #[serde(default)]
    pub issues: Vec<AggregateIssueDelta>,
}

/// Per-issue aggregate delta entry.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregateIssueDelta {
    pub issue_id: String,
    pub classification: DeltaClassification,
}

/// Raw mutation-bound diagnostic evidence (§5.1). Producers fill this; the gate
/// evaluator consumes it without mutating it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticEvidenceSnapshotV1 {
    pub schema_version: String,
    pub snapshot_id: String,
    pub repo_id: String,
    pub worktree_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blueprint_generation: Option<String>,
    pub blueprint_freshness: BlueprintFreshness,
    /// Full workspace epoch envelope, embedded whole (§4).
    pub workspace_epoch: WorkspaceEpochV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_id: Option<String>,
    pub request_max_cost: CostClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absolute_deadline_ms: Option<u64>,
    #[serde(default)]
    pub coverage_obligations: Vec<CoverageObligationV1>,
    #[serde(default)]
    pub observations: Vec<ObservationV1>,
    #[serde(default)]
    pub issues: Vec<DiagnosticIssueV1>,
    #[serde(default)]
    pub coverage_lanes: Vec<CoverageLaneV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blueprint_delta: Option<BlueprintDeltaV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_delta: Option<AggregateDeltaV1>,
    #[serde(default)]
    pub omissions: Vec<TypedOmission>,
    pub produced_at_ms: u64,
}

/// Versioned planner-owned gate policy profile (§5.2). The evaluator never
/// invents policy; it only applies this profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatePolicyProfileV1 {
    pub profile_name: String,
    pub policy_version: String,
    pub policy_digest: String,
    #[serde(default)]
    pub blocking_codes: Vec<String>,
    #[serde(default)]
    pub required_capabilities: Vec<CapabilityVocabulary>,
}

impl Default for GatePolicyProfileV1 {
    fn default() -> Self {
        Self {
            profile_name: "default".to_string(),
            policy_version: String::new(),
            policy_digest: String::new(),
            blocking_codes: Vec::new(),
            required_capabilities: Vec::new(),
        }
    }
}

/// Deterministic result of evaluating planner policy against exact evidence
/// (§5.2). Hosts enforce this decision; presentation cannot alter it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticGateDecisionV1 {
    pub schema_version: String,
    pub snapshot_id: String,
    pub policy_profile: String,
    pub policy_version: String,
    pub policy_digest: String,
    pub outcome: GateOutcome,
    #[serde(default)]
    pub blocking_issue_ids: Vec<String>,
    #[serde(default)]
    pub required_obligations: Vec<CoverageObligationV1>,
    #[serde(default)]
    pub reason_codes: Vec<String>,
    #[serde(default)]
    pub omissions: Vec<TypedOmission>,
}

// ============================================================================
// Gate evaluation
// ============================================================================

/// Purely evaluate planner-owned `policy` against `snapshot` evidence bound to
/// `expected_epoch`.
///
/// Precedence follows design §5.3, whose exactness is deliberately **asymmetric**:
/// one current, exact blocking observation proves `dirty_exact` even when other
/// required capabilities degraded, while proving `clean_exact` requires complete
/// exact satisfaction of *every* required capability. Everything else is unknown;
/// there is deliberately no `clean_partial` / `clean_stale`.
///
/// 1. Identity, supersession, and manifest conflict: cross-repo/worktree
///    evidence is `unknown_conflict`; a snapshot whose epoch is not the expected
///    epoch (older bytes, or newer/diverged ancestry via the derivable
///    `parent_epoch` hop) is `superseded`; equal epochs with differing manifest
///    digests or file hashes are `unknown_conflict`.
/// 2. Any observation with `severity_hint == Blocking`, or whose code is listed
///    in `policy.blocking_codes`, proves `dirty_exact`.
/// 3. Otherwise `clean_exact` requires, for every required capability, some lane
///    with an exact convergence class (`pull_exact`, `push_versioned_exact`,
///    `snapshot_checker_exact`), `complete` state, and the snapshot's own bound
///    epoch — and no obligation left unsatisfied/unavailable/timed-out/
///    unsupported. Advisory convergence and quiet windows never clear the fence.
/// 4. Otherwise the applicable `unknown_*` outcome with stable snake_case
///    reason codes.
fn lane_scope_covers_required(lane_scope: &[String], required: &[String]) -> bool {
    if required.is_empty() {
        return true;
    }
    if lane_scope.is_empty() {
        return false;
    }
    let set: std::collections::HashSet<&str> = lane_scope.iter().map(|s| s.as_str()).collect();
    required.iter().all(|p| set.contains(p.as_str()))
}

pub fn evaluate_gate(
    snapshot: &DiagnosticEvidenceSnapshotV1,
    expected_epoch: &WorkspaceEpochV1,
    policy: &GatePolicyProfileV1,
) -> DiagnosticGateDecisionV1 {
    let mut decision = DiagnosticGateDecisionV1 {
        schema_version: DIAGNOSTIC_GATE_DECISION_SCHEMA_VERSION.to_string(),
        snapshot_id: snapshot.snapshot_id.clone(),
        policy_profile: policy.profile_name.clone(),
        policy_version: policy.policy_version.clone(),
        policy_digest: policy.policy_digest.clone(),
        outcome: GateOutcome::UnknownIncomplete,
        blocking_issue_ids: Vec::new(),
        required_obligations: snapshot.coverage_obligations.clone(),
        reason_codes: Vec::new(),
        omissions: snapshot.omissions.clone(),
    };
    let finish = |mut decision: DiagnosticGateDecisionV1,
                  outcome,
                  reasons: Vec<String>|
     -> DiagnosticGateDecisionV1 {
        decision.outcome = outcome;
        decision.reason_codes = reasons;
        decision
    };

    // 1. Identity, supersession, and manifest conflict (§4, §5.3 step 1).
    let observed = &snapshot.workspace_epoch;
    if observed.repo_id != expected_epoch.repo_id
        || observed.worktree_id != expected_epoch.worktree_id
    {
        return finish(
            decision,
            GateOutcome::UnknownConflict,
            vec!["identity_mismatch".to_string()],
        );
    }
    if observed.epoch != expected_epoch.epoch {
        // Older evidence is stale. Newer evidence cannot clear an older
        // expectation either: the fence binds to exact bytes, so diverged or
        // advanced lineage (derivable through `parent_epoch`) still supersedes
        // the expectation and the host must await/re-evaluate the newest epoch.
        return finish(
            decision,
            GateOutcome::Superseded,
            vec!["superseded_epoch".to_string()],
        );
    }
    if observed.source_manifest_digest != expected_epoch.source_manifest_digest
        || observed.changed_file_hashes != expected_epoch.changed_file_hashes
    {
        return finish(
            decision,
            GateOutcome::UnknownConflict,
            vec!["manifest_conflict".to_string()],
        );
    }

    // 2. Exact blockers prove dirty regardless of any degraded lane
    //    (§5.3 step 2: dirty state proven for the bound bytes).
    let mut observed_blocking_ids = HashSet::new();
    let blocking_observation_ids: Vec<&str> = snapshot
        .observations
        .iter()
        .filter(|observation| {
            (observation.severity_hint == SeverityHint::Blocking
                || policy
                    .blocking_codes
                    .iter()
                    .any(|code| code == &observation.code))
                && observed_blocking_ids.insert(observation.observation_id.as_str())
        })
        .map(|observation| observation.observation_id.as_str())
        .collect();
    if !blocking_observation_ids.is_empty() {
        let mut emitted_issue_ids = HashSet::new();
        let mut represented_observation_ids = HashSet::new();
        let mut blocking_issue_ids = Vec::new();
        for issue in &snapshot.issues {
            let represented_for_issue: Vec<String> = issue
                .observations
                .iter()
                .filter(|observation| {
                    blocking_observation_ids.contains(&observation.observation_id.as_str())
                })
                .map(|observation| observation.observation_id.clone())
                .collect();
            if !represented_for_issue.is_empty() && emitted_issue_ids.insert(issue.issue_id.clone())
            {
                blocking_issue_ids.push(issue.issue_id.clone());
                represented_observation_ids.extend(represented_for_issue);
            }
        }
        // Observations not grouped into any issue still count; list them by id.
        for observation_id in blocking_observation_ids {
            if !represented_observation_ids.contains(observation_id) {
                blocking_issue_ids.push(observation_id.to_string());
            }
        }
        decision.blocking_issue_ids = blocking_issue_ids;
        return finish(
            decision,
            GateOutcome::DirtyExact,
            vec!["exact_blocker".to_string()],
        );
    }

    // Guard: empty policy + empty obligations + empty lanes must never be clean (rejected shape: empty arrays as clean).
    if policy.required_capabilities.is_empty() && snapshot.coverage_obligations.is_empty() {
        if snapshot.coverage_lanes.is_empty() {
            return finish(
                decision,
                GateOutcome::UnknownIncomplete,
                vec!["empty_coverage_no_evidence".to_string()],
            );
        }
        return finish(
            decision,
            GateOutcome::UnknownIncomplete,
            vec!["no_required_capabilities".to_string()],
        );
    }

    // 3./4. Exact coverage of every required capability decides clean vs unknown
    //       (§5.3 steps 3–4, §8).
    let bound_epoch = snapshot.workspace_epoch.epoch;
    let mut any_capability_timed_out = false;
    let mut any_capability_unavailable = false;
    let mut every_capability_covered_exactly = true;
    let mut uncovered_capabilities: Vec<&'static str> = Vec::new();
    let mut timed_out_providers: Vec<String> = Vec::new();
    let mut partial_providers: Vec<String> = Vec::new();

    for capability in &policy.required_capabilities {
        let mut complete = false;
        let mut timed_out = false;
        let mut unavailable = false;
        let mut candidate_count = 0usize;
        let required_scope: Vec<String> = snapshot
            .coverage_obligations
            .iter()
            .find(|o| &o.capability == capability)
            .map(|o| o.required_scope.paths.clone())
            .unwrap_or_else(|| {
                let mut set = std::collections::BTreeSet::new();
                for p in &snapshot.workspace_epoch.changed_paths {
                    set.insert(p.clone());
                }
                for h in &snapshot.workspace_epoch.changed_file_hashes {
                    set.insert(h.path.clone());
                }
                set.into_iter().collect()
            });
        for lane in &snapshot.coverage_lanes {
            let exact_convergence = matches!(
                lane.convergence_class,
                ConvergenceClass::PullExact
                    | ConvergenceClass::PushVersionedExact
                    | ConvergenceClass::SnapshotCheckerExact
            );
            if !lane.capabilities_covered.contains(capability)
                || !exact_convergence
                || lane.bound_workspace_epoch != bound_epoch
                || !lane_scope_covers_required(&lane.scope, &required_scope)
            {
                continue;
            }
            candidate_count += 1;
            match lane.state {
                LaneState::Complete => complete = true,
                LaneState::TimedOut => {
                    timed_out = true;
                    timed_out_providers.push(lane.provider_id.clone());
                }
                LaneState::Unavailable | LaneState::Unsupported => unavailable = true,
                LaneState::Partial => partial_providers.push(lane.provider_id.clone()),
            }
        }
        if complete {
            continue;
        }
        every_capability_covered_exactly = false;
        if timed_out {
            any_capability_timed_out = true;
        } else if unavailable {
            any_capability_unavailable = true;
            uncovered_capabilities.push(capability.as_str());
        } else if candidate_count == 0 {
            uncovered_capabilities.push(capability.as_str());
        }
    }

    let mut unsatisfied_obligation_capabilities: Vec<&'static str> = Vec::new();
    for obligation in &snapshot.coverage_obligations {
        if matches!(
            obligation.state,
            ObligationState::Unsatisfied
                | ObligationState::Unavailable
                | ObligationState::TimedOut
                | ObligationState::Unsupported
        ) {
            unsatisfied_obligation_capabilities.push(obligation.capability.as_str());
        }
    }

    if every_capability_covered_exactly && unsatisfied_obligation_capabilities.is_empty() {
        return finish(decision, GateOutcome::CleanExact, Vec::new());
    }

    let mut seen = HashSet::new();
    let mut reasons = Vec::new();
    let mut push_reason = |code: String, reasons: &mut Vec<String>| {
        if seen.insert(code.clone()) {
            reasons.push(code);
        }
    };
    for capability in &uncovered_capabilities {
        push_reason(format!("capability_uncovered:{capability}"), &mut reasons);
    }
    for provider in &timed_out_providers {
        push_reason(format!("lane_timed_out:{provider}"), &mut reasons);
    }
    for capability in &unsatisfied_obligation_capabilities {
        push_reason(format!("obligation_unsatisfied:{capability}"), &mut reasons);
    }
    for provider in &partial_providers {
        push_reason(format!("partial_lane:{provider}"), &mut reasons);
    }

    let outcome = if any_capability_timed_out {
        GateOutcome::UnknownTimedOut
    } else if any_capability_unavailable {
        GateOutcome::UnknownUnavailable
    } else {
        GateOutcome::UnknownIncomplete
    };
    finish(decision, outcome, reasons)
}

// ============================================================================
// Tests
// ============================================================================
