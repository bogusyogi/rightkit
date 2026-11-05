use super::*;
use super::{clipboard::*, insert::*, targets::*};

use std::time::Instant;

pub fn deliver_text(transcript: &str) -> Result<DeliveryRecord, DeliveryError> {
    deliver_text_with_enter(transcript, false)
}

pub fn deliver_text_with_enter(
    transcript: &str,
    send_enter: bool,
) -> Result<DeliveryRecord, DeliveryError> {
    let started = Instant::now();
    let mut steps = Vec::new();
    let delivery_id = next_delivery_id(now_ms());
    let target_started = Instant::now();
    let target = snapshot_target();
    steps.push(DeliveryTimingStep::new(
        "target_snapshot",
        elapsed_ms(target_started),
    ));

    if transcript.trim().is_empty() {
        let total_ms = elapsed_ms(started);
        return Ok(DeliveryRecord::new_with_timing(
            delivery_id,
            "",
            DeliveryOutcome::CopiedFallback {
                reason: CopyFallbackReason::EmptyTranscript,
            },
            target,
            total_ms,
        )
        .with_delivery_timing(DeliveryTimings::from_steps(total_ms, steps)));
    }

    // Insert the transcript via clipboard + Ctrl+V (Windows) or clipboard + Cmd+V
    // (macOS). The bare-Alt KeyTips bug that originally motivated the switch to
    // type_unicode is gone: send_paste_keystroke never sends a lone VK_MENU key-up.
    // Clipboard+paste is instant for any length text and works in every target
    // including Electron/Claude (type_unicode is per-character and Electron fires a
    // synthetic `input` event for each keystroke, producing char-by-char insertion).
    // The user's existing clipboard is saved before the write and restored after the
    // paste so dictation never clobbers it (Wispr/Superwhisper pattern).
    match insert_transcript(transcript, &target, send_enter) {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        Ok(InsertResult::Confirmed {
            steps: mut insert_steps,
            proof,
        }) => {
            steps.append(&mut insert_steps);
            let total_ms = elapsed_ms(started);
            Ok(
                pasted_delivery_record(delivery_id, transcript, target, total_ms, proof)
                    .with_delivery_timing(DeliveryTimings::from_steps(total_ms, steps)),
            )
        }
        Err(insert_error) => {
            // Spec 4.4: a posted-but-unproven insertion is exactly
            // `PasteSentUnconfirmed` — never `Pasted`, never
            // `CopiedFallback`. The promised transaction still stages the
            // transcript for recovery, so no second clipboard write runs.
            if insert_error.code == "E_UNCONFIRMED_INSERTION" {
                let total_ms = elapsed_ms(started);
                return Ok(DeliveryRecord::new_with_timing(
                    delivery_id,
                    transcript,
                    DeliveryOutcome::PasteSentUnconfirmed {
                        reason: insert_error.message.clone(),
                    },
                    target,
                    total_ms,
                )
                .with_delivery_timing(DeliveryTimings::from_steps(total_ms, steps)));
            }
            // Delivery spec section 4.5: any clipboard transaction failure
            // leaves ownership uncertain (including snapshot-to-stage races).
            // Never follow it with destructive fallback text, which could
            // overwrite a newer clipboard owner. Recovery offers explicit
            // Copy instead.
            if should_leave_clipboard_untouched(&insert_error) {
                let reason = copy_fallback_reason_for_insert_error(&insert_error);
                let total_ms = elapsed_ms(started);
                return Ok(DeliveryRecord::new_with_timing(
                    delivery_id,
                    transcript,
                    DeliveryOutcome::CopiedFallback { reason },
                    target,
                    total_ms,
                )
                .with_delivery_timing(DeliveryTimings::from_steps(total_ms, steps)));
            }
            let fallback_started = Instant::now();
            let reason = match write_clipboard_text(transcript) {
                // The clipboard write itself is the truthful failure here —
                // it takes precedence over whatever made the paste attempt
                // fail, since it is the more recent, more specific cause.
                Err(reason) => reason,
                // Paste failed but the clipboard fallback landed: report WHY
                // the paste attempt failed instead of the generic
                // `PasteFailed` guess. Previously this branch discarded
                // `insert_error` entirely (matched as `Err(_)`), so history
                // and telemetry could not distinguish "Accessibility not
                // granted" / "clipboard snapshot failed" / "focus moved
                // mid-paste" from an ordinary keystroke-post failure — see
                // `copy_fallback_reason_for_insert_error` for the mapping.
                Ok(()) => copy_fallback_reason_for_insert_error(&insert_error),
            };
            steps.push(DeliveryTimingStep::new(
                "fallback_clipboard_write",
                elapsed_ms(fallback_started),
            ));
            let total_ms = elapsed_ms(started);
            Ok(DeliveryRecord::new_with_timing(
                delivery_id,
                transcript,
                DeliveryOutcome::CopiedFallback { reason },
                target,
                total_ms,
            )
            .with_delivery_timing(DeliveryTimings::from_steps(total_ms, steps)))
        }
    }
}

