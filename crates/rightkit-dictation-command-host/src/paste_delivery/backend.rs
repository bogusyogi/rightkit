#[allow(unused_imports)]
use super::*;

// Delivery - OS side effects only: clipboard write, Ctrl+V / Enter SendInput,
// foreground-window snapshot, and the real backend that wires them together.
// The pure record shapes + id/time helpers live in
// `rightkit_transcript_library::delivery` (re-exported by the parent module).
// Every app-specific value (timeouts, per-app lists, the event sink, the
// delivery method, the macOS AX reads) comes in through `DeliveryHost`.

#[allow(unused_imports)]
use crate::evidence_gate::{
    authorize_evidence, authorize_with_input, ControlId, ControlMatch, Refusal, SecureEvidence,
    WriteKind, WritePermit,
};
#[allow(unused_imports)]
use crate::gated_writes;
pub use crate::paste_gate::paste_dispatch_gate;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use crate::restore_gate;

use std::time::Instant;

/// Report what a fallback did to the clipboard (never any text).
fn log_fallback_clipboard(host: &DeliveryHost, stage: &str, clipboard: FallbackClipboard) {
    host.log(
        "delivery_fallback_clipboard",
        &format!("stage={stage} clipboard={}", clipboard.code()),
    );
}

/// macOS: a delivery that wrote the pasteboard (the promised transcript) is giving up before the
/// paste was sent. The fallback does NOT write the pasteboard: if this generation still owns
/// its recorded write and `changeCount` still equals it, the transcript is the pasteboard
/// content (dropping `promised` leaves the declaration in place) and the fallback is
/// `TranscriptStaged`; otherwise the user's newer value is kept and the host offers Copy.
#[cfg(target_os = "macos")]
fn abandon_stage_macos(
    host: &DeliveryHost,
    generation: restore_gate::Generation,
    promised: &rightkit_automation::desktop::macos::PromisedPasteboard,
    stage: &str,
) -> FallbackClipboard {
    let now = u64::try_from(rightkit_automation::desktop::macos::pasteboard_change_count()).ok();
    let ownership_lost = promised.receipt().ownership_lost;
    let verdict = restore_gate::restore_verdict(generation, now, ownership_lost, false);
    let clipboard = FallbackClipboard::from_verdict(verdict);
    log_fallback_clipboard(host, stage, clipboard);
    clipboard
}

/// Windows twin of `abandon_stage_macos`: generation, then the clipboard sequence number, with
/// our own delayed render the one advance accepted (only while the owner window still owns the
/// clipboard). No clipboard write happens here; dropping `promised` while the window still owns
/// the clipboard makes Windows ask it to render, which materializes the transcript.
#[cfg(target_os = "windows")]
fn abandon_stage_windows(
    host: &DeliveryHost,
    generation: restore_gate::Generation,
    promised: &rightkit_automation::desktop::windows::PromisedClipboard,
    stage: &str,
) -> FallbackClipboard {
    let now = Some(u64::from(
        rightkit_automation::desktop::windows::clipboard_sequence_number(),
    ));
    let ownership_lost = promised.receipt().ownership_lost;
    let verdict = restore_gate::restore_verdict(generation, now, ownership_lost, true);
    let clipboard = FallbackClipboard::from_verdict(verdict);
    log_fallback_clipboard(host, stage, clipboard);
    clipboard
}

/// Every restore of the prior clipboard goes through this verdict. The caller passes what the
/// OS says now; a refused restore leaves the clipboard alone and the caller reports it.
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn log_restore_refused(host: &DeliveryHost, verdict: restore_gate::RestoreVerdict, stage: &str) {
    host.log(
        "delivery_clipboard_restore_refused",
        &format!("stage={stage} reason={}", verdict.code()),
    );
}

/// macOS: restore the prior pasteboard only if this delivery's generation still owns the
/// recorded write AND `NSPasteboard.changeCount` still equals the value recorded right after
/// that write. Otherwise the pasteboard is left alone. Returns whether it was restored.
#[cfg(target_os = "macos")]
fn restore_prior_macos(
    host: &DeliveryHost,
    generation: restore_gate::Generation,
    promised: &mut rightkit_automation::desktop::macos::PromisedPasteboard,
    prior: &rightkit_automation::desktop::macos::PasteboardSnapshot,
    stage: &str,
) -> bool {
    let now = u64::try_from(rightkit_automation::desktop::macos::pasteboard_change_count()).ok();
    let ownership_lost = promised.receipt().ownership_lost;
    let verdict = restore_gate::restore_verdict(generation, now, ownership_lost, false);
    if verdict != restore_gate::RestoreVerdict::Proceed {
        log_restore_refused(host, verdict, stage);
        return false;
    }
    promised.restore_if_unchanged(prior)
}

/// Windows twin of `restore_prior_macos`: generation, then the clipboard sequence number
/// (`GetClipboardSequenceNumber`) against the value recorded right after our write. Our own
/// delayed render is the one write allowed to advance the sequence, and only while the
/// clipboard window still owns the clipboard; the library's `restore_if_unchanged` then
/// re-checks owner and exact rendered sequence under `OpenClipboard`.
#[cfg(target_os = "windows")]
fn restore_prior_windows(
    host: &DeliveryHost,
    generation: restore_gate::Generation,
    promised: &mut rightkit_automation::desktop::windows::PromisedClipboard,
    prior: rightkit_automation::desktop::windows::ClipboardSnapshot,
    stage: &str,
) -> bool {
    let now = Some(u64::from(
        rightkit_automation::desktop::windows::clipboard_sequence_number(),
    ));
    let ownership_lost = promised.receipt().ownership_lost;
    let verdict = restore_gate::restore_verdict(generation, now, ownership_lost, true);
    if verdict != restore_gate::RestoreVerdict::Proceed {
        log_restore_refused(host, verdict, stage);
        return false;
    }
    promised.restore_if_unchanged(prior)
}

#[derive(Debug)]
pub struct RealDeliveryBackend {
    host: &'static DeliveryHost,
}

impl RealDeliveryBackend {
    pub fn new(host: &'static DeliveryHost) -> Self {
        Self { host }
    }
}

impl DeliveryBackend for RealDeliveryBackend {
    fn deliver(&mut self, transcript: &str) -> Result<DeliveryRecord, DeliveryError> {
        self.deliver_inner(transcript, false)
    }
}

impl RealDeliveryBackend {
    /// Like `deliver`, but presses Enter only after attributable paste readback
    ///.
    pub fn deliver_submitting(
        &mut self,
        transcript: &str,
        send_enter: bool,
    ) -> Result<DeliveryRecord, DeliveryError> {
        self.deliver_submitting_with_progress(transcript, send_enter, || {})
    }

    pub fn deliver_submitting_with_progress(
        &mut self,
        transcript: &str,
        send_enter: bool,
        mut on_inserted: impl FnMut(),
    ) -> Result<DeliveryRecord, DeliveryError> {
        self.deliver_inner_with_progress(
            transcript,
            send_enter,
            None,
            false,
            #[cfg(target_os = "windows")]
            None,
            #[cfg(target_os = "macos")]
            None,
            #[cfg(target_os = "macos")]
            false,
            &mut on_inserted,
        )
    }

    /// Deliver to frozen stop-time destination with exact AX editor retained.
    #[cfg(target_os = "macos")]
    pub fn deliver_stop_submitting_with_progress(
        &mut self,
        transcript: &str,
        expected_target: &TargetSnapshot,
        pin: Option<&rightkit_automation::desktop::macos::FocusPin>,
        send_enter: bool,
        mut on_inserted: impl FnMut(),
    ) -> Result<DeliveryRecord, DeliveryError> {
        self.deliver_inner_with_progress(
            transcript,
            send_enter,
            Some(expected_target),
            false,
            #[cfg(target_os = "windows")]
            None,
            pin,
            true,
            &mut on_inserted,
        )
    }

    /// Deliver to destination captured at recording stop. The token carries
    /// UIA runtime identity through transcription; no start-focus restore or
    /// delivery-time destination reacquisition is permitted.
    #[cfg(target_os = "windows")]
    pub fn deliver_stop_target_with_progress(
        &mut self,
        transcript: &str,
        stop_target: &WindowsStopTarget,
        send_enter: bool,
        mut on_inserted: impl FnMut(),
    ) -> Result<DeliveryRecord, DeliveryError> {
        self.deliver_inner_with_progress(
            transcript,
            send_enter,
            None,
            stop_target.snapshot().focused_text_input == Some(true),
            Some(stop_target),
            &mut on_inserted,
        )
    }

    #[cfg(target_os = "windows")]
    pub fn deliver_stop_windows_with_progress(
        &mut self,
        transcript: &str,
        stop_target: &WindowsStopTarget,
        send_enter: bool,
        on_inserted: impl FnMut(),
    ) -> Result<DeliveryRecord, DeliveryError> {
        self.deliver_stop_target_with_progress(transcript, stop_target, send_enter, on_inserted)
    }

    fn deliver_inner(
        &mut self,
        transcript: &str,
        send_enter: bool,
    ) -> Result<DeliveryRecord, DeliveryError> {
        self.deliver_inner_with_progress(
            transcript,
            send_enter,
            None,
            false,
            #[cfg(target_os = "windows")]
            None,
            #[cfg(target_os = "macos")]
            None,
            #[cfg(target_os = "macos")]
            false,
            &mut || {},
        )
    }

