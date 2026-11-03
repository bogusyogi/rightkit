//! PL-009 — durable external-observation facts and corrective TaskGraph work.
//!
//! The loop deliberately has two phases:
//!
//! 1. [`ExternalObservationLoop::observe`] normalizes and records an external
//!    fact.  Callers persist [`ExternalObservationLoop::snapshot`] at this
//!    boundary.
//! 2. [`ExternalObservationLoop::deliver_pending`] re-checks controller,
//!    trust, policy, approval, and the *current* owner before composing a
//!    corrective task through PL-006's task-list projection.
//!
//! This ordering is important.  A poll is not an action, a repeated poll is
//! not a new action, and a failed/denied delivery must not erase the fact that
//! caused it.  `TaskNode::open_questions` carries the reverse causal link;
//! the fact carries the canonical corrective task id.
//!
//! Donor lineage (`REPAIR/WIRE`, canon PL-009): Prime Agent's observation /
//! heartbeat work (commit `2c30b48b`, `packages/coding-agent/src/core/
//! agent-observe.ts` and the orchestration-heartbeat skill) supplied the
//! observation-loop shape.  No donor storage or task implementation is used:
//! durable facts are native to this module and corrective work is composed
//! through RightKit's canonical `task_list_projection` seam.

use std::collections::BTreeMap;

use rightkit_agent_engine_protocol::{TaskNode, TaskState};
use rightkit_agent_orchestration::coding::task_list_projection::{
    assign_owner, create_task, mark_active, mark_done, update_task, CreateTaskInput,
    ProjectionError, TaskPatch,
};
use serde::{Deserialize, Serialize};

/// Version of the serialized observation ledger.
pub const OBSERVATION_LEDGER_VERSION: u16 = 1;

/// The external signal classes PL-009 turns into corrective work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ObservationKind {
    #[default]
    CiFailure,
    ReviewFinding,
    MergeConflict,
    CheckRegression,
}

impl ObservationKind {
    /// Deterministically classify a provider/source label and signal summary.
    /// Callers with a provider-native enum may set `ExternalObservationInput::kind`
    /// directly; this helper keeps string-based pollers on the same taxonomy.
    pub fn classify(source: &str, signal: &str) -> Self {
        let text = format!("{} {}", source, signal).to_ascii_lowercase();
        if text.contains("conflict") || text.contains("merge failure") {
            Self::MergeConflict
        } else if text.contains("review")
            || text.contains("finding")
            || text.contains("pull request")
        {
            Self::ReviewFinding
        } else if text.contains("regression") {
            Self::CheckRegression
        } else {
            Self::CiFailure
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::CiFailure => "ciFailure",
            Self::ReviewFinding => "reviewFinding",
            Self::MergeConflict => "mergeConflict",
            Self::CheckRegression => "checkRegression",
        }
    }
}

/// Whether the latest observation says that work is still required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ObservationState {
    Active,
    Resolved,
}

/// An external observation received from a poller, webhook, or supervisor.
///
/// `external_id` should be the provider's stable event/check/finding id.  If
/// it is empty, the loop falls back to the stable subject/evidence identity;
/// mutable summary text is intentionally not part of that fallback when an
/// evidence digest is supplied.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalObservationInput {
    pub source: String,
    #[serde(default)]
    pub external_id: String,
    pub workspace_root: String,
    pub subject: String,
    pub kind: ObservationKind,
    pub summary: String,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub evidence_digest: String,
    pub actionable: bool,
    pub trusted: bool,
    pub observed_at: String,
}

