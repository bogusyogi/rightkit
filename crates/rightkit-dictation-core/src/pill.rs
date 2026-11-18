//! The dictation pill's pure parts: the event a host's state machine feeds the pill
//! ([`PillEvent`], with a fixed serde wire form), the event-to-render-state mapping, the
//! visibility rule, the hover hit-test, and the event-driven hover scheduling.
//!
//! Nothing here draws, owns a window or reads the OS. Native surfaces (the Windows pill crate, a
//! macOS panel) and webview renderers consume these types; the product's text, colours and
//! layout stay in the host.

mod event;
mod hover;
mod model;

pub use event::PillEvent;
pub use hover::{HoverEdgeTracker, HoverMsg, HoverPacer, HoverSignal};
pub use model::{
    pill_hover_hitbox_contains, pill_should_be_visible, state_for_event, windows_hover_gate_input,
    PillState, PILL_CURSOR_EMIT_MIN_DELTA, PILL_HOVER_ACTIVE_POLL_MS, PILL_HOVER_HITBOX_MARGIN,
};
