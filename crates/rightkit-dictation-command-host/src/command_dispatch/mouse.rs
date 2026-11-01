// Mouse — click / scroll via mouse_event (Windows SendInput / macOS CGEvent).

use super::entry::{DispatchError, DispatchOutcome, DispatchResult};

#[cfg(target_os = "windows")]
pub(super) fn dispatch_mouse(
    action: &str,
    button: Option<&str>,
    clicks: Option<u32>,
    direction: Option<&str>,
    page: bool,
) -> DispatchResult {
    use rightkit_automation::win::{send_input_batch, RawInput, RawMouse, RawMouseButton};
    // A held stop-hotkey modifier turns "left click" into Ctrl+click etc. —
    // neutralize physical modifiers first, same as command chords.
    super::keys::release_held_modifiers()?;

    match action {
        "click" => {
            let n = clicks.unwrap_or(1).max(1).min(3) as usize;
            let which = match button.unwrap_or("left") {
                "left" => RawMouseButton::Left,
                "right" => RawMouseButton::Right,
                "middle" => RawMouseButton::Middle,
                other => {
                    return Err(DispatchError::new(
                        "E_BAD_BUTTON",
                        format!("unknown mouse button {:?}", other),
                    ))
                }
            };
            let mut events = Vec::with_capacity(n * 2);
            for _ in 0..n {
                events.push(RawInput::mouse(RawMouse::Button {
                    button: which,
                    up: false,
                }));
                events.push(RawInput::mouse(RawMouse::Button {
                    button: which,
                    up: true,
                }));
            }
            if let Err(error) = send_input_batch(&events) {
                return Err(DispatchError::new(
                    "E_SENDINPUT",
                    format!(
                        "mouse SendInput failed for {} events: {}",
                        events.len(),
                        error
                    ),
                ));
            }
            Ok(DispatchOutcome::new(
                "mouse_click",
                format!("button={:?} clicks={}", button, n),
            ))
        }
        "scroll" => {
            // Wheel delta: 120 per notch. Page scroll = bigger.
            let notches: i32 = if page { 10 } else { 3 };
            let delta: i32 = 120 * notches;
            let event = match direction.unwrap_or("down") {
                "up" => RawMouse::Wheel(delta),
                "down" => RawMouse::Wheel(-delta),
                "right" => RawMouse::HWheel(delta),
                "left" => RawMouse::HWheel(-delta),
                other => {
                    return Err(DispatchError::new(
                        "E_BAD_DIRECTION",
                        format!("unknown scroll direction {:?}", other),
                    ))
                }
            };
            if send_input_batch(&[RawInput::mouse(event)]).is_err() {
                return Err(DispatchError::new("E_SENDINPUT", "scroll SendInput failed"));
            }
            Ok(DispatchOutcome::new(
                "mouse_scroll",
                format!("direction={:?} page={}", direction, page),
            ))
        }
        other => Err(DispatchError::new(
            "E_BAD_MOUSE_ACTION",
            format!("unknown mouse action {:?}", other),
        )),
    }
}

#[cfg(target_os = "macos")]
pub(super) fn dispatch_mouse(
    action: &str,
    button: Option<&str>,
    clicks: Option<u32>,
    direction: Option<&str>,
    page: bool,
) -> DispatchResult {
    use crate::macos_input::{self, MouseButton};
    match action {
        "click" => {
            let n = clicks.unwrap_or(1).clamp(1, 3);
            let btn = match button.unwrap_or("left") {
                "left" => MouseButton::Left,
                "right" => MouseButton::Right,
                "middle" => MouseButton::Center,
                other => {
                    return Err(DispatchError::new(
                        "E_BAD_BUTTON",
                        format!("unknown mouse button {:?}", other),
                    ))
                }
            };
            if macos_input::mouse_click(btn, n) {
                Ok(DispatchOutcome::new(
                    "mouse_click",
                    format!("button={:?} clicks={}", button, n),
                ))
            } else {
                Err(DispatchError::new(
                    "E_CGEVENT",
                    "could not post mouse click (Accessibility permission not granted?)",
                ))
            }
        }
        "scroll" => {
            let lines: i32 = if page { 10 } else { 3 };
            let (v, h) = match direction.unwrap_or("down") {
                "up" => (lines, 0),
                "down" => (-lines, 0),
                "right" => (0, lines),
                "left" => (0, -lines),
                other => {
                    return Err(DispatchError::new(
                        "E_BAD_DIRECTION",
                        format!("unknown scroll direction {:?}", other),
                    ))
                }
            };
            if macos_input::scroll(v, h) {
                Ok(DispatchOutcome::new(
                    "mouse_scroll",
                    format!("direction={:?} page={}", direction, page),
                ))
            } else {
                Err(DispatchError::new("E_CGEVENT", "could not post scroll"))
            }
        }
        other => Err(DispatchError::new(
            "E_BAD_MOUSE_ACTION",
            format!("unknown mouse action {:?}", other),
        )),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(super) fn dispatch_mouse(
    _a: &str,
    _b: Option<&str>,
    _c: Option<u32>,
    _d: Option<&str>,
    _p: bool,
) -> DispatchResult {
    Err(DispatchError::new(
        "E_UNSUPPORTED",
        "mouse dispatch not implemented for this OS",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_mouse_unknown_action_is_bad_action() {
        let result = dispatch_mouse("teleport", None, None, None, false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "E_BAD_MOUSE_ACTION");
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn dispatch_mouse_unknown_button_is_bad_button() {
        let result = dispatch_mouse("click", Some("laser"), None, None, false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "E_BAD_BUTTON");
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn dispatch_mouse_unknown_direction_is_bad_direction() {
        let result = dispatch_mouse("scroll", None, None, Some("sideways"), false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "E_BAD_DIRECTION");
    }
}
