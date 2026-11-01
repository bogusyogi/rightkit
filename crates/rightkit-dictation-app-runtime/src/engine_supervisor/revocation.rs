// Operation-revocation acknowledgement (#149).
//
// The shell watchdog may only re-arm the pill once the sidecar has confirmed
// it stopped the stuck operation, or the sidecar process has been killed. The
// existing cancel reply is the ack: `TranscribingStarted { stop_kind: Cancel }`
// with the request id echoed and no error.

use rightkit_dictation_core::engine::{EngineFrame, EnginePayload, StopKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevocationOutcome {
    /// Sidecar confirmed the cancel.
    Acked,
    /// No sidecar process; nothing can still be running.
    NotRunning,
    /// No valid ack within the bound; the sidecar was shut down to force revocation.
    Failed { reason: String },
}

pub fn classify_revocation_reply(reply: Result<EngineFrame, String>) -> RevocationOutcome {
    match reply {
        Err(err) => RevocationOutcome::Failed { reason: err },
        Ok(frame) => {
            if let Some(error) = frame.error {
                return RevocationOutcome::Failed {
                    reason: format!("sidecar error: {}", error.message),
                };
            }
            match frame.payload {
                Some(EnginePayload::TranscribingStarted {
                    stop_kind: Some(StopKind::Cancel),
                    ..
                }) => RevocationOutcome::Acked,
                _ => RevocationOutcome::Failed {
                    reason: "unexpected revocation reply".to_string(),
                },
            }
        }
    }
}
