//! Host-owned background-review adapter.
//!
//! This adapter reads only canonical live conduit events, keeps foreground
//! memory state explicit, and advances its cursor after result validation.
//! Missing durable history is surfaced as typed unavailability; no storage or
//! semantic write is invented here.

use crate::api::LiveSession;
use crate::background_provider::{authority_from_config, ConfiguredBackgroundProvider};
use crate::membrane_binding::{
    BackgroundEventProvenanceV1, BackgroundReviewJobKindV1, BackgroundSemanticReviewBudgetV1,
    BackgroundSemanticReviewRequestV1, BackgroundSemanticReviewResultV1, BackgroundSessionEventV1,
    ForegroundMemoryCursorV1, ForegroundMemoryStateV1, LiveProviderAuthorityV1,
    RestrictedCapabilitiesV1,
};
use rightkit_agent_engine_core::ConduitReplay;
use rightkit_agent_engine_protocol::ProtocolEvent;
use serde::Serialize;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct BackgroundReviewJobInputV1 {
    pub job_id: String,
    pub request_id: String,
    pub job_kind: BackgroundReviewJobKindV1,
    pub task_id: Option<String>,
    pub turn_id: String,
    pub foreground_memory_state: ForegroundMemoryStateV1,
    pub budgets: BackgroundSemanticReviewBudgetV1,
    pub deadline_at_unix_ms: u64,
    pub restricted_capabilities: RestrictedCapabilitiesV1,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum BackgroundReviewAdapterOutcomeV1 {
    Completed {
        result: Box<BackgroundSemanticReviewResultV1>,
    },
    Unavailable {
        code: &'static str,
        message: String,
        cursor: ForegroundMemoryCursorV1,
    },
}

#[derive(Clone)]
pub struct BackgroundReviewAdapter {
    session: Arc<LiveSession>,
    provider: ConfiguredBackgroundProvider,
    authority: LiveProviderAuthorityV1,
    cursor: Arc<Mutex<ForegroundMemoryCursorV1>>,
}

impl BackgroundReviewAdapter {
    pub fn new(
        session: Arc<LiveSession>,
        config: &crate::api::EngineConfig,
        cursor: ForegroundMemoryCursorV1,
    ) -> Result<Self, BackgroundReviewAdapterOutcomeV1> {
        if cursor.session_id.as_str() != session.id.as_str() {
            return Err(Self::unavailable(
                cursor,
                "cursor_session_mismatch",
                "foreground cursor does not belong to live session",
            ));
        }
        if let Err(error) = cursor.validate() {
            return Err(Self::unavailable(
                cursor,
                "cursor_unavailable",
                error.to_string(),
            ));
        }
        let authority = authority_from_config(config);
        Ok(Self {
            session,
            provider: ConfiguredBackgroundProvider::new(config, authority.clone()),
            authority,
            cursor: Arc::new(Mutex::new(cursor)),
        })
    }

    pub fn cursor(&self) -> ForegroundMemoryCursorV1 {
        self.cursor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Build one request from canonical conduit events, invoke configured
    /// provider, and commit `next_cursor` only after full validation.
    pub async fn run(&self, input: BackgroundReviewJobInputV1) -> BackgroundReviewAdapterOutcomeV1 {
        let cursor = self.cursor();
        let events = match self.session.conduit.replay_after(cursor.last_seq as i64) {
            ConduitReplay::Events(events) => events,
            ConduitReplay::ResyncRequired { .. } => {
                return Self::unavailable(
                    cursor,
                    "canonical_events_unavailable",
                    "live conduit history no longer covers foreground cursor",
                );
            }
        };
        let events = match events
            .into_iter()
            .map(|event| canonical_event(self.session.id.as_str(), event))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(events) => events,
            Err(message) => {
                return Self::unavailable(cursor, "canonical_events_unavailable", message);
            }
        };

        let request = assemble_review_request(
            self.session.id.as_str(),
            &self.authority,
            cursor.clone(),
            events,
            input,
        );
        if let Err(error) = request.validate() {
            return Self::unavailable(cursor, "invalid_review_request", error.to_string());
        }

        let result = match self.provider.execute(&request).await {
            Ok(result) => result,
            Err(error) => {
                return Self::unavailable(cursor, "provider_invocation_failed", error.to_string());
            }
        };
        if let Err(error) = result.validate_against(&request) {
            return Self::unavailable(cursor, "result_validation_failed", error.to_string());
        }
        if let Some(next_cursor) = result.next_cursor.clone() {
            let mut current = self
                .cursor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *current != cursor {
                return Self::unavailable(
                    cursor,
                    "cursor_advance_conflict",
                    "foreground cursor changed while review was running",
                );
            }
            *current = next_cursor;
        }
        BackgroundReviewAdapterOutcomeV1::Completed {
            result: Box::new(result),
        }
    }

    fn unavailable(
        cursor: ForegroundMemoryCursorV1,
        code: &'static str,
        message: impl Into<String>,
    ) -> BackgroundReviewAdapterOutcomeV1 {
        BackgroundReviewAdapterOutcomeV1::Unavailable {
            code,
            message: message.into(),
            cursor,
        }
    }
}

/// Bind one Membrane background-review request from the caller-supplied
/// job facts (task, capacity/budget, deadline, capability) and the live
/// session identity. Every task/capacity/deadline/capability field on the
/// returned request is copied verbatim from `input`/`session_id` — this
/// function must never substitute a default or placeholder value for any
/// of them (CRA-04 / HCTX-001, HCTX-002 extended).
fn assemble_review_request(
    session_id: &str,
    authority: &LiveProviderAuthorityV1,
    cursor: ForegroundMemoryCursorV1,
    events: Vec<BackgroundSessionEventV1>,
    input: BackgroundReviewJobInputV1,
) -> BackgroundSemanticReviewRequestV1 {
    BackgroundSemanticReviewRequestV1 {
        schema_version: BackgroundSemanticReviewRequestV1::SCHEMA_VERSION,
        job_id: input.job_id,
        request_id: input.request_id,
        job_kind: input.job_kind,
        session_id: session_id.to_string(),
        task_id: input.task_id,
        turn_id: input.turn_id,
        cursor,
        events,
        foreground_memory_state: input.foreground_memory_state,
        budgets: input.budgets,
        deadline_at_unix_ms: input.deadline_at_unix_ms,
        restricted_capabilities: input.restricted_capabilities,
        provider_authority: authority.clone(),
    }
}

fn canonical_event(
    session_id: &str,
    event: ProtocolEvent,
) -> Result<BackgroundSessionEventV1, String> {
    let encoded = serde_json::to_vec(&event).map_err(|error| error.to_string())?;
    let value: serde_json::Value =
        serde_json::from_slice(&encoded).map_err(|error| error.to_string())?;
    let object = value
        .as_object()
        .ok_or_else(|| "canonical protocol event is not an object".to_string())?;
    let event_session = object
        .get("sessionId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "canonical protocol event has no sessionId".to_string())?;
    if event_session != session_id {
        return Err("canonical protocol event session identity mismatch".into());
    }
    let seq = object
        .get("seq")
        .and_then(serde_json::Value::as_i64)
        .filter(|seq| *seq > 0)
        .ok_or_else(|| "canonical protocol event has no positive seq".to_string())?
        as u64;
    let event_type = object
        .get("$kind")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "canonical protocol event has no kind".to_string())?;
    let digest = sha256_digest(&encoded);
    Ok(BackgroundSessionEventV1 {
        schema_version: BackgroundSessionEventV1::SCHEMA_VERSION,
        session_id: session_id.to_string(),
        seq,
        event_id: format!("{session_id}:{seq}:{event_type}"),
        event_type: event_type.to_string(),
        payload: value,
        content_hash: digest.clone(),
        provenance: BackgroundEventProvenanceV1 {
            source: crate::host_identity::schema("session-conduit"),
            receipt_id: format!("conduit:{session_id}:{seq}"),
            receipt_digest: digest,
        },
    })
}

fn sha256_digest(bytes: &[u8]) -> String {
    let mut digest = rightkit_fs::digest::Sha256Hasher::new();
    digest.update(bytes);
    format!("sha256:{}", digest.finalize())
}
