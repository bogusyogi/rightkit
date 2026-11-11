//! Task envelope, the ten statuses and the legal transitions between them.
//! Covers the status list, at-least-once delivery, `cancel-requested` (not terminal),
//! `uncertain` after an unknown outcome and the approval recheck before execution.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::device::DeviceRef;
use super::intent::Intent;
use super::principal::Principal;

/// Version of the [`TaskEnvelope`] record. Version 2 added `kind`; version 1 envelopes still parse
/// (`kind` defaults to empty) but the engine's submit refuses any version other than this one.
pub const TASK_ENVELOPE_SCHEMA_VERSION: u32 = 2;
/// Vocabulary version for [`TaskStatus`] and its transition table.
pub const TASK_STATUS_SCHEMA_VERSION: u32 = 1;
/// Version of the [`UtteranceRef`] record.
pub const UTTERANCE_REF_SCHEMA_VERSION: u32 = 1;
/// Version of the [`AuthorityRef`] record.
pub const AUTHORITY_REF_SCHEMA_VERSION: u32 = 1;
/// Vocabulary version for [`AuthorityKind`].
pub const AUTHORITY_KIND_SCHEMA_VERSION: u32 = 1;

/// Task envelope. Binds stable IDs, the addressed utterance and generation,
/// origin, least-privilege context references, the authority reference,
/// target device, idempotency key, deadline, policy, owner epoch, checkpoint
/// and output references.
///
/// Context and output fields hold references, not content. Transcript copies
/// do not belong in daemon logs (task contract).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEnvelope {
    pub schema_version: u32,
    /// Stable task ID. `ACCEPTANCE` commits this ID, with the utterance dedupe mapping.
    pub task_id: String,
    /// The job kind the engine runs (for example `storage.snapshot`). Empty when the submitter
    /// names none; the engine refuses an empty kind at submit. Added in version 2; a version 1
    /// record without it parses with an empty kind.
    #[serde(default)]
    pub kind: String,
    pub conversation_id: String,
    pub utterance: UtteranceRef,
    /// Who submitted the task.
    pub origin: Principal,
    /// Least-privilege context references, one per granted item.
    pub context_refs: Vec<String>,
    /// Authority reference. `None` when `intent` alone authorizes the act.
    pub authority: Option<AuthorityRef>,
    /// Provenance of the authority (`intent.source`).
    pub intent: Intent,
    /// Device the task names. Never moved silently to another device.
    pub target_device: DeviceRef,
    /// Stable submission key. A retry with the same key resolves to the original task.
    pub idempotency_key: String,
    /// Epoch milliseconds. `None` means no deadline.
    pub deadline_ms: Option<u64>,
    /// Policy reference applied to this task.
    pub policy_ref: String,
    /// Fencing epoch of the current owner. Output from an older epoch is rejected.
    pub owner_epoch: u64,
    pub status: TaskStatus,
    /// Resume point: an acknowledged event cursor or checkpoint reference.
    pub checkpoint_ref: Option<String>,
    /// References to outputs. Content travels separately, redacted.
    pub output_refs: Vec<String>,
}

/// The addressed utterance and its generation, used for dedupe and cancel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UtteranceRef {
    pub schema_version: u32,
    pub utterance_id: String,
    pub generation: u64,
}

/// Reference to what authorizes the act beyond the intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityRef {
    pub schema_version: u32,
    pub kind: AuthorityKind,
    /// Approval, grant or policy ID.
    pub reference: String,
    /// Revision of that record. Rechecked after waits and before execution.
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthorityKind {
    /// A specific approval binding (see `approval::ApprovalBinding`).
    Approval,
    /// A local grant the owner set, unexpired.
    Grant,
    /// Standing policy.
    StandingPolicy,
    /// Existing workspace authorization, for example a repository the owner
    /// already authorized for commit and push.
    WorkspaceAuthorization,
}

/// The ten statuses. Serialized in kebab-case, which matches the design strings exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskStatus {
    Accepted,
    Leased,
    Running,
    WaitingUser,
    WaitingDevice,
    CancelRequested,
    Succeeded,
    Failed,
    Cancelled,
    /// Outcome unknown. Query the receipt and app state; never repeat the effect blindly.
    Uncertain,
}

impl TaskStatus {
    /// Terminal statuses: `succeeded`, `failed`, `cancelled`. `cancel-requested`
    /// and `uncertain` are not terminal. `cancelled` means no further effect can occur.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Succeeded | TaskStatus::Failed | TaskStatus::Cancelled
        )
    }
}

/// A transition the table does not allow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("illegal task transition from {from:?} to {to:?}")]
pub struct IllegalTransition {
    pub from: TaskStatus,
    pub to: TaskStatus,
}

/// Whether `from` may move to `to`. Pure table; no runtime state.
///
/// Notes on the table:
/// * `waiting-device` is reached only before an effect can start (`accepted`,
///   `leased`). A running task that loses its device goes to `uncertain`, so
///   an effect that may already have happened is never re-leased.
/// * Re-lease after `waiting-device` takes a new `owner_epoch`. The runtime
///   enforces that; this table does not.
/// * `cancel-requested` may end in `succeeded` (completion can win the race).
/// * `uncertain` resolves to `running` when a receipt shows execution still
///   in progress, or to a terminal status after a receipt query.
pub const fn is_legal_transition(from: TaskStatus, to: TaskStatus) -> bool {
    use TaskStatus::*;
    matches!(
        (from, to),
        (
            Accepted,
            Leased | WaitingDevice | CancelRequested | Cancelled | Failed
        ) | (WaitingDevice, Leased | CancelRequested | Cancelled | Failed)
            | (
                Leased,
                Running | WaitingDevice | WaitingUser | CancelRequested | Cancelled | Failed
            )
            | (
                Running,
                WaitingUser | CancelRequested | Succeeded | Failed | Uncertain
            )
            | (WaitingUser, Running | CancelRequested | Cancelled | Failed)
            | (CancelRequested, Cancelled | Succeeded | Failed | Uncertain)
            | (
                Uncertain,
                Running | Succeeded | Failed | Cancelled | CancelRequested
            )
    )
}

/// Checked transition: returns `to` when the table allows it.
pub fn transition(from: TaskStatus, to: TaskStatus) -> Result<TaskStatus, IllegalTransition> {
    if is_legal_transition(from, to) {
        Ok(to)
    } else {
        Err(IllegalTransition { from, to })
    }
}
