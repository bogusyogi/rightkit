//! The destination a recording starts in: the cheap hotkey-time target that is frozen into the
//! start frame, and the detached late capture of the start field context and focus pin.
//!
//! On macOS start deliberately does no window-server, accessibility or UIA work: the target is the
//! focus observer's non-blocking cached anchor, and `None` is an unresolved start identity, never an
//! empty proof. Expensive enrichment is the detached late capture only.
//!
//! The `_for_host` functions are the same captures with the pin boxed as a [`HostPin`] for the
//! host seam; the typed ones serve a caller that stores [`FinalPin`] directly.

use rightkit_dictation_command_host::paste_delivery::{self as delivery, DeliveryHost};
use rightkit_transcript_library::delivery::TargetSnapshot;

use crate::final_delivery::FinalPin;
use crate::host::HostPin;

/// The target frozen into the engine's start frame. macOS: only a target with both a process and a
/// window identity (otherwise the start identity stays unresolved); elsewhere: the target as is.
pub fn resolve_start_target(target: &TargetSnapshot) -> Option<TargetSnapshot> {
    #[cfg(target_os = "macos")]
    {
        (target.process_id.is_some() && target.window_handle.is_some()).then(|| target.clone())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(target.clone())
    }
}

/// macOS: copy app and window labels from a late sample only after the cheap hotkey anchor still
/// names this same native window. Identity, continuity and editor proof remain owned by the frozen
/// target; this late sample cannot bind delivery.
#[cfg(target_os = "macos")]
fn enrich_mac_start_target_metadata(mut target: TargetSnapshot) -> TargetSnapshot {
    let Some(live) = delivery::current_target_snapshot_for_stop() else {
        return target;
    };
    let same_native_destination = live.process_id == target.process_id
        && live.window_handle == target.window_handle
        && live.foreground_target == target.foreground_target;
    if same_native_destination {
        target.process_name = target.process_name.or(live.process_name);
        target.window_title = target.window_title.or(live.window_title);
    }
    target
}

/// Capture the start destination, its focus pin, the field context and the content fingerprint on
/// the detached capture thread.
///
/// If no hotkey-time target was available (`frozen` is `None`), the sample is explicitly late: it
/// may enrich diagnostics and context, but the platform capture routines cannot turn it into an
/// original-editor binding without a start continuity proof.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn capture_start_destination_late(
    host: &'static DeliveryHost,
    frozen: Option<TargetSnapshot>,
) -> (
    TargetSnapshot,
    Option<FinalPin>,
    rightkit_dictation_core::engine::RecordingContext,
    Option<u64>,
) {
    let capture_target = frozen.unwrap_or_else(|| {
        #[cfg(target_os = "macos")]
        {
            delivery::current_target_snapshot_for_stop().unwrap_or_else(TargetSnapshot::empty)
        }
        #[cfg(target_os = "windows")]
        {
            delivery::current_target_snapshot_windows(host, false, None)
        }
    });
    #[cfg(target_os = "macos")]
    let capture_target = enrich_mac_start_target_metadata(capture_target);
    #[cfg(target_os = "macos")]
    return delivery::capture_start_destination_macos(host, capture_target);
    #[cfg(target_os = "windows")]
    return delivery::capture_start_destination_windows(host, capture_target);
}

/// Observer-backed anchor for the start capture. Coverage was armed just before the start
/// sequence (which does not wait); its first sample can land a few ms later, or a notification can
/// briefly invalidate it. Each read stays one non-blocking cached read; retry only while coverage is
/// wanted and for at most 100 ms, instead of dropping to stop-time recovery for a miss that settles
/// on its own. Off the main thread.
#[cfg(target_os = "macos")]
fn start_focus_anchor() -> Option<rightkit_automation::desktop::HotkeyFocusAnchor> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    loop {
        if let Some(anchor) = rightkit_automation::desktop::capture_hotkey_focus_anchor() {
            return Some(anchor);
        }
        if std::time::Instant::now() >= deadline
            || !rightkit_automation::desktop::focus_coverage_wanted()
        {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The hotkey-time start target.
///
/// Windows keeps its established start snapshot path. macOS uses only the observer's non-blocking
/// cached anchor; the expensive enrichment stays detached in `capture_start_destination_late`.
pub fn observe_start_target(host: &'static DeliveryHost) -> TargetSnapshot {
    #[cfg(target_os = "macos")]
    {
        let _ = host;
        start_focus_anchor()
            .map(|anchor| TargetSnapshot {
                process_id: Some(anchor.process_id as u32),
                process_name: None,
                window_title: None,
                window_handle: Some(anchor.window_identity),
                // AX editor identity belongs only in StopContinuity. Keeping this target
                // editor-less preserves macOS window-target matching.
                focused_control_handle: None,
                foreground_target: Some(delivery::ForegroundTarget::WindowHandle {
                    handle: anchor.window_identity,
                }),
                focused_text_input: None,
                is_elevated: None,
                stop_continuity: Some(delivery::StopContinuity {
                    captured_at: anchor.captured_at,
                    process_instance: anchor.process_instance,
                    native_control: anchor.native_control,
                }),
                exe_path: None,
            })
            .unwrap_or_else(TargetSnapshot::empty)
    }
    #[cfg(target_os = "windows")]
    {
        let mut target = delivery::current_target_snapshot_windows(host, false, None);
        if let (Some(pid), Some(window)) = (target.process_id, target.window_handle) {
            if let Some((captured_at, process_instance)) =
                rightkit_automation::desktop::capture_focus_continuity(pid, window)
            {
                target.stop_continuity = Some(delivery::StopContinuity {
                    captured_at,
                    process_instance,
                    native_control: target.focused_control_handle,
                });
            }
        }
        target
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        delivery::snapshot_target(host)
    }
}

/// Freeze the stop destination and its focus pin at the first stop boundary. A platform without
/// pins returns the foreground snapshot and no pin.
pub fn capture_stop_destination(host: &'static DeliveryHost) -> (TargetSnapshot, Option<FinalPin>) {
    #[cfg(target_os = "macos")]
    {
        delivery::capture_stop_target_macos(host)
    }
    #[cfg(target_os = "windows")]
    {
        delivery::capture_stop_target_windows(host)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        (delivery::snapshot_target(host), None)
    }
}

/// [`capture_stop_destination`] with the pin boxed for the host seam.
pub fn capture_stop_destination_for_host(
    host: &'static DeliveryHost,
) -> (TargetSnapshot, Option<HostPin>) {
    let (target, pin) = capture_stop_destination(host);
    (target, pin.map(|pin| std::sync::Arc::new(pin) as HostPin))
}

/// [`capture_start_destination_late`] with the pin boxed for the host seam.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn capture_start_destination_late_for_host(
    host: &'static DeliveryHost,
    frozen: Option<TargetSnapshot>,
) -> (
    TargetSnapshot,
    Option<HostPin>,
    rightkit_dictation_core::engine::RecordingContext,
    Option<u64>,
) {
    let (target, pin, context, fingerprint) = capture_start_destination_late(host, frozen);
    (
        target,
        pin.map(|pin| std::sync::Arc::new(pin) as HostPin),
        context,
        fingerprint,
    )
}
