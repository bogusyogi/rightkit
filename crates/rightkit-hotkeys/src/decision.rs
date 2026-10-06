//! Pure predicates shared by the platform hooks.

/// `kCGMouseButtonCenter`: the only `OtherMouseDown` button that may toggle dictation. The event
/// also carries buttons 3-31 (back/forward, DPI, EasySwitch); those must pass through untouched.
pub const MOUSE_BUTTON_MIDDLE: i64 = 2;

pub fn other_mouse_down_is_middle(button: i64) -> bool {
    button == MOUSE_BUTTON_MIDDLE
}

/// A suppressing fn chord must be backed by the modifier bit on the key event *and* a preceding fn
/// transition observed by the tap itself. Rejects stale/synthetic Space or Escape events that
/// inherit SecondaryFn.
pub fn should_accept_fn_chord(event_has_fn: bool, cached_fn_down: bool, enabled: bool) -> bool {
    event_has_fn && cached_fn_down && enabled
}