/// The durable fact for one deduplicated external observation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalObservationFact {
    pub id: String,
    pub dedup_key: String,
    pub source: String,
    pub external_id: String,
    pub workspace_root: String,
    pub subject: String,
    pub kind: ObservationKind,
    pub summary: String,
    pub evidence: String,
    pub evidence_digest: String,
    pub actionable: bool,
    pub state: ObservationState,
    pub trusted: bool,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub revision: u64,
    /// Increments for each false -> true actionability transition.
    pub actionability_epoch: u64,
    /// True only after the corrective task for the current epoch has been
    /// durably associated with this fact.
    pub reaction_emitted: bool,
    pub reaction_task_id: Option<String>,
    /// True once a resolved fact has been reflected in its corrective task.
    #[serde(default)]
    pub resolution_emitted: bool,
    /// The controller epoch that observed the latest poll.  It is audit data;
    /// delivery is authorized by the current lease, never by this field.
    pub observed_by_controller_epoch: u64,
}

/// The result of recording one observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationDisposition {
    New,
    Duplicate,
    Updated,
    Resolved,
    Reopened,
}

#[derive(Debug, Clone)]
pub struct ObservationReceipt {
    pub observation_id: String,
    pub disposition: ObservationDisposition,
    pub newly_actionable: bool,
    pub fact: ExternalObservationFact,
}

/// Serialized durable state.  Keep this type public so the daemon's storage
/// owner can put it beside its other session state without this loop owning a
/// second database or task store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationLedger {
    pub version: u16,
    /// Durable controller fence. Restore advances it before accepting work.
    #[serde(default = "default_controller_epoch")]
    pub controller_epoch: u64,
    pub facts: BTreeMap<String, ExternalObservationFact>,
}

fn default_controller_epoch() -> u64 {
    1
}

impl Default for ObservationLedger {
    fn default() -> Self {
        Self {
            version: OBSERVATION_LEDGER_VERSION,
            controller_epoch: default_controller_epoch(),
            facts: BTreeMap::new(),
        }
    }
}

/// Fail-closed gates for corrective work.  These are checked at delivery,
/// not when the external fact is received.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReactionPolicy {
    pub workspace_trusted: bool,
    pub policy_allows: bool,
    pub approval_granted: bool,
}

impl ReactionPolicy {
    /// Convenient explicit opt-in for a caller that has already performed the
    /// daemon's normal trust/policy/approval checks.
    pub const fn allow_all() -> Self {
        Self {
            workspace_trusted: true,
            policy_allows: true,
            approval_granted: true,
        }
    }
}

/// A lease identifies the currently active observation controller.  A stale
/// lease can never deliver a reaction after takeover/rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerLease {
    epoch: u64,
}

impl ControllerLease {
    pub const fn epoch(self) -> u64 {
        self.epoch
    }
}

/// Owner resolution is deliberately a delivery-time operation.  Ownership
/// hints from an external event are never persisted as authority.
pub trait ObservationOwnerResolver {
    fn resolve_owner(&self, fact: &ExternalObservationFact) -> Option<String>;
}

