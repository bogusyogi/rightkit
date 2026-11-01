use super::protocol::*;
use serde_json::Value;

pub fn validate_frame(frame: &EngineFrame) -> Result<(), EngineContractError> {
    if frame.protocol_major != PROTOCOL_MAJOR {
        return Err(EngineContractError::ProtocolMajorMismatch {
            expected: PROTOCOL_MAJOR,
            actual: frame.protocol_major,
        });
    }
    if frame.schema_version != CURRENT_SCHEMA_VERSION {
        return Err(EngineContractError::UnsupportedSchemaVersion {
            actual: frame.schema_version,
        });
    }

    match frame.schema_name {
        EngineSchemaName::EngineError => {
            if frame.error.is_none() {
                return Err(EngineContractError::MissingError);
            }
            if frame.payload.is_some() {
                return Err(EngineContractError::PayloadSchemaMismatch);
            }
        }
        EngineSchemaName::EngineHealth => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::Health { .. })
            })?;
        }
        EngineSchemaName::EngineCapabilities => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::Capabilities { .. })
            })?;
        }
        EngineSchemaName::EngineInfo => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::EngineInfo { .. })
            })?;
        }
        EngineSchemaName::EngineAck => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::EngineAck { .. })
            })?;
        }
        EngineSchemaName::ReplaceEngineConfig => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::ReplaceEngineConfig { .. })
            })?;
        }
        EngineSchemaName::SetProviderKeys => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::SetProviderKeys { .. })
            })?;
        }
        EngineSchemaName::ReconfigureL3 => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::ReconfigureL3 { .. })
            })?;
        }
        EngineSchemaName::ReplaceVocabulary => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::ReplaceVocabulary { .. })
            })?;
        }
        EngineSchemaName::EngineStateRequest => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::EngineStateRequest)
            })?;
        }
        EngineSchemaName::EngineStateSnapshot => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::EngineStateSnapshot { .. })
            })?;
        }
        EngineSchemaName::RecentHistoryRequest => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::RecentHistoryRequest { .. })
            })?;
        }
        EngineSchemaName::RecentHistoryResult => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::RecentHistoryResult { .. })
            })?;
        }
        EngineSchemaName::ReplaceRecentHistory => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::ReplaceRecentHistory { .. })
            })?;
        }
        EngineSchemaName::RepasteLastRequest => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::RepasteLastRequest)
            })?;
        }
        EngineSchemaName::ManualDeliveryResult => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::ManualDeliveryResult { .. })
            })?;
        }
        EngineSchemaName::CopyLastRequest => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::CopyLastRequest)
            })?;
        }
        EngineSchemaName::CopyLastResult => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::CopyLastResult { .. })
            })?;
        }
        EngineSchemaName::FileTranscriptionRequest => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::FileTranscriptionRequest { .. })
            })?;
        }
        EngineSchemaName::FileTranscriptionCancel => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::FileTranscriptionCancel { .. })
            })?;
        }
        EngineSchemaName::FileTranscriptionResult => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::FileTranscriptionResult { .. })
            })?;
        }
        EngineSchemaName::RecordingStarted => {
            reject_error(frame)?;
            require_session(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::RecordingStarted { .. })
            })?;
        }
        EngineSchemaName::RecordingContextCaptured => {
            reject_error(frame)?;
            require_session(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::RecordingContextCaptured { .. })
            })?;
        }
        EngineSchemaName::RecordingLevel => {
            reject_error(frame)?;
            require_session(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::RecordingLevel { .. })
            })?;
        }
        EngineSchemaName::TranscribingStarted => {
            reject_error(frame)?;
            require_session(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::TranscribingStarted { .. })
            })?;
        }
        EngineSchemaName::TranscriptPartial => {
            reject_error(frame)?;
            require_session(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::TranscriptPartial { .. })
            })?;
        }
        EngineSchemaName::TranscriptFinal => {
            reject_error(frame)?;
            require_session(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::TranscriptFinal { .. })
            })?;
        }
        EngineSchemaName::WakeListenStarted => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::WakeListenStarted { .. })
            })?;
        }
        EngineSchemaName::WakeListenStopped => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::WakeListenStopped { .. })
            })?;
        }
        EngineSchemaName::WakeEvent => {
            reject_error(frame)?;
            require_payload(frame, |payload| {
                matches!(payload, EnginePayload::WakeFired { .. })
            })?;
        }
    }

    Ok(())
}

fn reject_error(frame: &EngineFrame) -> Result<(), EngineContractError> {
    if frame.error.is_some() {
        return Err(EngineContractError::UnexpectedErrorPayload);
    }
    Ok(())
}

fn require_session(frame: &EngineFrame) -> Result<(), EngineContractError> {
    frame
        .session_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|_| ())
        .ok_or(EngineContractError::MissingSessionId)
}

fn require_payload(
    frame: &EngineFrame,
    predicate: impl FnOnce(&EnginePayload) -> bool,
) -> Result<(), EngineContractError> {
    match frame.payload.as_ref() {
        Some(payload) if predicate(payload) => Ok(()),
        Some(_) => Err(EngineContractError::PayloadSchemaMismatch),
        None => Err(EngineContractError::MissingPayload),
    }
}

pub(crate) fn reject_ui_concepts(value: &Value) -> Result<(), EngineContractError> {
    const RUST_OWNED_FIELDS: &[&str] = &[
        "pill_state",
        "ui_state",
        "paste_outcome",
        "paste_method",
        "copied_fallback",
        "repaste",
        "focus_snapshot",
        "tray_state",
        "connected_state",
    ];

    fn walk(value: &Value) -> Option<String> {
        match value {
            Value::Object(map) => {
                for key in map.keys() {
                    if RUST_OWNED_FIELDS.contains(&key.as_str()) {
                        return Some(key.clone());
                    }
                }
                map.values().find_map(walk)
            }
            Value::Array(items) => items.iter().find_map(walk),
            _ => None,
        }
    }

    if let Some(field) = walk(value) {
        Err(EngineContractError::UiConceptLeaked(field))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct FakeEngine {
    transcript: String,
}

impl FakeEngine {
    pub fn fixed_transcript(transcript: impl Into<String>) -> Self {
        Self {
            transcript: transcript.into(),
        }
    }

    pub fn transcript_final_frame(
        &self,
        session_id: &str,
        request_id: &str,
        trace_id: &str,
    ) -> EngineFrame {
        EngineFrame::base(
            EngineSchemaName::TranscriptFinal,
            request_id,
            Some(session_id.to_string()),
            trace_id,
            Some(EnginePayload::TranscriptFinal {
                text: self.transcript.clone(),
                confidence: Some(1.0),
                diagnostics: None,
            }),
            None,
        )
    }
}
