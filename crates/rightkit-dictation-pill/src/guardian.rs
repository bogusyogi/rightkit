//! Event-driven scheduling for the pill visibility guardian (platform-free).
//!
//! The guardian sleeps on a channel with NO timeout while idle; the OS tells it when something
//! that can change the answer happened (foreground/activation change, Space change,
//! minimise/restore, a resize of the frontmost window). This module holds the pure
//! pieces so they unit-test without an OS: the event vocabulary, the coalescer that
//! turns a burst of events into one evaluation, the rate gate for the controller
//! probe, and the edge-tolerance rectangle test the macOS fullscreen fallback uses. (The Windows
//! fullscreen verdict is `rightkit_overlay::windows::foreground_fullscreen_on`; the host passes
//! its probe.) Every period is the host's ([`GuardianTimings`]); the crate has no default.

use std::sync::mpsc::SyncSender;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::clock::after;

/// The guardian's periods, in milliseconds. No default: the host decides them. Validated by
/// [`GuardianTimings::is_valid`] (the worker refuses to start with an invalid set).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuardianTimings {
    /// Trailing window that merges the burst a single focus change produces (Windows:
    /// foreground + location + minimise events; macOS: activation + AX focused-window + Space
    /// notifications) into one evaluation. `1..=60_000`.
    pub focus_coalesce_ms: u64,
    /// Resizing a window emits a continuous stream; evaluate at most this often. `1..=60_000`.
    pub geometry_coalesce_ms: u64,
    /// One follow-up pass after a focus/Space/minimise event, because a fullscreen transition
    /// and a freshly activated window's z-order settle after the event fires. A single
    /// event-triggered pass, never a recurring timer. `1..=60_000`.
    pub settle_pass_ms: u64,
    /// Last-resort cadence used only when no OS event source could be installed. Healthy
    /// operation never uses it. `1_000..=3_600_000`.
    pub degraded_repair_interval_ms: u64,
    /// Minimum spacing between the host's controller probes (see [`IntervalGate`]).
    /// `1..=3_600_000`.
    pub probe_min_interval_ms: u64,
    /// macOS: how long one Accessibility request (observer install, notification registration,
    /// focused-window lookup) may block the worker before it gives up. `10..=10_000`. Unused on
    /// Windows, still validated.
    pub ax_messaging_timeout_ms: u64,
    /// Windows: how long the worker waits for the WinEvent pump thread to report that its hooks
    /// are installed before it runs without an OS event source (degraded mode). `100..=60_000`.
    /// Unused on macOS, still validated.
    pub os_source_start_timeout_ms: u64,
}

impl GuardianTimings {
    pub fn is_valid(&self) -> bool {
        let ms = |v: u64, low: u64, high: u64| v >= low && v <= high;
        ms(self.focus_coalesce_ms, 1, 60_000)
            && ms(self.geometry_coalesce_ms, 1, 60_000)
            && ms(self.settle_pass_ms, 1, 60_000)
            && ms(self.degraded_repair_interval_ms, 1_000, 3_600_000)
            && ms(self.probe_min_interval_ms, 1, 3_600_000)
            && ms(self.ax_messaging_timeout_ms, 10, 10_000)
            && ms(self.os_source_start_timeout_ms, 100, 60_000)
    }

    pub fn probe_min_interval(&self) -> Duration {
        Duration::from_millis(self.probe_min_interval_ms)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardianEvent {
    /// A different application / window became foreground.
    ForegroundChanged,
    /// macOS: the active Space changed (native fullscreen windows own a Space).
    SpaceChanged,
    /// A window was minimised or restored, or the focused/main window changed.
    WindowStateChanged,
    /// The frontmost window was resized or moved.
    GeometryChanged,
    /// Shutdown: wakes the worker so it can exit without a timeout.
    Stop,
}

impl GuardianEvent {
    /// Events that can bury the pill in the z-order or change which window owns
    /// the screen also drive `enforce_visibility_invariant`.
    pub fn drives_invariant(self) -> bool {
        matches!(
            self,
            GuardianEvent::ForegroundChanged
                | GuardianEvent::SpaceChanged
                | GuardianEvent::WindowStateChanged
        )
    }

    fn coalesce_window_ms(self, timings: &GuardianTimings) -> u64 {
        match self {
            GuardianEvent::GeometryChanged => timings.geometry_coalesce_ms,
            _ => timings.focus_coalesce_ms,
        }
    }
}

/// What the worker must do now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Due {
    /// Re-evaluate fullscreen once and apply the session's fullscreen update.
    pub evaluate: bool,
    /// Also run the visibility invariant (and the Windows
    /// topmost / probe repairs).
    pub invariant: bool,
}

impl Due {
    pub fn any(self) -> bool {
        self.evaluate || self.invariant
    }
}

/// Merges event bursts into one evaluation and schedules the single settle pass.
#[derive(Debug)]
pub struct Coalescer {
    timings: GuardianTimings,
    evaluate_at: Option<Instant>,
    invariant: bool,
    settle_at: Option<Instant>,
}

impl Coalescer {
    pub fn new(timings: GuardianTimings) -> Coalescer {
        Coalescer {
            timings,
            evaluate_at: None,
            invariant: false,
            settle_at: None,
        }
    }

