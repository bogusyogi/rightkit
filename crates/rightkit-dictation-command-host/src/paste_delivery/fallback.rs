//! What an automatic clipboard fallback did to the clipboard.
//!
//! A refused or failed delivery used to end in "copy the transcript to the clipboard" with an
//! unconditional write, which could overwrite something the user copied in the meantime, or a
//! value the delivery never snapshotted. Now no automatic path writes the clipboard after the
//! delivery's own staged write. The fallback is one of two outcomes:
//!
//! * [`FallbackClipboard::TranscriptStaged`]: this delivery wrote the clipboard (the promised
//!   transcript), still owns that write by generation and OS change count (the evidence the
//!   restore gate uses), and nobody changed it since. The transcript is on the clipboard because
//!   the delivery put it there; nothing is written.
//! * [`FallbackClipboard::Preserved`]: anything else. The clipboard is left exactly as found and
//!   the host offers an explicit Copy action. Only that user-initiated copy
//!   (`copy_text` / `write_clipboard_text`) may overwrite the clipboard.
//!
//! The record carries a preserved outcome as `CopiedFallback` with [`preserved_reason`], the
//! library's dedicated `clipboard_preserved` reason, which the shell maps to "ready to copy" with
//! the recovery card's Copy action.

use rightkit_transcript_library::delivery::{CopyFallbackReason, DeliveryOutcome, DeliveryRecord};

use crate::restore_gate::RestoreVerdict;

/// Why the clipboard was left alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreservedCause {
    /// The OS change count or clipboard ownership shows another writer since this delivery's
    /// write: the user's newer copy is kept.
    ChangedByOther,
    /// This delivery never wrote the clipboard (the refusal came before the stage, or the stage
    /// failed), or a newer delivery owns it. There is nothing of this delivery's to keep and the
    /// user's value was never snapshotted, so it is not touched.
    NotOwned,
    /// The change count could not be read: ownership cannot be shown, so nothing is written.
    Unverifiable,
}

impl PreservedCause {
    pub fn code(self) -> &'static str {
        match self {
            PreservedCause::ChangedByOther => "changed_by_other",
            PreservedCause::NotOwned => "not_owned",
            PreservedCause::Unverifiable => "unverifiable",
        }
    }
}

/// The clipboard side of an automatic fallback. The transcript stays available to the host in
/// both cases (the record carries it); this says whether the clipboard holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackClipboard {
    /// This delivery's own staged write is still current and still holds the transcript.
    TranscriptStaged,
    /// Left as found; the host offers an explicit Copy.
    Preserved(PreservedCause),
}

impl FallbackClipboard {
    /// The state before a delivery has written anything.
    pub const NOT_OWNED: Self = Self::Preserved(PreservedCause::NotOwned);

    /// Map the restore gate's ownership verdict (generation + OS change count + owner signal).
    pub fn from_verdict(verdict: RestoreVerdict) -> Self {
        match verdict {
            RestoreVerdict::Proceed => Self::TranscriptStaged,
            RestoreVerdict::ClipboardChanged => Self::Preserved(PreservedCause::ChangedByOther),
            RestoreVerdict::StaleGeneration => Self::Preserved(PreservedCause::NotOwned),
            RestoreVerdict::CountUnreadable => Self::Preserved(PreservedCause::Unverifiable),
        }
    }

    pub fn is_preserved(self) -> bool {
        matches!(self, Self::Preserved(_))
    }

    /// Event-sink detail: `staged` or `preserved_<cause>`.
    pub fn code(self) -> String {
        match self {
            Self::TranscriptStaged => "staged".to_string(),
            Self::Preserved(cause) => format!("preserved_{}", cause.code()),
        }
    }

    /// The reason the delivery record carries: the original fallback reason while the transcript
    /// is on the clipboard, [`preserved_reason`] when the clipboard was left alone.
    pub fn record_reason(self, original: CopyFallbackReason) -> CopyFallbackReason {
        match self {
            Self::TranscriptStaged => original,
            Self::Preserved(_) => preserved_reason(),
        }
    }
}

/// The `CopyFallbackReason` a record carries when the clipboard was preserved: the library's
/// dedicated `CopyFallbackReason::clipboard_preserved()` (`rightkit-transcript-library`, commit
/// `b4e2bd189`). On the wire it is the extensible `Other("clipboard_preserved")`, so an older
/// reader opens the record. The shell maps `is_clipboard_preserved()` to its "ready to copy" look
/// (`state_decisions::pill_look` in `rightkit-dictation-tauri-session`); the true cause is in the
/// `delivery_fallback_clipboard` event and the `fallback_clipboard_*` timing step.
pub fn preserved_reason() -> CopyFallbackReason {
    CopyFallbackReason::clipboard_preserved()
}

/// True when `record` is a fallback that left the clipboard alone: the dedicated preserved reason,
/// or a lossy snapshot (which also wrote nothing). The host must offer the explicit Copy and must
/// not say "copied".
pub fn clipboard_preserved(record: &DeliveryRecord) -> bool {
    match &record.outcome {
        DeliveryOutcome::CopiedFallback { reason } => {
            reason.is_clipboard_preserved() || *reason == CopyFallbackReason::ClipboardLossySnapshot
        }
        _ => false,
    }
}
