//! Frontmost application identity, window identity, and app activation.

use core_foundation::base::{CFType, TCFType};
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
use std::time::Duration;

use super::ffi::{
    ax_copy, ax_element_pid, ax_set_bool, ax_system_element, AXIsProcessTrusted, AXUIElementRef,
};
use super::focus::{
    ax_raise_app, focus_pin_is_current, focus_pin_running_identity_matches, FocusPin,
};

pub struct FrontApp {
    pub pid: i32,
    pub name: Option<String>,
    pub bundle: Option<String>,
    /// Filesystem path of the app bundle (`bundleURL`), for icon lookup.
    pub bundle_path: Option<String>,
}

pub(super) fn ax_frontmost_pid() -> Option<i32> {
    unsafe {
        let system = ax_system_element()?;
        let app = ax_copy(
            system.as_CFTypeRef() as AXUIElementRef,
            "AXFocusedApplication",
        )?;
        let app = CFType::wrap_under_create_rule(app);
        ax_element_pid(app.as_CFTypeRef() as AXUIElementRef)
    }
}

pub fn frontmost() -> Option<FrontApp> {
    let ws = NSWorkspace::sharedWorkspace();
    // NSWorkspace can remain cached on first target when queried repeatedly
    // from delivery worker thread. System-wide AX owns current keyboard focus;
    // use its PID for exact delivery identity, retaining NSWorkspace only as a
    // fallback when Accessibility is unavailable.
    let app = ax_frontmost_pid()
        .and_then(NSRunningApplication::runningApplicationWithProcessIdentifier)
        .or_else(|| ws.frontmostApplication())?;
    Some(FrontApp {
        pid: app.processIdentifier(),
        name: app.localizedName().map(|s| s.to_string()),
        bundle: app.bundleIdentifier().map(|s| s.to_string()),
        bundle_path: super::observation::app_bundle_path(&app),
    })
}

/// AppKit-only hotkey-time identity snapshot. It performs no AX messaging.
pub fn frontmost_fast() -> Option<FrontApp> {
    let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
    Some(FrontApp {
        pid: app.processIdentifier(),
        name: app.localizedName().map(|s| s.to_string()),
        bundle: app.bundleIdentifier().map(|s| s.to_string()),
        bundle_path: super::observation::app_bundle_path(&app),
    })
}

/// Return the CoreGraphics window-server identity for the frontmost eligible
/// normal-layer window owned by `pid`.
///
/// This is deliberately AppKit/CoreGraphics only: stop handling uses it as a
/// cheap native destination snapshot and must not enter the AX tree. The
/// window list is ordered front-to-back, so the first matching normal window
/// is the only window identity we retain. Window titles and AX object hashes
/// are not stable window identities.
pub fn frontmost_window_identity(pid: i32) -> Option<isize> {
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_graphics::window::{
        kCGWindowLayer, kCGWindowNumber, kCGWindowOwnerName, kCGWindowOwnerPID,
    };

    let windows = super::window_metadata()?;
    let owner_pid_key = unsafe { CFString::wrap_under_get_rule(kCGWindowOwnerPID) };
    let layer_key = unsafe { CFString::wrap_under_get_rule(kCGWindowLayer) };
    let number_key = unsafe { CFString::wrap_under_get_rule(kCGWindowNumber) };
    let owner_name_key = unsafe { CFString::wrap_under_get_rule(kCGWindowOwnerName) };

    windows.iter().find_map(|raw_window| {
        let window = unsafe {
            CFDictionary::<CFString, CFType>::wrap_under_get_rule(*raw_window as CFDictionaryRef)
        };
        let owner_pid = window
            .find(&owner_pid_key)
            .and_then(|value| value.downcast::<CFNumber>())
            .and_then(|value| value.to_i32());
        let layer = window
            .find(&layer_key)
            .and_then(|value| value.downcast::<CFNumber>())
            .and_then(|value| value.to_i32());
        if owner_pid != Some(pid) || layer != Some(0) {
            return None;
        }
        // The app's own windows must never become a destination when an
        // overlay is frontmost during the stop race.
        if window
            .find(&owner_name_key)
            .and_then(|value| value.downcast::<CFString>())
            .is_some_and(|name| {
                super::own_window_owner_prefix().is_some_and(|prefix| {
                    name.to_string()
                        .to_ascii_lowercase()
                        .starts_with(&prefix.to_ascii_lowercase())
                })
            })
        {
            return None;
        }
        window
            .find(&number_key)
            .and_then(|value| value.downcast::<CFNumber>())
            .and_then(|value| value.to_i64())
            .filter(|number| *number > 0)
            .map(|number| number as isize)
    })
}

pub fn frontmost_pid() -> Option<i32> {
    ax_frontmost_pid().or_else(|| {
        NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .map(|app| app.processIdentifier())
    })
}

pub fn activate_pid(pid: i32) -> bool {
    let ax = ax_raise_app(pid);
    let ns = match NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
        Some(app) => app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows),
        None => false,
    };
    ax || ns
}

pub fn restore_pid(pid: i32, attempts: usize, delay: std::time::Duration) -> bool {
    if frontmost_pid() == Some(pid) {
        return true;
    }
    activate_pid(pid);
    for _ in 0..attempts {
        std::thread::sleep(delay);
        if frontmost_pid() == Some(pid) {
            return true;
        }
    }
    false
}

/// Restore exact stop-time editor focus after transcription. App activation
/// alone leaves WebKit/Chromium's internal editor unfocused, so request focus
/// on retained AX element and accept only exact pin revalidation.
pub fn restore_focus_pin(pin: &FocusPin) -> bool {
    unsafe {
        if AXIsProcessTrusted() == 0 || !focus_pin_running_identity_matches(pin) {
            return false;
        }
        // Do not mutate focus when the exact retained editor is already
        // current. Redundant AXFocused writes can churn Chromium's editor
        // observation during otherwise unchanged-focus delivery.
        if focus_pin_is_current(pin) {
            return true;
        }
        if !restore_pid(pin.pid, 12, Duration::from_millis(5)) {
            return false;
        }
        // Re-select retained window before editor. App activation can otherwise
        // restore a different window of the same process.
        for attribute in ["AXMain", "AXFocused"] {
            let _ = ax_set_bool(pin.window.as_CFTypeRef() as AXUIElementRef, attribute, true);
        }
        let _ = ax_set_bool(
            pin.element.as_CFTypeRef() as AXUIElementRef,
            "AXFocused",
            true,
        );
        for _ in 0..12 {
            if focus_pin_is_current(pin) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }
}
