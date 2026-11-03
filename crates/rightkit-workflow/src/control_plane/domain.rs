//! Explicit IDs, types, states, commands, events, and failure classes for the
//! workflow control plane.
//!
//! The model is intentionally **string-free for state transitions**. Every
//! state and failure is an enum variant. This is the only acceptable shape:
//! the reducer and the compiler must reject unknown transition strings at
//! the type system, not at runtime.
//!
//! `WP-01-T01` acceptance: explicit IDs and types for definitions, versions,
//! triggers, events, occurrences, runs, nodes, attempts, waits, approvals,
//! effects, claims, evidence, deliveries, and attention. State and failure
//! classes are enums with no free-form transition strings.
//!
//! This vocabulary keeps compiler, reducer, storage, and runtime wiring on
//! strongly typed values.

use serde::{Deserialize, Serialize};

use crate::control_plane::ids::{NamespacedId, Prefix};

// ─── IDs ─────────────────────────────────────────────────────────────────────

/// `wfd_*` — mutable identity and ownership metadata.
pub type WorkflowId = NamespacedId;
/// `wfv_*` — immutable canonical specification.
pub type WorkflowVersionId = NamespacedId;
/// `wft_*` — manual / one-time / cron / webhook / internal trigger.
pub type TriggerId = NamespacedId;
/// `wfo_*` — one trigger decision before run admission.
pub type OccurrenceId = NamespacedId;
/// `wfr_*` — pinned-version lifecycle state.
pub type RunId = NamespacedId;
/// `wfn_*` — one materialized node instance (incl. map/loop lineage).
pub type NodeId = NamespacedId;
/// `wfa_*` — one leased executor attempt with generation fencing.
pub type AttemptId = NamespacedId;
/// `wfw_*` — durable timer or event wait.
pub type WaitId = NamespacedId;
/// `wfp_*` — principal/role/expiry-bound decision.
pub type ApprovalId = NamespacedId;
/// `wfe_*` — durable external side-effect intent.
pub type EffectId = NamespacedId;
/// `wf_attn_*` — projection for approvals, input, failures, etc.
pub type AttentionId = NamespacedId;
/// `wf_delivery_*` — user-facing delivery record.
pub type DeliveryId = NamespacedId;
/// `wf_evt_*` — admitted trigger event (normalized).
pub type TriggerEventId = NamespacedId;
/// Stable claim identity bound to one node attempt.
pub type ClaimId = NamespacedId;
/// Stable evidence identity for reducer-visible observations.
pub type EvidenceId = NamespacedId;

macro_rules! id_helpers {
    ($($fn:ident, $prefix:ident, $ty:ty);* $(;)?) => {
        $(
            #[inline]
            pub fn $fn(value: impl Into<String>) -> $ty {
                NamespacedId::new(Prefix::$prefix, value)
            }
        )*
    };
}

id_helpers! {
    workflow_id,            WorkflowDefinition, WorkflowId;
    workflow_version_id,    WorkflowVersion,    WorkflowVersionId;
    trigger_id,             WorkflowTrigger,    TriggerId;
    occurrence_id,          WorkflowOccurrence, OccurrenceId;
    run_id,                 WorkflowRun,        RunId;
    node_id,                NodeRun,            NodeId;
    attempt_id,             NodeAttempt,        AttemptId;
    wait_id,                Wait,               WaitId;
    approval_id,            Approval,           ApprovalId;
    effect_id,              EffectIntent,       EffectId;
    attention_id,           AttentionItem,      AttentionId;
    delivery_id,            Delivery,           DeliveryId;
    trigger_event_id,       TriggerEvent,       TriggerEventId;
}

/// Explicit helper matching the domain vocabulary used by compiler inputs.
/// Keep `workflow_id` as backwards-compatible shorthand for callers that
/// already use it, while exposing the unambiguous definition name.
#[inline]
pub fn workflow_definition_id(value: impl Into<String>) -> WorkflowId {
    workflow_id(value)
}

#[inline]
pub fn claim_id(value: impl Into<String>) -> ClaimId {
    NamespacedId::new(Prefix::Claim, value)
}

#[inline]
pub fn evidence_id(value: impl Into<String>) -> EvidenceId {
    NamespacedId::new(Prefix::Evidence, value)
}

// ─── Hashes ──────────────────────────────────────────────────────────────────

/// Stable, content-bound hash. Always rendered as `sha256:<hex>` so the
/// canonical form is unambiguous across crates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct ContentHash(String);

impl ContentHash {
    /// Compute the canonical hash of a canonical-JSON byte slice.
    pub fn sha256(bytes: &[u8]) -> Self {
        let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
        hasher.update(bytes);
        Self(format!("sha256:{}", hasher.finalize()))
    }

    /// Borrow the underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ─── Revision / monotonic counter ───────────────────────────────────────────

/// Optimistic revision counter. Every durable object carries one. The
/// reducer only accepts a command whose `expected_revision` matches the
/// current projection revision, or whose `commandId` has already been
/// recorded.
#[derive(
    Debug, Copy, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(transparent)]
pub struct Revision(pub u64);