    fn deliver_inner_with_progress(
        &mut self,
        transcript: &str,
        send_enter: bool,
        expected_target: Option<&TargetSnapshot>,
        require_editable: bool,
        #[cfg(target_os = "windows")] stop_target: Option<&WindowsStopTarget>,
        #[cfg(target_os = "macos")] retained_pin: Option<
            &rightkit_automation::desktop::macos::FocusPin,
        >,
        #[cfg(target_os = "macos")] require_retained_pin: bool,
        on_inserted: &mut dyn FnMut(),
    ) -> Result<DeliveryRecord, DeliveryError> {
        let host = self.host;
        (host.mark_delivery_started)();
        let started = Instant::now();
        let mut steps = Vec::new();
        let delivery_id = next_delivery_id(0);
        let target_started = Instant::now();
        #[cfg(target_os = "windows")]
        let target = stop_target
            .map(|captured| captured.snapshot().clone())
            .unwrap_or_else(|| current_target_snapshot(host));
        #[cfg(not(target_os = "windows"))]
        #[allow(unused_mut)]
        let mut target = current_target_snapshot(host);
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

        #[cfg(target_os = "windows")]
        if stop_target.is_some_and(|captured| !validate_stop_target_for_delivery(host, captured)) {
            // This delivery has not touched the clipboard, so it owns nothing there and writes
            // nothing: the user's clipboard stays as it is and the host offers an explicit Copy.
            let clipboard = FallbackClipboard::NOT_OWNED;
            log_fallback_clipboard(host, "stop_target_revalidation_failed", clipboard);
            steps.push(DeliveryTimingStep::new(
                "stop_target_revalidation_failed",
                0,
            ));
            let total_ms = elapsed_ms(started);
            let reason = clipboard.record_reason(CopyFallbackReason::FocusChanged);
            return Ok(DeliveryRecord::new_with_timing(
                delivery_id,
                transcript,
                DeliveryOutcome::CopiedFallback { reason },
                target,
                total_ms,
            )
            .with_delivery_timing(DeliveryTimings::from_steps(total_ms, steps)));
        }

        // The live snapshot is ONE instantaneous AX/window-server read. A
        // transient miss (AX read timing out, the app re-laying out its
        // composer) looks identical to a focus change and has sent live
        // dictations to the clipboard (measured 2 of 50). Re-read briefly: a
        // real focus change stays mismatched, a transient one settles.
        #[cfg(target_os = "macos")]
        if let Some(expected) = expected_target {
            let mut first = true;
            let (_, retries) = settle_transient_macos(host, || {
                if !std::mem::take(&mut first) {
                    target = current_target_snapshot(host);
                }
                targets_match_for_delivery(host, &target, expected, require_editable)
            });
            if retries > 0 {
                host.log(
                    "delivery_target_revalidation_retry",
                    &format!(
                        "retries={retries} matched={} same_pid={} same_window={} same_foreground={} live_control={} live_text_input={:?}",
                        targets_match_for_delivery(host, &target, expected, require_editable),
                        target.process_id == expected.process_id,
                        target.window_handle == expected.window_handle,
                        target.foreground_target == expected.foreground_target,
                        target.focused_control_handle.is_some(),
                        target.focused_text_input,
                    ),
                );
            }
        }
        if expected_target.is_some_and(|expected| {
            !targets_match_for_delivery(host, &target, expected, require_editable)
        }) {
            // Same as above: nothing of this delivery's is on the clipboard, nothing is written.
            let clipboard = FallbackClipboard::NOT_OWNED;
            log_fallback_clipboard(host, "expected_target_revalidation_failed", clipboard);
            steps.push(DeliveryTimingStep::new(
                "expected_target_revalidation_failed",
                0,
            ));
            let total_ms = elapsed_ms(started);
            let reason = clipboard.record_reason(CopyFallbackReason::FocusChanged);
            return Ok(DeliveryRecord::new_with_timing(
                delivery_id,
                transcript,
                DeliveryOutcome::CopiedFallback { reason },
                target,
                total_ms,
            )
            .with_delivery_timing(DeliveryTimings::from_steps(total_ms, steps)));
        }

        // Live AX/UIA identity is readback evidence, not a replacement stop.
        let method = (host.delivery_method)();
        // What the insertion left on the clipboard when it gave up; starts as "this delivery
        // wrote nothing" and is set at every site that staged the transcript first.
        let mut clipboard = FallbackClipboard::NOT_OWNED;
        let outcome = match insert_transcript_tracked(
            host,
            transcript,
            &target,
            &method,
            require_editable,
            send_enter,
            #[cfg(target_os = "windows")]
            stop_target,
            #[cfg(target_os = "macos")]
            retained_pin,
            #[cfg(target_os = "macos")]
            require_retained_pin,
            &mut steps,
            on_inserted,
            &mut clipboard,
        ) {
            Ok(()) => DeliveryOutcome::Pasted,
            Err(reason) => {
                let reason_text = reason.to_string();
                if reason_text == "paste_readback_unconfirmed"
                    || reason_text == "paste_receipt_unconfirmed"
                    || reason_text == "paste_readback_unavailable"
                    || reason_text == "native_insertion_unconfirmed"
                {
                    DeliveryOutcome::PasteSentUnconfirmed {
                        reason: reason_text,
                    }
                } else if reason == CopyFallbackReason::ClipboardLossySnapshot {
                    // Delivery spec section 4.5: the prior clipboard holds
                    // formats this transaction could not preserve, so the
                    // fallback write (a destructive EmptyClipboard+set) must
                    // not run either — it would destroy exactly the content
                    // we refused to snapshot. Leave the user's clipboard
                    // untouched; recovery offers the transcript with an
                    // explicit Copy that replaces the clipboard on request.
                    DeliveryOutcome::CopiedFallback { reason }
                } else {
                    // Insertion failed. This fallback WRITES NOTHING to the clipboard (it used
                    // to write unconditionally, which could overwrite a copy the user made
                    // meanwhile). `clipboard` says what the clipboard holds: this delivery's own
                    // staged transcript, still current by generation and OS change count
                    // (`TranscriptStaged`), or the user's value, left exactly as found
                    // (`Preserved`), in which case the record carries `preserved_reason()` and
                    // the host offers an explicit Copy.
                    log_fallback_clipboard(host, "insert_refused", clipboard);
                    steps.push(DeliveryTimingStep::new(
                        if clipboard.is_preserved() {
                            "fallback_clipboard_preserved"
                        } else {
                            "fallback_clipboard_staged"
                        },
                        0,
                    ));
                    DeliveryOutcome::CopiedFallback {
                        reason: clipboard.record_reason(reason),
                    }
                }
            }
        };

        let total_ms = elapsed_ms(started);
        Ok(
            DeliveryRecord::new_with_timing(delivery_id, transcript, outcome, target, total_ms)
                .with_delivery_timing(DeliveryTimings::from_steps(total_ms, steps)),
        )
    }
}

/// The recovery record for a delivery that never reached the paste route (no destination, a
/// stale editor, a start context that no longer holds). It records the outcome and WRITES NOTHING:
/// this caller has no clipboard transaction, so it owns nothing there, and the old unconditional
/// write could overwrite what the user copied (or a value that was never snapshotted). The record
/// carries `preserved_reason()`; the host offers the explicit Copy (`copy_text`).
pub fn copy_fallback_record(
    transcript: &str,
    reason: CopyFallbackReason,
    target: TargetSnapshot,
) -> DeliveryRecord {
    // The library's record builder replaces `reason` with the clipboard side's reason when the
    // "write" reports an error; the closure reports that the clipboard was left alone, exactly as a
    // failed write does. The caller's `reason` is therefore not carried in the record.
    copy_fallback_record_with_clipboard(transcript, reason, target, |_| Err(preserved_reason()))
}

/// The explicit, user-initiated copy (the recovery card's Copy, history, copy-last). This and
/// `write_clipboard_text` are the only functions that may overwrite the clipboard on their own
/// account; no automatic fallback calls them.
pub fn copy_text(text: &str) -> Result<(), DeliveryError> {
    copy_text_with_clipboard(text, write_clipboard_text)
}

/// Unconditional clipboard write. Explicit copies only (see [`copy_text`]).
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn write_clipboard_text(text: &str) -> Result<(), CopyFallbackReason> {
    if rightkit_automation::desktop::clipboard_write(text) {
        Ok(())
    } else {
        Err(CopyFallbackReason::ClipboardUnavailable)
    }
}

// The timeouts, budgets and poll intervals of the paste transaction (promised
// clipboard receipt wait, restore quiet cap, readback deadlines, terminal
// budgets, Enter settle) are `DeliveryConfig` fields; the host owns the values.

#[cfg(target_os = "macos")]
pub static PASTE_EVENT_PROBE_TOKEN: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(1);

/// Moves a restore transaction (promised pasteboard + snapshot) to the restore
/// thread. NSPasteboard is thread-safe and the retained provider is only
/// messaged or released; the transaction is used by exactly one thread at a time.
#[cfg(target_os = "macos")]
struct AssertSend<T>(T);
#[cfg(target_os = "macos")]
unsafe impl<T> Send for AssertSend<T> {}
#[cfg(target_os = "macos")]
impl<T> AssertSend<T> {
    fn into_inner(self) -> T {
        self.0
    }
}

#[cfg(target_os = "macos")]
pub fn focused_field_security_with_retry_macos(host: &DeliveryHost, pid: i32) -> Option<bool> {
    let deadline =
        Instant::now() + std::time::Duration::from_millis(host.config.secure_retry_deadline_ms);
    loop {
        // `focused_field_is_secure_for_pid` has a compatibility window
        // fallback for context callers. Require an independently observed
        // focused editable element here so a window fallback can never turn
        // into a fail-open `Some(false)` delivery decision.
        if (host.focused_text_input)() != Some(true) {
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(
                host.config.secure_retry_interval_ms,
            ));
            continue;
        }
        if let Some(secure) =
            rightkit_automation::desktop::macos::focused_field_is_secure_for_pid(pid)
        {
            return Some(secure);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(
            host.config.secure_retry_interval_ms,
        ));
    }
}

/// Re-validate that the focused window hasn't changed during paste readback.
/// The user can switch apps while we poll — Enter must go to the
/// same target where the text was pasted, not whatever is focused now.
pub fn target_ready_for_input(
    host: &DeliveryHost,
    original: &TargetSnapshot,
    require_editable: bool,
) -> bool {
    #[cfg(target_os = "windows")]
    let live = current_target_snapshot_for_revalidation(host, original);
    #[cfg(not(target_os = "windows"))]
    let live = current_target_snapshot(host);
    let ready = targets_match_for_delivery(host, &live, original, require_editable);
    if !ready {
        host.log(
            "delivery_target_revalidation_failed",
            &format!(
                "original_editable={:?} live_editable={:?} require_editable={require_editable}",
                original.focused_text_input, live.focused_text_input
            ),
        );
    }
    ready
}

/// Every paste-time target check is ONE instantaneous AX / window-server read.
/// Chromium-class apps briefly report a different focused element while they
/// re-lay out (e.g. right after the previous paste), which looked identical
/// to a focus change and cost live dictations. Re-ask up to three times,
/// 60 ms apart: a real focus change stays false, a transient one settles.
/// Runs before any paste; it can only end in "ready" or the same fallback.
#[cfg(target_os = "macos")]
fn settle_transient_macos(host: &DeliveryHost, mut check: impl FnMut() -> bool) -> (bool, u8) {
    let mut retries = 0u8;
    loop {
        if check() {
            return (true, retries);
        }
        if retries == host.config.settle_max_retries {
            return (false, retries);
        }
        std::thread::sleep(std::time::Duration::from_millis(
            host.config.settle_retry_interval_ms,
        ));
        retries += 1;
    }
}

