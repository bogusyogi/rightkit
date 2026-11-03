//! Events an overlay reports to its host, and the coalescing queue the owner thread builds them in.

/// Something that happened to the overlay window. Coordinates are logical pixels relative to the
/// window top-left.
///
/// # Why this is not `rightkit_overlay::OverlayEvent`
///
/// `rightkit_overlay::OverlayEvent` is the pill **state** feed that drives an overlay from the
/// outside (`Hide`, `Idle`, `Starting`, `Recording`, `Processing`, `Status { label }`, `Error
/// { label }`; serde, no coordinates). This enum is what the window **reports back**: pointer
/// hover, clicks and display changes, with ids and positions. The two flow in opposite directions
/// and share no variant, so one type cannot carry both without making half of each unusable.
/// They only collide by name: import this one by path, or alias it
/// (`use rightkit_overlay_win::OverlayEvent as WindowEvent`). Folding the input events into
/// `rightkit-overlay` would be a change to that crate's public enum, which this crate does not
/// make.
#[derive(Clone, Debug, PartialEq)]
pub enum OverlayEvent {
    /// The pointer entered hit region `id` (after the suite hover debounce).
    HoverEnter(u32),
    /// The pointer moved inside hit region `id`, to `(x, y)`. Consecutive moves in one region are
    /// merged into the latest.
    HoverMove(u32, f32, f32),
    /// The pointer left hit region `id` (after the suite leave debounce).
    HoverLeave(u32),
    /// The left button was pressed and released on hit region `id`.
    Click(u32),
    /// The effective DPI changed; the value is the new scale factor (1.5 at 144 DPI). Cached
    /// layers were dropped and the next frame is painted at the new scale.
    DpiChanged(f32),
    /// The window moved to a different monitor.
    MonitorChanged,
    /// A character typed while the window is in edit mode (`OverlayHandle::set_focusable(true)`).
    /// Control characters are not sent here; the editing keys are reported as [`OverlayEvent::Key`].
    Char(char),
    /// An editing key pressed while the window is in edit mode.
    Key(OverlayKey),
    /// The window gained (`true`) or lost (`false`) keyboard focus while in edit mode.
    Focus(bool),
    /// The window is gone. Always the last event; the receiver disconnects after it.
    Closed,
}

/// The editing keys reported as [`OverlayEvent::Key`] in edit mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayKey {
    Enter,
    Escape,
    Backspace,
}

/// Events waiting to be sent, merged as they are pushed so a flood of pointer moves costs one
/// event per message-loop pass rather than one per `WM_MOUSEMOVE`.
#[derive(Debug, Default)]
pub(crate) struct EventQueue {
    pending: Vec<OverlayEvent>,
}

impl EventQueue {
    pub(crate) fn push(&mut self, event: OverlayEvent) {
        if let Some(last) = self.pending.last_mut() {
            let replace = match (&event, &*last) {
                // The newest position in the same region replaces the one before it.
                (OverlayEvent::HoverMove(id, ..), OverlayEvent::HoverMove(last_id, ..)) => {
                    id == last_id
                }
                (OverlayEvent::DpiChanged(_), OverlayEvent::DpiChanged(_)) => true,
                // A repeat of these adds nothing.
                (OverlayEvent::MonitorChanged, OverlayEvent::MonitorChanged)
                | (OverlayEvent::Closed, OverlayEvent::Closed) => return,
                _ => false,
            };
            if replace {
                *last = event;
                return;
            }
        }
        self.pending.push(event);
    }

    pub(crate) fn extend(&mut self, events: Vec<OverlayEvent>) {
        for event in events {
            self.push(event);
        }
    }

    pub(crate) fn drain(&mut self) -> Vec<OverlayEvent> {
        std::mem::take(&mut self.pending)
    }
}
