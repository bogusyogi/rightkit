use rightkit_agent_task_contract::TaskStatus;
use serde::{Deserialize, Serialize};

/// Durable settlement state for an external effect attempt.
///
/// `InterruptedUnknown` is the fail-closed state for a process/network/external
/// mutation that may have executed but whose result was not durably committed.
/// Such work must be reconciled explicitly; automatic replay is unsafe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalEffectSettlement {
    NotStarted,
    Running,
    Succeeded,
    Failed,
    InterruptedUnknown,
}

impl ExternalEffectSettlement {
    pub fn may_have_executed(self) -> bool {
        matches!(
            self,
            Self::Running | Self::Succeeded | Self::Failed | Self::InterruptedUnknown
        )
    }

    /// Whether recovery may automatically execute the same non-idempotent
    /// effect again. Only a durably known `NotStarted` attempt is safe by
    /// default; callers may layer a stronger idempotency contract separately.
    pub fn allows_automatic_non_idempotent_replay(self) -> bool {
        matches!(self, Self::NotStarted)
    }

    pub fn requires_reconciliation(self) -> bool {
        matches!(self, Self::InterruptedUnknown)
    }

    /// The task contract's status for a task whose current effect is in this
    /// settlement: `running`, `succeeded`, `failed`, and `uncertain` for
    /// `InterruptedUnknown` (outcome unknown: query the receipt, never repeat
    /// the effect blindly). `NotStarted` is `None`: no effect has begun, so
    /// the task keeps the status it has.
    pub fn task_status(self) -> Option<TaskStatus> {
        match self {
            Self::NotStarted => None,
            Self::Running => Some(TaskStatus::Running),
            Self::Succeeded => Some(TaskStatus::Succeeded),
            Self::Failed => Some(TaskStatus::Failed),
            Self::InterruptedUnknown => Some(TaskStatus::Uncertain),
        }
    }
}
