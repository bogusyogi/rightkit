use super::event::PillEvent;

/// Hover follows the rendered pill capsule/action dock exactly on macOS, where
/// the native window may include transparent slack. On Windows the pill window
/// is kept pill-only, so the stable HWND rect is the hover target.
pub const PILL_HOVER_HITBOX_MARGIN: f64 = 0.0;
/// Minimum spacing between hover passes while the cursor moves over the pill, and
/// the delay between the leave-debounce passes (`HoverGate` needs several outside
/// ticks). This is a pacing gap for input-driven passes, never a poll period: nothing
/// wakes while the cursor is away from the pill.
pub const PILL_HOVER_ACTIVE_POLL_MS: u64 = 40;
pub const PILL_CURSOR_EMIT_MIN_DELTA: f64 = 1.0;

pub fn pill_should_be_visible(active: bool, always_show_resting: bool, fullscreen: bool) -> bool {
    // Active states (recording, processing, status, error) override full-screen
    // suppression; only the resting sliver follows it.
    active || (always_show_resting && !fullscreen)
}

/// Hit-test a cursor point in WebView client coordinates against the actual
/// rendered pill capsule/action-dock rectangle reported by the frontend. The
/// overlay window can be taller than the pill when a popup is visible; hover
/// expansion must follow this hitbox, not the whole transparent window.
pub fn pill_hover_hitbox_contains(
    hitbox_x: f64,
    hitbox_y: f64,
    hitbox_width: f64,
    hitbox_height: f64,
    client_x: f64,
    client_y: f64,
) -> bool {
    if !hitbox_x.is_finite()
        || !hitbox_y.is_finite()
        || !hitbox_width.is_finite()
        || !hitbox_height.is_finite()
        || !client_x.is_finite()
        || !client_y.is_finite()
    {
        return false;
    }
    if hitbox_width < 1.0 || hitbox_height < 1.0 {
        return false;
    }
    let margin = PILL_HOVER_HITBOX_MARGIN;
    client_x >= hitbox_x - margin
        && client_x <= hitbox_x + hitbox_width + margin
        && client_y >= hitbox_y - margin
        && client_y <= hitbox_y + hitbox_height + margin
}

pub fn windows_hover_gate_input(inside_window: bool) -> bool {
    inside_window
}

/// Render mode (distinct from `PillEvent`, which drives it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillState {
    Idle,
    Recording,
    Processing,
    Success,
    Error,
}

/// Map an incoming event to a render mode. `Hide` -> None (hide the pill); `Voice` -> None
/// (updates the level only: it does NOT change the mode, so it never resets the timer).
pub fn state_for_event(ev: &PillEvent) -> Option<PillState> {
    match ev {
        PillEvent::Idle => Some(PillState::Idle),
        PillEvent::Starting => Some(PillState::Processing),
        PillEvent::Recording => Some(PillState::Recording),
        PillEvent::Processing { .. } => Some(PillState::Processing),
        PillEvent::Success | PillEvent::Status { .. } => Some(PillState::Success),
        PillEvent::Error { .. } => Some(PillState::Error),
        PillEvent::Hide | PillEvent::Voice { .. } => None,
    }
}
