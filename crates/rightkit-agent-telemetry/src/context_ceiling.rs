//! Host-owned H8 remaining-context observations.
//!
//! the host app can report a complete ceiling only when both active model
//! capacity metadata and a provider-reported current context value exist.
//! `UsageUpdated` arrives after a provider turn, so its row is the measured
//! ceiling available to the next request. Missing inputs stay typed
//! unavailable; they are never coerced to zero.

use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub const REMAINING_CONTEXT_CEILING_SCHEMA_VERSION: u32 = 1;
pub const REMAINING_CONTEXT_CEILING_SCHEMA: &str = "coderight.remaining-context-ceiling.v1";

/// This basis is provider-reported context-token units.  Membrane must use
/// this exact basis for the reduction plan it compares against this ceiling.
pub const H8_ESTIMATOR_BASIS_ID: &str = "provider_reported_context_tokens";
pub const H8_ESTIMATOR_BASIS_VERSION: &str = "1";
const MAX_ROWS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationCoverage {
    Complete,
    Unavailable,
}

/// Reasons match Membrane's host-observation reason vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationUnavailableReason {
    NotInstrumented,
    HubInactive,
    ProviderOmitted,
    HostUnsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObservedField<T> {
    pub coverage: ObservationCoverage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<ObservationUnavailableReason>,
}

impl<T> ObservedField<T> {
    fn complete(value: T) -> Self {
        Self {
            coverage: ObservationCoverage::Complete,
            value: Some(value),
            unavailable_reason: None,
        }
    }

