//! Final-transcript delivery policy: which destination receives the text and what happens when the
//! destination cannot be proven.
//!
//! The recording's start context decides the policy (`recording_delivery_policy*`):
//!
//! - original-bound: the provider saw the start field, so the text goes only to that exact editor.
//!   The retained start pin is re-validated against the content fingerprint captured at start; a
//!   replaced editor is accepted only when it is provably the same box (`replacement_editor_may_
//!   receive`: original gone, same process and window, candidate not secure, same content). Anything
//!   less recovers to the clipboard and never focuses a stale editor.
//! - unbound: the frozen stop destination receives it; the delivery backend still validates secure
//!   field, elevation and that the destination is current at paste time.
//! - recovery: the clipboard, never a paste.
//!
//! Unknown refuses. A secure-field probe result (`Option<bool>`) is passed to the proof contract
//! unchanged and never negated or defaulted; only `Some(false)` can permit a write. A platform with
//! no secure-field probe has no automatic write at all.
//!
//! The outcome type is [`crate::host::FinalOutcome`]; [`deliver_final_for_host`] is the adapter for
//! the host seam (`HostPin` in, `FinalOutcome` out).
//!
//! This module only decides and delivers through `rightkit_dictation_command_host::paste_delivery`;
//! the host supplies the [`DeliveryHost`] values (timeouts, budgets, per-app lists, sink). Nothing
//! here holds state: the caller owns the session, its pins and its history.

use rightkit_dictation_command_host::paste_delivery::{
    self as delivery, copy_fallback_record, CopyFallbackReason, DeliveryBackend, DeliveryHost,
    DeliveryRecord, RealDeliveryBackend, TargetSnapshot,
};
use rightkit_dictation_core::engine::{
    recording_delivery_policy, recording_delivery_policy_with_stop_proof, RecordingContextState,
    RecordingDeliveryPolicy,
};

pub use crate::host::FinalOutcome;

/// An exact retained editor: the macOS accessibility focus pin.
#[cfg(target_os = "macos")]
pub type FinalPin = rightkit_automation::desktop::macos::FocusPin;
/// An exact retained editor: the Windows UIA, native-window or WezTerm stop target.
#[cfg(target_os = "windows")]
pub type FinalPin = delivery::WindowsStopTarget;
/// No platform pins exist on this OS.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub enum FinalPin {}

/// Everything one final delivery needs.
pub struct FinalDeliveryInput<'a> {
    /// Transcript text to insert, with any control tail already removed. Not empty: an empty
    /// result goes through [`deliver_empty`].
    pub transcript: &'a str,
    /// Press Enter after the text lands.
    pub want_enter: bool,
    /// Whether the start field context was readable.
    pub start_context_state: Option<RecordingContextState>,
    /// Whether that context reached a provider.
    pub context_request_attempted: Option<bool>,
    /// Whether the provider bound the transcript to the start field.
    pub context_original_binding: Option<bool>,
    /// Destination frozen when recording started.
    pub start_target: Option<&'a TargetSnapshot>,
    /// Destination frozen at the first stop boundary.
    pub stop_target: Option<&'a TargetSnapshot>,
    /// Focus pin captured at start.
    pub start_pin: Option<&'a FinalPin>,
    /// Focus pin captured at stop.
    pub stop_pin: Option<&'a FinalPin>,
    /// Fingerprint of the start field content.
    pub start_content_fingerprint: Option<u64>,
    /// Called once the text has been inserted (before any Enter).
    pub on_inserted: &'a mut dyn FnMut(),
}

/// Deliver an empty result (a command-only utterance).
pub fn deliver_empty(host: &'static DeliveryHost) -> Result<DeliveryRecord, String> {
    RealDeliveryBackend::new(host)
        .deliver("")
        .map_err(|err| err.to_string())
}

