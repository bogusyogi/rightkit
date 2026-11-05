// The per-platform transcript transaction: clipboard stage + paste keystroke
// + attributable confirmation proof, with optional deferred Enter submit.
//
// Windows: snapshot every prior clipboard format, write transcript, send
// Ctrl+V, then restore snapshot after settle if no newer write occurred.
// Works in Win32, Electron, WebView2, and every other Windows target.
// `type_unicode` is not used here: Electron synthesises a DOM `input` event
// per KEYEVENTF_UNICODE keystroke, causing char-by-char insertion.
// `send_paste_keystroke` never sends a bare VK_MENU (would trigger Alt-KeyTips).
//
// macOS: stage a promised pasteboard value, require AppKit's post-Cmd+V
// provider receipt, then confirm an attributable before/after read-back
// delta before reporting success or submitting Enter (spec 4.2/4.3). A
// receipt without attributable proof stays Unconfirmed: the transcript
// remains staged for recovery and the prior pasteboard is never restored
// over it.

#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::time::Instant;

#[cfg(any(target_os = "windows", target_os = "macos"))]
use super::keystrokes::{send_enter_keystroke, send_paste_keystroke};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use super::targets::target_ready_for_input;
use super::*;

#[cfg(target_os = "windows")]
pub(crate) const PASTE_SETTLE_MS: u64 = 300;
// AppKit's promised pasteboard callback is authoritative consumption proof;
// AX read-back remains optional and must never turn a cold target into a
// copied fallback when AppKit has requested our staged text.
#[cfg(target_os = "macos")]
const PASTE_RECEIPT_TIMEOUT_MS: u64 = 1_500;
#[cfg(target_os = "macos")]
const PASTE_RECEIPT_SETTLE_MS: u64 = 200;
#[cfg(target_os = "macos")]
const PASTE_READBACK_TIMEOUT_MS: u64 = 650;
#[cfg(target_os = "macos")]
const SECURE_RETRY_INTERVAL_MS: u64 = 40;
#[cfg(target_os = "windows")]
pub(crate) const TERMINAL_PASTE_SETTLE_MS: u64 = 1_000;

/// A successful insertion always carries attributable before/after proof.
/// A posted OS keystroke without proof is an explicit clipboard fallback,
/// never a `Pasted` record and never authority to submit Enter.
pub(crate) enum InsertResult {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    Confirmed {
        steps: Vec<DeliveryTimingStep>,
        proof: InsertionProof,
    },
}