    fn unavailable(reason: ObservationUnavailableReason) -> Self {
        Self {
            coverage: ObservationCoverage::Unavailable,
            value: None,
            unavailable_reason: Some(reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EstimatorBasis {
    pub id: String,
    pub version: String,
}

impl Default for EstimatorBasis {
    fn default() -> Self {
        Self {
            id: H8_ESTIMATOR_BASIS_ID.to_string(),
            version: H8_ESTIMATOR_BASIS_VERSION.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TokenEstimate {
    pub basis: EstimatorBasis,
    pub estimate: ObservedField<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostObservationProvenance {
    pub schema_version: u32,
    pub receipt_id: String,
    pub source: String,
    pub observed_at_unix_ms: u64,
    pub receipt_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemainingContextCeilingV1 {
    pub schema_version: u32,
    pub ceiling_id: String,
    pub session_id: String,
    pub task_id: ObservedField<String>,
    pub requested_at_unix_ms: u64,
    pub remaining_tokens: TokenEstimate,
    pub provenance_receipt: HostObservationProvenance,
}

/// Inputs collected from the host app's real session/model seams.
///
/// `observed_context_tokens` is the latest provider-reported context usage
/// emitted by the engine.  It is absent before the first completed turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemainingContextInput {
    pub session_id: String,
    pub task_id: Option<String>,
    pub model_id: String,
    pub context_window_tokens: Option<u64>,
    pub reserve_tokens: Option<u64>,
    pub observed_context_tokens: Option<u64>,
    pub requested_at_unix_ms: u64,
}

impl RemainingContextInput {
    pub fn unavailable_for_session(
        session_id: impl Into<String>,
        model_id: impl Into<String>,
        context_window_tokens: Option<u64>,
        reserve_tokens: Option<u64>,
        requested_at_unix_ms: u64,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            task_id: None,
            model_id: model_id.into(),
            context_window_tokens,
            reserve_tokens,
            observed_context_tokens: None,
            requested_at_unix_ms,
        }
    }
}

/// Produce one H8 record from only supplied host facts.
pub fn produce_remaining_context_ceiling(
    input: RemainingContextInput,
) -> RemainingContextCeilingV1 {
    let basis = EstimatorBasis::default();
    let unavailable_reason = match (
        input.context_window_tokens,
        input.reserve_tokens,
        input.observed_context_tokens,
    ) {
        (None, _, _) | (_, None, _) => ObservationUnavailableReason::ProviderOmitted,
        (Some(window), Some(reserve), None) if window == 0 || reserve >= window => {
            ObservationUnavailableReason::HostUnsupported
        }
        (Some(_), Some(_), None) => ObservationUnavailableReason::NotInstrumented,
        (Some(window), Some(reserve), Some(current)) => {
            if window == 0 || reserve >= window || reserve.checked_add(current).is_none() {
                ObservationUnavailableReason::HostUnsupported
            } else {
                // This arm is replaced by a complete estimate below.
                let _ = (window, reserve, current);
                ObservationUnavailableReason::NotInstrumented
            }
        }
    };

    let remaining = match (
        input.context_window_tokens,
        input.reserve_tokens,
        input.observed_context_tokens,
    ) {
        (Some(window), Some(reserve), Some(current))
            if window > 0 && reserve < window && reserve.checked_add(current).is_some() =>
        {
            Some(window.saturating_sub(reserve + current))
        }
        _ => None,
    };

    let estimate = match remaining {
        Some(value) => ObservedField::complete(value),
        None => ObservedField::unavailable(unavailable_reason),
    };
    let task_id = input
        .task_id
        .filter(|value| !value.trim().is_empty())
        .map_or_else(
            || ObservedField::unavailable(ObservationUnavailableReason::NotInstrumented),
            ObservedField::complete,
        );

    let receipt_material = format!(
        "schema={REMAINING_CONTEXT_CEILING_SCHEMA};session={};task={:?};model={};window={:?};reserve={:?};context={:?};requested_at={}",
        input.session_id,
        task_id.value.as_deref(),
        input.model_id,
        input.context_window_tokens,
        input.reserve_tokens,
        input.observed_context_tokens,
        input.requested_at_unix_ms,
    );
    let receipt_digest = sha256_hex(receipt_material.as_bytes());
    let receipt_id = format!(
        "rightkit-agent-h8:{}:{}",
        input.session_id, input.requested_at_unix_ms
    );

    RemainingContextCeilingV1 {
        schema_version: REMAINING_CONTEXT_CEILING_SCHEMA_VERSION,
        ceiling_id: format!(
            "rightkit-agent-h8:{}:{}",
            input.session_id, input.requested_at_unix_ms
        ),
        session_id: input.session_id,
        task_id,
        requested_at_unix_ms: input.requested_at_unix_ms,
        remaining_tokens: TokenEstimate { basis, estimate },
        provenance_receipt: HostObservationProvenance {
            schema_version: REMAINING_CONTEXT_CEILING_SCHEMA_VERSION,
            receipt_id,
            source: "rightkit-agent".to_string(),
            observed_at_unix_ms: input.requested_at_unix_ms,
            receipt_digest,
        },
    }
}

fn sink() -> &'static Mutex<Vec<RemainingContextCeilingV1>> {
    static SINK: OnceLock<Mutex<Vec<RemainingContextCeilingV1>>> = OnceLock::new();
    SINK.get_or_init(|| Mutex::new(Vec::new()))
}

/// Replace the latest row for a session, retaining at most `MAX_ROWS` rows.
pub fn record_remaining_context_ceiling(input: RemainingContextInput) {
    let row = produce_remaining_context_ceiling(input);
    let mut rows = sink()
        .lock()
        .expect("context ceiling telemetry sink lock poisoned");
    if let Some(existing) = rows
        .iter_mut()
        .find(|existing| existing.session_id == row.session_id)
    {
        *existing = row;
        return;
    }
    if rows.len() >= MAX_ROWS {
        rows.remove(0);
    }
    rows.push(row);
}

pub fn remaining_context_ceilings() -> Vec<RemainingContextCeilingV1> {
    sink()
        .lock()
        .expect("context ceiling telemetry sink lock poisoned")
        .clone()
}

pub fn reset_remaining_context_telemetry() {
    sink()
        .lock()
        .expect("context ceiling telemetry sink lock poisoned")
        .clear();
}

pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut digest = String::with_capacity(71);
    digest.push_str("sha256:");
    for byte in rightkit_crypto::sha256(bytes) {
        let _ = write!(digest, "{byte:02x}");
    }
    digest
}
