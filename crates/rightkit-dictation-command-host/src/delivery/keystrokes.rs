// Paste / Enter keystroke posts (SendInput on Windows, CGEvent combos on
// macOS). Every variant short-circuits to success in test mode.

use super::*;

#[cfg(target_os = "windows")]
pub(crate) fn send_paste_keystroke() -> Result<(), DeliveryError> {
    if test_mode() {
        return Ok(());
    }
    if rightkit_automation::desktop::windows::send_paste_keystroke_after_hotkey() {
        Ok(())
    } else {
        Err(DeliveryError::new(
            "E_SENDINPUT",
            "SendInput did not send Ctrl+V",
        ))
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn send_paste_keystroke() -> Result<(), DeliveryError> {
    if test_mode() {
        return Ok(());
    }
    if !crate::macos_input::accessibility_trusted(false) {
        return Err(DeliveryError::new(
            "E_ACCESSIBILITY",
            "Accessibility permission is not granted",
        ));
    }
    if crate::macos_input::paste_combo() {
        Ok(())
    } else {
        Err(DeliveryError::new(
            "E_CGEVENT",
            "could not post Cmd+V (Accessibility permission not granted?)",
        ))
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(crate) fn send_paste_keystroke() -> Result<(), DeliveryError> {
    if test_mode() {
        return Ok(());
    }
    Err(DeliveryError::new(
        "E_UNSUPPORTED",
        "paste is unsupported on this platform",
    ))
}

#[cfg(target_os = "windows")]
pub fn send_enter_keystroke() -> Result<(), DeliveryError> {
    if test_mode() {
        return Ok(());
    }
    if rightkit_automation::desktop::windows::send_enter_keystroke() {
        Ok(())
    } else {
        Err(DeliveryError::new(
            "E_SENDINPUT",
            "SendInput did not send Enter",
        ))
    }
}

#[cfg(target_os = "macos")]
pub fn send_enter_keystroke() -> Result<(), DeliveryError> {
    if test_mode() {
        return Ok(());
    }
    if !crate::macos_input::accessibility_trusted(false) {
        return Err(DeliveryError::new(
            "E_ACCESSIBILITY",
            "Accessibility permission is not granted",
        ));
    }
    if crate::macos_input::press_return() {
        Ok(())
    } else {
        Err(DeliveryError::new(
            "E_CGEVENT",
            "could not post Return (Accessibility permission not granted?)",
        ))
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn send_enter_keystroke() -> Result<(), DeliveryError> {
    if test_mode() {
        return Ok(());
    }
    Err(DeliveryError::new(
        "E_UNSUPPORTED",
        "send Enter is unsupported on this platform",
    ))
}
