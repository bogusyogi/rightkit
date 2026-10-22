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
    /// The window is gone. Always the last event; the receiver disconnects after it.
    Closed,
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

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use OverlayEvent::*;

    #[test]
    fn consecutive_moves_in_one_region_merge_to_the_latest() {
        let mut q = EventQueue::default();
        q.push(HoverEnter(1));
        q.push(HoverMove(1, 1.0, 1.0));
        q.push(HoverMove(1, 2.0, 2.0));
        q.push(HoverMove(1, 3.0, 3.0));
        assert_eq!(q.len(), 2);
        assert_eq!(q.drain(), vec![HoverEnter(1), HoverMove(1, 3.0, 3.0)]);
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn moves_in_different_regions_are_not_merged() {
        let mut q = EventQueue::default();
        q.push(HoverMove(1, 1.0, 1.0));
        q.push(HoverMove(2, 5.0, 5.0));
        assert_eq!(
            q.drain(),
            vec![HoverMove(1, 1.0, 1.0), HoverMove(2, 5.0, 5.0)]
        );
    }

    #[test]
    fn a_move_never_merges_across_a_leave_or_enter() {
        let mut q = EventQueue::default();
        q.push(HoverMove(1, 1.0, 1.0));
        q.push(HoverLeave(1));
        q.push(HoverEnter(1));
        q.push(HoverMove(1, 9.0, 9.0));
        assert_eq!(
            q.drain(),
            vec![
                HoverMove(1, 1.0, 1.0),
                HoverLeave(1),
                HoverEnter(1),
                HoverMove(1, 9.0, 9.0)
            ]
        );
    }

    #[test]
    fn clicks_are_never_coalesced() {
        let mut q = EventQueue::default();
        q.push(Click(1));
        q.push(Click(1));
        assert_eq!(q.drain(), vec![Click(1), Click(1)]);
    }

    #[test]
    fn dpi_and_monitor_changes_collapse_when_adjacent() {
        let mut q = EventQueue::default();
        q.push(DpiChanged(1.25));
        q.push(DpiChanged(1.5));
        q.push(MonitorChanged);
        q.push(MonitorChanged);
        q.push(Closed);
        q.push(Closed);
        assert_eq!(q.drain(), vec![DpiChanged(1.5), MonitorChanged, Closed]);
    }

    #[test]
    fn extend_applies_the_same_rules() {
        let mut q = EventQueue::default();
        q.extend(vec![HoverMove(3, 0.0, 0.0), HoverMove(3, 4.0, 4.0)]);
        assert_eq!(q.drain(), vec![HoverMove(3, 4.0, 4.0)]);
    }
}
