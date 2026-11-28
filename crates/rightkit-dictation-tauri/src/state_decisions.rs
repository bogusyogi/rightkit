//! Pure decisions over [`AppState`]: which committed states carry a stored record, which leave the
//! transcript on the clipboard without a confirmed insertion, which end a sidecar emission batch,
//! and what look the pill takes for a state. No I/O, no process-wide state, no wording: the host
//! turns a [`PillLook`] into its own labels and widgets.
//!
//! Two rules hold across the module:
//!
//! - A diagnostic names a state only through [`state_tag`]; `AppState` derives `Debug` and carries
//!   transcripts, so it is never formatted into a log or telemetry line.
//! - A look asserts a success only when the state proves it. "Copied" needs a clipboard write the
//!   delivery reported as successful ([`fallback_confirms_clipboard_write`]); anything else that is
//!   not proven takes the recovery look, which offers an explicit Copy and claims nothing.

use rightkit_dictation_core::state::AppState;
use rightkit_transcript_library::delivery::CopyFallbackReason;

/// The record id a committed delivery state carries (`Pasted`, `PasteSentUnconfirmed`,
/// `CopiedFallback`); `None` for every other state.
pub fn delivered_record_id(state: &AppState) -> Option<&str> {
    match state {
        AppState::Pasted { delivery_id, .. }
        | AppState::PasteSentUnconfirmed { delivery_id, .. }
        | AppState::CopiedFallback { delivery_id, .. } => Some(delivery_id.as_str()),
        _ => None,
    }
}

/// Decides whether `state` ends without a confirmed insertion and still holds a transcript the
/// user may want, in which case a recovery surface must be offered so the user can paste or copy it
/// manually.
///
/// `PasteSentUnconfirmed` and `CopiedFallback` both end with no confirmed paste, so both trigger
/// recovery, whether or not the transcript reached the clipboard (a `CopiedFallback` whose reason is
/// `ClipboardUnavailable`, `ClipboardLossySnapshot` or a preserved clipboard (`is_clipboard_preserved()`) wrote nothing;
/// the recovery surface's Copy action is then the only copy). `Pasted` is a confirmed insertion and
/// never does. An empty or whitespace-only transcript never does.
pub fn clipboard_recovery_transcript(state: &AppState) -> Option<&str> {
    let transcript = match state {
        AppState::PasteSentUnconfirmed { transcript, .. } => transcript,
        AppState::CopiedFallback { transcript, .. } => transcript,
        _ => return None,
    };
    if transcript.trim().is_empty() {
        return None;
    }
    Some(transcript.as_str())
}

/// A stable, content-free tag for a state, for logs, telemetry and event sinks. It names the
/// variant only: no session id, delivery id, transcript, reason text or error message.
///
/// The match has no wildcard arm, so a new `AppState` variant does not compile until it is tagged.
pub fn state_tag(state: &AppState) -> &'static str {
    match state {
        AppState::Armed => "armed",
        AppState::Recording { .. } => "recording",
        AppState::Transcribing { .. } => "transcribing",
        AppState::Pasting { .. } => "pasting",
        AppState::Pasted { .. } => "pasted",
        AppState::PasteSentUnconfirmed { .. } => "paste_sent_unconfirmed",
        AppState::CopiedFallback { .. } => "copied_fallback",
        AppState::Cancelled { .. } => "cancelled",
        AppState::Error { .. } => "error",
    }
}

/// `CopyFallbackReason::Other` codes that the delivery paths report only after their own fallback
/// clipboard write succeeded (the evidence gate's refusals, the unverifiable-insertion refusal and
/// the original-editor recoveries of `final_delivery`).
const WRITTEN_OTHER_CODES: &[&str] = &[
    "secure_target",
    "secure_unknown",
    "evidence_timeout",
    "kind_mismatch",
    "synthetic_insertion_unverifiable",
    "start_context_unavailable",
    "original_editor_restore_failed",
    "original_editor_unchanged",
    "original_editor_changed",
    "original_editor_unreadable",
    "original_editor_destroyed",
    "original_editor_missing_baseline",
];

/// Whether a `CopiedFallback` reason says the transcript was written to the clipboard.
///
/// The delivery backend replaces the reason with `ClipboardUnavailable` when its fallback write
/// fails, refuses the write for `ClipboardLossySnapshot`, and a preserved clipboard
/// (`CopyFallbackReason::is_clipboard_preserved()`) writes nothing, so none of those is a copy. The
/// answer is an allow-list: a reason this function does not name (a new code, an unknown `Other`)
/// is not confirmed, and the pill shows the recovery look instead of "Copied". The preserved
/// reason is named below only to say so explicitly; the allow-list would exclude it anyway.
pub fn fallback_confirms_clipboard_write(reason: &CopyFallbackReason) -> bool {
    if reason.is_clipboard_preserved() {
        return false;
    }
    match reason {
        CopyFallbackReason::ElevatedTarget
        | CopyFallbackReason::FocusChanged
        | CopyFallbackReason::PasteFailed
        | CopyFallbackReason::NoTextField
        | CopyFallbackReason::NoFocusPin => true,
        CopyFallbackReason::Other(code) => WRITTEN_OTHER_CODES.contains(&code.as_str()),
        // No write happened or it failed; an unsupported platform has no clipboard write.
        CopyFallbackReason::ClipboardUnavailable
        | CopyFallbackReason::ClipboardLossySnapshot
        | CopyFallbackReason::UnsupportedPlatform
        | CopyFallbackReason::EmptyTranscript => false,
    }
}