impl Revision {
    /// Initial revision (no commands yet).
    pub const ZERO: Self = Self(0);
    /// Bump the revision after a successful reducer pass.
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl std::fmt::Display for Revision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "r{}", self.0)
    }
}

// ─── Capabilities ────────────────────────────────────────────────────────────

/// A single capability string. Examples: `fs.read_workspace`,
/// `network.fetch`, `effect.publish`, `effect.merge`. The set is the
/// intersection of product, organization, workspace, workflow, node,
/// runtime, and operator policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct Capability(String);

impl Capability {
    /// Build a capability from a string. Caller is responsible for
    /// validating that the string matches the product capability registry.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    /// Borrow the underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Effective capability set for a node attempt. Computed by the compiler at
/// compile time and re-checked by the reducer before every dispatch.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct CapabilitySet {
    caps: Vec<Capability>,
}

impl CapabilitySet {
    /// Build from a list. Duplicates are removed; order is canonicalized
    /// (lexicographic) so canonical hashing is stable.
    #[allow(clippy::should_implement_trait)]
    pub fn from_iter<I: IntoIterator<Item = Capability>>(iter: I) -> Self {
        let mut caps: Vec<_> = iter.into_iter().collect();
        caps.sort();
        caps.dedup();
        Self { caps }
    }

    /// True when `cap` is in the set.
    pub fn contains(&self, cap: &Capability) -> bool {
        self.caps.binary_search(cap).is_ok()
    }

    /// Borrow the underlying slice.
    pub fn as_slice(&self) -> &[Capability] {
        &self.caps
    }

    /// Empty capability set.
    pub fn empty() -> Self {
        Self { caps: Vec::new() }
    }
}

// ─── Node kinds ──────────────────────────────────────────────────────────────

/// The supported V1 node kinds. Each maps to one bounded executor or
/// deterministic control node. See
/// `docs/pending/workflow-control-plane.md` § "Node
/// kinds for V1".
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// Adaptive agent: native, Codex, Claude, or bounded conductor.
    Agent,
    /// Existing recipe execution behind a node adapter.
    Recipe,
    /// Deterministic bounded transformation.
    Transform,
    /// Boolean predicate; no model evaluation.
    Condition,
    /// Joins upstream branches.
    Join,
    /// Bounded `map` over a list.
    Map,
    /// Bounded progress-checked `loop`.
    Loop,
    /// Durable timer.
    WaitTimer,
    /// Durable event wait.
    WaitEvent,
    /// Principal/role/expiry-bound decision over exact hashes.
    Approval,
    /// Effect: draft/review-only adapter with reconciliation.
    Effect,
    /// Subworkflow: published version pinned at compile/admission time.
    Subworkflow,
}

/// A node in the workflow graph. Its closed fields are compiled into an
/// immutable execution plan; executor-specific data stays outside this pure
/// lifecycle model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NodeSpec {
    /// Stable id within the workflow version (`n_<ulid>`).
    pub id: String,
    /// Node kind.
    pub kind: NodeKind,
    /// Optional human-readable label.
    pub label: Option<String>,
    /// Capabilities declared by the node.
    pub capabilities: CapabilitySet,
    /// Edge targets (typed by edge kind in WP-04).
    pub edges: Vec<EdgeSpec>,
    /// Bound node retry policy.
    pub retry: RetryPolicy,
    #[serde(default)]
    pub config: NodeConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeConfig {
    #[default]
    None,
    Execution {
        runtime: String,
        #[serde(default)]
        instruction: String,
        #[serde(default)]
        runtime_target: Option<String>,
        #[serde(default)]
        recipe_ref: Option<String>,
        #[serde(default)]
        inputs_json: String,
        #[serde(default)]
        transform_operation: Option<String>,
        #[serde(default)]
        template: Option<String>,
        max_runtime_ms: u64,
        max_output_bytes: u64,
        output_schema: String,
    },
    Map {
        max_items: u32,
    },
    Loop {
        max_iterations: u32,
        per_iteration_budget: u64,
        progress_fingerprint: String,
        no_progress_limit: u32,
    },
    Wait {
        max_matches: u32,
        timeout_ms: u64,
        #[serde(default)]
        topic: Option<String>,
        #[serde(default)]
        correlation_key: Option<String>,
        #[serde(default)]
        filter_expression: Option<crate::control_plane::expression::Expr>,
        #[serde(default)]
        filter_hash: Option<String>,
    },
    Approval {
        role: String,
        expires_after_ms: u64,
    },
    Effect {
        adapter: String,
        operation: String,
        draft_only: bool,
        max_attempts: u32,
    },
    Condition {
        predicate: String,
    },
    Join {
        mode: String,
    },
}

/// Outgoing edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EdgeSpec {
    /// `n_<ulid>` of the target node.
    pub target: String,
    /// When `Some`, edge is taken only when the named predicate is true.
    pub when: Option<String>,
}

