//! App state machine - pure transition logic. No IO, no GUI.

use rightkit_transcript_library::delivery::{CopyFallbackReason, DeliveryOutcome, DeliveryRecord};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[derive(Default)]
pub enum AppState {
    #[default]
    Armed,
    Recording {
        session_id: String,
    },
    Transcribing {
        session_id: String,
    },
    Pasting {
        session_id: String,
        transcript: String,
        #[serde(default)]
        send_enter: bool,
    },
    Pasted {
        delivery_id: String,
        transcript: String,
        #[serde(default)]
        send_enter: bool,
    },
    PasteSentUnconfirmed {
        delivery_id: String,
        transcript: String,
        reason: String,
    },
    CopiedFallback {
        delivery_id: String,
        transcript: String,
        reason: CopyFallbackReason,
        #[serde(default)]
        send_enter: bool,
    },
    /// A cancel (any source) finalized to encrypted history instead of an OS
    /// delivery — no clipboard write, no paste, no Enter. `transcript` is the
    /// raw/L0 text that was stored. Terminal, like `Pasted`/`CopiedFallback`;
    /// returns to `Armed` on `ResetToArmed` or the next `StartRecording`.
    Cancelled {
        transcript: String,
    },
    Error {
        message: String,
        #[serde(default)]
        last_transcript: Option<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AppEvent {
    StartRecording {
        session_id: String,
    },
    StopRecording,
    TranscriptFinal {
        transcript: String,
        #[serde(default)]
        send_enter: bool,
    },
    PasteSucceeded {
        delivery_id: String,
        #[serde(default)]
        send_enter: bool,
    },
    PasteSentUnconfirmed {
        delivery_id: String,
        reason: String,
    },
    CopyFallback {
        delivery_id: String,
        reason: CopyFallbackReason,
        #[serde(default)]
        send_enter: bool,
    },
    Fail {
        message: String,
    },
    ResetToArmed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransitionResult {
    pub previous_state: AppState,
    pub event: AppEvent,
    pub next_state: AppState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransitionError {
    pub state: Box<AppState>,
    pub event: Box<AppEvent>,
}

pub fn transition(
    current_state: AppState,
    event: AppEvent,
) -> Result<TransitionResult, TransitionError> {
    let next_state = match (&current_state, &event) {
        (AppState::Armed, AppEvent::StartRecording { session_id }) => AppState::Recording {
            session_id: session_id.clone(),
        },
        (AppState::Recording { session_id }, AppEvent::StopRecording) => AppState::Transcribing {
            session_id: session_id.clone(),
        },
        (
            AppState::Transcribing { session_id },
            AppEvent::TranscriptFinal {
                transcript,
                send_enter,
            },
        ) if !transcript.trim().is_empty() => AppState::Pasting {
            session_id: session_id.clone(),
            transcript: transcript.clone(),
            send_enter: *send_enter,
        },
        (
            AppState::Transcribing { .. },
            AppEvent::TranscriptFinal {
                transcript,
                send_enter,
            },
        ) if transcript.trim().is_empty() => AppState::CopiedFallback {
            delivery_id: String::new(),
            transcript: String::new(),
            reason: CopyFallbackReason::EmptyTranscript,
            send_enter: *send_enter,
        },
        (
            AppState::Pasting {
                transcript,
                send_enter,
                ..
            },
            AppEvent::PasteSucceeded {
                delivery_id,
                send_enter: event_send_enter,
            },
        ) => AppState::Pasted {
            delivery_id: delivery_id.clone(),
            transcript: transcript.clone(),
            send_enter: *send_enter || *event_send_enter,
        },
        (
            AppState::Pasting { transcript, .. },
            AppEvent::PasteSentUnconfirmed {
                delivery_id,
                reason,
            },
        ) => AppState::PasteSentUnconfirmed {
            delivery_id: delivery_id.clone(),
            transcript: transcript.clone(),
            reason: reason.clone(),
        },
        (
            AppState::Pasting {
                transcript,
                send_enter,
                ..
            },
            AppEvent::CopyFallback {
                delivery_id,
                reason,
                send_enter: event_send_enter,
            },
        ) => AppState::CopiedFallback {
            delivery_id: delivery_id.clone(),
            transcript: transcript.clone(),
            reason: reason.clone(),
            send_enter: *send_enter || *event_send_enter,
        },
        (
            AppState::Pasted { .. }
            | AppState::PasteSentUnconfirmed { .. }
            | AppState::CopiedFallback { .. }
            | AppState::Cancelled { .. }
            | AppState::Error { .. },
            AppEvent::ResetToArmed,
        ) => AppState::Armed,
        (_, AppEvent::Fail { message }) => AppState::Error {
            message: message.clone(),
            last_transcript: last_transcript_for_error(&current_state),
        },
        _ => {
            return Err(TransitionError {
                state: Box::new(current_state),
                event: Box::new(event),
            });
        }
    };

    Ok(TransitionResult {
        previous_state: current_state,
        event,
        next_state,
    })
}

pub fn delivery_outcome_event(record: &DeliveryRecord) -> AppEvent {
    delivery_outcome_event_with_send(record, false)
}

/// A requested Enter only becomes submitted after delivery records its posted
/// keystroke. Legacy records without step timings retain their event intent.
pub fn delivery_enter_was_submitted(record: &DeliveryRecord, requested: bool) -> bool {
    requested
        && record.delivery_timing.as_ref().is_none_or(|timing| {
            timing.steps.iter().any(|step| {
                // "paste_settle_and_submit" - clipboard+paste path (macOS
                // and Windows), kept even where the wait is now AX/UIA-
                // evidence-gated rather than a fixed settle, so this literal
                // stays accurate in name only, not behavior.
                // "enter_keystroke" - retained for legacy/older records.
                step.name == "paste_settle_and_submit" || step.name == "enter_keystroke"
            })
        })
}

pub fn delivery_outcome_event_with_send(record: &DeliveryRecord, send_enter: bool) -> AppEvent {
    let send_enter = delivery_enter_was_submitted(record, send_enter);
    match &record.outcome {
        DeliveryOutcome::Pasted => AppEvent::PasteSucceeded {
            delivery_id: record.delivery_id.clone(),
            send_enter,
        },
        DeliveryOutcome::PasteSentUnconfirmed { reason } => AppEvent::PasteSentUnconfirmed {
            delivery_id: record.delivery_id.clone(),
            reason: reason.clone(),
        },
        DeliveryOutcome::CopiedFallback { reason } => AppEvent::CopyFallback {
            delivery_id: record.delivery_id.clone(),
            reason: reason.clone(),
            send_enter,
        },
        DeliveryOutcome::Error { message, .. } => AppEvent::Fail {
            message: message.clone(),
        },
        // Unreachable by design: a `Cancelled` record is finalized directly by
        // `EngineRuntime::accept_cancel_to_history` (mirrors
        // `accept_sidecar_reset`'s direct `self.state = ...; observer(...)`
        // idiom), never through `accept_sidecar_delivery` /
        // `delivery_outcome_event_with_send`. This arm exists only so the match
        // stays exhaustive; a loud `Fail` is safer than silently misreporting a
        // cancel as `CopiedFallback` if that invariant is ever broken.
        DeliveryOutcome::Cancelled => AppEvent::Fail {
            message: "internal: cancelled delivery routed through the normal delivery pipeline"
                .to_string(),
        },
        // Unreachable by design, for the same reason as `Cancelled` above: a
        // `Draft` record is written directly via `HistoryStore::upsert_draft`
        // from the shell's `TranscriptPartial` handler and superseded
        // directly by `accept_sidecar_delivery`/`accept_cancel_to_history`,
        // never through this pipeline. Kept only for match exhaustiveness.
        DeliveryOutcome::Draft => AppEvent::Fail {
            message: "internal: draft record routed through the normal delivery pipeline"
                .to_string(),
        },
    }
}

fn last_transcript_for_error(state: &AppState) -> Option<String> {
    match state {
        AppState::Pasting { transcript, .. }
        | AppState::Pasted { transcript, .. }
        | AppState::PasteSentUnconfirmed { transcript, .. }
        | AppState::CopiedFallback { transcript, .. } => Some(transcript.clone()),
        AppState::Error {
            last_transcript, ..
        } => last_transcript.clone(),
        _ => None,
    }
}