/// True for the committed states that end a dictation with a record or a discard. A sidecar
/// emission batch defers the pill's final look for the last such state so it lands after every
/// mirror emission of the batch.
pub fn is_terminal_pill_state(state: &AppState) -> bool {
    matches!(
        state,
        AppState::Pasted { .. }
            | AppState::PasteSentUnconfirmed { .. }
            | AppState::CopiedFallback { .. }
            | AppState::Cancelled { .. }
    )
}

/// What the pill should look like for a committed state. The host maps each variant to its own
/// event type and wording.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PillLook<'a> {
    /// Armed: the resting look (the host decides whether the resting sliver is visible).
    Idle,
    /// Recording. Committed only after backend acceptance; start never routes through a spinner.
    Recording,
    /// Transcribing or pasting. Final-only ASR: there is no editable preview to cancel.
    Processing,
    /// Confirmed insertion; `send_enter` says whether Enter was part of it.
    Pasted { send_enter: bool },
    /// The paste was sent but its insertion could not be confirmed.
    PastedUnverified,
    /// Nothing was delivered: a cancel (finalized to history, not to an application) or an empty
    /// transcript.
    Cancelled,
    /// Recovery: no clipboard write is confirmed (a lossy clipboard snapshot was refused, the write
    /// failed, or the clipboard was preserved, `is_clipboard_preserved()`, and nothing was written), so the clipboard may
    /// hold something else. The host shows an explicit Copy action and must not say "Copied".
    ReadyToCopy,
    /// The transcript was copied to the clipboard instead of inserted, and the delivery reported
    /// that write as successful.
    Copied,
    /// A failed dictation; the host picks the wording from the message.
    Error { message: &'a str },
}

/// The pill look for a committed state.
pub fn pill_look(state: &AppState) -> PillLook<'_> {
    match state {
        AppState::Armed => PillLook::Idle,
        AppState::Recording { .. } => PillLook::Recording,
        AppState::Transcribing { .. } | AppState::Pasting { .. } => PillLook::Processing,
        AppState::Pasted { send_enter, .. } => PillLook::Pasted {
            send_enter: *send_enter,
        },
        AppState::PasteSentUnconfirmed { .. } => PillLook::PastedUnverified,
        AppState::CopiedFallback {
            reason: CopyFallbackReason::EmptyTranscript,
            ..
        } => PillLook::Cancelled,
        // The delivery left the user's clipboard as it found it (the library's
        // `clipboard_preserved` reason): the recovery look with an explicit Copy, never "Copied".
        AppState::CopiedFallback { reason, .. } if reason.is_clipboard_preserved() => {
            PillLook::ReadyToCopy
        }
        AppState::CopiedFallback { reason, .. } if fallback_confirms_clipboard_write(reason) => {
            PillLook::Copied
        }
        // ClipboardUnavailable, ClipboardLossySnapshot, and any reason not proven to have followed a
        // successful write: recovery with an explicit Copy.
        AppState::CopiedFallback { .. } => PillLook::ReadyToCopy,
        AppState::Cancelled { .. } => PillLook::Cancelled,
        AppState::Error { message, .. } => PillLook::Error {
            message: message.as_str(),
        },
    }
}

/// Whether a dictation session is in flight: recording, transcribing or pasting. The states an
/// engine restart, a model activation or a settings change must not tear down under.
pub fn session_busy(state: &AppState) -> bool {
    matches!(
        state,
        AppState::Recording { .. } | AppState::Transcribing { .. } | AppState::Pasting { .. }
    )
}

/// Which tray icon a committed state takes. The host maps each variant to its own icon file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayLook {
    /// Resting: armed, and every finished dictation (delivered, copied, cancelled).
    Armed,
    /// Recording.
    Recording,
    /// Transcribing or pasting.
    Transcribing,
    /// A failed dictation.
    Disabled,
}

/// The tray look for a committed state.
pub fn tray_look(state: &AppState) -> TrayLook {
    match state {
        AppState::Recording { .. } => TrayLook::Recording,
        AppState::Transcribing { .. } | AppState::Pasting { .. } => TrayLook::Transcribing,
        AppState::Error { .. } => TrayLook::Disabled,
        AppState::Armed
        | AppState::Pasted { .. }
        | AppState::PasteSentUnconfirmed { .. }
        | AppState::CopiedFallback { .. }
        | AppState::Cancelled { .. } => TrayLook::Armed,
    }
}

/// What a committed state does to a "microphone is blocked" alert.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlertSignal {
    /// Turn the alert on.
    Raise,
    /// Turn the alert off.
    Clear,
    /// Leave it as it is.
    Keep,
}

/// The microphone-alert decision for a committed state: an error that says microphone access was
/// denied raises it ([`crate::dictation_error::is_microphone_access_denied_error`]); recording or
/// transcribing proves capture opened, so it clears; every other state keeps it.
pub fn microphone_alert(state: &AppState) -> AlertSignal {
    match state {
        AppState::Error { message, .. }
            if crate::dictation_error::is_microphone_access_denied_error(message) =>
        {
            AlertSignal::Raise
        }
        AppState::Recording { .. } | AppState::Transcribing { .. } => AlertSignal::Clear,
        _ => AlertSignal::Keep,
    }
}