/// Per-node retry policy. Model prose cannot widen this; the compiler
/// enforces the `max_attempts` and `max_wall_clock_ms` ceilings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RetryPolicy {
    /// Total attempts including the first one. Must be `>= 1`.
    pub max_attempts: u32,
    /// Backoff strategy. `Fixed` requires a constant delay; `Exponential`
    /// requires a base and a cap.
    pub backoff: BackoffStrategy,
    /// Failure classes that may retry. Anything not in this list is
    /// terminal on first occurrence.
    pub retryable: Vec<FailureClass>,
}

/// Backoff strategies available in V1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BackoffStrategy {
    /// Constant delay of `delay_ms`.
    Fixed { delay_ms: u64 },
    /// Exponential with `base_ms` and capped at `max_ms`. Optional `jitter`
    /// in `[0.0, 1.0]` adds proportional randomness.
    Exponential {
        base_ms: u64,
        max_ms: u64,
        jitter: Option<f64>,
    },
}

// ─── Workflow version + definition ──────────────────────────────────────────

/// Mutable identity and ownership metadata for a workflow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinition {
    pub id: WorkflowId,
    pub name: String,
    pub description: String,
    pub owner_scope: String,
    pub revision: Revision,
    pub created_at: String,
    pub updated_at: String,
}

/// Immutable canonical specification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowVersion {
    pub id: WorkflowVersionId,
    pub definition_id: WorkflowId,
    pub version_no: u32,
    pub spec_version: String,
    pub canonical_json: String,
    pub content_hash: ContentHash,
    pub compiled_plan: CompiledPlan,
    pub diagnostics: Vec<Diagnostic>,
    pub publication_state: PublicationState,
    pub created_by: String,
    pub created_at: String,
    pub published_at: Option<String>,
}

/// Publication state of a version. Once `Published`, a version is
/// immutable; further edits produce a new version.
#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PublicationState {
    Draft,
    Published,
    Retired,
}

/// Compile-time diagnostic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
    pub node_id: Option<String>,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
}

/// Deterministic compiled plan persisted with each immutable version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompiledPlan {
    pub entry_node: String,
    pub nodes: Vec<NodeSpec>,
    pub effective_capabilities: CapabilitySet,
}

// ─── Failure taxonomy ───────────────────────────────────────────────────────

/// Failure classification. Used by the reducer to decide whether a
/// transient attempt may retry, must escalate, or is terminal.
///
/// See `docs/pending/workflow-control-plane-support/SECURITY-RELIABILITY-EVALUATION.md` § "Failure
/// taxonomy".
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum FailureClass {
    /// Invalid definition / binding / output. No automatic retry.
    InvalidDefinition,
    InvalidBinding,
    InvalidOutput,
    /// Policy / tool denial. No automatic retry.
    PolicyDenied,
    /// Transient provider / network / lease / executor problem. Bounded
    /// retry if declared.
    TransientProvider,
    TransientNetwork,
    LeaseLost,
    ExecutorUnavailable,
    /// Hard budget / deadline / cancellation. Terminal or attention.
    BudgetExceeded,
    DeadlineExceeded,
    Cancelled,
    /// Review rejected or no progress. Bounded repair/escalation.
    VerificationRejected,
    NoProgress,
    /// External effect reconciliation result.
    EffectOutcomeUnknown,
    /// Workspace state problem.
    WorkspaceConflict,
    WorkspaceUnavailable,
    SecretUnavailable,
    /// Storage integrity problem — dispatch must stop.
    StorageIntegrity,
    JournalProjectionMismatch,
}

// ─── Compiler / reducer outcomes ─────────────────────────────────────────────

/// Outcome of compiling a workflow definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CompilerOutcome {
    /// Definition compiled successfully into an immutable workflow version.
    Compiled {
        version_id: WorkflowVersionId,
        content_hash: ContentHash,
        plan: CompiledPlan,
    },
    /// Compilation failed with diagnostics.
    Failed { diagnostics: Vec<Diagnostic> },
}

/// Outcome of running the reducer on one command against one state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ReducerOutcome {
    /// Command applied; state advanced to `next_state` at `next_revision`.
    Applied {
        next_state: Box<LifecycleState>,
        next_revision: Revision,
        journal_events: Vec<JournalEvent>,
    },
    /// Command was a no-op (idempotent replay).
    Duplicate { observed_revision: Revision },
    /// Command was refused; state unchanged.
    Rejected {
        reason: crate::control_plane::reducer::ReducerError,
        observed_revision: Revision,
    },
}

/// Snapshot of the lifecycle state after a reducer pass. The shape evolves
/// in WP-01-T03 once the reducer is wired.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LifecycleState {
    pub workflow_id: WorkflowId,
    pub version_id: WorkflowVersionId,
    pub revision: Revision,
    pub phase: LifecyclePhase,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LifecyclePhase {
    Pending,
    Running,
    Suspended,
    Cancelled,
    Succeeded,
    Failed,
    /// Dispatch halted pending journal/projection rebuild.
    Degraded,
}

/// One journal event. The reducer emits a sequence of these; the storage
/// layer appends them in one transaction with the projection update.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JournalEvent {
    pub event_id: String,
    pub run_id: RunId,
    pub sequence: u64,
    pub kind: String,
    pub payload_json: String,
    pub content_hash: ContentHash,
    pub occurred_at_ms: i64,
}
