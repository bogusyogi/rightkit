//! Typed degradation for optional Membrane capabilities.

use serde::{Deserialize, Serialize};

/// Surface on which a host-side Membrane capability was unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingSurfaceV1 {
    H8Observation,
    H7VariantExperiment,
    H9LoadedContext,
    H10PacketDelivery,
    PacketReductionPlan,
    BackgroundSemanticReview,
    ForegroundMemoryCursor,
}

/// Closed reason vocabulary.  Missing data remains typed instead of becoming
/// an empty packet, zero budget, or successful review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReasonV1 {
    NotConfigured,
    HubInactive,
    ProviderOmitted,
    ProviderUnavailable,
    ProviderDenied,
    ProviderTimeout,
    NotInstrumented,
    HostUnsupported,
    ReductionPlanOmitted,
    CursorOmitted,
    CursorStale,
    AuthenticationFailed,
    ProtocolMismatch,
}

/// Explicit unavailable state carried to an integration caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnavailableStateV1 {
    pub surface: BindingSurfaceV1,
    pub reason: UnavailableReasonV1,
    pub retryable: bool,
}

impl UnavailableStateV1 {
    pub const fn new(
        surface: BindingSurfaceV1,
        reason: UnavailableReasonV1,
        retryable: bool,
    ) -> Self {
        Self {
            surface,
            reason,
            retryable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_state_round_trips_without_empty_success_shape() {
        let state = UnavailableStateV1::new(
            BindingSurfaceV1::ForegroundMemoryCursor,
            UnavailableReasonV1::CursorOmitted,
            true,
        );
        let value = serde_json::to_value(state).expect("typed state serializes");
        assert_eq!(value["surface"], "foreground_memory_cursor");
        assert_eq!(value["reason"], "cursor_omitted");
        assert_eq!(value["retryable"], true);
        assert!(serde_json::from_value::<UnavailableStateV1>(value).is_ok());
    }
}
