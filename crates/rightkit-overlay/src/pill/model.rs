// Pill — pure parts: the `PillEvent` payload (serde), the work-area clamp
// geometry, and the legacy message→event mapping. The Tauri window show/hide,
// positioning, and monitor queries live in `src-tauri/src/pill.rs`.

use serde::{Deserialize, Serialize};

/// Pill control-surface states (LOCKED spec; visual signed off via
/// `scripts/pill_states_mockup.html`). The pill is an
/// icon/timer surface plus short transient status text for completed actions.
/// `Error.label` is for the tooltip/aria only, never rendered in the pill.
/// `Voice` is a separate
/// high-frequency amplitude update that drives the recording glow WITHOUT
/// changing the rendered mode (so it doesn't reset the timer). Serializes
/// `{ "kind": "voice", "level": 0.5 }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PillEvent {
    Hide,
    /// Resting sliver (copper). Emitted at rest (Armed) and at startup — the
    /// pill is an always-on overlay. Hovering the
    /// sliver is the start affordance.
    Idle,
    /// Immediate acknowledgement while the audio stream is resuming. The
    /// renderer promotes this to `Recording` only after the first mic samples.
    Starting,
    /// Recording: reactive bars + timer. Renderer ticks the timer locally.
    Recording,
    /// Mic amplitude 0.0..=1.0 for the contained voice glow. Updates the glow
    /// only; does not change mode. (Fallback: renderer breathes gently if no
    /// Voice events arrive — amplitude wiring is Phase 3 Chunk C.)
    Voice {
        level: f32,
    },
    /// Copper breath. `cancellable=false` for final-only ASR after stop, so the
    /// renderer shows no controls on hover.
    Processing {
        cancellable: bool,
    },
    /// Legacy generic completion event. Shared React renderers downgrade this
    /// to a text status ("Done"); new producers should prefer `Status`.
    Success,
    /// Short transient non-interactive status, e.g. `Pasted`, `Sent`, `Saved`, `Copied`.
    Status {
        label: String,
    },
    /// Pulsing red warning. `label` is rendered inline and exposed as its accessible name.
    Error {
        label: String,
    },
}

/// Hover follows the rendered pill capsule/action dock exactly on macOS, where
/// the native window may include transparent slack. On Windows the pill window
/// is kept pill-only, so the stable HWND rect is the hover target.
pub const PILL_HOVER_HITBOX_MARGIN: f64 = 0.0;
/// Minimum spacing between hover passes while the cursor moves over the pill, and
/// the delay between the leave-debounce passes (`HoverGate` needs several outside
/// ticks). This is a pacing gap for input-driven passes, never a poll period: the
/// old 90 ms idle poll (`PILL_HOVER_IDLE_POLL_MS`) was retired with the timer
/// (#293), so nothing wakes while the cursor is away from the pill.
pub const PILL_HOVER_ACTIVE_POLL_MS: u64 = 40;
pub const PILL_CURSOR_EMIT_MIN_DELTA: f64 = 1.0;

pub fn pill_should_be_visible(active: bool, always_show_resting: bool, fullscreen: bool) -> bool {
    // Active states (recording, processing, status, error) override full-screen
    // suppression; only the resting sliver follows it (#162).
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

/// Legacy message → event mapping for the static `show_pill(&str)` API.
/// Reduced to the v2 states (no text inside the pill). Unknown/empty → Idle/Hide.
pub fn event_for_message(message: &str) -> PillEvent {
    let lower = message.to_ascii_lowercase();
    if lower.contains("listening") || lower.contains("recording") {
        PillEvent::Recording
    } else if lower.contains("transcribing")
        || lower.contains("thinking")
        || lower.contains("pasting")
    {
        PillEvent::Processing { cancellable: false }
    } else if lower.contains("error") {
        PillEvent::Error {
            label: message.to_string(),
        }
    } else if lower.is_empty() {
        PillEvent::Hide
    } else {
        PillEvent::Idle
    }
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

/// Map an incoming event to a render mode. `Hide` → None (hide the pill); `Voice` → None
/// (updates the glow level only — does NOT change the mode, so it never resets the timer).
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
