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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_unknown_is_never_blindly_replayed() {
        let state = ExternalEffectSettlement::InterruptedUnknown;
        assert!(state.may_have_executed());
        assert!(state.requires_reconciliation());
        assert!(!state.allows_automatic_non_idempotent_replay());
    }

    #[test]
    fn only_known_not_started_is_default_replay_safe() {
        for state in [
            ExternalEffectSettlement::Running,
            ExternalEffectSettlement::Succeeded,
            ExternalEffectSettlement::Failed,
            ExternalEffectSettlement::InterruptedUnknown,
        ] {
            assert!(!state.allows_automatic_non_idempotent_replay());
        }
        assert!(ExternalEffectSettlement::NotStarted.allows_automatic_non_idempotent_replay());
    }
}
