//! Hover model: turns pointer samples into `HoverEnter` / `HoverMove` / `HoverLeave`, using the
//! suite `HoverGate` for the enter and leave debounce.
//!
//! `HoverGate` counts ticks: it enters on the first tick the pointer is inside and leaves only
//! after [`rightkit_overlay::HOVER_LEAVE_TICKS`] consecutive ticks outside. A tick here is one
//! observation. While the pointer is over a region every pointer message is an observation (no
//! timer). Once the pointer leaves, the owner arms a short timer so the remaining outside ticks
//! arrive on a clock ([`HOVER_TICK_MS`] apart); the timer is killed the moment the gate settles.
//! While nothing is hovered no timer exists and the owner thread blocks in `GetMessage`.
//!
//! The tracker is pure: it returns events and a [`TimerAction`], and the owner does the Win32.

use rightkit_overlay::HoverGate;

use crate::events::OverlayEvent;

/// Spacing of the leave-debounce ticks. Matches the suite hover pacing gap.
pub const HOVER_TICK_MS: u32 = 40;

/// What the owner should do with the debounce timer after an observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TimerAction {
    None,
    Arm,
    Kill,
}

/// One look at the pointer: the region under it (if any) and its logical window coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Sample {
    pub region: Option<u32>,
    pub x: f32,
    pub y: f32,
}

impl Sample {
    pub(crate) fn outside() -> Self {
        Self {
            region: None,
            x: 0.0,
            y: 0.0,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct HoverTracker {
    gate: HoverGate,
    /// The region currently reported as hovered.
    current: Option<u32>,
    /// Whether the last observation was over a region.
    last_inside: bool,
    timer_armed: bool,
}

impl HoverTracker {
    fn observe(&mut self, sample: Sample, out: &mut Vec<OverlayEvent>) {
        let inside = sample.region.is_some();
        let hovering = self.gate.update(inside);
        self.last_inside = inside;
        match (self.current, sample.region, hovering) {
            (None, Some(id), true) => {
                self.current = Some(id);
                out.push(OverlayEvent::HoverEnter(id));
                out.push(OverlayEvent::HoverMove(id, sample.x, sample.y));
            }
            (None, _, _) => {}
            (Some(current), Some(id), true) => {
                if id != current {
                    out.push(OverlayEvent::HoverLeave(current));
                    out.push(OverlayEvent::HoverEnter(id));
                    self.current = Some(id);
                }
                out.push(OverlayEvent::HoverMove(id, sample.x, sample.y));
            }
            // Outside, but the gate is still holding the hover through its leave debounce.
            (Some(_), None, true) => {}
            (Some(current), _, false) => {
                self.current = None;
                out.push(OverlayEvent::HoverLeave(current));
            }
        }
    }

    /// Hovered and the pointer is away from every region: ticks must keep coming until the gate
    /// releases.
    fn needs_ticks(&self) -> bool {
        self.current.is_some() && !self.last_inside
    }

    /// A pointer message over the window (`WM_MOUSEMOVE`), or the pointer leaving the window
    /// (`WM_MOUSELEAVE`, pass [`Sample::outside`]).
    pub(crate) fn on_pointer(
        &mut self,
        sample: Sample,
        out: &mut Vec<OverlayEvent>,
    ) -> TimerAction {
        if sample.region.is_some() {
            self.observe(sample, out);
            return self.disarm();
        }
        if self.current.is_none() || self.timer_armed {
            // Nothing hovered, or the timer is already counting the outside ticks.
            return TimerAction::None;
        }
        self.observe(sample, out);
        if self.needs_ticks() {
            self.timer_armed = true;
            TimerAction::Arm
        } else {
            TimerAction::None
        }
    }

    /// The debounce timer fired; `sample` is a fresh look at the pointer.
    pub(crate) fn on_tick(&mut self, sample: Sample, out: &mut Vec<OverlayEvent>) -> TimerAction {
        self.observe(sample, out);
        if self.needs_ticks() {
            self.timer_armed = true;
            TimerAction::Arm
        } else {
            self.disarm()
        }
    }

    /// The window hid, closed or lost the pointer by other means: end any hover at once.
    pub(crate) fn reset(&mut self, out: &mut Vec<OverlayEvent>) -> TimerAction {
        if let Some(current) = self.current.take() {
            out.push(OverlayEvent::HoverLeave(current));
        }
        self.gate.reset();
        self.last_inside = false;
        self.disarm()
    }

    fn disarm(&mut self) -> TimerAction {
        if std::mem::take(&mut self.timer_armed) {
            TimerAction::Kill
        } else {
            TimerAction::None
        }
    }
}