#[cfg(target_os = "windows")]
fn wait_for_paste_consumed_windows(
    transcript: &str,
    before: Option<&str>,
    budget: std::time::Duration,
) -> bool {
    // No readable pre-state means no mutation can be attributed to this
    // paste. Stay conservative: never confirm, never fire Enter, never
    // restore the clipboard over the transcript.
    let Some(before) = before else {
        return false;
    };
    let deadline = Instant::now() + budget;
    loop {
        if let Some(after) = crate::focus::focused_field_raw_text_for_proof() {
            if insertion_attributable(before, &after, transcript) {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
}

// Collapses the repeated "restore the prior clipboard snapshot if nothing
// newer has written it since" pattern used at every error/success exit of
// `insert_transcript` below. Same two args, same fire-and-forget semantics
// (`let _ =`) as every call site it replaces.
#[cfg(target_os = "windows")]
fn restore_prior_windows(
    prior: Option<rightkit_automation::desktop::windows::ClipboardSnapshot>,
    sequence: u32,
) {
    if let Some(prior) = prior {
        let _ = rightkit_automation::desktop::windows::restore_general_clipboard_if_unchanged(
            prior, sequence,
        );
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn insert_transcript(
    transcript: &str,
    target: &TargetSnapshot,
    send_enter: bool,
) -> Result<InsertResult, DeliveryError> {
    let mut steps = Vec::new();

    // HR-DELIVERY-004 / HR-CONTROL-011 + spec 3.3: POSITIVE password evidence
    // fails closed BEFORE the clipboard is touched; unknown readability no
    // longer vetoes an ordinary paste. The shell twin does the same in
    // `src-tauri/src/delivery/backend.rs`.
    let secure_state = crate::focus::windows_focused_element_is_password();
    // Positive password evidence is the only security veto (spec 3.3):
    // unknown readability never refuses an otherwise valid ordinary paste.
    if crate::focus::password_state_vetoes_paste(secure_state) {
        return Err(DeliveryError::new("E_SECURE_TARGET", "secure_target"));
    }

    let clipboard_snapshot_started = Instant::now();
    let prior = Some(
        rightkit_automation::desktop::windows::snapshot_general_clipboard()
            .ok_or_else(|| DeliveryError::new("E_CLIPBOARD", "could not snapshot clipboard"))?,
    );
    // Delivery spec section 4.5: a snapshot that cannot represent every prior
    // format must never back the destructive transcript write — silent format
    // loss is forbidden. Route to visible recovery instead.
    if let Some(prior) = &prior {
        if !prior.is_lossless() {
            tracing::warn!(
                formats_dropped = prior.dropped_formats(),
                dropped_format_details = ?prior.dropped_format_diagnostics(),
                "delivery_clipboard_snapshot_lossy"
            );
            return Err(DeliveryError::new(
                "E_CLIPBOARD",
                CopyFallbackReason::ClipboardLossySnapshot.to_string(),
            ));
        }
    }
    steps.push(DeliveryTimingStep::new(
        "clipboard_snapshot",
        elapsed_ms(clipboard_snapshot_started),
    ));
    let clipboard_write_started = Instant::now();
    let transcript_sequence = match rightkit_automation::desktop::windows::stage_clipboard_text(
        transcript,
        prior.as_ref().expect("live clipboard snapshot").sequence(),
    ) {
        Ok(sequence) => sequence,
        Err(failure) => {
            if let Some(restore_sequence) = failure.restore_sequence {
                restore_prior_windows(prior, restore_sequence);
            }
            return Err(DeliveryError::new(
                "E_CLIPBOARD",
                CopyFallbackReason::ClipboardUnavailable.to_string(),
            ));
        }
    };
    steps.push(DeliveryTimingStep::new(
        "clipboard_write",
        elapsed_ms(clipboard_write_started),
    ));
    // Captured BEFORE Ctrl+V so any confirmation is attributable to THIS
    // paste rather than to text that merely happened to already be there
    // (HR-DELIVERY-006). `None` on a failed read is handled
    // as "no proof possible" by `wait_for_paste_consumed_windows`.
    let paste_proof = crate::focus::focused_field_raw_text_for_proof();
    let paste_started = Instant::now();
    let paste_result = send_paste_keystroke();
    steps.push(DeliveryTimingStep::new(
        "paste_keystroke",
        elapsed_ms(paste_started),
    ));
    if let Err(error) = paste_result {
        restore_prior_windows(prior, transcript_sequence);
        return Err(error);
    }

    // A `Pasted` record built via `pasted_delivery_record` requires a real
    // read-back delta (HR-DELIVERY-006), and Enter fires only after that
    // proof AND a fresh target revalidation (HR-DELIVERY-007) — never after a
    // blind fixed sleep with no read-back at all. Paste-only delivery also
    // requires this same proof before it can be reported as `Pasted` or
    // restore the prior clipboard.
    if send_enter {
        let settle_ms = paste_settle_ms_for_target(target);
        let submit_started = Instant::now();
        let consumed = wait_for_paste_consumed_windows(
            transcript,
            paste_proof.as_deref(),
            std::time::Duration::from_millis(settle_ms),
        );
        if !consumed || !target_ready_for_input(target) {
            steps.push(DeliveryTimingStep::new(
                "paste_unconfirmed_or_target_changed_skipped_enter",
                elapsed_ms(submit_started),
            ));
            // Never restore on an unconfirmed paste or a focus change alone
            // — only on confirmed consumption (HR-DELIVERY-009). Never fire
            // Enter without proof + revalidation (HR-DELIVERY-007). The
            // transcript stays on the clipboard either way, explicit and
            // recoverable (HR-DELIVERY-008).
            if consumed {
                if let Some(prior) = prior {
                    let _ = rightkit_automation::desktop::windows::restore_general_clipboard_if_unchanged(
                        prior,
                        transcript_sequence,
                    );
                }
                return Ok(InsertResult::Confirmed {
                    steps,
                    proof: InsertionProof::from_confirmed_delta(true)
                        .expect("consumed is true by construction above"),
                });
            }
            return Err(DeliveryError::new(
                "E_UNCONFIRMED_INSERTION",
                "Ctrl+V could not be confirmed against the focused element before send-enter deadline",
            ));
        }
        // Submitting needs POSITIVE non-password evidence at Enter time; an
        // Unknown probe (timeout/saturated/unavailable) is unproven, so the
        // paste stays confirmed but Enter is withheld.
        let enter_security = crate::focus::windows_focused_element_is_password();
        if !crate::focus::password_state_allows_submit(enter_security) {
            steps.push(DeliveryTimingStep::new(
                "enter_skipped_password_state_unproven",
                elapsed_ms(submit_started),
            ));
            return Ok(InsertResult::Confirmed {
                steps,
                proof: InsertionProof::from_confirmed_delta(true)
                    .expect("consumed is true by construction above"),
            });
        }
        if let Err(error) = send_enter_keystroke() {
            restore_prior_windows(prior, transcript_sequence);
            return Err(error);
        }
        steps.push(DeliveryTimingStep::new(
            "paste_settle_and_submit",
            elapsed_ms(submit_started),
        ));
        if let Some(prior) = prior {
            let restore_started = Instant::now();
            let _ = rightkit_automation::desktop::windows::restore_general_clipboard_if_unchanged(
                prior,
                transcript_sequence,
            );
            steps.push(DeliveryTimingStep::new(
                "clipboard_restore",
                elapsed_ms(restore_started),
            ));
        }
        Ok(InsertResult::Confirmed {
            steps,
            proof: InsertionProof::from_confirmed_delta(true)
                .expect("consumed is true by construction above"),
        })
    } else {
        let settle_ms = paste_settle_ms_for_target(target);
        let consumed = wait_for_paste_consumed_windows(
            transcript,
            paste_proof.as_deref(),
            std::time::Duration::from_millis(settle_ms),
        );
        if !consumed {
            // Spec 4.4: reason codes let consumers record exactly why the
            // outcome is Unconfirmed rather than a generic paste failure.
            let reason = if paste_proof.is_none() {
                "paste_readback_unavailable"
            } else {
                "paste_readback_unconfirmed"
            };
            return Err(DeliveryError::new("E_UNCONFIRMED_INSERTION", reason));
        }
        restore_prior_windows(prior, transcript_sequence);
        Ok(InsertResult::Confirmed {
            steps,
            proof: InsertionProof::from_confirmed_delta(true)
                .expect("consumed is true by construction above"),
        })
    }
}

// NOTE: on BOTH platforms, `finalize_transcript` returns
// `FinalizeOutcome::Transcript` and the Tauri shell performs the real
// paste+Enter via its own delivery backend
// (`src-tauri/src/delivery/backend.rs::deliver_submitting`, which
// sequences settle -> Enter). This engine-side `insert_transcript` is only
// reachable via `repaste_last`
// (`rightkit-dictation-runtime/src/runtime/delivery_history.rs`),
// which always passes `send_enter = false`. It still honors `send_enter`
// correctly for API consistency with `deliver_text_with_enter`. Per spec
// section 4, delivery is a single clipboard+paste dispatch; no native
// AX/UIA text write precedes it on either platform.

#[cfg(target_os = "macos")]
fn focused_field_security_with_retry_macos(pid: i32) -> Option<bool> {
    let deadline = Instant::now() + std::time::Duration::from_millis(240);
    loop {
        // This probe only accepts an actual focused control; a missing or
        // window-level AX result must fail closed for password safety.
        if let Some(secure) =
            rightkit_automation::desktop::macos::focused_field_is_secure_for_pid(pid)
        {
            return Some(secure);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(SECURE_RETRY_INTERVAL_MS));
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn insert_transcript(
    transcript: &str,
    target: &TargetSnapshot,
    send_enter: bool,
) -> Result<InsertResult, DeliveryError> {
    use std::time::Duration;
    let mut steps = Vec::new();

    // Posting Cmd+V without Accessibility is never attributable insertion.
    // Keep the transcript recoverable via the outer clipboard-only fallback.
    let access_started = Instant::now();
    if !crate::macos_input::accessibility_trusted(false) {
        steps.push(DeliveryTimingStep::new(
            "accessibility_check",
            elapsed_ms(access_started),
        ));
        return Err(DeliveryError::new(
            "E_ACCESSIBILITY",
            "Accessibility permission is not granted",
        ));
    }
    steps.push(DeliveryTimingStep::new(
        "accessibility_check",
        elapsed_ms(access_started),
    ));

    // Never post Cmd+V into a password field. The platform probe is bounded;
    // unknown security state remains clipboard-only and never receives Enter.
    let secure_started = Instant::now();
    let secure_state = target
        .process_id
        .and_then(|pid| focused_field_security_with_retry_macos(pid as i32));
    steps.push(DeliveryTimingStep::new(
        "secure_target_revalidation",
        elapsed_ms(secure_started),
    ));
    // Positive secure-field evidence is the only security veto (spec 3.3):
    // unknown readability never refuses an otherwise valid ordinary paste.
    if secure_state == Some(true) {
        return Err(DeliveryError::new("E_SECURE_TARGET", "secure_target"));
    }

    // Optional before-text evidence captured BEFORE Cmd+V: read-back
    // verification afterwards compares against THIS, so a confirmed delta is
    // attributable to this dispatch — never inferred from post-paste text
    // alone. `None` means no proof possible (an unreadable
    // field); the post-paste check then reports Unconfirmed rather than
    // claiming insertion it cannot attribute.
    let pre_paste_baseline: Option<rightkit_automation::desktop::macos::FocusedFieldProof> = target
        .process_id
        .map(|pid| rightkit_automation::desktop::macos::focused_field_proof_for_pid(pid as i32));
    steps.push(DeliveryTimingStep::new(
        if pre_paste_baseline
            .as_ref()
            .is_some_and(|baseline| !baseline.is_blind())
        {
            "pre_paste_baseline_captured"
        } else {
            "pre_paste_baseline_unavailable"
        },
        0,
    ));

    // Keep the app's copy fallback from interleaving with this transaction.
    // AppKit has no atomic compare-and-declare API; external writers are
    // checked at snapshot/stage/restore boundaries, with a residual OS-level
    // check/commit race that cannot be closed from this process.
    let _clipboard_transaction =
        rightkit_automation::desktop::macos::acquire_clipboard_transaction();

    let clipboard_snapshot_started = Instant::now();
    let mut prior = Some(
        rightkit_automation::desktop::macos::snapshot_general_pasteboard()
            .ok_or_else(|| DeliveryError::new("E_CLIPBOARD", "could not snapshot clipboard"))?,
    );
    steps.push(DeliveryTimingStep::new(
        "clipboard_snapshot",
        elapsed_ms(clipboard_snapshot_started),
    ));
    let stage_started = Instant::now();
    let mut promised = {
        match rightkit_automation::desktop::macos::stage_promised_clipboard_text(
            transcript,
            prior
                .as_ref()
                .expect("live pasteboard snapshot")
                .change_count(),
        ) {
            Ok(promised) => Some(promised),
            Err(failed_write_change_count) => {
                if let (Some(prior), Some(change_count)) = (prior.take(), failed_write_change_count)
                {
                    let _ =
                        rightkit_automation::desktop::macos::restore_general_pasteboard_if_unchanged_owned(
                            prior,
                            change_count,
                        );
                }
                return Err(DeliveryError::new(
                    "E_CLIPBOARD",
                    CopyFallbackReason::ClipboardUnavailable.to_string(),
                ));
            }
        }
    };
    // AppKit's promised provider receipt is the authoritative proof. AX
    // read-back can be blank or delayed for Chromium/Electron and is not used
    // to gate paste, Enter, or restoration.
    steps.push(DeliveryTimingStep::new(
        "promised_clipboard_stage",
        elapsed_ms(stage_started),
    ));
    if !target_ready_for_input(target) {
        if let (Some(promised), Some(prior)) = (&mut promised, prior.take()) {
            let _ = promised.restore_if_unchanged_owned(prior);
        }
        return Err(DeliveryError::new(
            "E_FOCUS_CHANGED",
            "focused target changed before Cmd+V",
        ));
    }

    let paste_started = Instant::now();
    if let Some(promised) = &promised {
        promised.mark_posted();
    }
    let result = send_paste_keystroke();
    steps.push(DeliveryTimingStep::new(
        "paste_keystroke",
        elapsed_ms(paste_started),
    ));
    if let Err(error) = result {
        if let (Some(promised), Some(prior)) = (&mut promised, prior.take()) {
            let _ = promised.restore_if_unchanged_owned(prior);
        }
        return Err(error);
    }

    let receipt_started = Instant::now();
    let receipt = promised
        .as_ref()
        .map(|promised| promised.wait_for_receipt(Duration::from_millis(PASTE_RECEIPT_TIMEOUT_MS)));
    let receipt_observed = receipt.as_ref().is_some_and(|receipt| receipt.observed());
    steps.push(DeliveryTimingStep::new(
        if receipt_observed {
            "paste_receipt_observed"
        } else {
            "paste_receipt_timeout_clipboard_retained"
        },
        elapsed_ms(receipt_started),
    ));
    if !receipt_observed {
        // Keep staged text available for manual recovery. Outer fallback may
        // copy it eagerly, but must never claim Pasted or send Enter.
        return Err(DeliveryError::new(
            "E_UNCONFIRMED_INSERTION",
            "paste_receipt_unconfirmed",
        ));
    }

    // Spec 4.2/6: a receipt alone is consumption evidence, not insertion
    // proof. Verify the post-paste field against the PRE-DISPATCH baseline
    // captured on the same editor before staging: a delta attributable to
    // this dispatch confirms insertion; a receipt without attributable proof
    // stays Unconfirmed and never submits Enter.
    let baseline = match pre_paste_baseline.as_ref() {
        Some(baseline) if !baseline.is_blind() => baseline,
        _ => {
            steps.push(DeliveryTimingStep::new(
                "paste_readback_unavailable",
                elapsed_ms(receipt_started),
            ));
            return Err(DeliveryError::new(
                "E_UNCONFIRMED_INSERTION",
                "paste_readback_unavailable",
            ));
        }
    };
    let Some(pid) = target.process_id else {
        steps.push(DeliveryTimingStep::new(
            "paste_readback_unavailable",
            elapsed_ms(receipt_started),
        ));
        return Err(DeliveryError::new(
            "E_UNCONFIRMED_INSERTION",
            "paste_readback_unavailable",
        ));
    };
    let deadline = Instant::now() + Duration::from_millis(PASTE_READBACK_TIMEOUT_MS);
    let attributable = loop {
        if rightkit_automation::desktop::macos::insertion_confirmed_for_pid(
            pid as i32, baseline, transcript,
        ) {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if !attributable {
        steps.push(DeliveryTimingStep::new(
            "paste_readback_unconfirmed",
            elapsed_ms(receipt_started),
        ));
        return Err(DeliveryError::new(
            "E_UNCONFIRMED_INSERTION",
            "paste_readback_unconfirmed",
        ));
    }

    if send_enter {
        let submit_started = Instant::now();
        if !target_ready_for_input(target) {
            steps.push(DeliveryTimingStep::new(
                "paste_settle_skipped_enter",
                elapsed_ms(submit_started),
            ));
        } else if let Err(error) = send_enter_keystroke() {
            // Receipt proves insertion already happened; do not retry paste.
            steps.push(DeliveryTimingStep::new(
                "paste_receipt_submit_failed",
                elapsed_ms(submit_started),
            ));
            let _ = error;
        } else {
            steps.push(DeliveryTimingStep::new(
                "paste_settle_and_submit",
                elapsed_ms(submit_started),
            ));
        }
    }

    if let (Some(receipt), Some(promised), Some(prior)) = (receipt, &mut promised, prior.take()) {
        let settle_until = receipt
            .last_at
            .map(|last_at| last_at + Duration::from_millis(PASTE_RECEIPT_SETTLE_MS));
        if let Some(settle_until) = settle_until {
            let remaining = settle_until.saturating_duration_since(Instant::now());
            if !remaining.is_zero() {
                std::thread::sleep(remaining);
            }
        }
        let restore_started = Instant::now();
        if promised.restore_if_unchanged_owned(prior) {
            steps.push(DeliveryTimingStep::new(
                "clipboard_restore_after_receipt",
                elapsed_ms(restore_started),
            ));
        }
    }
    Ok(InsertResult::Confirmed {
        steps,
        proof: InsertionProof::from_confirmed_delta(true)
            .expect("receipt observed always yields insertion proof"),
    })
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(crate) fn insert_transcript(
    _transcript: &str,
    _target: &TargetSnapshot,
    _send_enter: bool,
) -> Result<InsertResult, DeliveryError> {
    Err(DeliveryError::new(
        "E_UNSUPPORTED",
        "delivery unsupported on this platform",
    ))
}

#[cfg(target_os = "windows")]
pub(crate) fn paste_settle_ms_for_target(target: &TargetSnapshot) -> u64 {
    if crate::focus::windows_terminal_target(
        target.process_name.as_deref(),
        target.window_title.as_deref(),
        None,
    ) {
        TERMINAL_PASTE_SETTLE_MS
    } else {
        PASTE_SETTLE_MS
    }
}
