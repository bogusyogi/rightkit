#[allow(unused_imports)]
use super::*;

// Delivery - OS side effects only: clipboard write, Ctrl+V / Enter SendInput,
// foreground-window snapshot, and the real backend that wires them together.
// The pure record shapes + id/time helpers live in `rightkit_transcript_library::delivery`
// (unit-tested there). `src-tauri` keeps the Windows calls.

pub use rightkit_transcript_library::delivery::*;

use std::time::Instant;

#[derive(Debug, Default)]
pub struct RealDeliveryBackend;

impl RealDeliveryBackend {
    pub fn new() -> Self {
        Self
    }
}

impl DeliveryBackend for RealDeliveryBackend {
    fn deliver(&mut self, transcript: &str) -> Result<DeliveryRecord, DeliveryError> {
        self.deliver_inner(transcript, false)
    }
}

impl RealDeliveryBackend {
    /// Like `deliver`, but presses Enter only after attributable paste readback
    /// (zephyr SEND).
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
        pin: Option<&crate::desktop::macos::FocusPin>,
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
        #[cfg(target_os = "macos")] retained_pin: Option<&crate::desktop::macos::FocusPin>,
        #[cfg(target_os = "macos")] require_retained_pin: bool,
        on_inserted: &mut dyn FnMut(),
    ) -> Result<DeliveryRecord, DeliveryError> {
        rightkit_dictation_telemetry::telemetry::utterance_timeline::mark(|t| {
            &mut t.delivery_started_ms
        });
        let started = Instant::now();
        let mut steps = Vec::new();
        let delivery_id = next_delivery_id(0);
        let target_started = Instant::now();
        #[cfg(target_os = "windows")]
        let target = stop_target
            .map(|captured| captured.snapshot().clone())
            .unwrap_or_else(current_target_snapshot);
        #[cfg(not(target_os = "windows"))]
        #[allow(unused_mut)]
        let mut target = current_target_snapshot();
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
        if stop_target.is_some_and(|captured| !validate_stop_target_for_delivery(captured)) {
            let fallback_started = Instant::now();
            let clipboard_result = write_clipboard_text(transcript);
            steps.push(DeliveryTimingStep::new(
                "stop_target_revalidation_failed",
                elapsed_ms(fallback_started),
            ));
            let total_ms = elapsed_ms(started);
            let reason = clipboard_result
                .err()
                .unwrap_or(CopyFallbackReason::FocusChanged);
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
            let (_, retries) = settle_transient_macos(|| {
                if !std::mem::take(&mut first) {
                    target = current_target_snapshot();
                }
                targets_match_for_delivery(&target, expected, require_editable)
            });
            if retries > 0 {
                rightkit_dictation_telemetry::telemetry::log_message(
                    "delivery_target_revalidation_retry",
                    &format!(
                        "retries={retries} matched={} same_pid={} same_window={} same_foreground={} live_control={} live_text_input={:?}",
                        targets_match_for_delivery(&target, expected, require_editable),
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
            !targets_match_for_delivery(&target, expected, require_editable)
        }) {
            let fallback_started = Instant::now();
            let clipboard_result = write_clipboard_text(transcript);
            steps.push(DeliveryTimingStep::new(
                "expected_target_revalidation_failed",
                elapsed_ms(fallback_started),
            ));
            let total_ms = elapsed_ms(started);
            let reason = clipboard_result
                .err()
                .unwrap_or(CopyFallbackReason::FocusChanged);
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
        let method = crate::desktop::delivery::delivery_method();
        let outcome = match insert_transcript(
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
                    // Insertion failed - guarantee the transcript is on the clipboard
                    // so it is never lost, then report the fallback. Idempotent in the
                    // paste path (already set), the real save in the keystroke path.
                    let fallback_started = Instant::now();
                    let clipboard_result = write_clipboard_text(transcript);
                    steps.push(DeliveryTimingStep::new(
                        "fallback_clipboard_write",
                        elapsed_ms(fallback_started),
                    ));
                    let reason = clipboard_result.err().unwrap_or(reason);
                    DeliveryOutcome::CopiedFallback { reason }
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

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn write_clipboard_text(text: &str) -> Result<(), CopyFallbackReason> {
    if crate::desktop::clipboard_write(text) {
        Ok(())
    } else {
        Err(CopyFallbackReason::ClipboardUnavailable)
    }
}

// Promised clipboard receipt wait. AppKit/Win32 provider callbacks are
// authoritative consumption proof; accessibility read-back is advisory only.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub const PASTE_RECEIPT_TIMEOUT_MS: u64 = 1_500;

/// Quiet period after the last clipboard receipt before the prior clipboard
/// is restored (see `paste_restore_quiet_remaining`), and the hard cap on how
/// long that wait may extend.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const PASTE_RESTORE_QUIET_CAP_MS: u64 = 1_000;

#[cfg(target_os = "macos")]
pub const PASTE_READBACK_TIMEOUT_MS: u64 = 650;
#[cfg(target_os = "macos")]
pub static PASTE_EVENT_PROBE_TOKEN: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(1);
// Named to match the macOS constant of the same meaning (was an inline `650`
// literal at the readback-poll call site).
#[cfg(target_os = "windows")]
pub const PASTE_READBACK_TIMEOUT_MS: u64 = 650;

#[cfg(target_os = "windows")]
const TERMINAL_PASTE_READBACK_TIMEOUT_MS: u64 = 2_500;

/// Wall-clock cap for every pre-paste WezTerm probe (pane check + baseline
/// read, run concurrently). Ctrl+V goes out within about this long of delivery
/// start; on exhaustion the paste proceeds without the unfinished piece.
#[cfg(target_os = "windows")]
const WEZTERM_PREPASTE_BUDGET_MS: u64 = 150;

/// A terminal with no readback baseline can only be confirmed by the clipboard
/// receipt. Wait this long for it, then report Pasted-with-unknown instead of
/// holding the pill for the whole terminal readback deadline.
#[cfg(target_os = "windows")]
const TERMINAL_NO_BASELINE_RECEIPT_GRACE_MS: u64 = 300;

/// Per-poll budget for post-paste readback. A `wezterm cli get-text` spawn
/// takes ~145 ms on the field Dell, so the 100 ms UIA budget timed out every
/// terminal poll (18 polls, none ever completing).
#[cfg(target_os = "windows")]
const TERMINAL_READBACK_POLL_MS: u64 = 400;

/// Bound on waiting for readback to show the pasted text before Return.
/// Browser editors (Edge field case) insert after the clipboard read.
#[cfg(any(target_os = "windows", target_os = "macos"))]
const ENTER_INSERTION_SETTLE_MS: u64 = 400;

/// Settle before Return when the editor has no readback channel.
#[cfg(any(target_os = "windows", target_os = "macos"))]
const ENTER_BLIND_SETTLE_MS: u64 = 60;

/// Clipboard restore that runs after the Pasted state was reported. The next
/// delivery waits on this (bounded) so it never snapshots the transcript our
/// own pending restore is about to replace.
#[cfg(any(target_os = "windows", target_os = "macos"))]
static CLIPBOARD_RESTORE_PENDING: (std::sync::Mutex<bool>, std::sync::Condvar) =
    (std::sync::Mutex::new(false), std::sync::Condvar::new());

/// Set by the next delivery so a pending restore that is only waiting for a LATE
/// receipt (unknown terminal paste) gives up at once instead of holding that
/// delivery for up to 1.5 s. A restore already inside its quiet window is not
/// interrupted (bounded by `PASTE_RESTORE_QUIET_CAP_MS`).
#[cfg(any(target_os = "windows", target_os = "macos"))]
static CLIPBOARD_RESTORE_SUPERSEDED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Wait (bounded) for the previous delivery's background restore; returns the
/// time spent so it can be recorded as a delivery step.
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn supersede_pending_clipboard_restore(max_wait: std::time::Duration) -> u64 {
    let started = Instant::now();
    CLIPBOARD_RESTORE_SUPERSEDED.store(true, std::sync::atomic::Ordering::SeqCst);
    wait_for_clipboard_restore_idle(max_wait);
    CLIPBOARD_RESTORE_SUPERSEDED.store(false, std::sync::atomic::Ordering::SeqCst);
    elapsed_ms(started)
}

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

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn set_clipboard_restore_pending(pending: bool) {
    if let Ok(mut flag) = CLIPBOARD_RESTORE_PENDING.0.lock() {
        *flag = pending;
    }
    CLIPBOARD_RESTORE_PENDING.1.notify_all();
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn wait_for_clipboard_restore_idle(max_wait: std::time::Duration) {
    let deadline = Instant::now() + max_wait;
    let Ok(mut flag) = CLIPBOARD_RESTORE_PENDING.0.lock() else {
        return;
    };
    while *flag {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        match CLIPBOARD_RESTORE_PENDING.1.wait_timeout(flag, remaining) {
            Ok((next, _)) => flag = next,
            Err(_) => return,
        }
    }
}

#[cfg(target_os = "macos")]
pub const SECURE_RETRY_INTERVAL_MS: u64 = 40;

#[cfg(target_os = "macos")]
pub fn focused_field_security_with_retry_macos(pid: i32) -> Option<bool> {
    let deadline = Instant::now() + std::time::Duration::from_millis(240);
    loop {
        // `focused_field_is_secure_for_pid` has a compatibility window
        // fallback for context callers. Require an independently observed
        // focused editable element here so a window fallback can never turn
        // into a fail-open `Some(false)` delivery decision.
        if crate::desktop::input_macos::focused_text_input() != Some(true) {
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(SECURE_RETRY_INTERVAL_MS));
            continue;
        }
        if let Some(secure) = crate::desktop::macos::focused_field_is_secure_for_pid(pid) {
            return Some(secure);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(SECURE_RETRY_INTERVAL_MS));
    }
}

/// Re-validate that the focused window hasn't changed during paste readback.
/// The user can switch apps while we poll — Enter must go to the
/// same target where the text was pasted, not whatever is focused now.
pub fn target_ready_for_input(original: &TargetSnapshot, require_editable: bool) -> bool {
    #[cfg(target_os = "windows")]
    let live = current_target_snapshot_for_revalidation(original);
    #[cfg(not(target_os = "windows"))]
    let live = current_target_snapshot();
    let ready = targets_match_for_delivery(&live, original, require_editable);
    if !ready {
        rightkit_dictation_telemetry::telemetry::log_message(
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
fn settle_transient_macos(mut check: impl FnMut() -> bool) -> (bool, u8) {
    let mut retries = 0u8;
    loop {
        if check() {
            return (true, retries);
        }
        if retries == 3 {
            return (false, retries);
        }
        std::thread::sleep(std::time::Duration::from_millis(60));
        retries += 1;
    }
}

#[cfg(target_os = "macos")]
fn log_settle(check: &str, ok: bool, retries: u8) {
    if retries > 0 {
        rightkit_dictation_telemetry::telemetry::log_message(
            "delivery_check_settled",
            &format!("check={check} ok={ok} retries={retries}"),
        );
    }
}

/// Final gate evaluated immediately before any synthetic paste keystroke
/// (issue #148): the focused target must still equal the bound target and
/// must not be positively secure. Any failure aborts to clipboard fallback.
/// Unknown secure readability is not a veto (spec 3.3); positive evidence is.
pub fn paste_dispatch_gate(
    target_bound_and_ready: bool,
    secure_state: Option<bool>,
) -> Result<(), CopyFallbackReason> {
    if secure_state == Some(true) {
        return Err(CopyFallbackReason::Other("secure_target".to_string()));
    }
    if !target_bound_and_ready {
        return Err(CopyFallbackReason::FocusChanged);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn macos_target_ready_for_input(
    target: &TargetSnapshot,
    require_editable: bool,
    pin: Option<&crate::desktop::macos::FocusPin>,
) -> bool {
    pin.map_or_else(
        || target_ready_for_input(target, require_editable),
        |pin| {
            target.process_id == Some(pin.pid() as u32)
                && (!require_editable
                    || crate::desktop::input_macos::focused_text_input() == Some(true))
                && current_target_snapshot_for_stop().is_some_and(|live| {
                    live.process_id == target.process_id
                        && live.window_handle.is_some()
                        && live.window_handle == target.window_handle
                })
                && crate::desktop::macos::stop_focus_pin_is_current(pin)
        },
    )
}

#[cfg(target_os = "windows")]
pub fn windows_target_ready_for_input(
    target: &TargetSnapshot,
    require_editable: bool,
    stop_target: Option<&WindowsStopTarget>,
) -> bool {
    stop_target.map_or_else(
        || target_ready_for_input(target, require_editable),
        validate_stop_target_for_delivery,
    )
}

pub fn targets_match_for_delivery(
    current: &TargetSnapshot,
    expected: &TargetSnapshot,
    require_editable: bool,
) -> bool {
    target_matches_expected(current, expected, require_editable)
        || macos_window_target_matches(current, expected, require_editable)
        || windows_opaque_target_matches(current, expected)
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
pub fn windows_opaque_target_matches(current: &TargetSnapshot, expected: &TargetSnapshot) -> bool {
    // WhatsApp Desktop's WinUI/WebView host reports a different transient
    // focused-child HWND on consecutive probes and reports the composer as
    // non-editable. Bind delivery to its stable process + top-level window;
    // the promised-clipboard receipt remains the mutation gate.
    windows_opaque_target(current)
        && windows_opaque_target(expected)
        && current.process_id == expected.process_id
        && current.window_handle.is_some()
        && current.window_handle == expected.window_handle
        && current.foreground_target == expected.foreground_target
        && current.focused_control_handle.is_some()
}

#[cfg(target_os = "windows")]
pub fn windows_opaque_target(target: &TargetSnapshot) -> bool {
    target
        .process_name
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case("WhatsApp.Root.exe"))
}

#[cfg(not(target_os = "windows"))]
pub fn windows_opaque_target_matches(_: &TargetSnapshot, _: &TargetSnapshot) -> bool {
    false
}

/// Insert `transcript` into the focused app. `method` is "paste" (clipboard + ⌘V,
/// default) or "keystroke" (synthetic unicode typing, no clipboard touch). When
/// `send_enter` is set, presses Return only after attributable readback (zephyr SEND).
#[cfg(target_os = "macos")]
pub fn insert_transcript(
    transcript: &str,
    target: &TargetSnapshot,
    method: &str,
    require_editable: bool,
    send_enter: bool,
    retained_pin: Option<&crate::desktop::macos::FocusPin>,
    require_retained_pin: bool,
    steps: &mut Vec<DeliveryTimingStep>,
    on_inserted: &mut dyn FnMut(),
) -> Result<(), CopyFallbackReason> {
    use std::time::Duration;

    // The previous delivery's background clipboard restore must finish before
    // this one snapshots the pasteboard (bounded; normally already idle).
    let restore_wait_ms = supersede_pending_clipboard_restore(Duration::from_millis(1_500));
    steps.push(DeliveryTimingStep::new(
        "clipboard_restore_wait",
        restore_wait_ms,
    ));

    if require_retained_pin {
        let Some(pin) = retained_pin else {
            return Err(CopyFallbackReason::FocusChanged);
        };
        let (current, retries) =
            settle_transient_macos(|| crate::desktop::macos::stop_focus_pin_is_current(pin));
        log_settle("retained_pin_current", current, retries);
        if !current {
            return Err(CopyFallbackReason::FocusChanged);
        }
    }

    let access_started = Instant::now();
    if !crate::desktop::input_macos::accessibility_trusted(false) {
        rightkit_dictation_telemetry::telemetry::log_message(
            "delivery_accessibility_denied",
            "AXIsProcessTrusted() returned false; delivery falls back to clipboard",
        );
        return Err(CopyFallbackReason::PasteFailed);
    }
    steps.push(DeliveryTimingStep::new(
        "accessibility_check",
        elapsed_ms(access_started),
    ));

    // Never post Cmd+V into a password field. Native insertion already rejects
    // secure controls, but clipboard fallback must enforce same boundary.
    // Caller leaves transcript recoverable on clipboard as copied fallback.
    let secure_state = retained_pin.map_or_else(
        || {
            target
                .process_id
                .and_then(|pid| focused_field_security_with_retry_macos(pid as i32))
        },
        crate::desktop::macos::focused_field_is_secure_for_pin,
    );
    // Positive secure-field evidence is the only security veto (spec 3.3):
    // unknown readability never refuses an otherwise valid ordinary paste.
    if secure_state == Some(true) {
        return Err(CopyFallbackReason::Other("secure_target".to_string()));
    }

    if method == "keystroke" {
        // Synthetic typing has no attributable read-back contract. Retire it
        // to passive clipboard fallback instead of reporting or submitting an
        // unproven insertion.
        Err(CopyFallbackReason::Other(
            "synthetic_insertion_unverifiable".to_string(),
        ))
    } else {
        // Single dispatch-only path (spec section 4): ordinary dictation
        // always delivers through ONE clipboard + Cmd+V sequence. No native
        // AX write precedes it. Read-only AX evidence below may accelerate
        // restore/send decisions, but it never owns insertion.
        let snapshot_started = Instant::now();
        let prior = crate::desktop::macos::snapshot_general_pasteboard()
            .ok_or(CopyFallbackReason::ClipboardUnavailable)?;
        steps.push(DeliveryTimingStep::new(
            "clipboard_snapshot",
            elapsed_ms(snapshot_started),
        ));
        let stage_started = Instant::now();
        let mut promised = match crate::desktop::macos::stage_promised_clipboard_text(
            transcript,
            prior.change_count(),
        ) {
            Ok(promised) => promised,
            Err(failed_change_count) => {
                if let Some(change_count) = failed_change_count {
                    let _ = crate::desktop::macos::restore_general_pasteboard_if_unchanged(
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
        let (ready, retries) = settle_transient_macos(|| {
            macos_target_ready_for_input(target, require_editable, retained_pin)
        });
        log_settle("ready_before_secure_check", ready, retries);
        if !ready {
            let _ = promised.restore_if_unchanged(&prior);
            return Err(CopyFallbackReason::FocusChanged);
        }
        let secure_started = Instant::now();
        let secure_state = retained_pin.map_or_else(
            || {
                target
                    .process_id
                    .and_then(|pid| focused_field_security_with_retry_macos(pid as i32))
            },
            crate::desktop::macos::focused_field_is_secure_for_pin,
        );
        steps.push(DeliveryTimingStep::new(
            "secure_target_revalidation",
            elapsed_ms(secure_started),
        ));
        // Positive secure-field evidence is the only security veto (spec 3.3);
        // unknown readability never refuses an otherwise valid ordinary paste.
        if secure_state == Some(true) {
            let _ = promised.restore_if_unchanged(&prior);
            return Err(CopyFallbackReason::Other("secure_target".to_string()));
        }
        let (ready, retries) = settle_transient_macos(|| {
            macos_target_ready_for_input(target, require_editable, retained_pin)
        });
        log_settle("ready_after_secure_check", ready, retries);
        if !ready {
            let _ = promised.restore_if_unchanged(&prior);
            return Err(CopyFallbackReason::FocusChanged);
        }

        // Readable AX targets must prove an attributable text mutation. A
        // blind proof (terminal grids and opaque web editors) cannot authorize
        // delivery through this retained stop path, so recovery keeps transcript
        // available without targeting another field.
        let paste_proof = retained_pin
            .map(crate::desktop::macos::focused_field_proof_for_pin)
            .or_else(|| {
                (!require_retained_pin).then(|| {
                    target
                        .process_id
                        .map(|pid| crate::desktop::macos::focused_field_proof_for_pid(pid as i32))
                        .unwrap_or_default()
                })
            });
        let readback_required = paste_proof.as_ref().is_some_and(|proof| !proof.is_blind());
        rightkit_dictation_telemetry::telemetry::log_message(
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
        if !macos_target_ready_for_input(target, require_editable, retained_pin) {
            let _ = promised.restore_if_unchanged(&prior);
            return Err(CopyFallbackReason::FocusChanged);
        }

        let (paste_event_probe, paste_event_token) = {
            use std::sync::atomic::Ordering;
            let probe_started = Instant::now();
            let token = PASTE_EVENT_PROBE_TOKEN.fetch_add(1, Ordering::Relaxed);
            let probe = target.process_id.and_then(|pid| {
                crate::desktop::paste_event_probe_macos::PasteEventProbe::start(
                    pid as i32,
                    token,
                    Duration::from_millis(PASTE_RECEIPT_TIMEOUT_MS + 250),
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
        // Probe setup is debug-only and may take time. Revalidate destination
        // after it so instrumentation cannot weaken normal focus continuity.
        // Final bound-target + non-secure check immediately before Cmd+V.
        let final_secure = retained_pin.map_or_else(
            || {
                target
                    .process_id
                    .and_then(|pid| focused_field_security_with_retry_macos(pid as i32))
            },
            crate::desktop::macos::focused_field_is_secure_for_pin,
        );
        if let Err(reason) = paste_dispatch_gate(
            macos_target_ready_for_input(target, require_editable, retained_pin),
            final_secure,
        ) {
            let _ = promised.restore_if_unchanged(&prior);
            return Err(reason);
        }

        // Mark immediately before Cmd+V. The provider ignores all reads before
        // this boundary, while reads during CGEvent posting remain valid
        // target-consumption evidence.
        let paste_started = Instant::now();
        promised.mark_posted();
        let paste_posted = crate::desktop::input_macos::paste_combo_tagged(paste_event_token);
        if !paste_posted {
            let _ = promised.restore_if_unchanged(&prior);
            return Err(CopyFallbackReason::PasteFailed);
        }
        rightkit_dictation_telemetry::telemetry::utterance_timeline::mark(|t| {
            &mut t.paste_issued_ms
        });
        steps.push(DeliveryTimingStep::new(
            "paste_keystroke",
            elapsed_ms(paste_started),
        ));
        let receipt_started = Instant::now();
        let receipt = promised.wait_for_receipt(Duration::from_millis(PASTE_RECEIPT_TIMEOUT_MS));
        let observation = paste_event_probe.map(|probe| probe.finish());
        let down_seen = observation.as_ref().is_some_and(|result| {
            result.events.iter().any(|event| {
                matches!(
                    event.kind,
                    crate::desktop::paste_event_probe_macos::PasteEventKind::Down
                )
            })
        });
        let up_seen = observation.as_ref().is_some_and(|result| {
            result.events.iter().any(|event| {
                matches!(
                    event.kind,
                    crate::desktop::paste_event_probe_macos::PasteEventKind::Up
                )
            })
        });
        let target_event_confirmed = down_seen && up_seen;
        let event_count = observation.as_ref().map_or(0, |result| result.events.len());
        rightkit_dictation_telemetry::telemetry::log_message(
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
        rightkit_dictation_telemetry::telemetry::log_message(
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
            let deadline = Instant::now() + Duration::from_millis(PASTE_READBACK_TIMEOUT_MS);
            loop {
                readback_attempts = readback_attempts.saturating_add(1);
                let confirmed = if let Some(pin) = retained_pin {
                    let confirmation = crate::desktop::macos::insertion_confirmation_for_pin(
                        pin, proof, transcript,
                    );
                    if confirmation.confirmed() {
                        readback_confirmation_path = confirmation.label();
                    }
                    confirmation.confirmed()
                } else {
                    let confirmed = crate::desktop::macos::insertion_confirmed_for_pid(
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
                std::thread::sleep(Duration::from_millis(5));
            }
        } else {
            false
        };
        let destination_current = retained_pin.is_some()
            && paste_proof
                .as_ref()
                .is_some_and(crate::desktop::macos::proof_destination_is_current);
        rightkit_dictation_telemetry::telemetry::log_message(
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
                    let settle_deadline =
                        settle_started + Duration::from_millis(ENTER_INSERTION_SETTLE_MS);
                    loop {
                        let confirmed = match retained_pin {
                            Some(pin) => crate::desktop::macos::insertion_confirmation_for_pin(
                                pin, proof, transcript,
                            )
                            .confirmed(),
                            None => crate::desktop::macos::insertion_confirmed_for_pid(
                                pid as i32, proof, transcript,
                            ),
                        };
                        if confirmed {
                            break true;
                        }
                        if Instant::now() >= settle_deadline {
                            break false;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(ENTER_BLIND_SETTLE_MS));
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
            let secure_started = Instant::now();
            let secure_state = retained_pin.map_or_else(
                || {
                    target
                        .process_id
                        .and_then(|pid| focused_field_security_with_retry_macos(pid as i32))
                },
                crate::desktop::macos::focused_field_is_secure_for_pin,
            );
            steps.push(DeliveryTimingStep::new(
                "secure_target_revalidation_before_enter",
                elapsed_ms(secure_started),
            ));
            let destination_current = paste_proof
                .as_ref()
                .is_some_and(crate::desktop::macos::proof_destination_is_current);
            if destination_current && secure_state == Some(false) {
                if crate::desktop::input_macos::press_return() {
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
        // next delivery waits for this thread (`supersede_pending_clipboard_restore`).
        set_clipboard_restore_pending(true);
        let job = AssertSend((promised, prior));
        let restore_thread = std::thread::Builder::new()
            .name("delivery-clipboard-restore".to_string())
            .spawn(move || {
                let (mut promised, prior) = job.into_inner();
                // Quiet window: never restore while the target may still be reading the
                // promised data (200 ms after the last receipt, capped at 1 s).
                let quiet_started = Instant::now();
                let cap = quiet_started + Duration::from_millis(PASTE_RESTORE_QUIET_CAP_MS);
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
                let restored = promised.restore_if_unchanged(&prior);
                rightkit_dictation_telemetry::telemetry::log_message(
                    "delivery_clipboard_restore_background",
                    &format!(
                        "clipboard_restore_quiet_window_ms={quiet_window_ms} clipboard_restore_after_receipt={restored} restore_ms={}",
                        elapsed_ms(restore_started)
                    ),
                );
                set_clipboard_restore_pending(false);
            });
        if restore_thread.is_err() {
            set_clipboard_restore_pending(false);
            rightkit_dictation_telemetry::telemetry::log_message(
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
    transcript: &str,
    target: &TargetSnapshot,
    _method: &str,
    require_editable: bool,
    send_enter: bool,
    stop_target: Option<&WindowsStopTarget>,
    steps: &mut Vec<DeliveryTimingStep>,
    on_inserted: &mut dyn FnMut(),
) -> Result<(), CopyFallbackReason> {
    use std::time::Duration;

    // A previous delivery's background clipboard restore must finish before
    // this one snapshots the clipboard (bounded; normally already idle).
    let restore_wait_ms = supersede_pending_clipboard_restore(Duration::from_millis(1_500));
    steps.push(DeliveryTimingStep::new(
        "clipboard_restore_wait",
        restore_wait_ms,
    ));
    // A terminal grid is never a password field and UIA on it is slow (hundreds
    // of ms per probe on WezTerm); positive secure evidence stays the only veto
    // for every other target.
    let skip_secure_probe = stop_target.is_some_and(WindowsStopTarget::is_wezterm);
    let password_state = if skip_secure_probe {
        Some(false)
    } else {
        windows_focused_field_is_password()
    };
    // Positive password evidence is the only security veto (spec 3.3):
    // unknown readability never refuses an otherwise valid ordinary paste.
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
    let prior = crate::desktop::windows::snapshot_general_clipboard()
        .ok_or(CopyFallbackReason::ClipboardUnavailable)?;
    if !prior.is_lossless() {
        rightkit_dictation_telemetry::telemetry::log_message(
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
    let mut promised = match crate::desktop::windows::stage_promised_clipboard_text(
        transcript,
        prior.sequence(),
    ) {
        Ok(promised) => promised,
        Err(restore_sequence) => {
            if let Some(restore_sequence) = restore_sequence {
                let _ = crate::desktop::windows::restore_general_clipboard_if_unchanged(
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
    if !windows_target_ready_for_input(target, require_editable, stop_target) {
        let _ = promised.restore_if_unchanged(prior);
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
    } else if windows_opaque_target(target) {
        None
    } else {
        windows_focused_element_identity(target)
    };
    if paste_identity.is_none() {
        rightkit_dictation_telemetry::telemetry::log_message(
            "delivery_identity_probe_unavailable",
            "stage=pre_paste",
        );
    }
    let secure_started = Instant::now();
    let secure_state = if skip_secure_probe {
        Some(false)
    } else {
        windows_focused_field_is_password()
    };
    steps.push(DeliveryTimingStep::new(
        "secure_target_revalidation",
        elapsed_ms(secure_started),
    ));
    // Positive secure-field evidence is the only security veto (spec 3.3);
    // unknown readability never refuses an otherwise valid ordinary paste.
    if secure_state == Some(true) {
        let _ = promised.restore_if_unchanged(prior);
        return Err(CopyFallbackReason::Other("secure_target".to_string()));
    }
    // Security probing itself can yield to a focus change. Re-check the HWND
    // target, and the UIA identity where the probe produced one, immediately
    // before marking/posting Ctrl+V.
    if !windows_target_ready_for_input(target, require_editable, stop_target) {
        let _ = promised.restore_if_unchanged(prior);
        return Err(CopyFallbackReason::FocusChanged);
    }
    if windows_identity_moved(paste_identity.as_ref(), target, "pre_paste") {
        let _ = promised.restore_if_unchanged(prior);
        return Err(CopyFallbackReason::FocusChanged);
    }
    // Capture proof immediately before Ctrl+V, after all focus/security
    // revalidation. Stop capture retains identity only; no stale stop-time
    // text may be paired with post-paste text. Retained UIA elements are read
    // on the persistent MTA even if current focus churns during the call.
    let baseline_started = Instant::now();
    let readback_before = if let Some(stop) = stop_target {
        if let Some((preflight, baseline)) =
            stop.wezterm_prepaste_probe(Duration::from_millis(WEZTERM_PREPASTE_BUDGET_MS))
        {
            // One concurrent, wall-clock-capped probe replaces the per-gate
            // pane validation and the five-spawn baseline read. A definitive
            // pane mismatch refuses the paste; an exhausted budget proceeds
            // (HWND, pid and process identity were just validated) and a
            // missing baseline only removes the readback channel.
            if preflight == crate::desktop::wezterm_win::PanePreflight::Moved {
                let _ = promised.restore_if_unchanged(prior);
                return Err(CopyFallbackReason::FocusChanged);
            }
            rightkit_dictation_telemetry::telemetry::log_message(
                "delivery_wezterm_prepaste_probe",
                &format!("pane={preflight:?} baseline_present={}", baseline.is_some()),
            );
            baseline
        } else {
            windows_stop_text_snapshot(stop)
        }
    } else {
        paste_identity
            .as_ref()
            .and_then(|identity| windows_focused_text_snapshot_with_identity(target, identity))
    }
    .filter(|text| windows_text_readback_is_usable(target, text));
    steps.push(DeliveryTimingStep::new(
        "prepaste_baseline_probe",
        elapsed_ms(baseline_started),
    ));
    // UIA proof reads may consume their bounded worker budget. Revalidate the
    // retained editor immediately before dispatch so focus churn cannot turn
    // this proof into a paste sent to a different field.
    let final_secure = if skip_secure_probe {
        Some(false)
    } else {
        windows_focused_field_is_password()
    };
    if let Err(reason) = paste_dispatch_gate(
        windows_target_ready_for_input(target, require_editable, stop_target)
            && !windows_identity_moved(paste_identity.as_ref(), target, "pre_paste_dispatch"),
        final_secure,
    ) {
        let _ = promised.restore_if_unchanged(prior);
        return Err(reason);
    }
    let paste_started = Instant::now();
    promised.mark_posted();
    // The hotkey's physical Ctrl/Shift may still be down this early after Stop; the chord waits
    // (bounded) for them to lift so the target never sees a bare "V". Surface that wait.
    let Ok(chord) = send_paste_keystroke() else {
        return Err(CopyFallbackReason::PasteFailed);
    };
    rightkit_dictation_telemetry::telemetry::utterance_timeline::mark(|t| &mut t.paste_issued_ms);
    if chord.waited_ms > 0 || chord.forced_release {
        steps.push(DeliveryTimingStep::new(
            if chord.forced_release {
                "paste_modifier_forced_release"
            } else {
                "paste_modifier_settle"
            },
            u64::from(chord.waited_ms),
        ));
    }
    steps.push(DeliveryTimingStep::new(
        "paste_keystroke",
        elapsed_ms(paste_started),
    ));

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
            TERMINAL_NO_BASELINE_RECEIPT_GRACE_MS
        } else {
            windows_paste_readback_timeout_ms(target, readback_before.as_ref())
        });
    let poll_budget_ms = if terminal_target {
        TERMINAL_READBACK_POLL_MS
    } else {
        100
    };
    let mut receipt = promised.wait_for_receipt(Duration::from_millis(5));
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
                windows_focused_text_snapshot_with_identity_until(target, identity, poll_deadline)
            } else {
                None
            };
            if after.is_some_and(|after| before.confirms(&after, transcript)) {
                readback_confirmed = true;
                break;
            }
        }
        if Instant::now() >= race_deadline {
            break;
        }
        receipt = promised.wait_for_receipt(Duration::from_millis(5));
        if !receipt.observed() && receipt.ownership_lost {
            // wait_for_receipt returns at once after ownership loss; do not spin.
            std::thread::sleep(Duration::from_millis(5));
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
    rightkit_dictation_telemetry::telemetry::log_message(
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
            // once (no clip card, no pill hold); Enter and the clipboard
            // restore stay off because both need proven consumption.
            paste_unknown = true;
            steps.push(DeliveryTimingStep::new(
                "paste_unconfirmed_terminal_no_channel",
                elapsed_ms(readback_started),
            ));
            rightkit_dictation_telemetry::telemetry::log_message(
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
                    settle_started + Duration::from_millis(ENTER_INSERTION_SETTLE_MS);
                let mut landed = false;
                while Instant::now() < settle_deadline {
                    let poll_deadline =
                        (Instant::now() + Duration::from_millis(50)).min(settle_deadline);
                    let after = if let Some(stop) = stop_target {
                        windows_stop_text_snapshot_until(stop, poll_deadline)
                    } else if let Some(identity) = paste_identity.as_ref() {
                        windows_focused_text_snapshot_with_identity_until(
                            target,
                            identity,
                            poll_deadline,
                        )
                    } else {
                        None
                    };
                    if after.is_some_and(|after| before.confirms(&after, transcript)) {
                        landed = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                landed
            } else {
                std::thread::sleep(Duration::from_millis(ENTER_BLIND_SETTLE_MS));
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
            let secure_before_enter = if skip_secure_probe {
                Some(false)
            } else {
                windows_focused_field_is_password()
            };
            if !windows_target_ready_for_input(target, require_editable, stop_target)
                || secure_before_enter != Some(false)
                || windows_identity_moved(paste_identity.as_ref(), target, "pre_enter")
            {
                steps.push(DeliveryTimingStep::new(
                    "paste_readback_target_changed_before_enter",
                    elapsed_ms(submit_started),
                ));
                on_inserted();
            } else {
                on_inserted();
                if send_enter_keystroke().is_err() {
                    submit_failed = true;
                    steps.push(DeliveryTimingStep::new(
                        "paste_receipt_submit_failed",
                        elapsed_ms(submit_started),
                    ));
                } else {
                    steps.push(DeliveryTimingStep::new(
                        "paste_settle_and_submit",
                        elapsed_ms(submit_started),
                    ));
                }
            }
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
    set_clipboard_restore_pending(true);
    let restore_thread = std::thread::Builder::new()
        .name("delivery-clipboard-restore".to_string())
        .spawn(move || {
            // Unknown terminal paste: restore only if a LATE receipt proves the
            // terminal eventually read the transcript; otherwise leave it
            // staged rather than hand the terminal the stale prior contents.
            let mut receipt_seen = receipt_seen;
            if await_late_receipt && !receipt_seen {
                let late_deadline = Instant::now() + Duration::from_millis(5_000);
                while Instant::now() < late_deadline
                    && !CLIPBOARD_RESTORE_SUPERSEDED.load(std::sync::atomic::Ordering::SeqCst)
                {
                    let late = promised.wait_for_receipt(Duration::from_millis(50));
                    if late.observed() {
                        receipt_seen = true;
                        break;
                    }
                    if late.ownership_lost {
                        break;
                    }
                }
                if !receipt_seen {
                    rightkit_dictation_telemetry::telemetry::log_message(
                        "delivery_clipboard_restore_skipped",
                        "reason=unknown_terminal_paste_no_late_receipt transcript_left_on_clipboard=true",
                    );
                    set_clipboard_restore_pending(false);
                    return;
                }
            }
            // Quiet window: never restore while the target may still be reading the
            // promised data (200 ms after the last receipt, capped at 1 s).
            let mut quiet_window_ms = 0_u64;
            if receipt_seen {
                let quiet_started = Instant::now();
                let cap = quiet_started + Duration::from_millis(PASTE_RESTORE_QUIET_CAP_MS);
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
            let restored = promised.restore_if_unchanged(prior);
            rightkit_dictation_telemetry::telemetry::log_message(
                "delivery_clipboard_restore_background",
                &format!(
                    "clipboard_restore_quiet_window_ms={quiet_window_ms} clipboard_restore_after_receipt={restored} restore_ms={}",
                    elapsed_ms(restore_started)
                ),
            );
            set_clipboard_restore_pending(false);
        });
    if restore_thread.is_err() {
        // The closure (and with it the staged promise) is gone; the transcript
        // stays on the clipboard. Release the gate so deliveries never stall.
        set_clipboard_restore_pending(false);
        rightkit_dictation_telemetry::telemetry::log_message(
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
// must stay a single source of truth shared with the command-host focus module
// rather than hand-copied in each crate. Both crates depend on the transcript library, so
// the allowlist lives once in `rightkit_transcript_library::delivery` and both call
// sites use it via the `pub use rightkit_transcript_library::delivery::*;` re-export at
// the top of this file.

// Do not delete these: their call sites in `insert_transcript` (Windows) and
// `windows_uia.rs` are Windows-only, so removing these types breaks the
// Windows build without failing the macOS gate.
/// Identity of UIA element currently owning keyboard focus. RuntimeId binds
/// virtual Chromium controls that share one host HWND; process + native HWND
/// additionally tie it to captured app/focus identity.
#[cfg(target_os = "windows")]
pub type WindowsFocusedElementIdentity = crate::desktop::uia_win::FocusedElementIdentity;

/// True only when BOTH the identity captured before the paste and a fresh
/// probe now resolve, and they differ. A `None` on either side is not
/// evidence of movement: the probe cannot resolve Chromium/Electron virtual
/// editors or terminal grids at all (see `insert_transcript`), and it can miss
/// transiently under UIA load. Those targets are guarded by the HWND-level
/// `target_ready_for_input` check the caller has already run. Logged so a
/// real identity change stays diagnosable from the field log.
#[cfg(target_os = "windows")]
pub fn windows_identity_moved(
    captured: Option<&WindowsFocusedElementIdentity>,
    target: &TargetSnapshot,
    stage: &str,
) -> bool {
    let Some(captured) = captured else {
        return false;
    };
    let Some(live) = windows_focused_element_identity(target) else {
        return false;
    };
    let moved = &live != captured;
    if moved {
        rightkit_dictation_telemetry::telemetry::log_message(
            "delivery_identity_moved",
            &format!("stage={stage}"),
        );
    }
    moved
}

#[cfg(target_os = "windows")]
pub fn windows_focused_element_identity(
    target: &TargetSnapshot,
) -> Option<WindowsFocusedElementIdentity> {
    crate::desktop::uia_win::focused_element_identity(target)
}

#[cfg(target_os = "windows")]
pub fn windows_text_readback_is_usable(
    _target: &TargetSnapshot,
    text: &crate::desktop::uia_win::TextProof,
) -> bool {
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
fn windows_focused_field_is_password() -> Option<bool> {
    crate::desktop::uia_win::focused_field_is_password()
}

#[cfg(target_os = "windows")]
fn windows_paste_readback_timeout_ms(
    target: &TargetSnapshot,
    proof: Option<&crate::desktop::uia_win::TextProof>,
) -> u64 {
    if windows_terminal_target(
        target.process_name.as_deref(),
        target.window_title.as_deref(),
        None,
    ) || proof.is_some_and(|proof| proof.terminal_rows.is_some())
    {
        TERMINAL_PASTE_READBACK_TIMEOUT_MS
    } else {
        PASTE_RECEIPT_TIMEOUT_MS
    }
}

#[cfg(target_os = "windows")]
pub fn windows_focused_text_snapshot_with_identity(
    target: &TargetSnapshot,
    expected: &WindowsFocusedElementIdentity,
) -> Option<crate::desktop::uia_win::TextProof> {
    crate::desktop::uia_win::focused_text_with_identity(
        target,
        expected,
        std::time::Duration::from_millis(600),
    )
}

#[cfg(target_os = "windows")]
pub fn windows_stop_text_snapshot(
    stop: &WindowsStopTarget,
) -> Option<crate::desktop::uia_win::TextProof> {
    stop.text_snapshot(std::time::Duration::from_millis(600))
}

#[cfg(target_os = "windows")]
pub fn windows_stop_text_snapshot_until(
    stop: &WindowsStopTarget,
    deadline: Instant,
) -> Option<crate::desktop::uia_win::TextProof> {
    stop.text_snapshot(deadline.saturating_duration_since(Instant::now()))
}

#[cfg(target_os = "windows")]
pub fn windows_focused_text_snapshot_with_identity_until(
    target: &TargetSnapshot,
    expected: &WindowsFocusedElementIdentity,
    deadline: Instant,
) -> Option<crate::desktop::uia_win::TextProof> {
    crate::desktop::uia_win::focused_text_with_identity(
        target,
        expected,
        deadline.saturating_duration_since(Instant::now()),
    )
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn insert_transcript(
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
pub fn write_clipboard_text(_text: &str) -> Result<(), CopyFallbackReason> {
    Err(CopyFallbackReason::UnsupportedPlatform)
}

/// Ctrl+V after the stop hotkey. Waits (bounded) for the user's physical modifiers to lift
/// before injecting, so the target never receives a bare "V"; the outcome carries that wait.
#[cfg(target_os = "windows")]
pub fn send_paste_keystroke() -> Result<crate::desktop::windows::PasteChordOutcome, DeliveryError> {
    let chord = crate::desktop::windows::send_paste_chord_after_hotkey();
    if chord.sent {
        Ok(chord)
    } else {
        Err(DeliveryError::new(
            "E_SENDINPUT",
            "SendInput did not send Ctrl+V",
        ))
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn send_paste_keystroke() -> Result<(), DeliveryError> {
    Err(DeliveryError::new(
        "E_UNSUPPORTED",
        "paste keystroke is unsupported on this platform",
    ))
}

#[cfg(target_os = "windows")]
pub fn send_enter_keystroke() -> Result<(), DeliveryError> {
    if crate::desktop::windows::send_enter_keystroke() {
        Ok(())
    } else {
        Err(DeliveryError::new(
            "E_SENDINPUT",
            "SendInput did not send Enter",
        ))
    }
}

#[cfg(test)]
mod paste_dispatch_gate_tests {
    use super::*;

    #[test]
    fn dispatch_requires_bound_target_and_no_positive_secure_evidence() {
        assert_eq!(paste_dispatch_gate(true, Some(false)), Ok(()));
        assert_eq!(paste_dispatch_gate(true, None), Ok(()));
        assert_eq!(
            paste_dispatch_gate(false, Some(false)),
            Err(CopyFallbackReason::FocusChanged)
        );
        assert_eq!(
            paste_dispatch_gate(true, Some(true)),
            Err(CopyFallbackReason::Other("secure_target".to_string()))
        );
        // Secure evidence outranks a moved target.
        assert_eq!(
            paste_dispatch_gate(false, Some(true)),
            Err(CopyFallbackReason::Other("secure_target".to_string()))
        );
    }
}
