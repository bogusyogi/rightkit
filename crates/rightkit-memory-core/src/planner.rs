//! Membrane planner: deterministic, pure, provider-neutral admission.
//!
//! Per dispatch G3A, this module owns:
//!   - ContextCandidateSet v1 shape parsing and normalization
//!   - Cross-root rejection (trustClass == remote_untrusted)
//!   - Version-authority filtering (superseded/proposed lose unless protected)
//!   - Source-class filters (memory relevance threshold; doc over memory)
//!   - Deduplication by id, canonical source hash, and normalized content hash
//!   - Ranking by (providerScore desc, freshness desc, kind priority, id asc)
//!   - Token-budget admission (sum estimatedTokens <= max_tokens)
//!   - Bound ContextPacket v1 emission with a Budget + per-layer allocations
//!   - Content-free ContextReceipt v2 emission per candidate
//!   - Fallback contract: provider capability or release-generation failures
//!     quarantine non-fallback candidates; stale graph evidence remains eligible
//!     with explicit stale status/provenance
//!   - Reason codes on every receipt (admitted, rejected, budget_exhausted,
//!     cross_root, superseded_version, memory_low_relevance,
//!     provider_capability_missing, fallback_quarantine, duplicate_id,
//!     within_global_budget, freshness_authority_outranked)
//!
//! # Cross-provider score policy (P1 — lanes, not recalibration)
//!
//! Raw `provider_score` values are **lane-local signals**, not a shared relevance
//! probability. Providers emit incompatible scales (e.g. overlay flat recency
//! priors ~0.6 vs memory cosine ~0.3–0.5). Admission therefore does **not** treat
//! a global sort of raw scores as cross-provider truth: reserved source-kind
//! lanes (`LEGACY_RESERVED_LANES`, overridable via `PlannerInput::reserved_lanes`)
//! admit within-lane by score first; only the residual
//! budget competes in global score order. Recalibrating every provider onto one
//! scale is explicitly deferred — lanes are the standing policy.
//!
//! The planner performs no I/O: no filesystem reads, no Blueprint queries, no
//! Git access, no Cortex DB queries. Producers feed it via JSON; consumers
//! receive the packet and the receipts back.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---- Schema mirror ----------------------------------------------------------
//
// These mirror the canonical v1 + v2 definitions in
// tools/lib/context-contracts.schema.json byte-for-byte. The planner rejects
// unknown schemaVersion and rejects v1 receipts that lack planner-observability
// fields when the caller accepts receipt v2.

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateV1 {
    pub id: String,
    pub layer: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub source_kind: String,
    pub source_ref: String,
    pub source_hash: String,
    pub trust_class: String,
    pub instruction_policy: String,
    pub provider_score: f64,
    #[serde(default)]
    pub score_components: BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness_class: Option<FreshnessClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    pub estimated_tokens: usize,
    pub protected: bool,
    pub exact: bool,
    pub recoverable: bool,
    pub resolver: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmissionV1 {
    pub id: String,
    #[serde(default)]
    pub layer: Option<u8>,
    pub reason: String,
    /// Content-free provider attribution retained across native Pull conversion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextCandidateSetV1 {
    pub schema_version: u32,
    pub trace_id: String,
    pub indexed_at: String,
    pub task: String,
    pub mode: String,
    pub provider: String,
    pub freshness: FreshnessV1,
    pub provider_ceiling: ProviderCeilingV1,
    pub candidates: Vec<CandidateV1>,
    pub omissions: Vec<OmissionV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreshnessV1 {
    pub revision: String,
    pub indexed_at: String,
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_state: Option<GraphState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_release_generation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_release_generation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_generation_status: Option<ReleaseGenerationStatus>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseGenerationStatus {
    Matched,
    Mismatch,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessClass {
    Current,
    CommittedSnapshot,
    DirtyOverlay,
    StaleSnapshot,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GraphState {
    Clean,
    DirtyOverlay,
    StaleSnapshot,
    MissingSnapshot,
    PartialReindex,
    ConcurrentUpdate,
    Indeterminate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCeilingV1 {
    pub max_candidates: usize,
    pub max_estimated_tokens: usize,
}

// ---- Planner input/output --------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannerInput {
    pub candidate_set: ContextCandidateSetV1,
    pub max_tokens: usize,
    /// Optional lower character ceiling selected for the active model. Exact rendered character
    /// enforcement belongs to the client finalizers; the pure planner only carries the contract.
    #[serde(default)]
    pub packet_char_budget_override: Option<usize>,
    #[serde(default)]
    pub packet_char_budget_model: Option<String>,
    /// Caller's accepted receipt versions, e.g. vec![1, 2]. The v2 fields are
    /// always populated; v1 will see them omitted.
    #[serde(default = "default_accepted_versions")]
    pub accepted_receipt_versions: Vec<u32>,
    /// Optional trace id override; defaults to candidate_set.trace_id.
    #[serde(default)]
    pub trace_id_override: Option<String>,
    /// Optional ScopeGrant v1 (server-minted). When present, the planner
    /// validates accepted_receipt_versions matches the grant; when absent the
    /// planner accepts receipt v1+v2 unconditionally.
    #[serde(default)]
    pub scope_grant_present: bool,
    /// Resolver mechanisms the consuming host has negotiated and can call.
    /// An empty list means Pull must choose an inline faithful alternative
    /// when content exists rather than emitting an unusable handle.
    #[serde(default)]
    pub consumer_resolvers: Vec<String>,
    /// Migration-only reserved source-kind lanes, as `(source_kind,
    /// token_cap)` pairs. `None` keeps the legacy memory/skill reservation —
    /// the isolated rollback control required by PUL-026. `Some(vec![])`
    /// retires all reserved lanes from normal selection. Composition may pass
    /// the retired value only after evidence-class coverage & allocation
    /// qualification evidence lands.
    #[serde(default)]
    pub reserved_lanes: Option<Vec<(String, usize)>>,
}

fn default_accepted_versions() -> Vec<u32> {
    vec![2]
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    Fresh,
    Stale,
    Unavailable,
    Degraded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FallbackMode {
    None,
    NonGraphSourcesOnly,
    PortableTextOnly,
}

impl FallbackMode {
    fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::NonGraphSourcesOnly => "non_graph_sources_only",
            Self::PortableTextOnly => "portable_text_only",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DegradationReason {
    None,
    BlueprintStale,
    BlueprintUnavailable,
    BlueprintCorrupt,
    ProviderCapabilityMissing,
    ReleaseGenerationMismatch,
    ReleaseGenerationUnavailable,
}

impl DegradationReason {
    fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::BlueprintStale => "blueprint_stale",
            Self::BlueprintUnavailable => "blueprint_unavailable",
            Self::BlueprintCorrupt => "blueprint_corrupt",
            Self::ProviderCapabilityMissing => "provider_capability_missing",
            Self::ReleaseGenerationMismatch => "release_generation_mismatch",
            Self::ReleaseGenerationUnavailable => "release_generation_unavailable",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPacketV1 {
    pub schema_version: u32,
    pub trace_id: String,
    pub task: String,
    pub mode: String,
    pub budget: BudgetV1,
    pub allocations: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_accounting: BTreeMap<String, ProviderAccountingV1>,
    pub blocks: Vec<BlockV1>,
    pub omissions: Vec<OmissionV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetV1 {
    pub max_tokens: usize,
    pub admitted_tokens: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet_char_budget_default: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet_char_budget_override: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet_char_budget_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured_packet_char_budget: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_packet_char_budget: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAccountingV1 {
    pub delivery_stage: DeliveryStage,
    pub selected_tokens: usize,
    pub rendered_tokens: usize,
    pub delivered_chars: usize,
    pub drop_reason: DropReason,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStage {
    Planned,
    Finalized,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryClass {
    ResolverBacked,
    MetadataOnly,
    Rendered,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    None,
    MissingResolver,
    TrustGateDropped,
    SealFailed,
    AdditionalContextCharCap,
    PacketBudgetExceeded,
    NotSelected,
    Multiple,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StatusScope {
    CandidateSet,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockV1 {
    pub id: String,
    pub layer: u8,
    pub provider: String,
    pub source_kind: String,
    pub source_ref: String,
    pub source_hash: String,
    pub trust_class: String,
    pub instruction_policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness_class: Option<FreshnessClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    pub priority: u8,
    pub estimated_tokens: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_stage: Option<DeliveryStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_class: Option<DeliveryClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_tokens: Option<usize>,
    /// Planner-owned transform target. `estimated_tokens` remains original
    /// candidate size; consumers may compress to this bounded allocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allotted_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_chars: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop_reason: Option<DropReason>,
    pub protected: bool,
    pub recoverable: bool,
    pub resolver: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextReceiptV2 {
    pub schema_version: u32,
    pub trace_id: String,
    pub id: String,
    pub layer: u8,
    pub provider: String,
    pub source_kind: String,
    pub source_ref: String,
    pub before_chars: usize,
    pub admitted_chars: usize,
    pub decision: String,
    pub reason: String,
    pub content_sha256: String,
    /// IDs absorbed into this winner during planner deduplication. IDs are
    /// content-free provenance, allowing a consumer to reconcile omission.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deduplicated_from: Vec<String>,
    /// Winning candidate ID when this candidate lost source/content deduplication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deduplicated_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness_class: Option<FreshnessClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_stage: Option<DeliveryStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_class: Option<DeliveryClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allotted_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_chars: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop_reason: Option<DropReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_scope: Option<StatusScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_provider: Option<String>,
    pub provider_status: ProviderStatus,
    pub fallback_mode: FallbackMode,
    pub degradation_reason: DegradationReason,
    pub source_generation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_release_generation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_release_generation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_generation_status: Option<ReleaseGenerationStatus>,
}

// ---- Errors ---------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlannerError {
    UnknownSchemaVersion {
        found: u32,
        supported: Vec<u32>,
    },
    EmptyTask,
    EmptyProvider,
    ReceiptVersionUnsupported {
        accepted: Vec<u32>,
    },
    /// Caller passed max_tokens = 0 or negative; we surface as 0 for parity with v1.
    ZeroBudget,
}

impl std::fmt::Display for PlannerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlannerError::UnknownSchemaVersion { found, supported } => {
                write!(f, "unknown schemaVersion {found}; supported: {supported:?}")
            }
            PlannerError::EmptyTask => write!(f, "candidate set task is empty"),
            PlannerError::EmptyProvider => write!(f, "candidate set provider is empty"),
            PlannerError::ReceiptVersionUnsupported { accepted } => write!(
                f,
                "caller accepted receipt versions {accepted:?}; planner emits v2 only"
            ),
            PlannerError::ZeroBudget => write!(f, "max_tokens must be >= 1"),
        }
    }
}

impl std::error::Error for PlannerError {}

// ---- Planner output -------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannerOutput {
    pub packet: ContextPacketV1,
    pub receipts: Vec<ContextReceiptV2>,
    pub provider_status: ProviderStatus,
    pub fallback_mode: FallbackMode,
    pub degradation_reason: DegradationReason,
    pub source_generation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_release_generation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_release_generation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_generation_status: Option<ReleaseGenerationStatus>,
    pub structured_event: StructuredFallbackEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredFallbackEvent {
    pub event: String,
    pub trace_id: String,
    pub provider: String,
    pub reason: String,
    pub mode: String,
}

// ---- Planner entry --------------------------------------------------------

pub fn plan(input: &PlannerInput) -> Result<PlannerOutput, PlannerError> {
    if input.candidate_set.schema_version != 1 {
        return Err(PlannerError::UnknownSchemaVersion {
            found: input.candidate_set.schema_version,
            supported: vec![1],
        });
    }
    if input.candidate_set.task.trim().is_empty() {
        return Err(PlannerError::EmptyTask);
    }
    if input.candidate_set.provider.trim().is_empty() {
        return Err(PlannerError::EmptyProvider);
    }
    if input.max_tokens == 0 {
        return Err(PlannerError::ZeroBudget);
    }
    if !input.accepted_receipt_versions.contains(&2) {
        return Err(PlannerError::ReceiptVersionUnsupported {
            accepted: input.accepted_receipt_versions.clone(),
        });
    }

    let freshness_stale = input.candidate_set.freshness.stale;
    let graph_degradation_reason = match input.candidate_set.freshness.graph_state {
        // Plan 1.2: a dirty worktree is informational, never a degradation. It
        // used to map to BlueprintStale, which is why an ordinary edited file put
        // every packet into portable-text fallback.
        Some(GraphState::StaleSnapshot | GraphState::PartialReindex) => {
            Some(DegradationReason::BlueprintStale)
        }
        Some(
            GraphState::MissingSnapshot | GraphState::ConcurrentUpdate | GraphState::Indeterminate,
        ) => Some(DegradationReason::BlueprintUnavailable),
        Some(GraphState::Clean | GraphState::DirtyOverlay) | None => None,
    };
    let release_degradation_reason = release_generation_degradation(&input.candidate_set.freshness);
    let provider_unsupported = input.candidate_set.provider.ends_with("-missing")
        || input.candidate_set.provider.ends_with("-unsupported");
    // Staleness is an honest freshness label, not a universal eligibility gate.
    // Release-generation or capability failures remain global fallback gates
    // because they invalidate the shared admission basis.
    let fallback_only = provider_unsupported || release_degradation_reason.is_some();
    let (fallback_mode, degradation_reason) = if provider_unsupported {
        (
            FallbackMode::NonGraphSourcesOnly,
            DegradationReason::ProviderCapabilityMissing,
        )
    } else if let Some(reason) = release_degradation_reason.clone() {
        (FallbackMode::PortableTextOnly, reason)
    } else if freshness_stale {
        // Stale graph data remains usable. The stale provider status and
        // degradation reason carry its freshness label to Pull consumers.
        (FallbackMode::None, DegradationReason::BlueprintStale)
    } else {
        (
            FallbackMode::None,
            graph_degradation_reason
                .clone()
                .unwrap_or(DegradationReason::None),
        )
    };
    let provider_status = if provider_unsupported || release_degradation_reason.is_some() {
        ProviderStatus::Unavailable
    } else if freshness_stale {
        ProviderStatus::Stale
    } else if graph_degradation_reason.is_some() {
        ProviderStatus::Degraded
    } else {
        ProviderStatus::Fresh
    };
    let source_generation = if fallback_mode == FallbackMode::None {
        input
            .candidate_set
            .freshness
            .snapshot_id
            .clone()
            .or_else(|| Some(input.candidate_set.freshness.revision.clone()))
    } else {
        None
    };

    let candidates = &input.candidate_set.candidates;
    let mut decisions: Vec<(CandidateV1, String, String)> = Vec::with_capacity(candidates.len());

    // 1. Cross-root rejection (remote_untrusted trust_class).
    let mut kept_after_root: Vec<&CandidateV1> = Vec::new();
    for cand in candidates {
        if cand.trust_class == "remote_untrusted" {
            decisions.push((cand.clone(), "rejected".into(), "cross_root".into()));
            continue;
        }
        kept_after_root.push(cand);
    }

    // 2. Version-authority filtering: IDs ending in ":superseded" or ":proposed"
    //    lose unless protected (e.g. a protected anchor pinning the historical fact).
    let mut kept_after_authority: Vec<&CandidateV1> = Vec::new();
    for cand in kept_after_root {
        if is_version_demoted(cand) && !cand.protected {
            decisions.push((cand.clone(), "rejected".into(), "superseded_version".into()));
            continue;
        }
        kept_after_authority.push(cand);
    }

    // 3. Source-class filters.
    let mut kept_after_class: Vec<&CandidateV1> = Vec::new();
    for cand in kept_after_authority {
        let cap_missing = cand
            .score_components
            .get("provider_capability_missing")
            .copied()
            .unwrap_or(0.0);
        if cap_missing > 0.0 {
            decisions.push((
                cand.clone(),
                "rejected".into(),
                "provider_capability_missing".into(),
            ));
            continue;
        }
        if cand.source_kind == "memory" {
            let structural = cand
                .score_components
                .get("structural")
                .copied()
                .unwrap_or(0.0);
            let lexical = cand.score_components.get("lexical").copied().unwrap_or(0.0);
            if structural <= 0.0 && lexical < 0.85 && !cand.protected {
                decisions.push((
                    cand.clone(),
                    "rejected".into(),
                    "memory_low_relevance".into(),
                ));
                continue;
            }
        }
        kept_after_class.push(cand);
    }

    // 3b. Consumer-capability eligibility.  A resolver-only candidate is not
    // evidence the current host can consume unless that resolver was negotiated
    // for this request.  When inline text exists, rendered delivery is the
    // faithful fallback.  Otherwise reject before ranking/budget admission and
    // surface a typed omission rather than publishing an unusable handle.
    let mut kept_after_delivery: Vec<&CandidateV1> = Vec::new();
    for cand in kept_after_class {
        let (_, drop_reason) = planner_delivery_intent(cand, &input.consumer_resolvers);
        if drop_reason == DropReason::MissingResolver {
            decisions.push((
                cand.clone(),
                "rejected".into(),
                "consumer_resolver_unavailable".into(),
            ));
            continue;
        }
        kept_after_delivery.push(cand);
    }

    // 4. Deduplicate by id (keep first occurrence), then canonical source hash
    // and normalized content. Ranking before content/source dedup makes every
    // winner deterministic rather than dependent on provider return order.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut deduped: Vec<&CandidateV1> = Vec::new();
    for cand in kept_after_delivery {
        if !seen.insert(cand.id.clone()) {
            decisions.push((cand.clone(), "rejected".into(), "duplicate_id".into()));
            continue;
        }
        deduped.push(cand);
    }

    // 5. Rank by (protected desc, providerScore desc, freshness desc, kind priority asc,
    //    exact desc, id asc). Protected admission authority must win source/content
    // deduplication before a higher-scored mutable provider can evict it.
    deduped.sort_by(|a, b| {
        let af = freshness_component(a);
        let bf = freshness_component(b);
        let ak = kind_priority(a);
        let bk = kind_priority(b);
        b.protected
            .cmp(&a.protected)
            .then_with(|| {
                b.provider_score
                    .partial_cmp(&a.provider_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then(bf.partial_cmp(&af).unwrap_or(std::cmp::Ordering::Equal))
            .then(ak.cmp(&bk))
            .then(b.exact.cmp(&a.exact))
            .then(a.id.cmp(&b.id))
    });

    let mut source_winners: BTreeMap<String, String> = BTreeMap::new();
    let mut content_winners: BTreeMap<String, String> = BTreeMap::new();
    let mut deduplicated_from: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut deduplicated_to: BTreeMap<String, String> = BTreeMap::new();
    let mut content_or_source_deduped: Vec<&CandidateV1> = Vec::new();
    for cand in deduped {
        let source_key = canonical_source_hash(&cand.source_hash);
        let content_key = normalized_content_hash(&cand.text);
        let duplicate = source_key
            .as_ref()
            .and_then(|key| {
                source_winners
                    .get(key)
                    .map(|winner| (winner, "duplicate_source_hash"))
            })
            .or_else(|| {
                content_key.as_ref().and_then(|key| {
                    content_winners
                        .get(key)
                        .map(|winner| (winner, "duplicate_normalized_content"))
                })
            });
        if let Some((winner, reason)) = duplicate {
            decisions.push((cand.clone(), "rejected".into(), reason.into()));
            deduplicated_from
                .entry(winner.clone())
                .or_default()
                .push(cand.id.clone());
            deduplicated_to.insert(cand.id.clone(), winner.clone());
            continue;
        }
        if let Some(key) = source_key {
            source_winners.insert(key, cand.id.clone());
        }
        if let Some(key) = content_key {
            content_winners.insert(key, cand.id.clone());
        }
        content_or_source_deduped.push(cand);
    }
    let deduped = content_or_source_deduped;

    // 6. Token-budget admission — RESERVED LANES then global fill (explicit policy, not a
    // temporary workaround). A fresh Blueprint repo-code result receives one block slot before
    // other providers can exhaust the packet block ceiling. Current Git identity then receives
    // two block slots before large memory/skill result sets can exhaust it. Per-source-class token lanes
    // then admit by score *within* each lane; the final pass fills remaining budget globally.
    // POLICY: raw provider_score is never treated
    // as cross-provider-comparable. Overlay's flat recency prior (~0.6) and memory's cosine
    // (~0.3–0.5) live on different scales; lanes keep them from competing for the whole budget.
    // Full recalibration is out of scope — these constants *are* the calibration substitute.
    // Lane size 0 restores single-pass behavior; unused reservation returns to pass 2.
    const BLUEPRINT_REPO_CODE_LANE_KIND: &str = "repo_code";
    const BLUEPRINT_REPO_CODE_LANE_BLOCKS: usize = 1;
    const IDENTITY_LANE_KIND: &str = "git_meta";
    const IDENTITY_LANE_BLOCKS: usize = 2;
    /// Isolated migration-rollback control (PUL-026): the legacy memory/skill
    /// reservation survives only as the `None` default for callers that have
    /// not yet qualified the evidence-class policy.
    const LEGACY_RESERVED_LANES: &[(&str, usize)] = &[("memory", 800), ("skill", 300)];
    const MAX_PACKET_BLOCKS: usize = 32;
    let mut admitted_tokens = 0usize;
    let mut admitted: Vec<&CandidateV1> = Vec::new();
    let mut lane_admitted: std::collections::HashSet<String> = std::collections::HashSet::new();
    if provider_status != ProviderStatus::Unavailable {
        for cand in deduped
            .iter()
            .filter(|candidate| {
                candidate.source_kind == BLUEPRINT_REPO_CODE_LANE_KIND
                    && candidate.provider.as_deref() == Some("blueprint")
            })
            .take(BLUEPRINT_REPO_CODE_LANE_BLOCKS)
        {
            let cost = cand.estimated_tokens;
            if admitted_tokens + cost > input.max_tokens {
                continue;
            }
            admitted_tokens += cost;
            admitted.push(*cand);
            lane_admitted.insert(cand.id.clone());
            decisions.push((
                (*cand).clone(),
                "admitted".into(),
                "within_blueprint_repo_code_lane".into(),
            ));
        }
    }
    for cand in deduped
        .iter()
        .filter(|candidate| {
            candidate.source_kind == IDENTITY_LANE_KIND
                && candidate.provider.as_deref() == Some("git")
        })
        .take(IDENTITY_LANE_BLOCKS)
    {
        if fallback_quarantines_candidate(cand, fallback_only) {
            continue;
        }
        let cost = cand.estimated_tokens;
        if admitted_tokens + cost > input.max_tokens {
            continue;
        }
        admitted_tokens += cost;
        admitted.push(*cand);
        lane_admitted.insert(cand.id.clone());
        decisions.push((
            (*cand).clone(),
            "admitted".into(),
            "within_identity_lane".into(),
        ));
    }
    let reserved_lanes: Vec<(String, usize)> = match &input.reserved_lanes {
        Some(lanes) => lanes.clone(),
        None => LEGACY_RESERVED_LANES
            .iter()
            .map(|(kind, budget)| ((*kind).to_owned(), *budget))
            .collect(),
    };
    for (lane_kind, lane_budget) in &reserved_lanes {
        let mut lane_tokens = 0usize;
        for cand in deduped
            .iter()
            .filter(|c| c.source_kind == lane_kind.as_str())
        {
            if admitted.len() >= MAX_PACKET_BLOCKS {
                continue; // pass 2 emits the packet_block_limit receipt
            }
            if fallback_quarantines_candidate(cand, fallback_only) {
                continue; // pass 2 emits the fallback_quarantine receipt
            }
            let cost = cand.estimated_tokens;
            if lane_tokens + cost > *lane_budget || admitted_tokens + cost > input.max_tokens {
                continue; // over the lane: falls through to pass 2's general fill
            }
            lane_tokens += cost;
            admitted_tokens += cost;
            admitted.push(*cand);
            lane_admitted.insert(cand.id.clone());
            decisions.push((
                (*cand).clone(),
                "admitted".into(),
                "within_reserved_lane".into(),
            ));
        }
    }
    for cand in deduped {
        if lane_admitted.contains(&cand.id) {
            continue;
        }
        if fallback_quarantines_candidate(cand, fallback_only) {
            decisions.push((
                cand.clone(),
                "rejected".into(),
                "fallback_quarantine".into(),
            ));
            continue;
        }
        if admitted.len() >= MAX_PACKET_BLOCKS {
            decisions.push((cand.clone(), "rejected".into(), "packet_block_limit".into()));
            continue;
        }
        let cost = cand.estimated_tokens;
        if admitted_tokens + cost > input.max_tokens {
            decisions.push((cand.clone(), "rejected".into(), "budget_exhausted".into()));
            continue;
        }
        admitted_tokens += cost;
        admitted.push(cand);
        decisions.push((
            cand.clone(),
            "admitted".into(),
            "within_global_budget".into(),
        ));
    }

    // 7. Build packet blocks.
    let trace_id = input
        .trace_id_override
        .clone()
        .unwrap_or_else(|| input.candidate_set.trace_id.clone());

    let mut allocations: BTreeMap<String, usize> = BTreeMap::new();
    allocations.insert("3".into(), 0);
    allocations.insert("7".into(), 0);
    allocations.insert("other".into(), 0);
    let mut provider_accounting: BTreeMap<String, ProviderAccountingV1> = BTreeMap::new();
    let mut blocks: Vec<BlockV1> = Vec::new();
    let allotments = score_proportional_allotments(&admitted);
    for cand in &admitted {
        let layer_key = match cand.layer {
            3 => "3",
            7 => "7",
            _ => "other",
        };
        allocations
            .entry(layer_key.into())
            .and_modify(|v| *v += cand.estimated_tokens)
            .or_insert(cand.estimated_tokens);
        let provider = candidate_provider(cand, &input.candidate_set.provider);
        let (delivery_class, drop_reason) =
            planner_delivery_intent(cand, &input.consumer_resolvers);
        provider_accounting
            .entry(provider.clone())
            .and_modify(|accounting| {
                accounting.selected_tokens += cand.estimated_tokens;
                accounting.drop_reason = combine_drop_reasons(accounting.drop_reason, drop_reason);
            })
            .or_insert(ProviderAccountingV1 {
                delivery_stage: DeliveryStage::Planned,
                selected_tokens: cand.estimated_tokens,
                rendered_tokens: 0,
                delivered_chars: 0,
                drop_reason,
            });
        blocks.push(BlockV1 {
            id: cand.id.clone(),
            layer: cand.layer,
            provider,
            source_kind: cand.source_kind.clone(),
            source_ref: cand.source_ref.clone(),
            source_hash: cand.source_hash.clone(),
            trust_class: cand.trust_class.clone(),
            instruction_policy: cand.instruction_policy.clone(),
            base_commit: cand.base_commit.clone(),
            overlay_digest: cand.overlay_digest.clone(),
            freshness_class: cand.freshness_class,
            snapshot_id: cand.snapshot_id.clone(),
            priority: (cand.provider_score * 100.0).round() as u8,
            estimated_tokens: cand.estimated_tokens,
            allotted_tokens: allotments.get(&cand.id).copied(),
            delivery_stage: Some(DeliveryStage::Planned),
            delivery_class: Some(delivery_class),
            selected_tokens: Some(cand.estimated_tokens),
            rendered_tokens: Some(0),
            delivered_chars: Some(0),
            drop_reason: Some(drop_reason),
            protected: cand.protected,
            recoverable: cand.recoverable,
            resolver: cand.resolver.clone(),
            text: cand.text.clone(),
        });
    }

    const DEFAULT_PACKET_CHAR_BUDGET: usize = 30_000;
    let packet_char_budget_override = input.packet_char_budget_override.filter(|value| *value > 0);
    let configured_packet_char_budget =
        packet_char_budget_override.unwrap_or(DEFAULT_PACKET_CHAR_BUDGET);
    let mut packet_omissions = input.candidate_set.omissions.clone();
    packet_omissions.extend(
        decisions
            .iter()
            .filter_map(|(candidate, decision, reason)| {
                (decision == "rejected" && reason == "consumer_resolver_unavailable").then(|| {
                    OmissionV1 {
                        id: candidate.id.clone(),
                        layer: Some(candidate.layer),
                        reason: reason.clone(),
                        detail_id: None,
                        stage: None,
                    }
                })
            }),
    );

    let packet = ContextPacketV1 {
        schema_version: 1,
        trace_id: trace_id.clone(),
        task: input.candidate_set.task.clone(),
        mode: input.candidate_set.mode.clone(),
        budget: BudgetV1 {
            max_tokens: input.max_tokens,
            admitted_tokens,
            packet_char_budget_default: Some(DEFAULT_PACKET_CHAR_BUDGET),
            packet_char_budget_override,
            packet_char_budget_model: input
                .packet_char_budget_model
                .as_ref()
                .filter(|value| !value.trim().is_empty())
                .cloned(),
            configured_packet_char_budget: Some(configured_packet_char_budget),
            // The pure planner has no rendered fragments or door prefix/header. Python/JS
            // finalization writes the effective value once exact remaining capacity is known.
            effective_packet_char_budget: None,
        },
        allocations,
        provider_accounting,
        blocks,
        omissions: packet_omissions,
    };

    // 8. Receipts. One per decision. Receipts are content-free: text is
    //    banned by schema; the planner never serializes raw prompt or repo
    //    content into the receipt payload.
    let mut receipts = Vec::with_capacity(decisions.len());
    for (cand, decision, reason) in decisions {
        let admitted = decision == "admitted";
        let before_chars = cand.estimated_tokens.saturating_mul(4).max(1);
        let admitted_chars = if admitted { before_chars } else { 0 };
        let (delivery_class, selected_tokens, drop_reason) = if admitted {
            let (delivery_class, drop_reason) =
                planner_delivery_intent(&cand, &input.consumer_resolvers);
            (delivery_class, cand.estimated_tokens, drop_reason)
        } else {
            (DeliveryClass::MetadataOnly, 0, DropReason::NotSelected)
        };
        let content_sha = format!("sha256:{}", sha256_hex(&cand.text));
        receipts.push(ContextReceiptV2 {
            schema_version: 2,
            trace_id: trace_id.clone(),
            id: cand.id.clone(),
            layer: cand.layer,
            provider: candidate_provider(&cand, &input.candidate_set.provider),
            source_kind: cand.source_kind.clone(),
            source_ref: cand.source_ref.clone(),
            before_chars,
            admitted_chars,
            decision,
            reason,
            content_sha256: content_sha,
            deduplicated_from: deduplicated_from.remove(&cand.id).unwrap_or_default(),
            deduplicated_to: deduplicated_to.remove(&cand.id),
            base_commit: cand.base_commit.clone(),
            overlay_digest: cand.overlay_digest.clone(),
            freshness_class: cand.freshness_class,
            snapshot_id: cand.snapshot_id.clone(),
            delivery_stage: Some(DeliveryStage::Planned),
            delivery_class: Some(delivery_class),
            selected_tokens: Some(selected_tokens),
            allotted_tokens: admitted
                .then(|| allotments.get(&cand.id).copied())
                .flatten(),
            rendered_tokens: Some(0),
            delivered_chars: Some(0),
            drop_reason: Some(drop_reason),
            status_scope: Some(StatusScope::CandidateSet),
            status_provider: Some(input.candidate_set.provider.clone()),
            provider_status: provider_status.clone(),
            fallback_mode: fallback_mode.clone(),
            degradation_reason: degradation_reason.clone(),
            source_generation: source_generation.clone(),
            expected_release_generation: input
                .candidate_set
                .freshness
                .expected_release_generation
                .clone(),
            observed_release_generation: input
                .candidate_set
                .freshness
                .observed_release_generation
                .clone(),
            release_generation_status: input.candidate_set.freshness.release_generation_status,
        });
    }

    let structured_event = StructuredFallbackEvent {
        event: "context_fallback".into(),
        trace_id: trace_id.clone(),
        provider: input.candidate_set.provider.clone(),
        reason: degradation_reason.as_str().into(),
        mode: fallback_mode.as_str().into(),
    };

    Ok(PlannerOutput {
        packet,
        receipts,
        provider_status,
        fallback_mode,
        degradation_reason,
        source_generation,
        expected_release_generation: input
            .candidate_set
            .freshness
            .expected_release_generation
            .clone(),
        observed_release_generation: input
            .candidate_set
            .freshness
            .observed_release_generation
            .clone(),
        release_generation_status: input.candidate_set.freshness.release_generation_status,
        structured_event,
    })
}

fn fallback_quarantines_candidate(candidate: &CandidateV1, fallback_only: bool) -> bool {
    if !fallback_only || candidate.protected || candidate.source_kind == "portable_text_fallback" {
        return false;
    }
    true
}

fn release_generation_degradation(freshness: &FreshnessV1) -> Option<DegradationReason> {
    let expected = freshness.expected_release_generation.as_deref();
    let observed = freshness.observed_release_generation.as_deref();
    let status = freshness.release_generation_status;
    if expected.is_none() && observed.is_none() && status.is_none() {
        return None;
    }

    let expected_valid = expected.is_some_and(valid_release_generation);
    let observed_valid = observed.is_some_and(valid_release_generation);
    if expected_valid
        && observed_valid
        && expected == observed
        && status == Some(ReleaseGenerationStatus::Matched)
    {
        return None;
    }
    if status == Some(ReleaseGenerationStatus::Mismatch)
        || (expected_valid && observed_valid && expected != observed)
    {
        return Some(DegradationReason::ReleaseGenerationMismatch);
    }
    Some(DegradationReason::ReleaseGenerationUnavailable)
}

fn valid_release_generation(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

// ---- Helpers --------------------------------------------------------------

fn freshness_component(cand: &CandidateV1) -> f64 {
    cand.score_components
        .get("freshness")
        .copied()
        .unwrap_or(1.0)
}

fn is_version_demoted(cand: &CandidateV1) -> bool {
    cand.id.ends_with(":superseded") || cand.id.ends_with(":proposed")
}

fn kind_priority(cand: &CandidateV1) -> u8 {
    match cand.source_kind.as_str() {
        "repo_code" | "repo_code_overlay" => 0,
        "handoff_envelope"
        | "audit_finding"
        | "architect_decision"
        | "portable_text_fallback"
        | "skill" => 1,
        "doc" => 2,
        "memory" => 3,
        _ => 2,
    }
}

/// Allocate each admitted source-kind lane proportionally to scores normalized
/// within its provider. This retains each provider's relative ordering while
/// stopping incompatible provider scales from deciding cross-provider shares.
/// Largest-remainder rounding is deterministic by id & never changes a lane's
/// total admitted budget.
fn score_proportional_allotments(candidates: &[&CandidateV1]) -> BTreeMap<String, usize> {
    let mut by_lane: BTreeMap<&str, Vec<&CandidateV1>> = BTreeMap::new();
    for candidate in candidates {
        by_lane
            .entry(candidate.source_kind.as_str())
            .or_default()
            .push(*candidate);
    }
    let mut out = BTreeMap::new();
    for lane in by_lane.values_mut() {
        lane.sort_by(|left, right| left.id.cmp(&right.id));
        let total = lane
            .iter()
            .map(|candidate| candidate.estimated_tokens)
            .sum::<usize>();
        let mut provider_maxima: BTreeMap<&str, f64> = BTreeMap::new();
        for candidate in lane.iter() {
            let provider = candidate.provider.as_deref().unwrap_or("").trim();
            let score = candidate.provider_score.max(0.0);
            provider_maxima
                .entry(provider)
                .and_modify(|maximum| *maximum = maximum.max(score))
                .or_insert(score);
        }
        let weights = lane
            .iter()
            .map(|candidate| {
                let provider = candidate.provider.as_deref().unwrap_or("").trim();
                let maximum = provider_maxima[provider];
                if maximum > 0.0 {
                    candidate.provider_score.max(0.0) / maximum
                } else {
                    0.0
                }
            })
            .collect::<Vec<_>>();
        let weight_total = weights.iter().sum::<f64>();
        let denominator = if weight_total > 0.0 {
            weight_total
        } else {
            lane.len() as f64
        };
        let mut allocated = 0usize;
        let mut fractions = Vec::new();
        for (index, candidate) in lane.iter().enumerate() {
            let weight = if weight_total > 0.0 {
                weights[index]
            } else {
                1.0
            };
            let exact = total as f64 * weight / denominator;
            let floor = exact.floor() as usize;
            allocated += floor;
            out.insert(candidate.id.clone(), floor);
            fractions.push((index, exact - floor as f64));
        }
        fractions.sort_by(
            |(left_index, left_fraction), (right_index, right_fraction)| {
                right_fraction
                    .partial_cmp(left_fraction)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(lane[*left_index].id.cmp(&lane[*right_index].id))
            },
        );
        for (index, _) in fractions.into_iter().take(total.saturating_sub(allocated)) {
            *out.entry(lane[index].id.clone()).or_default() += 1;
        }
    }
    out
}

fn sha256_hex(s: &str) -> String {
    rightkit_fs::digest::sha256_bytes(s.as_bytes())
}

fn canonical_source_hash(source_hash: &str) -> Option<String> {
    let digest = source_hash
        .trim()
        .strip_prefix("sha256:")
        .or_else(|| source_hash.trim().strip_prefix("SHA256:"))?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("sha256:{}", digest.to_ascii_lowercase()))
}

fn normalized_content_hash(text: &str) -> Option<String> {
    let normalized = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    let normalized = normalized.trim();
    (!normalized.is_empty()).then(|| sha256_hex(normalized))
}

fn candidate_provider(candidate: &CandidateV1, fallback: &str) -> String {
    candidate
        .provider
        .as_deref()
        .filter(|provider| !provider.trim().is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn planner_delivery_intent(
    candidate: &CandidateV1,
    consumer_resolvers: &[String],
) -> (DeliveryClass, DropReason) {
    let resolver = candidate.resolver.trim();
    if resolver.is_empty() {
        return if candidate.text.trim().is_empty() {
            (DeliveryClass::MetadataOnly, DropReason::MissingResolver)
        } else {
            (DeliveryClass::Rendered, DropReason::None)
        };
    }
    let callable = consumer_resolvers.iter().any(|capability| {
        let capability = capability.trim();
        !capability.is_empty()
            && (resolver == capability
                || resolver.starts_with(&format!("{capability}:"))
                || resolver.starts_with(&format!("{capability} ")))
    });
    if callable {
        (DeliveryClass::ResolverBacked, DropReason::None)
    } else if !candidate.text.trim().is_empty() {
        (DeliveryClass::Rendered, DropReason::None)
    } else {
        (DeliveryClass::MetadataOnly, DropReason::MissingResolver)
    }
}

fn combine_drop_reasons(left: DropReason, right: DropReason) -> DropReason {
    if left == right {
        left
    } else {
        DropReason::Multiple
    }
}

// ---- Tests ---------------------------------------------------------------
