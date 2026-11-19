//! macOS fullscreen verdict for the frontmost application.

use crate::guardian::{rect_covers_display, EdgeTolerance};

/// Native fullscreen state of the focused window of `pid` (`None` when the app does not expose
/// it). Host input: the host owns its Accessibility options and attribute reads.
pub type FocusedWindowIsFullscreen = fn(i32) -> Option<bool>;

/// Whether the frontmost application is full screen. `tolerance` is the host's: how many points
/// each edge of a window may differ from a display's and still count as covering it (the geometry
/// fallback only; a native answer from `focused_window_is_fullscreen` is never overridden).
pub fn frontmost_is_fullscreen(
    focused_window_is_fullscreen: FocusedWindowIsFullscreen,
    tolerance: EdgeTolerance,
) -> bool {
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_graphics::display::CGDisplay;
    use core_graphics::geometry::CGRect;
    use core_graphics::window::{kCGWindowBounds, kCGWindowLayer, kCGWindowOwnerPID};

    let Some(frontmost_pid) = rightkit_automation::desktop::macos::frontmost_pid() else {
        return false;
    };
    let native_fullscreen = focused_window_is_fullscreen(frontmost_pid);
    if native_fullscreen == Some(true) {
        return true;
    }
    if native_fullscreen == Some(false) {
        return false;
    }

    // Compatibility fallback only: some games and non-AppKit windows expose no
    // AXFullScreen attribute. Geometry must never override an explicit native
    // `false`, which would misclassify maximized and presentation windows.
    let Some(windows) = rightkit_automation::desktop::macos::window_metadata() else {
        return false;
    };
    let Ok(display_ids) = CGDisplay::active_displays() else {
        return false;
    };
    let owner_pid_key = unsafe { CFString::wrap_under_get_rule(kCGWindowOwnerPID) };
    let layer_key = unsafe { CFString::wrap_under_get_rule(kCGWindowLayer) };
    let bounds_key = unsafe { CFString::wrap_under_get_rule(kCGWindowBounds) };

    for raw_window in windows.iter() {
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
        if owner_pid != Some(frontmost_pid) || layer != Some(0) {
            continue;
        }
        let Some(bounds) = window
            .find(&bounds_key)
            .and_then(|value| value.downcast::<CFDictionary>())
            .and_then(|value| CGRect::from_dict_representation(&value))
        else {
            continue;
        };
        let window_edges = [
            bounds.origin.x,
            bounds.origin.y,
            bounds.origin.x + bounds.size.width,
            bounds.origin.y + bounds.size.height,
        ];
        if display_ids.iter().any(|display_id| {
            let display = CGDisplay::new(*display_id).bounds();
            rect_covers_display(
                window_edges,
                [
                    display.origin.x,
                    display.origin.y,
                    display.origin.x + display.size.width,
                    display.origin.y + display.size.height,
                ],
                tolerance,
            )
        }) {
            return true;
        }
    }
    false
}
