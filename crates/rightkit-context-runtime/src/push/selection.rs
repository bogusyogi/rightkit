//! Request-time Push plan construction & host-capacity selection.
//!
//! Pull owns admission. This module only turns the finalized planner packet
//! into a bounded, content-free reduction ladder, then applies the host's
//! validated H8 ceiling without inventing capacity.

use rightkit_context_protocol::host_observation::{
    ObservationCoverageV1, ObservationUnavailableReasonV1, RemainingContextCeilingV1,
};
use rightkit_context_protocol::push::{
    PacketReductionPlanError, PacketReductionPlanV1, PacketReductionRepresentationV1,
    PacketReductionSelectionError, PacketReductionSelectionReceiptV1,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Typed refusal at the request boundary. No missing or non-exact H8 value is
/// converted into a numeric fallback.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RequestTimeH8Error {
    #[error("remainingContextCeiling is required on every context request")]
    Missing,
    #[error("remainingContextCeiling is invalid: {0}")]
    Invalid(String),
    #[error(
        "request-time H8 {field} does not match request identity: expected={expected}, observed={observed}"
    )]
    IdentityMismatch {
        field: &'static str,
        expected: String,
        observed: String,
    },
    #[error("request-time H8 is not exact: coverage={coverage:?}, reason={reason:?}")]
    Inexact {
        coverage: ObservationCoverageV1,
        reason: Option<ObservationUnavailableReasonV1>,
    },
}

/// Typed refusal while constructing or selecting a request-time plan.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PacketReductionRequestError {
    #[error("request-time H8 refused: {0}")]
    H8(#[source] RequestTimeH8Error),
    #[error("packet reduction plan cannot be built: packet {0} is empty")]
    EmptyPacket(&'static str),
    #[error("packet reduction plan cannot be built: block {block} has empty {field}")]
    EmptyBlockField { block: String, field: &'static str },
    #[error("packet reduction plan cannot be built: no protected material")]
    NoProtectedMaterial,
    #[error("packet reduction plan cannot be built: protected floor exceeds full packet")]
    NoViableFloor,
    #[error("packet reduction plan is invalid: {0}")]
    InvalidPlan(#[source] PacketReductionPlanError),
    #[error("packet reduction content serialization failed: {0}")]
    ContentSerialization(String),
    #[error("packet reduction selection refused: {0}")]
    Selection(#[source] PacketReductionSelectionError),
}

impl PacketReductionRequestError {
    /// Stable response kind for route consumers. Detail remains in `reason`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::H8(RequestTimeH8Error::Missing)
            | Self::H8(RequestTimeH8Error::Inexact { .. }) => "h8_unavailable",
            Self::H8(RequestTimeH8Error::IdentityMismatch { .. }) => "h8_identity_mismatch",
            Self::H8(RequestTimeH8Error::Invalid(_)) => "h8_invalid",
            Self::EmptyPacket(_)
            | Self::EmptyBlockField { .. }
            | Self::NoProtectedMaterial
            | Self::NoViableFloor
            | Self::InvalidPlan(_)
            | Self::ContentSerialization(_)
            | Self::Selection(_) => "packet_reduction_refused",
        }
    }
}

/// Wire result returned only after plan validation & same-request selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PacketReductionSelectionV1 {
    pub plan: PacketReductionPlanV1,
    pub selected_representation: PacketReductionRepresentationV1,
    pub selection_receipt: PacketReductionSelectionReceiptV1,
}

/// Parse, validate & bind the H8 row carried on one `/federate` request.
pub fn parse_request_time_h8(
    body: &Value,
    expected_session_id: &str,
    expected_task_id: &str,
) -> Result<RemainingContextCeilingV1, RequestTimeH8Error> {
    let raw = body
        .get("remainingContextCeiling")
        .ok_or(RequestTimeH8Error::Missing)?;
    let ceiling: RemainingContextCeilingV1 = serde_json::from_value(raw.clone())
        .map_err(|error| RequestTimeH8Error::Invalid(error.to_string()))?;

    // An imprecise/inexact observation (e.g. `Complete` coverage claimed
    // without a value, or genuinely `Partial`/`Unavailable` coverage) must be
    // refused as typed `Inexact` rather than surfacing as a generic `Invalid`
    // schema error. Check these coverage/value obligations directly, before
    // the general structural `validate()` pass below, so a malformed-but-
    // inexact ceiling is never misreported as merely `Invalid`: a fake or
    // imprecise H8 must never be accepted under either error shape.
    if ceiling.task_id.coverage != ObservationCoverageV1::Complete
        || ceiling.task_id.value.is_none()
    {
        return Err(RequestTimeH8Error::Inexact {
            coverage: ceiling.task_id.coverage,
            reason: ceiling.task_id.unavailable_reason,
        });
    }
    let estimate = &ceiling.remaining_tokens.estimate;
    if estimate.coverage != ObservationCoverageV1::Complete || estimate.value.is_none() {
        return Err(RequestTimeH8Error::Inexact {
            coverage: estimate.coverage,
            reason: estimate.unavailable_reason,
        });
    }

    ceiling
        .validate()
        .map_err(|error| RequestTimeH8Error::Invalid(error.to_string()))?;

    if ceiling.session_id != expected_session_id {
        return Err(RequestTimeH8Error::IdentityMismatch {
            field: "sessionId",
            expected: expected_session_id.to_owned(),
            observed: ceiling.session_id.clone(),
        });
    }

    if ceiling.requested_at_unix_ms == 0 || ceiling.provenance_receipt.observed_at_unix_ms == 0 {
        return Err(RequestTimeH8Error::Invalid(
            "request and provenance timestamps must be non-zero".to_owned(),
        ));
    }

    let observed_task = ceiling
        .task_id
        .value
        .as_deref()
        .expect("task_id value presence already checked above for Complete coverage");
    if observed_task.trim().is_empty() {
        return Err(RequestTimeH8Error::Invalid(
            "taskId value must not be empty".to_owned(),
        ));
    }
    if observed_task != expected_task_id {
        return Err(RequestTimeH8Error::IdentityMismatch {
            field: "taskId",
            expected: expected_task_id.to_owned(),
            observed: observed_task.to_owned(),
        });
    }

    Ok(ceiling)
}

pub use super::packet_selection::*;