/// Hash the authoritative text of a Windows stop target the way the start capture fingerprinted it.
#[cfg(target_os = "windows")]
fn content_fingerprint(host: &DeliveryHost, pin: &FinalPin) -> Option<u64> {
    pin.text_snapshot(std::time::Duration::from_millis(
        host.config.uia_text_read_ms,
    ))
    .and_then(|proof| {
        use std::hash::{Hash, Hasher};
        let text = proof.authoritative_text()?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        Some(hasher.finish())
    })
}

/// Choose the delivery policy for this recording.
fn delivery_policy(
    start_context_state: Option<RecordingContextState>,
    context_request_attempted: Option<bool>,
    context_original_binding: Option<bool>,
    stop_target: Option<&TargetSnapshot>,
) -> RecordingDeliveryPolicy {
    #[cfg(target_os = "windows")]
    {
        // Binding Some(false): no field context or selection reached a provider, so the
        // transcript is not bound to the start box and start->stop continuity is not required. It
        // goes to the frozen stop destination like any ordinary dictation; the delivery backend
        // still validates secure field, elevation and that the destination is current at paste
        // time. Binding None/Some(true) never reaches this shortcut and stays fail-closed.
        let stop_destination_available = context_original_binding == Some(false)
            && stop_target.is_some_and(|target| {
                target.process_id.is_some()
                    && target.window_handle.is_some()
                    && target.process_id != Some(std::process::id())
            });
        recording_delivery_policy_with_stop_proof(
            start_context_state,
            context_request_attempted,
            context_original_binding,
            stop_destination_available,
        )
    }
    #[cfg(target_os = "macos")]
    {
        // Unbound dictation belongs to the frozen stop destination. Start-context availability is
        // not destination evidence. The delivery backend still validates stop continuity, secure
        // fields and target identity immediately before dispatch.
        let stop_destination_available = stop_target
            .is_some_and(|target| target.process_id.is_some() && target.window_handle.is_some());
        recording_delivery_policy_with_stop_proof(
            start_context_state,
            context_request_attempted,
            context_original_binding,
            stop_destination_available,
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = stop_target;
        // Unknown (`None`) is the fail-closed value: "attempted and bound", never "unbound". The
        // other platforms' branches likewise never turn `None` into the unbound shortcut.
        recording_delivery_policy(
            start_context_state,
            context_request_attempted.unwrap_or(true),
            context_original_binding.unwrap_or(true),
        )
    }
}

/// Deliver a non-empty final transcript.
///
/// `Err` is a delivery backend failure; the caller records it as a failed dictation.
#[allow(unused_variables, unused_mut)]
pub fn deliver_final(
    host: &'static DeliveryHost,
    input: FinalDeliveryInput<'_>,
) -> Result<FinalOutcome, String> {
    let FinalDeliveryInput {
        transcript,
        want_enter,
        start_context_state,
        context_request_attempted,
        context_original_binding,
        start_target,
        stop_target,
        start_pin,
        stop_pin,
        start_content_fingerprint,
        on_inserted,
    } = input;
    let policy = delivery_policy(
        start_context_state,
        context_request_attempted,
        context_original_binding,
        stop_target,
    );
    let original_bound = matches!(policy, RecordingDeliveryPolicy::OriginalBound);
    let context_recovery = matches!(policy, RecordingDeliveryPolicy::Recovery);
    host.log(
        "start_context_delivery_policy",
        &format!(
            "state={:?} request_attempted={:?} original_binding={:?} recovery={}",
            start_context_state,
            context_request_attempted,
            context_original_binding,
            context_recovery
        ),
    );
    #[cfg(target_os = "windows")]
    {
        let mut original_proof_valid = !original_bound
            || start_content_fingerprint.is_some_and(|expected| {
                start_pin
                    .and_then(|pin| content_fingerprint(host, pin))
                    .is_some_and(|fingerprint| fingerprint == expected)
            });
        let mut replacement_accepted = false;
        if original_bound && !original_proof_valid {
            // UIA elements can be recreated while dictation is in flight. Accept replacement only
            // after the original element is no longer valid, same process/window holds, candidate
            // is non-secure, & exact content fingerprint matches. A failed read is never treated
            // as gone.
            let original_gone = start_pin.is_some_and(|pin| pin.is_gone().unwrap_or(false));
            let same_place = start_target.zip(stop_target).is_some_and(|(start, stop)| {
                start.process_id.is_some()
                    && start.process_id == stop.process_id
                    && start.window_handle == stop.window_handle
                    && start.foreground_target == stop.foreground_target
            });
            let candidate_fingerprint = stop_pin.and_then(|pin| content_fingerprint(host, pin));
            replacement_accepted = original_gone
                && delivery::replacement_editor_may_receive(
                    same_place,
                    start_content_fingerprint,
                    candidate_fingerprint,
                    // The probe's own tri-state, never negated or defaulted: `Some(true)` is a
                    // password field, `Some(false)` an ordinary one, `None` unknown (a timed-out
                    // or failed read). `replacement_editor_may_receive` requires
                    // `candidate_secure == Some(false)`, so only a positively non-secure field
                    // passes and unknown refuses. (Revision 1 passed `.map(|secure| !secure)`,
                    // which handed a password field to the contract as `Some(false)`.)
                    stop_pin.and_then(|_| delivery::windows_uia(host).focused_field_is_password()),
                );
            if replacement_accepted {
                host.log(
                    "start_context_replacement_editor",
                    "platform=windows reason=same_window_same_content",
                );
            }
        }
        if original_bound && original_proof_valid {
            let still_current =
                start_pin.is_some_and(|pin| delivery::validate_stop_target(host, pin));
            // Context was sent for this box. Losing focus recovers to clipboard; never restore it
            // or redirect to another box.
            original_proof_valid = still_current
                && start_content_fingerprint.is_some_and(|expected| {
                    start_pin
                        .and_then(|pin| content_fingerprint(host, pin))
                        .is_some_and(|fingerprint| fingerprint == expected)
                });
        } else if original_bound && !replacement_accepted {
            // Never focus a stale editor when the retained start content already changed or became
            // unreadable.
            original_proof_valid = false;
        }
        if context_recovery || (!original_proof_valid && !replacement_accepted) {
            return Ok(FinalOutcome::Recovery(copy_fallback_record(
                transcript,
                CopyFallbackReason::NoFocusPin,
                start_target.cloned().unwrap_or_else(TargetSnapshot::empty),
            )));
        }
        // Spec 3.2: a missing UIA stop token must not veto an otherwise valid ordinary paste. The
        // frozen snapshot still validates the destination; only a missing destination forces
        // recovery.
        let pin = if original_bound && !replacement_accepted {
            start_pin
        } else {
            stop_pin
        };
        let record = match pin {
            Some(pin) => RealDeliveryBackend::new(host)
                .deliver_stop_windows_with_progress(transcript, pin, want_enter, on_inserted)
                .map_err(|err| err.to_string())?,
            None => copy_fallback_record(
                transcript,
                CopyFallbackReason::NoFocusPin,
                stop_target.cloned().unwrap_or_else(TargetSnapshot::empty),
            ),
        };
        Ok(FinalOutcome::Delivered(record))
    }
    #[cfg(target_os = "macos")]
    {
        use rightkit_automation::desktop::macos as ax;
        // Original editor replaced by the app mid-recording: deliver to the box now standing in
        // its place, only when it is provably the same box (see `replacement_editor_may_receive`).
        use delivery::EditorContentStatus;
        let max_chars = host.config.editor_read_max_chars;
        let mut original_content_status = None;
        let mut original_intact_at_stop = None;
        let mut original_element_gone = None;
        let mut replacement_accepted = None;
        let original_bound = original_bound && {
            let fingerprint_of = |pin: &FinalPin| {
                ax::focused_field_context_and_fingerprint_for_pin(pin, max_chars)
                    .and_then(|(_, fingerprint)| fingerprint)
            };
            let status = delivery::read_bound_editor_content_macos(
                host,
                start_pin,
                start_content_fingerprint,
            );
            original_content_status = Some(status);
            let original_intact = status == EditorContentStatus::Unchanged;
            original_intact_at_stop = Some(original_intact);
            // "Replaced" needs the original to be GONE from the app, not merely unread: a flaky
            // read must never redirect the text.
            let original_gone = status == EditorContentStatus::Destroyed;
            original_element_gone = Some(original_gone);
            let replaced = !original_intact
                && original_gone
                && stop_pin.is_some_and(|candidate| {
                    let same_place = start_target.zip(stop_target).is_some_and(|(start, stop)| {
                        start.process_id.is_some()
                            && start.process_id == stop.process_id
                            && start.window_handle == stop.window_handle
                            && start.foreground_target == stop.foreground_target
                            && Some(candidate.pid() as u32) == start.process_id
                    });
                    ax::stop_focus_pin_is_current(candidate)
                        && delivery::replacement_editor_may_receive(
                            same_place,
                            start_content_fingerprint,
                            fingerprint_of(candidate),
                            ax::focused_field_is_secure_for_pin(candidate),
                        )
                });
            if replaced {
                host.log(
                    "start_context_replacement_editor",
                    "reason=same_window_same_content",
                );
            }
            replacement_accepted = Some(replaced);
            !replaced
        };
        // Spacing is judged on the VERIFIED delivery editor's CURRENT caret, read through the exact
        // pin the paste will use. A start-time hint would go stale if the caret moved or another
        // box in the same window took focus.
        let delivery_pin = if original_bound { start_pin } else { stop_pin };
        let needs_leading_space = delivery_pin
            .and_then(|pin| ax::focused_field_context_and_fingerprint_for_pin(pin, max_chars))
            .and_then(|(field, _)| field.char_before_caret)
            .is_some_and(|c| delivery::leading_space_needed(c.encode_utf8(&mut [0; 4]), None));
        let transcript = delivery::with_leading_space(transcript, needs_leading_space).into_owned();
        // Spacing/context reads can yield; validate again immediately before any focus
        // restoration, not from the earlier snapshot.
        if original_bound {
            original_content_status = Some(delivery::read_bound_editor_content_macos(
                host,
                start_pin,
                start_content_fingerprint,
            ));
        }
        let mut original_proof_valid =
            !original_bound || original_content_status == Some(EditorContentStatus::Unchanged);
        let original_read_match = original_proof_valid;
        let mut restore_succeeded = None;
        if original_bound && original_proof_valid {
            let restored = start_pin.is_some_and(ax::restore_focus_pin);
            restore_succeeded = Some(restored);
            if restored {
                original_content_status = Some(delivery::read_bound_editor_content_macos(
                    host,
                    start_pin,
                    start_content_fingerprint,
                ));
            }
            original_proof_valid =
                restored && original_content_status == Some(EditorContentStatus::Unchanged);
        } else if original_bound {
            // Never focus a stale editor when the retained start content already changed or became
            // unreadable.
            original_proof_valid = false;
        }
        if context_recovery || !original_proof_valid {
            let reason = if context_recovery {
                "start_context_unavailable".to_string()
            } else if restore_succeeded == Some(false) {
                "original_editor_restore_failed".to_string()
            } else {
                format!(
                    "original_editor_{}",
                    original_content_status
                        .unwrap_or(EditorContentStatus::Unreadable)
                        .label()
                )
            };
            host.log(
                "start_context_delivery_validation_rejected",
                &format!(
                    "policy_recovery={context_recovery} original_bound={original_bound} start_pin={} stop_pin={} same_editor={:?} original_intact={original_intact_at_stop:?} original_gone={original_element_gone:?} replacement={replacement_accepted:?} content_status={original_content_status:?} original_read_match={original_read_match} restore_succeeded={restore_succeeded:?} final_proof={original_proof_valid}",
                    start_pin.is_some(),
                    stop_pin.is_some(),
                    start_pin
                        .zip(stop_pin)
                        .map(|(start, stop)| start.editor_identity() == stop.editor_identity()),
                ),
            );
            return Ok(FinalOutcome::Recovery(copy_fallback_record(
                &transcript,
                CopyFallbackReason::Other(reason),
                start_target.cloned().unwrap_or_else(TargetSnapshot::empty),
            )));
        }
        // Stop delivery requires exact retained editor proof; a missing pin goes through recovery
        // and never reacquires current focus.
        let target = if original_bound {
            start_target
        } else {
            stop_target
        };
        let pin = if original_bound { start_pin } else { stop_pin };
        let record = match target {
            Some(stop_target) => RealDeliveryBackend::new(host)
                .deliver_stop_submitting_with_progress(
                    &transcript,
                    stop_target,
                    pin,
                    want_enter,
                    on_inserted,
                )
                .map_err(|err| err.to_string())?,
            None => copy_fallback_record(
                &transcript,
                CopyFallbackReason::NoFocusPin,
                TargetSnapshot::empty(),
            ),
        };
        Ok(FinalOutcome::Delivered(record))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        // No secure-field probe exists on this platform, and the legacy pasteable check treats an
        // unknown editability (`focused_text_input == None`) as pasteable. Unknown refuses, so
        // there is no automatic write here: the transcript goes to the clipboard through recovery
        // (Revision 2; Revision 1 pasted when the target was not positively non-editable).
        let target = delivery::snapshot_target(host);
        Ok(FinalOutcome::Recovery(copy_fallback_record(
            transcript,
            CopyFallbackReason::UnsupportedPlatform,
            target,
        )))
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn typed_pin<'a>(
    host: &DeliveryHost,
    which: &str,
    pin: Option<&'a crate::host::HostPin>,
) -> Option<&'a FinalPin> {
    let pin = pin?;
    let typed = pin.downcast_ref::<FinalPin>();
    if typed.is_none() {
        host.log("final_delivery_pin_type_mismatch", which);
    }
    typed
}

/// Host-seam adapter: the same delivery, with the pins arriving as [`crate::host::HostPin`]. A pin
/// that is not this platform's [`FinalPin`] is treated as no pin (so delivery recovers to the
/// clipboard, never to a guessed destination) and the mismatch is logged.
pub fn deliver_final_for_host(
    host: &'static DeliveryHost,
    request: crate::host::FinalDelivery<'_>,
) -> Result<FinalOutcome, String> {
    let crate::host::FinalDelivery {
        transcript,
        want_enter,
        start_context_state,
        context_request_attempted,
        context_original_binding,
        start_target,
        stop_target,
        start_pin,
        stop_pin,
        start_content_fingerprint,
        on_inserted,
    } = request;
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let (start_pin, stop_pin) = (
        typed_pin(host, "start", start_pin),
        typed_pin(host, "stop", stop_pin),
    );
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let (start_pin, stop_pin): (Option<&FinalPin>, Option<&FinalPin>) = {
        let _ = (start_pin, stop_pin);
        (None, None)
    };
    deliver_final(
        host,
        FinalDeliveryInput {
            transcript,
            want_enter,
            start_context_state,
            context_request_attempted,
            context_original_binding,
            start_target,
            stop_target,
            start_pin,
            stop_pin,
            start_content_fingerprint,
            on_inserted,
        },
    )
}

/// Drop the macOS accessibility observation retained for a focus pin. No other platform keeps one.
pub fn release_pending_accessibility_observation() {
    #[cfg(target_os = "macos")]
    rightkit_automation::desktop::macos::release_pending_accessibility_observation();
}
