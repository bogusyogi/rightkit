// Legacy entry points carry no DeliveryHost policy or retained control binding.
// Refuse before touching clipboard or target; use paste_delivery for gated writes.
use super::*;

// Retain legacy result shape while callers migrate to host-bound delivery.
#[allow(dead_code)]
pub(crate) enum InsertResult {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    Confirmed {
        steps: Vec<DeliveryTimingStep>,
        proof: InsertionProof,
    },
}

pub(crate) fn insert_transcript(
    _transcript: &str,
    _target: &TargetSnapshot,
    _send_enter: bool,
) -> Result<InsertResult, DeliveryError> {
    Err(DeliveryError::new(
        "E_DELIVERY_HOST_REQUIRED",
        "delivery_host_required",
    ))
}