    pub fn note(&mut self, event: GuardianEvent, now: Instant) {
        if event == GuardianEvent::Stop {
            return;
        }
        // The deadline is fixed from the first event of a burst (never pushed
        // back), so a continuous stream still yields periodic evaluations.
        let at = after(now, event.coalesce_window_ms(&self.timings));
        self.evaluate_at = Some(match self.evaluate_at {
            Some(existing) if existing <= at => existing,
            _ => at,
        });
        if event.drives_invariant() {
            self.invariant = true;
            // Refreshed by each focus event so one pass follows the LAST of a burst.
            self.settle_at = Some(after(now, self.timings.settle_pass_ms));
        }
    }

    /// Earliest instant at which `take_due` will have work; `None` means the
    /// worker has nothing scheduled and must block without a timeout.
    pub fn next_deadline(&self) -> Option<Instant> {
        match (self.evaluate_at, self.settle_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    pub fn is_idle(&self) -> bool {
        self.evaluate_at.is_none() && self.settle_at.is_none()
    }

    pub fn take_due(&mut self, now: Instant) -> Due {
        let mut due = Due::default();
        if self.evaluate_at.is_some_and(|at| at <= now) {
            due.evaluate = true;
            due.invariant = self.invariant;
            self.evaluate_at = None;
            self.invariant = false;
        }
        if self.settle_at.is_some_and(|at| at <= now) {
            due.evaluate = true;
            due.invariant = true;
            self.settle_at = None;
        }
        due
    }
}

/// How long the worker may block: `None` is "until an event arrives" (the idle
/// case: no periodic wake). Only the degraded mode ever returns a fixed cadence.
pub fn next_wait(
    deadline: Option<Instant>,
    now: Instant,
    degraded: bool,
    timings: &GuardianTimings,
) -> Option<Duration> {
    match deadline {
        Some(deadline) => Some(deadline.saturating_duration_since(now)),
        None if degraded => Some(Duration::from_millis(timings.degraded_repair_interval_ms)),
        None => None,
    }
}

/// Admits at most one action per `min` interval.
#[derive(Debug)]
pub struct IntervalGate {
    min: Duration,
    last: Option<Instant>,
}

impl IntervalGate {
    pub const fn new(min: Duration) -> Self {
        Self { min, last: None }
    }

    pub fn admit(&mut self, now: Instant) -> bool {
        match self.last {
            Some(last) if now.saturating_duration_since(last) < self.min => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

/// Events that may wait for the worker at once. Every event is a hint to evaluate again and the
/// worker coalesces a burst into one evaluation, so an event that finds the channel full is
/// dropped: the worker has a full channel of hints to act on already. A stop is never lost this
/// way: [`crate::guardian_worker::GuardianHandle`] sets a flag the worker reads before it waits
/// again, and a full channel wakes it anyway.
pub const EVENT_QUEUE_CAPACITY: usize = 64;

/// The worker's channel, owned by the guardian generation that installed it. OS callbacks reach
/// the worker only through this slot, and only by naming their own generation
/// ([`notify_generation`]): a stopped guardian leaves nothing for a late callback to reach, a late
/// callback from an old generation can never wake a newer worker, and a new guardian can install
/// its own once the old one cleared the slot. The channel is bounded ([`EVENT_QUEUE_CAPACITY`]).
static EVENT_TX: Mutex<Option<(u64, SyncSender<GuardianEvent>)>> = Mutex::new(None);

/// Registers the worker's channel for `generation`. Returns false if one is already installed.
pub fn install_sender(generation: u64, tx: SyncSender<GuardianEvent>) -> bool {
    let mut slot = EVENT_TX.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_some() {
        return false;
    }
    *slot = Some((generation, tx));
    true
}

/// Removes the channel installed by `generation` (a no-op for any other generation).
pub fn clear_sender(generation: u64) {
    let mut slot = EVENT_TX.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_some_and(|(owner, _)| *owner == generation) {
        *slot = None;
    }
}

/// Send `event` to the worker of `generation` only: an event for a generation that is not the
/// installed one (a late callback from a stopped guardian, or a handle of an old one) is dropped,
/// and so is an event that finds the channel full. Called from OS callbacks (AppKit main thread,
/// WinEvent pump thread): it only tries to enqueue; it never blocks and never touches AX / window
/// APIs.
pub fn notify_generation(generation: u64, event: GuardianEvent) {
    let slot = EVENT_TX.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((owner, tx)) = slot.as_ref() {
        if *owner == generation {
            let _ = tx.try_send(event);
        }
    }
}

/// How many pixels each edge of a window may differ from the display's and still count as
/// covering it. A host input with no default: how close is "full screen" is the host's call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeTolerance(f64);

impl EdgeTolerance {
    /// `px` must be finite and within `0.0..=64.0`; anything else is refused (`None`).
    pub fn new(px: f64) -> Option<EdgeTolerance> {
        if px.is_finite() && (0.0..=64.0).contains(&px) {
            Some(EdgeTolerance(px))
        } else {
            None
        }
    }

    pub fn px(self) -> f64 {
        self.0
    }
}

/// Two rectangles `[left, top, right, bottom]` coincide when every edge differs by at most the
/// host's `tolerance`.
pub fn rect_covers_display(window: [f64; 4], display: [f64; 4], tolerance: EdgeTolerance) -> bool {
    window
        .iter()
        .zip(display.iter())
        .all(|(window_edge, display_edge)| (window_edge - display_edge).abs() <= tolerance.px())
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: GuardianTimings = GuardianTimings {
        focus_coalesce_ms: 40,
        geometry_coalesce_ms: 150,
        settle_pass_ms: 500,
        degraded_repair_interval_ms: 30_000,
        probe_min_interval_ms: 30_000,
        ax_messaging_timeout_ms: 100,
        os_source_start_timeout_ms: 3_000,
    };
    const FOCUS: Duration = Duration::from_millis(40);
    const GEOMETRY: Duration = Duration::from_millis(150);
    const SETTLE: Duration = Duration::from_millis(500);

    #[test]
    fn idle_coalescer_has_no_deadline() {
        let c = Coalescer::new(T);
        assert!(c.is_idle());
        assert!(c.next_deadline().is_none());
        assert_eq!(next_wait(None, Instant::now(), false, &T), None);
        assert_eq!(
            next_wait(None, Instant::now(), true, &T),
            Some(Duration::from_millis(30_000))
        );
    }

    #[test]
    fn focus_event_runs_invariant_then_settle_pass() {
        let mut c = Coalescer::new(T);
        let t0 = Instant::now();
        c.note(GuardianEvent::ForegroundChanged, t0);
        let first = c.take_due(t0 + FOCUS);
        assert!(first.evaluate && first.invariant);
        let settle = c.take_due(t0 + SETTLE);
        assert!(settle.evaluate && settle.invariant);
        assert!(c.is_idle());
    }

    #[test]
    fn geometry_event_does_not_run_invariant() {
        let mut c = Coalescer::new(T);
        let t0 = Instant::now();
        c.note(GuardianEvent::GeometryChanged, t0);
        let due = c.take_due(t0 + GEOMETRY);
        assert!(due.evaluate && !due.invariant);
    }

    #[test]
    fn rect_tolerance_is_the_hosts() {
        let two = EdgeTolerance::new(2.0).unwrap();
        assert!(rect_covers_display(
            [0.0, 0.0, 100.0, 50.0],
            [2.0, -2.0, 98.0, 52.0],
            two
        ));
        assert!(!rect_covers_display(
            [0.0, 0.0, 100.0, 50.0],
            [3.0, 0.0, 100.0, 50.0],
            two
        ));
        assert!(EdgeTolerance::new(f64::NAN).is_none());
        assert!(EdgeTolerance::new(65.0).is_none());
    }

    #[test]
    fn the_sender_slot_belongs_to_one_generation_at_a_time() {
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        assert!(install_sender(7, tx));
        let (other_tx, _other_rx) = std::sync::mpsc::sync_channel(8);
        assert!(!install_sender(8, other_tx));
        notify_generation(7, GuardianEvent::ForegroundChanged);
        assert_eq!(rx.try_recv(), Ok(GuardianEvent::ForegroundChanged));
        // A callback naming another generation reaches nobody.
        notify_generation(8, GuardianEvent::SpaceChanged);
        assert!(rx.try_recv().is_err());
        // Another generation cannot clear it; its owner can.
        clear_sender(8);
        notify_generation(7, GuardianEvent::GeometryChanged);
        assert_eq!(rx.try_recv(), Ok(GuardianEvent::GeometryChanged));
        clear_sender(7);
        notify_generation(7, GuardianEvent::Stop);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn the_interval_gate_admits_one_action_per_interval() {
        let mut gate = IntervalGate::new(Duration::from_secs(30));
        let t0 = Instant::now();
        assert!(gate.admit(t0));
        assert!(!gate.admit(t0 + Duration::from_secs(29)));
        assert!(gate.admit(t0 + Duration::from_secs(30)));
    }

    #[test]
    fn a_burst_of_events_is_one_evaluation_with_a_deadline_that_is_never_pushed_back() {
        let mut c = Coalescer::new(T);
        let t0 = Instant::now();
        c.note(GuardianEvent::GeometryChanged, t0);
        let first = c.next_deadline().unwrap();
        c.note(
            GuardianEvent::GeometryChanged,
            t0 + Duration::from_millis(100),
        );
        assert_eq!(c.next_deadline(), Some(first));
        assert!(!c.take_due(t0 + Duration::from_millis(149)).any());
        assert!(c.take_due(t0 + GEOMETRY).evaluate);
    }
}