impl<F> ObservationOwnerResolver for F
where
    F: Fn(&ExternalObservationFact) -> Option<String>,
{
    fn resolve_owner(&self, fact: &ExternalObservationFact) -> Option<String> {
        self(fact)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrectiveWork {
    pub observation_id: String,
    pub task_id: String,
    pub owner: String,
    pub actionability_epoch: u64,
    pub created: bool,
    pub reopened: bool,
    pub resolved: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ObservationError {
    #[error("invalid external observation: {0}")]
    InvalidInput(String),
    #[error("observation ledger serialization failed: {0}")]
    Serialization(String),
    #[error("unsupported observation ledger version: {0}")]
    UnsupportedVersion(u16),
    #[error("stale observation controller lease (current epoch {current}, got {got})")]
    StaleController { current: u64, got: u64 },
    #[error("observation input is not trusted")]
    UntrustedInput,
    #[error("workspace trust is not established")]
    WorkspaceUntrusted,
    #[error("policy denied corrective work")]
    PolicyDenied,
    #[error("approval is required for corrective work")]
    ApprovalRequired,
    #[error("no current owner could be resolved for observation {0}")]
    OwnerUnavailable(String),
    #[error("corrective task id collision: {0}")]
    TaskIdCollision(String),
    #[error("task projection failed: {0}")]
    Projection(#[from] ProjectionError),
}

/// In-memory working view of the durable ledger.  The daemon owns persistence
/// by writing snapshots; this type owns no filesystem, database, or parallel
/// task store.
#[derive(Debug, Clone)]
pub struct ExternalObservationLoop {
    ledger: ObservationLedger,
    controller_epoch: u64,
}

impl Default for ExternalObservationLoop {
    fn default() -> Self {
        Self::new()
    }
}

impl ExternalObservationLoop {
    pub fn new() -> Self {
        Self {
            ledger: ObservationLedger::default(),
            controller_epoch: 1,
        }
    }

    pub fn from_ledger(ledger: ObservationLedger) -> Result<Self, ObservationError> {
        if ledger.version != OBSERVATION_LEDGER_VERSION {
            return Err(ObservationError::UnsupportedVersion(ledger.version));
        }
        let mut ledger = ledger;
        let controller_epoch = ledger.controller_epoch.saturating_add(1).max(1);
        ledger.controller_epoch = controller_epoch;
        Ok(Self {
            ledger,
            controller_epoch,
        })
    }

    /// Restore after a process restart.  Facts and emitted reaction markers
    /// survive, so replaying the same poll cannot create a second task.
    pub fn restore(bytes: &[u8]) -> Result<Self, ObservationError> {
        let ledger: ObservationLedger = serde_json::from_slice(bytes)
            .map_err(|error| ObservationError::Serialization(error.to_string()))?;
        Self::from_ledger(ledger)
    }

    /// Snapshot the durable fact boundary.  Persist this result before
    /// calling `deliver_pending`.
    pub fn snapshot(&self) -> Result<Vec<u8>, ObservationError> {
        serde_json::to_vec(&self.ledger)
            .map_err(|error| ObservationError::Serialization(error.to_string()))
    }

    pub fn ledger(&self) -> &ObservationLedger {
        &self.ledger
    }

    pub fn fact(&self, id: &str) -> Option<&ExternalObservationFact> {
        self.ledger.facts.get(id)
    }

    pub fn facts(&self) -> impl Iterator<Item = &ExternalObservationFact> {
        self.ledger.facts.values()
    }

    pub fn pending_observation_ids(&self) -> Vec<String> {
        self.ledger
            .facts
            .values()
            .filter(|fact| Self::is_pending(fact))
            .map(|fact| fact.id.clone())
            .collect()
    }

    fn pending_resolution_ids(&self) -> Vec<String> {
        self.ledger
            .facts
            .values()
            .filter(|fact| {
                fact.state == ObservationState::Resolved
                    && fact.reaction_task_id.is_some()
                    && !fact.resolution_emitted
            })
            .map(|fact| fact.id.clone())
            .collect()
    }

    pub fn current_lease(&self) -> ControllerLease {
        ControllerLease {
            epoch: self.controller_epoch,
        }
    }

    /// Rotate the controller.  Existing facts remain durable, but only the
    /// returned lease may deliver their pending reactions.
    pub fn rotate_controller(&mut self) -> ControllerLease {
        self.controller_epoch = self.controller_epoch.saturating_add(1).max(1);
        self.ledger.controller_epoch = self.controller_epoch;
        self.current_lease()
    }

    /// Record/merge one observation.  This method has no task side effects.
    /// The returned receipt is the point at which the caller must persist the
    /// ledger before allowing a reaction to run.
    pub fn observe(
        &mut self,
        input: ExternalObservationInput,
    ) -> Result<ObservationReceipt, ObservationError> {
        let input = normalize_input(input)?;
        let dedup_key = stable_dedup_key(&input);
        let observation_id = format!("obs-{}", short_hash(&dedup_key));
        let now = input.observed_at.clone();
        let controller_epoch = self.controller_epoch;

        if let Some(fact) = self.ledger.facts.get_mut(&observation_id) {
            let was_actionable = fact.actionable;
            let was_resolved = fact.state == ObservationState::Resolved;
            fact.source = input.source;
            fact.external_id = input.external_id;
            fact.workspace_root = input.workspace_root;
            fact.subject = input.subject;
            fact.kind = input.kind;
            fact.summary = input.summary;
            fact.evidence = input.evidence;
            fact.evidence_digest = input.evidence_digest;
            fact.trusted = input.trusted;
            fact.last_seen_at = now;
            fact.revision = fact.revision.saturating_add(1);
            fact.observed_by_controller_epoch = controller_epoch;

            let newly_actionable = input.actionable && !was_actionable;
            fact.actionable = input.actionable;
            fact.state = if input.actionable {
                ObservationState::Active
            } else {
                ObservationState::Resolved
            };

            let disposition = if input.actionable && was_resolved {
                fact.actionability_epoch = fact.actionability_epoch.saturating_add(1).max(1);
                fact.reaction_emitted = false;
                fact.resolution_emitted = false;
                ObservationDisposition::Reopened
            } else if !input.actionable && was_actionable {
                ObservationDisposition::Resolved
            } else if newly_actionable {
                fact.actionability_epoch = fact.actionability_epoch.saturating_add(1).max(1);
                fact.reaction_emitted = false;
                fact.resolution_emitted = false;
                ObservationDisposition::Updated
            } else if input.actionable {
                ObservationDisposition::Duplicate
            } else {
                ObservationDisposition::Resolved
            };

            return Ok(ObservationReceipt {
                observation_id,
                disposition,
                newly_actionable,
                fact: fact.clone(),
            });
        }

        let actionability_epoch = if input.actionable { 1 } else { 0 };
        let fact = ExternalObservationFact {
            id: observation_id.clone(),
            dedup_key,
            source: input.source,
            external_id: input.external_id,
            workspace_root: input.workspace_root,
            subject: input.subject,
            kind: input.kind,
            summary: input.summary,
            evidence: input.evidence,
            evidence_digest: input.evidence_digest,
            actionable: input.actionable,
            state: if input.actionable {
                ObservationState::Active
            } else {
                ObservationState::Resolved
            },
            trusted: input.trusted,
            first_seen_at: now.clone(),
            last_seen_at: now,
            revision: 1,
            actionability_epoch,
            reaction_emitted: false,
            reaction_task_id: None,
            resolution_emitted: false,
            observed_by_controller_epoch: controller_epoch,
        };
        self.ledger
            .facts
            .insert(observation_id.clone(), fact.clone());
        Ok(ObservationReceipt {
            observation_id,
            disposition: ObservationDisposition::New,
            newly_actionable: input.actionable,
            fact,
        })
    }

    /// Deliver all currently pending facts through PL-006's canonical
    /// projection.  Policy failures leave the fact pending and therefore
    /// retryable after the caller changes the policy or controller context.
    pub fn deliver_pending<R: ObservationOwnerResolver>(
        &mut self,
        tasks: &mut Vec<TaskNode>,
        lease: ControllerLease,
        policy: ReactionPolicy,
        owner_resolver: &R,
        now: String,
    ) -> Result<Vec<CorrectiveWork>, ObservationError> {
        if lease.epoch != self.controller_epoch {
            return Err(ObservationError::StaleController {
                current: self.controller_epoch,
                got: lease.epoch,
            });
        }

        let ids = self.pending_observation_ids();
        let mut emitted = Vec::with_capacity(ids.len());
        for id in ids {
            let fact = self
                .ledger
                .facts
                .get(&id)
                .cloned()
                .ok_or_else(|| ObservationError::InvalidInput(format!("missing fact {id}")))?;
            self.check_policy(&fact, policy)?;
            let owner = owner_resolver
                .resolve_owner(&fact)
                .map(|owner| owner.trim().to_string())
                .filter(|owner| !owner.is_empty())
                .ok_or_else(|| ObservationError::OwnerUnavailable(fact.id.clone()))?;
            let marker = causal_marker(&fact);
            let expected_title = format!("Corrective work: {}", fact.summary);
            let expected_done_when = format!("external observation {} is resolved", fact.id);
            let expected_category = format!("corrective/{}", fact.kind.as_str());

            // Reuse an unfinished task when a resolved observation becomes
            // actionable again.  A terminal task cannot be reactivated, so a
            // new epoch-specific node is created in that case.
            let reusable_task_id = fact.reaction_task_id.as_ref().and_then(|prior_id| {
                tasks
                    .iter()
                    .find(|task| task.id.as_str() == prior_id.as_str())
                    .and_then(|task| {
                        (!matches!(
                            task.state,
                            TaskState::Done
                                | TaskState::Failed
                                | TaskState::Merged
                                | TaskState::Split
                        ))
                        .then(|| task.id.clone())
                    })
            });
            let reopening = reusable_task_id.is_some();
            let task_id = reusable_task_id.unwrap_or_else(|| corrective_task_id(&fact));

            let (created, needs_causal_link) = if let Some(existing) =
                tasks.iter().find(|task| task.id == task_id)
            {
                if existing
                    .open_questions
                    .iter()
                    .any(|question| question == &marker)
                {
                    (false, false)
                } else if existing.open_questions.iter().any(|question| {
                    question.starts_with(&format!("caused-by-external-observation:{}:", fact.id))
                }) || (existing.title == expected_title
                    && existing.done_when == expected_done_when
                    && existing.category == expected_category)
                {
                    // A crash between PL-006 creation and its causal patch
                    // leaves this deterministic task behind. Complete the
                    // link instead of creating a duplicate after restart.
                    (false, true)
                } else {
                    return Err(ObservationError::TaskIdCollision(task_id));
                }
            } else {
                create_task(
                    tasks,
                    CreateTaskInput {
                        id: task_id.clone(),
                        title: expected_title,
                        deps: vec![],
                        done_when: expected_done_when,
                        category: expected_category,
                        estimated_duration: String::new(),
                        assigned_role: owner.clone(),
                        escalation_path: "operator".to_string(),
                        now: now.clone(),
                    },
                )?;
                (true, true)
            };

            if reopening {
                mark_active(tasks, &task_id, now.clone())?;
            }
            if !created {
                // Re-resolve and apply ownership for retries/reopens too; an
                // owner stored on an earlier task emission is not authority.
                assign_owner(tasks, &task_id, owner.clone(), now.clone())?;
            }
            if needs_causal_link {
                // PL-006 intentionally keeps creation input small. Add the
                // reverse causal link through its non-semantic patch seam,
                // preserving the prior epoch marker when a task is reopened.
                let mut questions = tasks
                    .iter()
                    .find(|task| task.id == task_id)
                    .map(|task| task.open_questions.clone())
                    .unwrap_or_default();
                if !questions.iter().any(|question| question == &marker) {
                    questions.push(marker);
                }
                update_task(
                    tasks,
                    &task_id,
                    TaskPatch {
                        open_questions: Some(questions),
                        now: now.clone(),
                        ..Default::default()
                    },
                )?;
            }

            if let Some(stored) = self.ledger.facts.get_mut(&id) {
                stored.reaction_emitted = true;
                stored.reaction_task_id = Some(task_id.clone());
            }
            emitted.push(CorrectiveWork {
                observation_id: id,
                task_id,
                owner,
                actionability_epoch: fact.actionability_epoch,
                created,
                reopened: reopening,
                resolved: false,
            });
        }

        // Resolution is also delivered only after the durable fact boundary.
        // Keep a failed/merged/split corrective task terminal rather than
        // pretending that an external green signal repaired its execution.
        for id in self.pending_resolution_ids() {
            let fact = self
                .ledger
                .facts
                .get(&id)
                .cloned()
                .ok_or_else(|| ObservationError::InvalidInput(format!("missing fact {id}")))?;
            self.check_policy(&fact, policy)?;
            let task_id = fact.reaction_task_id.clone().ok_or_else(|| {
                ObservationError::InvalidInput(format!("resolved fact {id} has no task"))
            })?;
            let task = tasks
                .iter()
                .find(|task| task.id == task_id)
                .cloned()
                .ok_or_else(|| {
                    ObservationError::Projection(ProjectionError::NotFound(task_id.clone()))
                })?;
            if !matches!(
                task.state,
                TaskState::Done
                    | TaskState::Failed
                    | TaskState::Cancelled
                    | TaskState::Merged
                    | TaskState::Split
            ) {
                mark_done(tasks, &task_id, now.clone())?;
            }
            if let Some(stored) = self.ledger.facts.get_mut(&id) {
                stored.resolution_emitted = true;
            }
            emitted.push(CorrectiveWork {
                observation_id: id,
                task_id,
                owner: task.assigned_role,
                actionability_epoch: fact.actionability_epoch,
                created: false,
                reopened: false,
                resolved: true,
            });
        }
        Ok(emitted)
    }

    fn check_policy(
        &self,
        fact: &ExternalObservationFact,
        policy: ReactionPolicy,
    ) -> Result<(), ObservationError> {
        if !fact.trusted {
            return Err(ObservationError::UntrustedInput);
        }
        if !policy.workspace_trusted {
            return Err(ObservationError::WorkspaceUntrusted);
        }
        if !policy.policy_allows {
            return Err(ObservationError::PolicyDenied);
        }
        if !policy.approval_granted {
            return Err(ObservationError::ApprovalRequired);
        }
        Ok(())
    }

    fn is_pending(fact: &ExternalObservationFact) -> bool {
        fact.actionable
            && fact.state == ObservationState::Active
            && !fact.reaction_emitted
            && fact.actionability_epoch > 0
    }
}

fn normalize_input(
    mut input: ExternalObservationInput,
) -> Result<ExternalObservationInput, ObservationError> {
    input.source = input.source.trim().to_ascii_lowercase();
    input.external_id = input.external_id.trim().to_string();
    input.workspace_root = input.workspace_root.trim().to_string();
    input.subject = input.subject.trim().to_string();
    input.summary = input.summary.trim().to_string();
    input.evidence_digest = input.evidence_digest.trim().to_ascii_lowercase();
    input.observed_at = input.observed_at.trim().to_string();
    if input.source.is_empty() || input.workspace_root.is_empty() || input.subject.is_empty() {
        return Err(ObservationError::InvalidInput(
            "source, workspace_root, and subject are required".to_string(),
        ));
    }
    if input.summary.is_empty() {
        return Err(ObservationError::InvalidInput(
            "summary is required".to_string(),
        ));
    }
    if input.observed_at.is_empty() {
        return Err(ObservationError::InvalidInput(
            "observed_at is required".to_string(),
        ));
    }
    Ok(input)
}

fn stable_dedup_key(input: &ExternalObservationInput) -> String {
    let identity = if input.external_id.is_empty() {
        format!(
            "{}|{}|{}|{}|{}",
            input.source,
            input.kind.as_str(),
            input.workspace_root,
            input.subject,
            if input.evidence_digest.is_empty() {
                input.summary.as_str()
            } else {
                input.evidence_digest.as_str()
            }
        )
    } else {
        format!(
            "{}|{}|{}|{}",
            input.source,
            input.kind.as_str(),
            input.workspace_root,
            input.external_id
        )
    };
    rightkit_fs::digest::sha256_bytes(identity.as_bytes())
}

fn short_hash(value: &str) -> String {
    rightkit_crypto::hex_encode(&rightkit_crypto::sha256(value.as_bytes())[..12])
}

fn corrective_task_id(fact: &ExternalObservationFact) -> String {
    format!("corrective:{}:epoch:{}", fact.id, fact.actionability_epoch)
}

fn causal_marker(fact: &ExternalObservationFact) -> String {
    format!(
        "caused-by-external-observation:{}:epoch:{}",
        fact.id, fact.actionability_epoch
    )
}