#[cfg(target_os = "macos")]
fn log_settle(host: &DeliveryHost, check: &str, ok: bool, retries: u8) {
    if retries > 0 {
        host.log(
            "delivery_check_settled",
            &format!("check={check} ok={ok} retries={retries}"),
        );
    }
}

#[cfg(target_os = "macos")]
pub fn macos_target_ready_for_input(
    host: &DeliveryHost,
    target: &TargetSnapshot,
    require_editable: bool,
    pin: Option<&rightkit_automation::desktop::macos::FocusPin>,
) -> bool {
    pin.map_or_else(
        || target_ready_for_input(host, target, require_editable),
        |pin| {
            target.process_id == Some(pin.pid() as u32)
                && (!require_editable || (host.focused_text_input)() == Some(true))
                && current_target_snapshot_for_stop().is_some_and(|live| {
                    live.process_id == target.process_id
                        && live.window_handle.is_some()
                        && live.window_handle == target.window_handle
                })
                && rightkit_automation::desktop::macos::stop_focus_pin_is_current(pin)
        },
    )
}

#[cfg(target_os = "windows")]
pub fn windows_target_ready_for_input(
    host: &DeliveryHost,
    target: &TargetSnapshot,
    require_editable: bool,
    stop_target: Option<&WindowsStopTarget>,
) -> bool {
    stop_target.map_or_else(
        || target_ready_for_input(host, target, require_editable),
        |stop| validate_stop_target_for_delivery(host, stop),
    )
}

pub fn targets_match_for_delivery(
    host: &DeliveryHost,
    current: &TargetSnapshot,
    expected: &TargetSnapshot,
    require_editable: bool,
) -> bool {
    target_matches_expected(current, expected, require_editable)
        || macos_window_target_matches(current, expected, require_editable)
        || windows_opaque_target_matches(host, current, expected)
}

#[cfg(target_os = "macos")]
pub fn macos_window_target_matches(
    current: &TargetSnapshot,
    expected: &TargetSnapshot,
    require_editable: bool,
) -> bool {
    // Stop snapshots intentionally omit AX editor identity. A live AX probe
    // may expose a stronger editor identity later; that is compatible with
    // frozen native-window continuity, provided the window-server ID agrees.
    current.process_id == expected.process_id
        && expected.window_handle.is_some()
        && current.window_handle == expected.window_handle
        && current.foreground_target == expected.foreground_target
        && expected.focused_control_handle.is_none()
        && current.focused_control_handle.is_some()
        && (!require_editable || current.focused_text_input == Some(true))
}

#[cfg(not(target_os = "macos"))]
pub fn macos_window_target_matches(_: &TargetSnapshot, _: &TargetSnapshot, _: bool) -> bool {
    false
}

#[cfg(target_os = "windows")]
pub fn windows_opaque_target_matches(
    host: &DeliveryHost,
    current: &TargetSnapshot,
    expected: &TargetSnapshot,
) -> bool {
    // An opaque WinUI/WebView host (listed in `DeliveryConfig::opaque_process_names`)
    // reports a different transient
    // focused-child HWND on consecutive probes and reports the composer as
    // non-editable. Bind delivery to its stable process + top-level window;
    // the promised-clipboard receipt remains the mutation gate.
    windows_opaque_target(host, current)
        && windows_opaque_target(host, expected)
        && current.process_id == expected.process_id
        && current.window_handle.is_some()
        && current.window_handle == expected.window_handle
        && current.foreground_target == expected.foreground_target
        && current.focused_control_handle.is_some()
}

#[cfg(target_os = "windows")]
pub fn windows_opaque_target(host: &DeliveryHost, target: &TargetSnapshot) -> bool {
    target.process_name.as_deref().is_some_and(|name| {
        host.config
            .opaque_process_names
            .iter()
            .any(|opaque| name.eq_ignore_ascii_case(opaque))
    })
}

#[cfg(not(target_os = "windows"))]
pub fn windows_opaque_target_matches(
    _: &DeliveryHost,
    _: &TargetSnapshot,
    _: &TargetSnapshot,
) -> bool {
    false
}

/// Insert `transcript` into the focused app. `method` is "paste" (clipboard + ⌘V,
/// default) or "keystroke" (synthetic unicode typing, no clipboard touch). When
/// `send_enter` is set, presses Return only after attributable readback.
#[cfg(target_os = "macos")]
pub fn insert_transcript(
    host: &'static DeliveryHost,
    transcript: &str,
    target: &TargetSnapshot,
    method: &str,
    require_editable: bool,
    send_enter: bool,
    retained_pin: Option<&rightkit_automation::desktop::macos::FocusPin>,
    require_retained_pin: bool,
    steps: &mut Vec<DeliveryTimingStep>,
    on_inserted: &mut dyn FnMut(),
) -> Result<(), CopyFallbackReason> {
    let mut clipboard = FallbackClipboard::NOT_OWNED;
    insert_transcript_tracked(
        host,
        transcript,
        target,
        method,
        require_editable,
        send_enter,
        retained_pin,
        require_retained_pin,
        steps,
        on_inserted,
        &mut clipboard,
    )
}