fn copy_fallback_reason_for_insert_error(error: &DeliveryError) -> CopyFallbackReason {
    match error.code.as_str() {
        // Clipboard snapshot/stage/write failed before any keystroke was
        // attempted — the truthful reason is clipboard, not paste. The lossy
        // snapshot case (spec section 4.5: destructive transaction refused
        // because the prior clipboard held unrepresentable formats) carries
        // its typed reason in the message.
        "E_CLIPBOARD" => match error.message.as_str() {
            "clipboard_lossy_snapshot" => CopyFallbackReason::ClipboardLossySnapshot,
            _ => CopyFallbackReason::ClipboardUnavailable,
        },
        // No Accessibility permission means CGEvent posts are silently
        // dropped by the OS; keep the fully-specific reason (there is no
        // dedicated variant) rather than collapsing it into a generic
        // "paste failed" the user cannot act on.
        "E_ACCESSIBILITY" => CopyFallbackReason::Other("accessibility_not_granted".to_string()),
        "E_UNSUPPORTED" => CopyFallbackReason::UnsupportedPlatform,
        // Focused control is a password field, or its security state could not
        // be verified — the transcript stays on the clipboard only, never
        // pasted or sent (HR-DELIVERY-004, HR-CONTROL-011).
        "E_SECURE_TARGET" => CopyFallbackReason::Other("secure_target".to_string()),
        "E_FOCUS_CHANGED" => CopyFallbackReason::FocusChanged,
        // Insertion was posted (native write or paste keystroke) but the
        // required before/after read-back never confirmed it landed before
        // the caller needed to decide whether to submit — HR-DELIVERY-006/
        // 007/008. Never treat this as an ordinary keystroke-post failure:
        // the transcript may or may not have landed, so no second automated
        // insertion is attempted; only the passive clipboard copy remains.
        "E_UNCONFIRMED_INSERTION" => CopyFallbackReason::PasteFailed,
        // "E_SENDINPUT" / "E_CGEVENT" (keystroke post itself failed) and any
        // other/unknown code keep the existing, still-accurate default.
        _ => CopyFallbackReason::PasteFailed,
    }
}

pub(crate) fn should_leave_clipboard_untouched(error: &DeliveryError) -> bool {
    error.code == "E_CLIPBOARD"
        || copy_fallback_reason_for_insert_error(error)
            == CopyFallbackReason::ClipboardLossySnapshot
}

pub fn copy_fallback_record(
    transcript: &str,
    reason: CopyFallbackReason,
    target: TargetSnapshot,
) -> DeliveryRecord {
    copy_fallback_record_with_clipboard(transcript, reason, target, write_clipboard_text)
}

pub fn copy_text(text: &str) -> Result<(), DeliveryError> {
    copy_text_with_clipboard(text, write_clipboard_text)
}

pub fn paste_text(text: &str) -> Result<(), DeliveryError> {
    insert_transcript(text, &snapshot_target(), false).map(|_| ())
}
