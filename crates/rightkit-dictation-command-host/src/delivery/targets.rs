// Foreground-target snapshot, revalidation, and restore helpers. These decide
// whether the window that was focused at dictation time is still a valid
// paste target before/after the transcript transaction runs.

use super::*;

pub fn snapshot_target() -> TargetSnapshot {
    crate::focus::snapshot_current_target()
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) fn target_ready_for_input(original: &TargetSnapshot) -> bool {
    if let Some(evidence) = original.stop_continuity.as_ref() {
        if !original
            .process_id
            .zip(original.window_handle)
            .is_some_and(|(pid, window)| {
                rightkit_automation::desktop::validate_focus_continuity(
                    pid,
                    window,
                    evidence.captured_at,
                    evidence.process_instance,
                    evidence.native_control,
                )
            })
        {
            return false;
        }
    }
    let live = snapshot_target();
    target_matches_expected(&live, original, false) || macos_window_target_matches(&live, original)
}

#[cfg(target_os = "macos")]
fn macos_window_target_matches(current: &TargetSnapshot, expected: &TargetSnapshot) -> bool {
    current.process_id == expected.process_id
        && expected.window_handle.is_some()
        && current.window_handle == expected.window_handle
        && current.foreground_target == expected.foreground_target
        && expected.focused_control_handle.is_none()
        && current.focused_control_handle.is_some()
        && current.focused_text_input != Some(false)
}

#[cfg(target_os = "windows")]
fn macos_window_target_matches(_: &TargetSnapshot, _: &TargetSnapshot) -> bool {
    false
}

#[cfg(target_os = "windows")]
pub fn restore_and_verify(target: &TargetSnapshot) -> Result<(), DeliveryError> {
    use std::{thread, time::Duration};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};

    let Some(handle) = target.window_handle else {
        return Err(DeliveryError::new(
            "E_NO_TARGET",
            "no captured window to restore",
        ));
    };
    let hwnd = handle as *mut core::ffi::c_void;
    // Pill windows are WS_EX_NOACTIVATE. When target remains foreground, its
    // Chromium/Electron text focus & blinking caret are already intact. Trying
    // to SetFocus then exact-match a child HWND false-fails render widgets &
    // turns valid pill stops into clipboard fallback.
    if unsafe { GetForegroundWindow() } == hwnd {
        return Ok(());
    }
    unsafe { SetForegroundWindow(hwnd) };
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(15));
        if unsafe { GetForegroundWindow() } == hwnd {
            return Ok(());
        }
    }
    Err(DeliveryError::new(
        "E_FOCUS_RESTORE",
        "could not bring the captured target window to the foreground",
    ))
}

#[cfg(target_os = "macos")]
pub fn restore_and_verify(target: &TargetSnapshot) -> Result<(), DeliveryError> {
    use std::time::Duration;

    let pid = target_process_id(target, "no captured app to restore")?;
    if rightkit_automation::desktop::macos::restore_pid(pid, 15, Duration::from_millis(15)) {
        return Ok(());
    }
    Err(focus_restore_error(
        "could not restore the captured target app",
    ))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn restore_and_verify(_target: &TargetSnapshot) -> Result<(), DeliveryError> {
    unsupported_focus_restore()
}