/// [`insert_transcript`] that also reports what a refusal left on the clipboard in `clipboard`
/// (set at every site that staged the transcript before giving up; otherwise it keeps the
/// caller's value, `FallbackClipboard::NOT_OWNED`). No path in here writes the clipboard to
/// "save" the transcript.
#[cfg(target_os = "macos")]
pub fn insert_transcript_tracked(
    host: &'static DeliveryHost,
    transcript: &str,
    target: &TargetSnapshot,
    method: &str,
    require_editable: bool,
    send_enter: bool,
    retained_pin: Option<&rightkit_automation::desktop::macos::FocusPin>,
    require_retained_pin: bool,
    steps: &mut Vec<DeliveryTimingStep>,
    on_inserted: &mut dyn FnMut(),
    clipboard: &mut FallbackClipboard,
) -> Result<(), CopyFallbackReason> {
    use std::time::Duration;

    // This delivery's clipboard generation. Its restore is bound to it, so an older
    // delivery's worker can never act on this delivery's clipboard.
    let generation = restore_gate::begin_delivery();
    let evidence_budget = Duration::from_millis(host.config.write_evidence_budget_ms);

    // The previous delivery's background clipboard restore must finish before
    // this one snapshots the pasteboard (bounded; normally already idle). A timeout
    // clears nothing that belongs to the older delivery.
    let restore_wait_ms = restore_gate::supersede_pending(
        generation,
        Duration::from_millis(host.config.clipboard_restore_wait_ms),
    );
    steps.push(DeliveryTimingStep::new(
        "clipboard_restore_wait",
        restore_wait_ms,
    ));

    if require_retained_pin {
        let Some(pin) = retained_pin else {
            return Err(CopyFallbackReason::FocusChanged);
        };
        let (current, retries) = settle_transient_macos(host, || {
            rightkit_automation::desktop::macos::stop_focus_pin_is_current(pin)
        });
        log_settle(host, "retained_pin_current", current, retries);
        if !current {
            return Err(CopyFallbackReason::FocusChanged);
        }
    }

    let access_started = Instant::now();
    if !rightkit_automation::desktop::macos::accessibility_trusted(false) {
        host.log(
            "delivery_accessibility_denied",
            "AXIsProcessTrusted() returned false; delivery falls back to clipboard",
        );
        return Err(CopyFallbackReason::PasteFailed);
    }
    steps.push(DeliveryTimingStep::new(
        "accessibility_check",
        elapsed_ms(access_started),
    ));

    // Early exit before the clipboard is touched: never post Cmd+V into a password field.
    // This is only an early refusal for positive evidence; the write itself is authorized
    // by the evidence gate below, which also refuses unknown.
    let secure_state = retained_pin.map_or_else(
        || {
            target
                .process_id
                .and_then(|pid| focused_field_security_with_retry_macos(host, pid as i32))
        },
        rightkit_automation::desktop::macos::focused_field_is_secure_for_pin,
    );
    if secure_state == Some(true) {
        return Err(CopyFallbackReason::Other("secure_target".to_string()));
    }
    // The control this delivery is bound to. Without one the gate refuses.
    let bound_control = macos_bound_control(target, retained_pin.is_some());

    if method == "keystroke" {
        // Synthetic typing has no attributable read-back contract. Retire it
        // to passive clipboard fallback instead of reporting or submitting an
        // unproven insertion.
        Err(CopyFallbackReason::Other(
            "synthetic_insertion_unverifiable".to_string(),
        ))
    } else {
        // Application identity, observed from the OS BEFORE the clipboard is touched. A terminal
        // refuses here (macOS has no pane confirmation, so no terminal write can be confirmed and
        // `terminal_pane_security` does not apply); an application on neither of the host's lists
        // follows the host's `UnclassifiedPaste`; unknown and error refuse. The identity bound
        // here is observed again inside the authorization of the paste and of Return.
        let bound_app = match macos_capture_app(host, target) {
            Ok(app) => app,
            Err(refusal) => {
                host.log(
                    "delivery_write_refused",
                    &format!("write=paste stage=entry reason={}", refusal.code()),
                );
                return Err(Refusal::App(refusal).fallback_reason());
            }
        };
        // Single dispatch-only path (spec section 4): ordinary dictation
        // always delivers through ONE clipboard + Cmd+V sequence. No native
        // AX write precedes it. Read-only AX evidence below may accelerate
        // restore/send decisions, but it never owns insertion.
        let snapshot_started = Instant::now();
        let prior = rightkit_automation::desktop::macos::snapshot_general_pasteboard()
            .ok_or(CopyFallbackReason::ClipboardUnavailable)?;
        steps.push(DeliveryTimingStep::new(
            "clipboard_snapshot",
            elapsed_ms(snapshot_started),
        ));
        let stage_started = Instant::now();
        let mut promised = match rightkit_automation::desktop::macos::stage_promised_clipboard_text(
            transcript,
            prior.change_count(),
        ) {
            Ok(promised) => promised,
            Err(failed_change_count) => {
                if let Some(change_count) = failed_change_count {
                    let _ = rightkit_automation::desktop::macos::restore_general_pasteboard_if_unchanged(
                        &prior,
                        change_count,
                    );
                }
                return Err(CopyFallbackReason::ClipboardUnavailable);
            }
        };
        steps.push(DeliveryTimingStep::new(
            "promised_clipboard_stage",
            elapsed_ms(stage_started),
        ));
        // Record our own clipboard write: this generation and the change count right after it.
        // A count that cannot be represented is not recorded: the generation then owns nothing
        // and its restore is refused (the pasteboard is left alone).
        if let Ok(change_count) = u64::try_from(promised.change_count()) {
            restore_gate::record_stage(generation, change_count);
        }
        let (ready, retries) = settle_transient_macos(host, || {
            macos_target_ready_for_input(host, target, require_editable, retained_pin)
        });
        log_settle(host, "ready_before_secure_check", ready, retries);
        if !ready {
            *clipboard =
                abandon_stage_macos(host, generation, &promised, "ready_before_secure_check");
            return Err(CopyFallbackReason::FocusChanged);
        }
        let secure_started = Instant::now();
        let secure_state = retained_pin.map_or_else(
            || {
                target
                    .process_id
                    .and_then(|pid| focused_field_security_with_retry_macos(host, pid as i32))
            },
            rightkit_automation::desktop::macos::focused_field_is_secure_for_pin,
        );
        steps.push(DeliveryTimingStep::new(
            "secure_target_revalidation",
            elapsed_ms(secure_started),
        ));
        // Early refusal on positive evidence; the gate before Cmd+V refuses unknown too.
        if secure_state == Some(true) {
            *clipboard =
                abandon_stage_macos(host, generation, &promised, "secure_target_revalidation");
            return Err(CopyFallbackReason::Other("secure_target".to_string()));
        }
        let (ready, retries) = settle_transient_macos(host, || {
            macos_target_ready_for_input(host, target, require_editable, retained_pin)
        });
        log_settle(host, "ready_after_secure_check", ready, retries);
        if !ready {
            *clipboard =
                abandon_stage_macos(host, generation, &promised, "ready_after_secure_check");
            return Err(CopyFallbackReason::FocusChanged);
        }

        // Readable AX targets must prove an attributable text mutation. A
        // blind proof (terminal grids and opaque web editors) cannot authorize
        // delivery through this retained stop path, so recovery keeps transcript
        // available without targeting another field.
        let paste_proof = retained_pin
            .map(rightkit_automation::desktop::macos::focused_field_proof_for_pin)
            .or_else(|| {
                (!require_retained_pin).then(|| {
                    target
                        .process_id
                        .map(|pid| {
                            rightkit_automation::desktop::macos::focused_field_proof_for_pid(
                                pid as i32,
                            )
                        })
                        .unwrap_or_default()
                })
            });
        let readback_required = paste_proof.as_ref().is_some_and(|proof| !proof.is_blind());
        host.log(
            "delivery_paste_baseline",
            &format!(
                "pid={:?} retained_pin={} proof_present={} proof={} source={} range_known={} captured_chars={}",
                target.process_id,
                retained_pin.is_some(),
                paste_proof.is_some(),
                paste_proof.as_ref().map_or("none", |proof| if proof.is_blind() { "blind" } else { "readable" }),
                paste_proof.as_ref().map_or("none", |proof| proof.source_label()),
                paste_proof.as_ref().is_some_and(|proof| proof.range_known()),
                paste_proof.as_ref().map_or(0, |proof| proof.captured_chars()),
            ),
        );
        // AX baseline reads can yield. Do not dispatch to a field that gained
        // focus while those reads were in flight.
        if !macos_target_ready_for_input(host, target, require_editable, retained_pin) {
            *clipboard = abandon_stage_macos(host, generation, &promised, "ready_after_baseline");
            return Err(CopyFallbackReason::FocusChanged);
        }

        let (paste_event_probe, paste_event_token) = {
            use std::sync::atomic::Ordering;
            let probe_started = Instant::now();
            let token = PASTE_EVENT_PROBE_TOKEN.fetch_add(1, Ordering::Relaxed);
            let probe = target.process_id.and_then(|pid| {
                rightkit_automation::desktop::paste_event_probe::PasteEventProbe::start(
                    pid as i32,
                    token,
                    Duration::from_millis(
                        host.config.paste_receipt_timeout_ms + host.config.probe_extra_ms,
                    ),
                    rightkit_automation::desktop::paste_event_probe::ProbeLimits {
                        max_events: host.config.probe_max_events,
                        ready_timeout: Duration::from_millis(host.config.probe_ready_timeout_ms),
                        max_lifetime: Duration::from_millis(host.config.probe_max_lifetime_ms),
                        poll_interval: Duration::from_millis(host.config.probe_poll_interval_ms),
                    },
                )
            });
            steps.push(DeliveryTimingStep::new(
                if probe.is_some() {
                    "paste_target_event_probe_ready"
                } else {
                    "paste_target_event_probe_unavailable"
                },
                elapsed_ms(probe_started),
            ));
            (probe, token)
        };
        // Build the Cmd+V batch BEFORE the gate: building looks up and allocates, and nothing of
        // that may sit between the authorization and the keystroke.
        let Some(paste_batch) = gated_writes::prepare_paste(paste_event_token) else {
            *clipboard = abandon_stage_macos(host, generation, &promised, "paste_not_prepared");
            return Err(CopyFallbackReason::PasteFailed);
        };
        // Probe setup may take time. Final evidence gate immediately before Cmd+V: fresh
        // secure-field read (unknown refuses), fresh focused control compared with the bound
        // one, in time. The only way to obtain the permit the keystroke needs.
        let gate_started = Instant::now();
        let permit = match authorize_with_input(
            WriteKind::PasteKeystroke,
            evidence_budget,
            || {
                retained_pin.map_or_else(
                    || {
                        target.process_id.and_then(|pid| {
                            focused_field_security_with_retry_macos(host, pid as i32)
                        })
                    },
                    rightkit_automation::desktop::macos::focused_field_is_secure_for_pin,
                )
            },
            || {
                macos_control_match_with_app(
                    host,
                    bound_control.as_ref(),
                    &bound_app,
                    target,
                    require_editable,
                    retained_pin,
                )
            },
            // The real physical-modifier sample (`physical_modifier_state`); unknown refuses.
            gated_writes::modifiers_quiet,
        ) {
            Ok(permit) => permit,
            Err(refusal) => {
                host.log(
                    "delivery_write_refused",
                    &format!("write=paste reason={}", refusal.code()),
                );
                *clipboard = abandon_stage_macos(host, generation, &promised, "paste_gate_refused");
                return Err(refusal.fallback_reason());
            }
        };
        // Nothing runs between the permit and the send except the redemption and the receipt
        // mark (the provider ignores all reads before the mark, while reads during the posting
        // remain valid target-consumption evidence). Timings are taken after the send.
        let paste_posted = gated_writes::paste(permit, &paste_batch, || promised.mark_posted());
        let paste_started = Instant::now();
        steps.push(DeliveryTimingStep::new(
            "write_evidence_gate_paste",
            paste_started.duration_since(gate_started).as_millis() as u64,
        ));
        if !paste_posted {
            *clipboard = abandon_stage_macos(host, generation, &promised, "paste_not_posted");
            return Err(CopyFallbackReason::PasteFailed);
        }
        (host.mark_paste_issued)();
        steps.push(DeliveryTimingStep::new("paste_keystroke", 0));
        let receipt_started = Instant::now();
        let receipt =
            promised.wait_for_receipt(Duration::from_millis(host.config.paste_receipt_timeout_ms));
        let observation = paste_event_probe.map(|probe| probe.finish());
        let down_seen = observation.as_ref().is_some_and(|result| {
            result.events.iter().any(|event| {
                matches!(
                    event.kind,
                    rightkit_automation::desktop::paste_event_probe::PasteEventKind::Down
                )
            })
        });
        let up_seen = observation.as_ref().is_some_and(|result| {
            result.events.iter().any(|event| {
                matches!(
                    event.kind,
                    rightkit_automation::desktop::paste_event_probe::PasteEventKind::Up
                )
            })
        });
        let target_event_confirmed = down_seen && up_seen;
        let event_count = observation.as_ref().map_or(0, |result| result.events.len());
        host.log(
            "delivery_paste_target_event",
            &format!(
                "pid={:?} token={} probe_available={} down_seen={} up_seen={} event_count={}",
                target.process_id,
                paste_event_token,
                observation.is_some(),
                down_seen,
                up_seen,
                event_count,
            ),
        );
        steps.push(DeliveryTimingStep::new(
            if target_event_confirmed {
                "paste_target_event_seen"
            } else if observation.is_some() {
                "paste_target_event_not_seen"
            } else {
                "paste_target_event_probe_unavailable"
            },
            elapsed_ms(paste_started),
        ));
        let first_receipt_ms = receipt
            .first_at
            .and_then(|at| at.checked_duration_since(paste_started))
            .map(|elapsed| elapsed.as_millis());
        let last_receipt_ms = receipt
            .last_at
            .and_then(|at| at.checked_duration_since(paste_started))
            .map(|elapsed| elapsed.as_millis());
        host.log(
            "delivery_paste_receipt",
            &format!(
                "pid={:?} observed={} count={} ownership_lost={} first_ms={:?} last_ms={:?}",
                target.process_id,
                receipt.observed(),
                receipt.count,
                receipt.ownership_lost,
                first_receipt_ms,
                last_receipt_ms,
            ),
        );
        steps.push(DeliveryTimingStep::new(
            if receipt.observed() {
                "paste_receipt_observed"
            } else {
                "paste_receipt_timeout_clipboard_retained"
            },
            elapsed_ms(receipt_started),
        ));

        // Receipt-first success rule (see `paste_outcome`). Readback is never
        // on the critical path when a receipt exists; it is only a second
        // chance when the target saw the key event but never read the
        // promised data.
        let readback_started = Instant::now();
        let mut readback_attempts = 0_u32;
        let mut readback_confirmation_path = "skipped_receipt";
        let readback_confirmed = if receipt.observed() || !target_event_confirmed {
            false
        } else if let (Some(pid), Some(proof)) = (
            target
                .process_id
                .or_else(|| retained_pin.map(|pin| pin.pid() as u32)),
            paste_proof.as_ref().filter(|proof| !proof.is_blind()),
        ) {
            readback_confirmation_path = "unconfirmed";
            let deadline =
                Instant::now() + Duration::from_millis(host.config.paste_readback_timeout_ms);
            loop {
                readback_attempts = readback_attempts.saturating_add(1);
                let confirmed = if let Some(pin) = retained_pin {
                    let confirmation =
                        rightkit_automation::desktop::macos::insertion_confirmation_for_pin(
                            pin, proof, transcript,
                        );
                    if confirmation.confirmed() {
                        readback_confirmation_path = confirmation.label();
                    }
                    confirmation.confirmed()
                } else {
                    let confirmed =
                        rightkit_automation::desktop::macos::insertion_confirmed_for_pid(
                            pid as i32, proof, transcript,
                        );
                    if confirmed {
                        readback_confirmation_path = "pid_retained_element";
                    }
                    confirmed
                };
                if confirmed {
                    break true;
                }
                if Instant::now() >= deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(
                    host.config.readback_retry_interval_ms,
                ));
            }
        } else {
            false
        };
        let destination_current = retained_pin.is_some()
            && paste_proof
                .as_ref()
                .is_some_and(rightkit_automation::desktop::macos::proof_destination_is_current);
        host.log(
            "delivery_paste_readback",
            &format!(
                "pid={:?} confirmed={} path={} attempts={} elapsed_ms={} target_event_seen={} receipt_observed={} destination_current={}",
                target.process_id,
                if receipt.observed() { "skipped_receipt".to_string() } else { readback_confirmed.to_string() },
                readback_confirmation_path,
                readback_attempts,
                readback_started.elapsed().as_millis(),
                target_event_confirmed,
                receipt.observed(),
                destination_current,
            ),
        );
        if let Err(reason) = paste_outcome(
            true,
            receipt.observed(),
            target_event_confirmed,
            readback_confirmed,
        ) {
            if !receipt.observed() {
                steps.push(DeliveryTimingStep::new(
                    if readback_required {
                        "paste_readback_unconfirmed"
                    } else {
                        "paste_readback_unavailable"
                    },
                    elapsed_ms(readback_started),
                ));
            }
            return Err(reason);
        }
        if readback_confirmed {
            steps.push(DeliveryTimingStep::new(
                "paste_readback_confirmed",
                elapsed_ms(readback_started),
            ));
        }

        on_inserted();

        let mut submit_failed = false;
        // Paste-only NEVER presses Return. Reaching here means `paste_outcome`
        // accepted the paste (clipboard receipt, or target event + readback).
        // Return still requires a proven, current, positively non-secure editor
        // (blind targets paste without Return).
        if send_enter {
            // The destination check for Return is the PROVEN editor, not the
            // pre-paste continuity window: the paste itself mutates the focused
            // element, so re-evaluating that window after insertion rejected the
            // very destination we had just inserted into and silently dropped
            // Return. `proof_destination_is_current` re-verifies the same
            // process, the same window and the exact editor the insertion landed
            // in, and requires positively known non-secure. The pre-paste
            // destination + security guards above are unchanged, and are what
            // stop a paste into a destination that has moved.
            //
            // A receipt proves the clipboard was read, not inserted; browser
            // editors insert asynchronously and an immediate Return can land
            // first. Wait (bounded) until the editor shows the text.
            if !readback_confirmed {
                let settle_started = Instant::now();
                let proof = paste_proof.as_ref().filter(|proof| !proof.is_blind());
                let pid = target
                    .process_id
                    .or_else(|| retained_pin.map(|pin| pin.pid() as u32));
                let landed = if let (Some(pid), Some(proof)) = (pid, proof) {
                    let settle_deadline = settle_started
                        + Duration::from_millis(host.config.enter_insertion_settle_ms);
                    loop {
                        let confirmed = match retained_pin {
                            Some(pin) => {
                                rightkit_automation::desktop::macos::insertion_confirmation_for_pin(
                                    pin, proof, transcript,
                                )
                                .confirmed()
                            }
                            None => {
                                rightkit_automation::desktop::macos::insertion_confirmed_for_pid(
                                    pid as i32, proof, transcript,
                                )
                            }
                        };
                        if confirmed {
                            break true;
                        }
                        if Instant::now() >= settle_deadline {
                            break false;
                        }
                        std::thread::sleep(Duration::from_millis(
                            host.config.enter_settle_interval_ms,
                        ));
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(host.config.enter_blind_settle_ms));
                    false
                };
                steps.push(DeliveryTimingStep::new(
                    if landed {
                        "enter_insertion_confirmed"
                    } else {
                        "enter_insertion_settle_elapsed"
                    },
                    elapsed_ms(settle_started),
                ));
            }
            // Return passes the same gate as the paste: fresh secure read (unknown refuses),
            // fresh control comparison, and additionally the proven editor must still be the
            // current destination (a blind proof never submits).
            let enter_batch = gated_writes::prepare_enter();
            let secure_started = Instant::now();
            let enter_permit = authorize_with_input(
                WriteKind::Enter,
                evidence_budget,
                || {
                    retained_pin.map_or_else(
                        || {
                            target.process_id.and_then(|pid| {
                                focused_field_security_with_retry_macos(host, pid as i32)
                            })
                        },
                        rightkit_automation::desktop::macos::focused_field_is_secure_for_pin,
                    )
                },
                || {
                    let destination_current = paste_proof.as_ref().is_some_and(
                        rightkit_automation::desktop::macos::proof_destination_is_current,
                    );
                    if !destination_current {
                        return ControlMatch::Changed;
                    }
                    macos_control_match_with_app(
                        host,
                        bound_control.as_ref(),
                        &bound_app,
                        target,
                        require_editable,
                        retained_pin,
                    )
                },
                // The real physical-modifier sample, read again for Return; unknown refuses.
                gated_writes::modifiers_quiet,
            );
            // The permit is redeemed and the prepared Return sent first; every timing and log
            // line follows the send.
            let enter_result = match (enter_permit, enter_batch) {
                (Ok(permit), Some(batch)) => Some(gated_writes::enter_prepared(permit, &batch)),
                (Ok(_), None) => Some(false),
                (Err(refusal), _) => {
                    steps.push(DeliveryTimingStep::new(
                        "secure_target_revalidation_before_enter",
                        elapsed_ms(secure_started),
                    ));
                    host.log(
                        "delivery_write_refused",
                        &format!("write=enter reason={}", refusal.code()),
                    );
                    None
                }
            };
            if let Some(sent) = enter_result {
                steps.push(DeliveryTimingStep::new(
                    "secure_target_revalidation_before_enter",
                    elapsed_ms(secure_started),
                ));
                if sent {
                    steps.push(DeliveryTimingStep::new(
                        "paste_settle_and_submit",
                        elapsed_ms(receipt_started),
                    ));
                } else {
                    submit_failed = true;
                    steps.push(DeliveryTimingStep::new(
                        "paste_receipt_submit_failed",
                        elapsed_ms(receipt_started),
                    ));
                }
            } else {
                steps.push(DeliveryTimingStep::new(
                    "paste_receipt_enter_skipped_target_or_secure",
                    elapsed_ms(receipt_started),
                ));
            }
        }
        // Pasted was reported above (on_inserted) and Return, if any, is out. The
        // receipt-anchored quiet window and the prior-pasteboard restore run on a
        // background thread with the same guards: the restore only happens while
        // the declaration still owns its change count, never on a blind timer. The
        // next delivery waits for this thread (`restore_gate::supersede_pending`).
        restore_gate::mark_pending(generation);
        let job = AssertSend((promised, prior));
        let restore_thread = std::thread::Builder::new()
            .name("delivery-clipboard-restore".to_string())
            .spawn(move || {
                let (mut promised, prior) = job.into_inner();
                // Quiet window: never restore while the target may still be reading the
                // promised data (200 ms after the last receipt, capped at 1 s).
                let quiet_started = Instant::now();
                let cap = quiet_started + Duration::from_millis(host.config.restore_quiet_cap_ms);
                let mut seen_last = promised.receipt().last_at;
                loop {
                    let wait = paste_restore_quiet_remaining(seen_last, Instant::now());
                    let wait = wait.min(cap.saturating_duration_since(Instant::now()));
                    if wait.is_zero() {
                        break;
                    }
                    std::thread::sleep(wait);
                    let latest = promised.receipt().last_at;
                    if latest == seen_last {
                        break;
                    }
                    seen_last = latest;
                }
                let quiet_window_ms = elapsed_ms(quiet_started);
                let restore_started = Instant::now();
                let restored = restore_prior_macos(
                    host,
                    generation,
                    &mut promised,
                    &prior,
                    "background_restore",
                );
                host.log(
                    "delivery_clipboard_restore_background",
                    &format!(
                        "clipboard_restore_quiet_window_ms={quiet_window_ms} clipboard_restore_after_receipt={restored} restore_ms={}",
                        elapsed_ms(restore_started)
                    ),
                );
                restore_gate::finish(generation);
            });
        if restore_thread.is_err() {
            restore_gate::finish(generation);
            host.log(
                "delivery_clipboard_restore_skipped",
                "reason=restore_thread_spawn_failed",
            );
        }
        if submit_failed {
            // The paste is proven, so never retry insertion. Keep Pasted
            // outcome while timing explicitly records Return failure; the
            // submit marker is absent, so consumers report no submission.
            return Ok(());
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
pub fn insert_transcript(
    host: &'static DeliveryHost,
    transcript: &str,
    target: &TargetSnapshot,
    method: &str,
    require_editable: bool,
    send_enter: bool,
    stop_target: Option<&WindowsStopTarget>,
    steps: &mut Vec<DeliveryTimingStep>,
    on_inserted: &mut dyn FnMut(),
) -> Result<(), CopyFallbackReason> {
    let mut clipboard = FallbackClipboard::NOT_OWNED;
    insert_transcript_tracked(
        host,
        transcript,
        target,
        method,
        require_editable,
        send_enter,
        stop_target,
        steps,
        on_inserted,
        &mut clipboard,
    )
}

/// [`insert_transcript`] that also reports what a refusal left on the clipboard in `clipboard`
/// (set at every site that staged the transcript before giving up; otherwise it keeps the
/// caller's value, `FallbackClipboard::NOT_OWNED`). No path in here writes the clipboard to
/// "save" the transcript.
#[cfg(target_os = "windows")]
pub fn insert_transcript_tracked(
    host: &'static DeliveryHost,
    transcript: &str,
    target: &TargetSnapshot,
    _method: &str,
    require_editable: bool,
    send_enter: bool,
    stop_target: Option<&WindowsStopTarget>,
    steps: &mut Vec<DeliveryTimingStep>,
    on_inserted: &mut dyn FnMut(),
    clipboard: &mut FallbackClipboard,
) -> Result<(), CopyFallbackReason> {
    use std::time::Duration;

    // Terminal identification, on EVERY entry point (`deliver*`, `deliver_stop_*`, `insert_transcript`,
    // `paste_text` all end here). The process named in the snapshot is matched against the host's
    // terminal list, independently of the pin: a terminal reached without a bound WezTerm pane is
    // refused before the clipboard is touched, under BOTH terminal-pane policies (the policy
    // relaxes only the secure-field requirement of a confirmed pane). A snapshot with no process
    // name is not classified here; the gate classifies the live foreground window right before the
    // write, and an unreadable process refuses there.
    let pane_pin = stop_target.filter(|stop| stop.is_wezterm());
    if pane_pin.is_none() && target_terminal_state(host, target) == Some(true) {
        host.log(
            "delivery_terminal_pane_refused",
            "stage=entry reason=no_bound_pane",
        );
        return Err(CopyFallbackReason::Other(
            Refusal::TerminalPaneUnverified.code().to_string(),
        ));
    }
    // Under `RequireEvidence` a pane has no affirmative "not secure" evidence (see
    // `wezterm_secure_state`), so an automatic write into it is refused before the clipboard is
    // touched: nothing is staged, the user's clipboard stays as it is, and the host offers an
    // explicit Copy. Under `NotApplicableWhenPaneConfirmed` the check is the host's declared
    // exception and the gate below decides, after a fresh pane check.
    if pane_pin.is_some() {
        if let SecureEvidence::Probed(state) = windows_secure_evidence(host, stop_target) {
            if state != Some(false) {
                host.log(
                    "delivery_wezterm_secure_unverifiable",
                    "automatic write refused: no affirmative not-secure evidence for the pane",
                );
                return Err(CopyFallbackReason::Other(
                    "wezterm_secure_unverifiable".to_string(),
                ));
            }
        }
    }
    // The Ctrl+V batch is built before anything is staged or sampled (see `gated_writes`).
    let Some(paste_batch) = gated_writes::prepare_paste() else {
        return Err(CopyFallbackReason::PasteFailed);
    };
    let enter_batch = if send_enter {
        gated_writes::prepare_enter()
    } else {
        None
    };

    // This delivery's clipboard generation: its restore acts only while this generation still
    // owns the recorded clipboard write.
    let generation = restore_gate::begin_delivery();
    let evidence_budget = Duration::from_millis(host.config.write_evidence_budget_ms);

    // A previous delivery's background clipboard restore must finish before
    // this one snapshots the clipboard (bounded; normally already idle). A timeout
    // clears nothing that belongs to the older delivery.
    let restore_wait_ms = restore_gate::supersede_pending(
        generation,
        Duration::from_millis(host.config.clipboard_restore_wait_ms),
    );
    steps.push(DeliveryTimingStep::new(
        "clipboard_restore_wait",
        restore_wait_ms,
    ));
    // The secure state of the destination: the UIA password probe, or for a WezTerm pane the
    // pane's own evidence (`wezterm_secure_state`: none exists, so `None`). This early read only
    // refuses on positive evidence; the gate below takes `windows_secure_evidence`, which for a
    // pane under `NotApplicableWhenPaneConfirmed` is the host's named exception. There is no
    // hard-coded "not secure" answer for any target.
    let password_state = windows_secure_state(host, stop_target);
    // Early exit before the clipboard is touched, on positive evidence only; the evidence gate
    // right before Ctrl+V refuses unknown as well.
    if password_state == Some(true) {
        return Err(CopyFallbackReason::Other("secure_target".to_string()));
    }

    // Single dispatch-only path (spec section 4): ordinary dictation always
    // delivers through ONE clipboard + Ctrl+V sequence; no UIA SetValue write
    // Delivery spec section 4.5: a snapshot that cannot represent every
    // prior format must never back the destructive transcript transaction —
    // silent format loss is forbidden. Route to visible recovery instead and
    // let explicit Copy replace the clipboard.
    let clipboard_snapshot_started = Instant::now();
    let prior = rightkit_automation::desktop::windows::snapshot_general_clipboard()
        .ok_or(CopyFallbackReason::ClipboardUnavailable)?;
    if !prior.is_lossless() {
        host.log(
            "delivery_clipboard_snapshot_lossy",
            &format!(
                "formats_dropped={} details={}",
                prior.dropped_formats(),
                prior
                    .dropped_format_diagnostics()
                    .iter()
                    .map(|entry| format!("{}:{}", entry.format, entry.reason.as_str()))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        );
        return Err(CopyFallbackReason::ClipboardLossySnapshot);
    }
    steps.push(DeliveryTimingStep::new(
        "clipboard_snapshot",
        elapsed_ms(clipboard_snapshot_started),
    ));
    let stage_started = Instant::now();
    let mut promised = match rightkit_automation::desktop::windows::stage_promised_clipboard_text(
        transcript,
        prior.sequence(),
    ) {
        Ok(promised) => promised,
        Err(restore_sequence) => {
            if let Some(restore_sequence) = restore_sequence {
                let _ =
                    rightkit_automation::desktop::windows::restore_general_clipboard_if_unchanged(
                        prior,
                        restore_sequence,
                    );
            }
            return Err(CopyFallbackReason::ClipboardUnavailable);
        }
    };
    steps.push(DeliveryTimingStep::new(
        "promised_clipboard_stage",
        elapsed_ms(stage_started),
    ));
    // Record our own clipboard write: this generation and the sequence number right after it.
    restore_gate::record_stage(generation, u64::from(promised.sequence()));
    if !windows_target_ready_for_input(host, target, require_editable, stop_target) {
        *clipboard = abandon_stage_windows(host, generation, &promised, "target_ready_after_stage");
        return Err(CopyFallbackReason::FocusChanged);
    }
    // Capture the UIA identity of the focused element before posting Ctrl+V.
    // It is ADVISORY: it sharpens the focus-change check when the target
    // exposes a UIA element whose native window handle matches the Win32
    // focus handle, and it feeds the optional readback telemetry. It is NOT a
    // precondition for pasting. Chromium/Electron (Claude, VS Code, Slack,
    // browsers) and terminal grids report the render-widget/host handle, not
    // `GetGUIThreadInfo().hwndFocus`, so the probe yields `None` for the most
    // common targets — 0.1.157 treated that as `focus_changed` and pasted
    // nothing on Windows at all (five for five copied_fallback in the field
    // log, every one inside 22 ms, every one on an editable target the HWND
    // gate had just passed). The HWND-level `target_ready_for_input` gate plus
    // the WM_RENDERFORMAT receipt below are the delivery contract; they are
    // what the last shipped Windows build pasted on.
    let paste_identity = if let Some(captured) = stop_target {
        captured.uia_identity().cloned()
    } else if windows_opaque_target(host, target) {
        None
    } else {
        windows_focused_element_identity(host, target)
    };
    if paste_identity.is_none() {
        host.log("delivery_identity_probe_unavailable", "stage=pre_paste");
    }
    let secure_started = Instant::now();
    let secure_state = windows_secure_state(host, stop_target);
    steps.push(DeliveryTimingStep::new(
        "secure_target_revalidation",
        elapsed_ms(secure_started),
    ));
    // Early refusal on positive evidence; the gate before Ctrl+V refuses unknown too.
    if secure_state == Some(true) {
        *clipboard =
            abandon_stage_windows(host, generation, &promised, "secure_target_revalidation");
        return Err(CopyFallbackReason::Other("secure_target".to_string()));
    }
    // Security probing itself can yield to a focus change. Re-check the HWND
    // target, and the UIA identity where the probe produced one, as an early exit; the
    // evidence gate below repeats the comparison fresh immediately before Ctrl+V.
    if !windows_target_ready_for_input(host, target, require_editable, stop_target) {
        *clipboard =
            abandon_stage_windows(host, generation, &promised, "target_ready_after_secure");
        return Err(CopyFallbackReason::FocusChanged);
    }
    if windows_identity_moved(host, paste_identity.as_ref(), target, "pre_paste") {
        *clipboard = abandon_stage_windows(host, generation, &promised, "identity_moved_pre_paste");
        return Err(CopyFallbackReason::FocusChanged);
    }
    // The control this delivery is bound to; the gate refuses when there is none.
    let bound_control = windows_bound_control(host, target, stop_target, paste_identity.as_ref());
    // Capture proof immediately before Ctrl+V, after all focus/security
    // revalidation. Stop capture retains identity only; no stale stop-time
    // text may be paired with post-paste text. Retained UIA elements are read
    // on the persistent MTA even if current focus churns during the call.
    let baseline_started = Instant::now();
    let readback_before = if let Some(stop) = stop_target {
        if stop.is_wezterm() {
            // The pane check is NOT run here any more: it runs fresh inside the gate below,
            // immediately before the chord (and again before Return). A pane contributes only
            // the baseline captured in the background after Stop; nothing is spawned here. A
            // missing baseline only removes the readback channel.
            stop.wezterm_baseline()
        } else {
            windows_stop_text_snapshot(host, stop)
        }
    } else {
        paste_identity.as_ref().and_then(|identity| {
            windows_focused_text_snapshot_with_identity(host, target, identity)
        })
    }
    .filter(|text| windows_text_readback_is_usable(target, text));
    steps.push(DeliveryTimingStep::new(
        "prepaste_baseline_probe",
        elapsed_ms(baseline_started),
    ));
    // UIA proof reads may consume their bounded worker budget. Everything that waits happens
    // BEFORE the gate: the hotkey's physical modifiers are waited out first, and a key still held
    // at the cap refuses the write (the chord is a fixed batch; it no longer releases and
    // re-presses the user's keys).
    let settle = gated_writes::settle_modifiers(host.config.write_modifier_settle_cap_ms);
    if settle.waited_ms > 0 {
        steps.push(DeliveryTimingStep::new(
            "paste_modifier_settle",
            settle.waited_ms,
        ));
    }
    if settle.still_held {
        host.log(
            "delivery_write_refused",
            &format!("write=paste reason={}", Refusal::InputBusy.code()),
        );
        *clipboard = abandon_stage_windows(host, generation, &promised, "modifiers_held");
        return Err(Refusal::InputBusy.fallback_reason());
    }
    // The evidence gate is the last thing before the write: a fresh secure sample (unknown
    // refuses; a pane under `RequireEvidence` has no source and refuses; under
    // `NotApplicableWhenPaneConfirmed` it is the named `NotApplicableByHostPolicy`), a fresh
    // control comparison at the bound level (for a pane: a fresh `PanePreflight::Current` of the
    // bound pane, and for any terminal window a bound pane at all, taken inside this sampling),
    // and a last sample of the modifier keys.
    let gate_started = Instant::now();
    let permit = match authorize_evidence(
        WriteKind::PasteKeystroke,
        evidence_budget,
        || windows_secure_evidence(host, stop_target),
        || {
            windows_control_match(
                host,
                bound_control.as_ref(),
                target,
                require_editable,
                stop_target,
            )
        },
        gated_writes::modifiers_quiet,
    ) {
        Ok(permit) => permit,
        Err(refusal) => {
            host.log(
                "delivery_write_refused",
                &format!("write=paste reason={}", refusal.code()),
            );
            *clipboard = abandon_stage_windows(host, generation, &promised, "paste_gate_refused");
            return Err(refusal.fallback_reason());
        }
    };
    // Nothing runs between the permit and the send except the redemption and the receipt mark
    // (see `gated_writes::paste`); the send is one SendInput batch. Timings follow the send.
    let paste_under_pane_policy = permit.secure_not_applicable();
    let chord_sent = gated_writes::paste(permit, &paste_batch, || promised.mark_posted());
    let paste_started = Instant::now();
    steps.push(DeliveryTimingStep::new(
        "write_evidence_gate_paste",
        paste_started.duration_since(gate_started).as_millis() as u64,
    ));
    if paste_under_pane_policy {
        // After the send, never between the authorization and the effect. No user content.
        host.log(
            "delivery_terminal_pane_policy",
            &format!("policy=not_applicable_when_pane_confirmed write=paste sent={chord_sent}"),
        );
    }
    if !chord_sent {
        // Not sent (SendInput failed or the permit expired). Nothing was pasted and nothing is
        // written to the clipboard.
        *clipboard = abandon_stage_windows(host, generation, &promised, "paste_not_sent");
        return Err(CopyFallbackReason::PasteFailed);
    }
    (host.mark_paste_issued)();
    steps.push(DeliveryTimingStep::new("paste_keystroke", 0));

    // Ctrl+V is async. Race the two success signals against ONE deadline
    // (receipt timeout, or the terminal budget): the WM_RENDERFORMAT receipt
    // (the target requested the promised data) and, when a UIA baseline
    // exists, attributable readback. Each 5 ms iteration checks the receipt
    // (condition-variable wait, returns the instant it arrives) and then polls
    // readback. Whichever lands first wins and ends the wait; readback never
    // adds latency on top of a receipt wait.
    let receipt_started = Instant::now();
    let terminal_target = windows_terminal_target(
        target.process_name.as_deref(),
        target.window_title.as_deref(),
        None,
    ) || stop_target.is_some_and(WindowsStopTarget::is_wezterm)
        || readback_before
            .as_ref()
            .is_some_and(|proof| proof.terminal_rows.is_some());
    // No baseline on a terminal means readback can never confirm, so the
    // receipt gets only a short grace window instead of the 2.5 s deadline.
    let no_confirm_channel =
        terminal_paste_has_no_confirm_channel(terminal_target, readback_before.is_some());
    let race_deadline = receipt_started
        + Duration::from_millis(if no_confirm_channel {
            host.config.terminal_no_baseline_receipt_grace_ms
        } else {
            windows_paste_readback_timeout_ms(host, target, readback_before.as_ref())
        });
    let poll_budget_ms = if terminal_target {
        host.config.terminal_readback_poll_ms
    } else {
        host.config.readback_poll_ms
    };
    let mut receipt = promised.wait_for_receipt(Duration::from_millis(host.config.receipt_poll_ms));
    let mut readback_confirmed = false;
    let mut readback_polls = 0_u32;
    loop {
        if receipt.observed() {
            break;
        }
        if let Some(before) = readback_before.as_ref() {
            readback_polls = readback_polls.saturating_add(1);
            let poll_deadline =
                (Instant::now() + Duration::from_millis(poll_budget_ms)).min(race_deadline);
            let after = if let Some(stop) = stop_target {
                windows_stop_text_snapshot_until(stop, poll_deadline)
            } else if let Some(identity) = paste_identity.as_ref() {
                windows_focused_text_snapshot_with_identity_until(
                    host,
                    target,
                    identity,
                    poll_deadline,
                )
            } else {
                None
            };
            if after.is_some_and(|after| {
                before.confirms(&after, transcript, &windows_uia(host).config().rules)
            }) {
                readback_confirmed = true;
                break;
            }
        }
        if Instant::now() >= race_deadline {
            break;
        }
        receipt = promised.wait_for_receipt(Duration::from_millis(host.config.receipt_poll_ms));
        if !receipt.observed() && receipt.ownership_lost {
            // wait_for_receipt returns at once after ownership loss; do not spin.
            std::thread::sleep(Duration::from_millis(host.config.receipt_poll_ms));
        }
    }
    let race_winner = if receipt.observed() {
        "receipt"
    } else if readback_confirmed {
        "readback"
    } else {
        "none"
    };
    // A receipt that landed while readback won still counts for the restore
    // quiet window below.
    let receipt = promised.receipt();
    steps.push(DeliveryTimingStep::new(
        if race_winner == "receipt" {
            "paste_receipt_observed"
        } else {
            "paste_receipt_unobserved_readback_pending"
        },
        elapsed_ms(receipt_started),
    ));
    let readback_started = receipt_started;
    host.log(
        "delivery_paste_readback",
        &format!(
            "winner={} confirmed={} receipt_observed={} baseline_present={} polls={} elapsed_ms={}",
            race_winner,
            if race_winner == "receipt" {
                "skipped_receipt".to_string()
            } else {
                readback_confirmed.to_string()
            },
            receipt.observed(),
            readback_before.is_some(),
            readback_polls,
            receipt_started.elapsed().as_millis(),
        ),
    );
    // Windows has no key-event probe; the keystroke send itself is the
    // event evidence, so readback alone may confirm when no receipt came.
    let mut paste_unknown = false;
    if let Err(reason) = paste_outcome(true, receipt.observed(), true, readback_confirmed) {
        if no_confirm_channel {
            // Terminal with neither a receipt nor a readback channel: the
            // keystroke went out, consumption is unknowable. Report Pasted at
            // once (no recovery card, no progress hold); Enter and the clipboard
            // restore stay off because both need proven consumption.
            paste_unknown = true;
            steps.push(DeliveryTimingStep::new(
                "paste_unconfirmed_terminal_no_channel",
                elapsed_ms(readback_started),
            ));
            host.log(
                "delivery_paste_unknown_terminal",
                &format!(
                    "reason={reason} send_enter_requested={send_enter} receipt_wait_ms={}",
                    receipt_started.elapsed().as_millis()
                ),
            );
        } else {
            if !receipt.observed() {
                steps.push(DeliveryTimingStep::new(
                    if readback_before.is_some() {
                        "paste_readback_unconfirmed"
                    } else {
                        "paste_readback_unavailable"
                    },
                    elapsed_ms(readback_started),
                ));
            }
            return Err(reason);
        }
    }
    if readback_confirmed {
        steps.push(DeliveryTimingStep::new(
            "paste_readback_confirmed",
            elapsed_ms(receipt_started),
        ));
    }

    // Paste succeeded (receipt or confirmed readback). Return is still gated
    // on a current, positively non-secure destination.
    let mut submit_failed = false;
    if send_enter {
        // A clipboard receipt proves the target read the clipboard, not that
        // the editor inserted it. Browser editors insert asynchronously, so an
        // Enter sent on the receipt alone can land before the text and be
        // lost. Wait (bounded) for readback to show the text before Return.
        if !paste_unknown && !readback_confirmed {
            let settle_started = Instant::now();
            let landed = if let Some(before) = readback_before.as_ref() {
                let settle_deadline =
                    settle_started + Duration::from_millis(host.config.enter_insertion_settle_ms);
                let mut landed = false;
                while Instant::now() < settle_deadline {
                    let poll_deadline = (Instant::now()
                        + Duration::from_millis(host.config.enter_settle_poll_budget_ms))
                    .min(settle_deadline);
                    let after = if let Some(stop) = stop_target {
                        windows_stop_text_snapshot_until(stop, poll_deadline)
                    } else if let Some(identity) = paste_identity.as_ref() {
                        windows_focused_text_snapshot_with_identity_until(
                            host,
                            target,
                            identity,
                            poll_deadline,
                        )
                    } else {
                        None
                    };
                    if after.is_some_and(|after| {
                        before.confirms(&after, transcript, &windows_uia(host).config().rules)
                    }) {
                        landed = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(host.config.enter_settle_interval_ms));
                }
                landed
            } else {
                std::thread::sleep(Duration::from_millis(host.config.enter_blind_settle_ms));
                false
            };
            steps.push(DeliveryTimingStep::new(
                if landed {
                    "enter_insertion_confirmed"
                } else {
                    "enter_insertion_settle_elapsed"
                },
                elapsed_ms(settle_started),
            ));
        }
        let submit_started = Instant::now();
        if paste_unknown {
            steps.push(DeliveryTimingStep::new(
                "paste_enter_skipped_unconfirmed_terminal",
                elapsed_ms(submit_started),
            ));
            on_inserted();
        } else {
            // Return passes the same gate as the paste (fresh secure read, unknown refuses;
            // fresh control comparison, for a pane a fresh pane check; last sample of the
            // modifier keys). Everything that waits (the modifier wait) happens first, and a key
            // still held at the cap skips Return. The host's `on_inserted` callback runs AFTER the
            // send: it used to run between the permit and the keystroke.
            let enter_settle =
                gated_writes::settle_modifiers(host.config.write_modifier_settle_cap_ms);
            let enter_permit = if enter_settle.still_held {
                host.log(
                    "delivery_write_refused",
                    &format!("write=enter reason={}", Refusal::InputBusy.code()),
                );
                None
            } else {
                match authorize_evidence(
                    WriteKind::Enter,
                    evidence_budget,
                    || windows_secure_evidence(host, stop_target),
                    || {
                        windows_control_match(
                            host,
                            bound_control.as_ref(),
                            target,
                            require_editable,
                            stop_target,
                        )
                    },
                    gated_writes::modifiers_quiet,
                ) {
                    Ok(permit) => Some(permit),
                    Err(refusal) => {
                        host.log(
                            "delivery_write_refused",
                            &format!("write=enter reason={}", refusal.code()),
                        );
                        None
                    }
                }
            };
            let enter_under_pane_policy = enter_permit
                .as_ref()
                .is_some_and(WritePermit::secure_not_applicable);
            let enter_sent = match (enter_permit, enter_batch.as_ref()) {
                (Some(permit), Some(batch)) => Some(gated_writes::enter_prepared(permit, batch)),
                (Some(_), None) => Some(false),
                (None, _) => None,
            };
            if enter_under_pane_policy {
                host.log(
                    "delivery_terminal_pane_policy",
                    &format!(
                        "policy=not_applicable_when_pane_confirmed write=enter sent={}",
                        enter_sent == Some(true)
                    ),
                );
            }
            match enter_sent {
                Some(false) => {
                    submit_failed = true;
                    steps.push(DeliveryTimingStep::new(
                        "paste_receipt_submit_failed",
                        elapsed_ms(submit_started),
                    ));
                }
                Some(true) => {
                    steps.push(DeliveryTimingStep::new(
                        "paste_settle_and_submit",
                        elapsed_ms(submit_started),
                    ));
                }
                None => {
                    steps.push(DeliveryTimingStep::new(
                        "paste_readback_target_changed_before_enter",
                        elapsed_ms(submit_started),
                    ));
                }
            }
            on_inserted();
        }
    } else {
        on_inserted();
    }

    // The Pasted state has been reported (on_inserted ran) and Enter, if any,
    // is out. The quiet window and the prior-clipboard restore now run on a
    // background thread holding the same guards: the restore still requires
    // this window to own the clipboard generation it produced, and
    // the receipt-anchored quiet window (200 ms after the last receipt, capped
    // at 1 s) still precedes it. The next delivery waits for this thread.
    let receipt_seen = receipt.observed();
    let await_late_receipt = paste_unknown;
    restore_gate::mark_pending(generation);
    let restore_thread = std::thread::Builder::new()
        .name("delivery-clipboard-restore".to_string())
        .spawn(move || {
            // Unknown terminal paste: restore only if a LATE receipt proves the
            // terminal eventually read the transcript; otherwise leave it
            // staged rather than hand the terminal the stale prior contents.
            let mut receipt_seen = receipt_seen;
            if await_late_receipt && !receipt_seen {
                let late_deadline = Instant::now() + Duration::from_millis(host.config.late_receipt_wait_ms);
                while Instant::now() < late_deadline
                    && !restore_gate::is_superseded(generation)
                {
                    let late = promised.wait_for_receipt(Duration::from_millis(host.config.late_receipt_poll_ms));
                    if late.observed() {
                        receipt_seen = true;
                        break;
                    }
                    if late.ownership_lost {
                        break;
                    }
                }
                if !receipt_seen {
                    host.log(
                        "delivery_clipboard_restore_skipped",
                        "reason=unknown_terminal_paste_no_late_receipt transcript_left_on_clipboard=true",
                    );
                    restore_gate::finish(generation);
                    return;
                }
            }
            // Quiet window: never restore while the target may still be reading the
            // promised data (200 ms after the last receipt, capped at 1 s).
            let mut quiet_window_ms = 0_u64;
            if receipt_seen {
                let quiet_started = Instant::now();
                let cap = quiet_started + Duration::from_millis(host.config.restore_quiet_cap_ms);
                let mut seen_last = promised.receipt().last_at;
                loop {
                    let wait = paste_restore_quiet_remaining(seen_last, Instant::now());
                    let wait = wait.min(cap.saturating_duration_since(Instant::now()));
                    if wait.is_zero() {
                        break;
                    }
                    std::thread::sleep(wait);
                    let latest = promised.receipt().last_at;
                    if latest == seen_last {
                        break;
                    }
                    seen_last = latest;
                }
                quiet_window_ms = elapsed_ms(quiet_started);
            }
            let restore_started = Instant::now();
            let restored =
                restore_prior_windows(host, generation, &mut promised, prior, "background_restore");
            host.log(
                "delivery_clipboard_restore_background",
                &format!(
                    "clipboard_restore_quiet_window_ms={quiet_window_ms} clipboard_restore_after_receipt={restored} restore_ms={}",
                    elapsed_ms(restore_started)
                ),
            );
            restore_gate::finish(generation);
        });
    if restore_thread.is_err() {
        // The closure (and with it the staged promise) is gone; the transcript
        // stays on the clipboard. Release the gate so deliveries never stall.
        restore_gate::finish(generation);
        host.log(
            "delivery_clipboard_restore_skipped",
            "reason=restore_thread_spawn_failed",
        );
    }
    if submit_failed {
        return Ok(());
    }

    Ok(())
}

// Terminal emulators generally do not expose their character-grid content
// through UI Automation the way normal text boxes do, so `windows_terminal_target`
// (the process/class/title allowlist used below, and by `windows_text_readback_is_usable`)
// stays a single source of truth in `rightkit_transcript_library::delivery`
// (re-exported by the parent module) and is not copied here.

// Do not delete these: their call sites in `insert_transcript` (Windows) and
// `windows_uia.rs` are Windows-only, so removing these types breaks the
// Windows build without failing the macOS gate.
/// Identity of UIA element currently owning keyboard focus. RuntimeId binds
/// virtual Chromium controls that share one host HWND; process + native HWND
/// additionally tie it to captured app/focus identity.
#[cfg(target_os = "windows")]
pub type WindowsFocusedElementIdentity =
    rightkit_automation::desktop::uia_worker::FocusedElementIdentity;

/// UIA text proof (independent value / document / terminal-row readbacks).
#[cfg(target_os = "windows")]
pub type WindowsTextProof = rightkit_automation::desktop::uia_worker::TextProof;

/// True only when BOTH the identity captured before the paste and a fresh
/// probe now resolve, and they differ. A `None` on either side is not
/// evidence of movement: the probe cannot resolve Chromium/Electron virtual
/// editors or terminal grids at all (see `insert_transcript`), and it can miss
/// transiently under UIA load. Those targets are guarded by the HWND-level
/// `target_ready_for_input` check the caller has already run. Logged so a
/// real identity change stays diagnosable from the field log.
#[cfg(target_os = "windows")]
pub fn windows_identity_moved(
    host: &DeliveryHost,
    captured: Option<&WindowsFocusedElementIdentity>,
    target: &TargetSnapshot,
    stage: &str,
) -> bool {
    let Some(captured) = captured else {
        return false;
    };
    let Some(live) = windows_focused_element_identity(host, target) else {
        return false;
    };
    let moved = &live != captured;
    if moved {
        host.log("delivery_identity_moved", &format!("stage={stage}"));
    }
    moved
}

#[cfg(target_os = "windows")]
pub fn windows_focused_element_identity(
    host: &DeliveryHost,
    target: &TargetSnapshot,
) -> Option<WindowsFocusedElementIdentity> {
    windows_uia(host).focused_element_identity(&windows_native_target(target))
}

#[cfg(target_os = "windows")]
pub fn windows_text_readback_is_usable(_target: &TargetSnapshot, text: &WindowsTextProof) -> bool {
    if text.terminal_rows.is_some() {
        return text.terminal_scope_runtime_id.is_some()
            && text.terminal_scope_native_window_handle.is_some();
    }
    // VS Code's integrated terminal advertises TextPattern, but returns its
    // static accessibility label instead of terminal-buffer content.
    ![text.value.as_deref(), text.document.as_deref()]
        .into_iter()
        .flatten()
        .any(|value| {
            value.starts_with("Terminal ")
                && value.contains("Toggle Screen Reader Accessibility Mode")
        })
}

#[cfg(target_os = "windows")]
fn windows_focused_field_is_password(host: &DeliveryHost) -> Option<bool> {
    windows_uia(host).focused_field_is_password()
}

/// Secure-field state of the destination of a Windows write: the UIA password probe, or for a
/// WezTerm pane the pane's own evidence. No target has a hard-coded answer; `None` is unknown
/// and refuses every automatic write.
#[cfg(target_os = "windows")]
fn windows_secure_state(
    host: &DeliveryHost,
    stop_target: Option<&WindowsStopTarget>,
) -> Option<bool> {
    match stop_target {
        Some(stop) if stop.is_wezterm() => wezterm_secure_state(host, stop),
        _ => windows_focused_field_is_password(host),
    }
}

#[cfg(target_os = "windows")]
fn windows_paste_readback_timeout_ms(
    host: &DeliveryHost,
    target: &TargetSnapshot,
    proof: Option<&WindowsTextProof>,
) -> u64 {
    if windows_terminal_target(
        target.process_name.as_deref(),
        target.window_title.as_deref(),
        None,
    ) || proof.is_some_and(|proof| proof.terminal_rows.is_some())
    {
        host.config.terminal_paste_readback_timeout_ms
    } else {
        host.config.paste_receipt_timeout_ms
    }
}

#[cfg(target_os = "windows")]
pub fn windows_focused_text_snapshot_with_identity(
    host: &DeliveryHost,
    target: &TargetSnapshot,
    expected: &WindowsFocusedElementIdentity,
) -> Option<WindowsTextProof> {
    windows_uia(host).focused_text_with_identity(
        &windows_native_target(target),
        expected,
        std::time::Duration::from_millis(host.config.uia_text_read_ms),
    )
}

#[cfg(target_os = "windows")]
pub fn windows_stop_text_snapshot(
    host: &DeliveryHost,
    stop: &WindowsStopTarget,
) -> Option<WindowsTextProof> {
    stop.text_snapshot(std::time::Duration::from_millis(
        host.config.uia_text_read_ms,
    ))
}

#[cfg(target_os = "windows")]
pub fn windows_stop_text_snapshot_until(
    stop: &WindowsStopTarget,
    deadline: Instant,
) -> Option<WindowsTextProof> {
    stop.text_snapshot(deadline.saturating_duration_since(Instant::now()))
}

#[cfg(target_os = "windows")]
pub fn windows_focused_text_snapshot_with_identity_until(
    host: &DeliveryHost,
    target: &TargetSnapshot,
    expected: &WindowsFocusedElementIdentity,
    deadline: Instant,
) -> Option<WindowsTextProof> {
    windows_uia(host).focused_text_with_identity(
        &windows_native_target(target),
        expected,
        deadline.saturating_duration_since(Instant::now()),
    )
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn insert_transcript(
    _host: &DeliveryHost,
    _transcript: &str,
    _target: &TargetSnapshot,
    _method: &str,
    _require_editable: bool,
    _send_enter: bool,
    _steps: &mut Vec<DeliveryTimingStep>,
    _on_inserted: &mut dyn FnMut(),
) -> Result<(), CopyFallbackReason> {
    Err(CopyFallbackReason::UnsupportedPlatform)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn insert_transcript_tracked(
    _host: &DeliveryHost,
    _transcript: &str,
    _target: &TargetSnapshot,
    _method: &str,
    _require_editable: bool,
    _send_enter: bool,
    _steps: &mut Vec<DeliveryTimingStep>,
    _on_inserted: &mut dyn FnMut(),
    _clipboard: &mut FallbackClipboard,
) -> Result<(), CopyFallbackReason> {
    Err(CopyFallbackReason::UnsupportedPlatform)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn write_clipboard_text(_text: &str) -> Result<(), CopyFallbackReason> {
    Err(CopyFallbackReason::UnsupportedPlatform)
}

// The public paste and Enter keystroke helpers that used to live here were removed: a keystroke
// can only be issued with a `WritePermit` from the evidence gate, through `gated_writes`.
