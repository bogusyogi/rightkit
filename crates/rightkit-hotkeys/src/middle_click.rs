//! Pure middle-click decision machinery for the macOS middle-mouse tap.
//!
//! An event-tap callback must return fast: the OS disables a tap whose callback blocks, and an
//! Accessibility lookup (is the click over a link?) can block for a long time. So the callback
//! never looks anything up. A middle press is **swallowed** at once and the point is handed to one
//! worker thread. The worker answers:
//!
//! * not a link: toggle (the press stays swallowed, the release is swallowed too);
//! * a link: replay the click (a synthetic press, then release) carrying a marker the tap
//!   recognises and passes through, so the browser still opens the link in a new tab.
//!
//! [`MiddleClickGate`] is the shared state between the callback and the worker. Everything here is
//! platform independent so the state machine is tested on every host.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::decision::other_mouse_down_is_middle;

/// Telemetry name for a non-middle `OtherMouseDown` that the tap passes through.
pub const DIAG_OTHER_BUTTON: &str = "middle_mouse_other_button";

/// While a lookup is outstanding a second press passes through untouched (fail open). After this
/// long the lookup is presumed hung and a new press starts a fresh one.
pub const DECIDE_STALE: Duration = Duration::from_millis(1_500);

#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    Idle,
    Deciding {
        generation: u64,
        x: f64,
        y: f64,
        since: Instant,
        up_seen: bool,
    },
    /// Decided "toggle"; the physical release has not arrived yet and must be swallowed.
    Toggled,
    /// Decided "link" and the press was replayed; the physical release must be replayed too.
    Replaying {
        x: f64,
        y: f64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DownAction {
    Pass,
    Lookup { generation: u64, x: f64, y: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UpAction {
    Pass,
    Swallow,
    SwallowAndReplayUp { x: f64, y: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Resolution {
    /// The lookup is stale (reset, or a newer press took over): do nothing.
    Ignore,
    Toggle,
    Replay {
        x: f64,
        y: f64,
        with_up: bool,
    },
}

struct Inner {
    state: State,
    next_generation: u64,
}

/// State shared by the tap callback and the lookup worker. Locks are held only for a few field
/// updates, never across a lookup, so the callback never waits on the worker.
pub struct MiddleClickGate {
    inner: Mutex<Inner>,
}

impl Default for MiddleClickGate {
    fn default() -> Self {
        Self::new()
    }
}

impl MiddleClickGate {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                state: State::Idle,
                next_generation: 0,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn on_down(&self, now: Instant, x: f64, y: f64) -> DownAction {
        let mut inner = self.lock();
        if let State::Deciding { since, .. } = inner.state {
            if now.saturating_duration_since(since) < DECIDE_STALE {
                return DownAction::Pass;
            }
        }
        inner.next_generation += 1;
        let generation = inner.next_generation;
        inner.state = State::Deciding {
            generation,
            x,
            y,
            since: now,
            up_seen: false,
        };
        DownAction::Lookup { generation, x, y }
    }

    pub fn on_up(&self) -> UpAction {
        let mut inner = self.lock();
        match inner.state {
            State::Idle => UpAction::Pass,
            State::Deciding {
                generation,
                x,
                y,
                since,
                up_seen,
            } => {
                if up_seen {
                    // The release of a later press that passed through while deciding.
                    UpAction::Pass
                } else {
                    inner.state = State::Deciding {
                        generation,
                        x,
                        y,
                        since,
                        up_seen: true,
                    };
                    UpAction::Swallow
                }
            }
            State::Toggled => {
                inner.state = State::Idle;
                UpAction::Swallow
            }
            State::Replaying { x, y } => {
                inner.state = State::Idle;
                UpAction::SwallowAndReplayUp { x, y }
            }
        }
    }

    /// The worker's answer for the press numbered `generation`.
    pub fn resolve(&self, generation: u64, is_link: bool) -> Resolution {
        let mut inner = self.lock();
        let State::Deciding {
            generation: current,
            x,
            y,
            up_seen,
            ..
        } = inner.state
        else {
            return Resolution::Ignore;
        };
        if current != generation {
            return Resolution::Ignore;
        }
        if is_link {
            inner.state = if up_seen {
                State::Idle
            } else {
                State::Replaying { x, y }
            };
            Resolution::Replay {
                x,
                y,
                with_up: up_seen,
            }
        } else {
            inner.state = if up_seen { State::Idle } else { State::Toggled };
            Resolution::Toggle
        }
    }

    /// Forget any click in flight (tap disabled by the OS, tap torn down, worker gone).
    pub fn reset(&self) {
        self.lock().state = State::Idle;
    }

    pub fn is_idle(&self) -> bool {
        self.lock().state == State::Idle
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEdge {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Swallow,
}

/// Work for the lookup worker.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Job {
    Lookup { generation: u64, x: f64, y: f64 },
    ReplayUp { x: f64, y: f64 },
}

/// The tap-callback side. Does only atomic and short-lock work plus one non-blocking `submit`.
/// `submit` returns `false` when the worker is gone, which makes the click pass through.
#[allow(clippy::too_many_arguments)]
pub fn handle_other_mouse(
    enabled: &AtomicBool,
    gate: &MiddleClickGate,
    edge: MouseEdge,
    button: i64,
    location: (f64, f64),
    now: Instant,
    submit: &dyn Fn(Job) -> bool,
    diag: &dyn Fn(&str, &str),
) -> Verdict {
    match edge {
        MouseEdge::Down => {
            if !enabled.load(Ordering::SeqCst) {
                return Verdict::Pass;
            }
            if !other_mouse_down_is_middle(button) {
                diag(DIAG_OTHER_BUTTON, &format!("button={button}"));
                return Verdict::Pass;
            }
            match gate.on_down(now, location.0, location.1) {
                DownAction::Pass => Verdict::Pass,
                DownAction::Lookup { generation, x, y } => {
                    if submit(Job::Lookup { generation, x, y }) {
                        Verdict::Swallow
                    } else {
                        gate.reset();
                        Verdict::Pass
                    }
                }
            }
        }
        MouseEdge::Up => {
            // Not gated on `enabled`: a release that belongs to an already-swallowed press must
            // still be accounted for after the tap is disabled mid-click.
            if !other_mouse_down_is_middle(button) {
                return Verdict::Pass;
            }
            match gate.on_up() {
                UpAction::Pass => Verdict::Pass,
                UpAction::Swallow => Verdict::Swallow,
                UpAction::SwallowAndReplayUp { x, y } => {
                    let _ = submit(Job::ReplayUp { x, y });
                    Verdict::Swallow
                }
            }
        }
    }
}

/// The worker's side: what a job does, with the platform pieces injected.
pub struct JobActions {
    /// Accessibility hit test; may block. Runs only on the worker.
    pub is_link: Box<dyn FnMut(f64, f64) -> bool + Send>,
    /// Post a marked synthetic middle press (`true`) or release (`false`) at a point.
    pub post: Box<dyn FnMut(bool, f64, f64) + Send>,
    pub toggle: Arc<dyn Fn() + Send + Sync + 'static>,
}

pub fn run_job(gate: &MiddleClickGate, job: Job, actions: &mut JobActions) {
    match job {
        Job::Lookup { generation, x, y } => {
            let link = (actions.is_link)(x, y);
            match gate.resolve(generation, link) {
                Resolution::Ignore => {}
                Resolution::Toggle => {
                    // A slow toggle callback must not hold up the next lookup.
                    let cb = actions.toggle.clone();
                    std::thread::spawn(move || cb());
                }
                Resolution::Replay { x, y, with_up } => {
                    (actions.post)(true, x, y);
                    if with_up {
                        (actions.post)(false, x, y);
                    }
                }
            }
        }
        Job::ReplayUp { x, y } => (actions.post)(false, x, y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn at(ms: u64, base: Instant) -> Instant {
        base + Duration::from_millis(ms)
    }

    fn lookup(action: DownAction) -> u64 {
        match action {
            DownAction::Lookup { generation, .. } => generation,
            other => panic!("expected a lookup, got {other:?}"),
        }
    }

    #[test]
    fn press_is_deferred_not_decided_in_the_callback() {
        let gate = MiddleClickGate::new();
        let now = Instant::now();
        let g = lookup(gate.on_down(now, 1.0, 2.0));
        assert_eq!(gate.on_up(), UpAction::Swallow);
        assert_eq!(
            gate.resolve(g, false),
            Resolution::Toggle,
            "release already seen: nothing left to swallow"
        );
        assert!(gate.is_idle());
    }

    #[test]
    fn toggle_swallows_a_release_that_comes_after_the_decision() {
        let gate = MiddleClickGate::new();
        let g = lookup(gate.on_down(Instant::now(), 1.0, 2.0));
        assert_eq!(gate.resolve(g, false), Resolution::Toggle);
        assert!(!gate.is_idle());
        assert_eq!(gate.on_up(), UpAction::Swallow);
        assert!(gate.is_idle());
        assert_eq!(gate.on_up(), UpAction::Pass);
    }

    #[test]
    fn link_replays_press_and_release_when_release_came_first() {
        let gate = MiddleClickGate::new();
        let g = lookup(gate.on_down(Instant::now(), 5.0, 6.0));
        assert_eq!(gate.on_up(), UpAction::Swallow);
        assert_eq!(
            gate.resolve(g, true),
            Resolution::Replay {
                x: 5.0,
                y: 6.0,
                with_up: true
            }
        );
        assert!(gate.is_idle());
    }

    #[test]
    fn link_replays_the_release_later_when_it_arrives_after_the_decision() {
        let gate = MiddleClickGate::new();
        let g = lookup(gate.on_down(Instant::now(), 5.0, 6.0));
        assert_eq!(
            gate.resolve(g, true),
            Resolution::Replay {
                x: 5.0,
                y: 6.0,
                with_up: false
            }
        );
        assert_eq!(
            gate.on_up(),
            UpAction::SwallowAndReplayUp { x: 5.0, y: 6.0 }
        );
        assert!(gate.is_idle());
    }

    #[test]
    fn second_press_while_deciding_passes_and_its_release_passes() {
        let gate = MiddleClickGate::new();
        let t = Instant::now();
        let g = lookup(gate.on_down(t, 1.0, 1.0));
        assert_eq!(gate.on_up(), UpAction::Swallow);
        assert_eq!(gate.on_down(at(5, t), 1.0, 1.0), DownAction::Pass);
        assert_eq!(gate.on_up(), UpAction::Pass);
        assert_eq!(gate.resolve(g, false), Resolution::Toggle);
    }

    #[test]
    fn hung_lookup_is_taken_over_and_its_late_answer_ignored() {
        let gate = MiddleClickGate::new();
        let t = Instant::now();
        let old = lookup(gate.on_down(t, 1.0, 1.0));
        assert_eq!(gate.on_down(at(1_000, t), 1.0, 1.0), DownAction::Pass);
        let new = lookup(gate.on_down(at(2_000, t), 2.0, 2.0));
        assert_ne!(old, new);
        assert_eq!(gate.resolve(old, false), Resolution::Ignore);
        assert_eq!(gate.resolve(new, false), Resolution::Toggle);
    }

    #[test]
    fn reset_drops_the_click_in_flight() {
        let gate = MiddleClickGate::new();
        let g = lookup(gate.on_down(Instant::now(), 1.0, 1.0));
        gate.reset();
        assert!(gate.is_idle());
        assert_eq!(gate.resolve(g, true), Resolution::Ignore);
        assert_eq!(gate.on_up(), UpAction::Pass);
    }

    fn middle_down(
        enabled: &AtomicBool,
        gate: &MiddleClickGate,
        button: i64,
        submit: &dyn Fn(Job) -> bool,
        diag: &dyn Fn(&str, &str),
    ) -> Verdict {
        handle_other_mouse(
            enabled,
            gate,
            MouseEdge::Down,
            button,
            (10.0, 20.0),
            Instant::now(),
            submit,
            diag,
        )
    }

    #[test]
    fn non_middle_button_passes_and_reports_its_number() {
        let enabled = AtomicBool::new(true);
        let gate = MiddleClickGate::new();
        let lines = Mutex::new(Vec::<(String, String)>::new());
        let diag = |k: &str, d: &str| lines.lock().unwrap().push((k.into(), d.into()));
        let submitted = Mutex::new(0);
        let submit = |_: Job| {
            *submitted.lock().unwrap() += 1;
            true
        };
        assert_eq!(
            middle_down(&enabled, &gate, 3, &submit, &diag),
            Verdict::Pass
        );
        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, "middle_mouse_other_button");
        assert_eq!(lines[0].1, "button=3");
        assert_eq!(*submitted.lock().unwrap(), 0);
        assert!(gate.is_idle());
    }

    #[test]
    fn disabled_tap_passes_without_diagnostics_or_work() {
        let enabled = AtomicBool::new(false);
        let gate = MiddleClickGate::new();
        let calls = Mutex::new(0);
        let diag = |_: &str, _: &str| *calls.lock().unwrap() += 1;
        let submit = |_: Job| {
            *calls.lock().unwrap() += 1;
            true
        };
        assert_eq!(
            middle_down(&enabled, &gate, 3, &submit, &diag),
            Verdict::Pass
        );
        assert_eq!(
            middle_down(&enabled, &gate, 2, &submit, &diag),
            Verdict::Pass
        );
        assert_eq!(*calls.lock().unwrap(), 0);
    }

    #[test]
    fn middle_press_swallows_and_only_submits_never_looks_up() {
        let enabled = AtomicBool::new(true);
        let gate = MiddleClickGate::new();
        let jobs = Mutex::new(Vec::<Job>::new());
        let submit = |j: Job| {
            jobs.lock().unwrap().push(j);
            true
        };
        let verdict = middle_down(&enabled, &gate, 2, &submit, &|_, _| {});
        assert_eq!(verdict, Verdict::Swallow);
        let jobs = jobs.lock().unwrap();
        assert_eq!(jobs.len(), 1);
        assert!(matches!(jobs[0], Job::Lookup { x, y, .. } if x == 10.0 && y == 20.0));
    }

    #[test]
    fn dead_worker_fails_open() {
        let enabled = AtomicBool::new(true);
        let gate = MiddleClickGate::new();
        let verdict = middle_down(&enabled, &gate, 2, &|_| false, &|_, _| {});
        assert_eq!(verdict, Verdict::Pass);
        assert!(gate.is_idle(), "no half-open click is left behind");
    }

    #[test]
    fn release_after_disable_is_still_accounted_for() {
        let enabled = AtomicBool::new(true);
        let gate = MiddleClickGate::new();
        assert_eq!(
            middle_down(&enabled, &gate, 2, &|_| true, &|_, _| {}),
            Verdict::Swallow
        );
        enabled.store(false, Ordering::SeqCst);
        let up = handle_other_mouse(
            &enabled,
            &gate,
            MouseEdge::Up,
            2,
            (0.0, 0.0),
            Instant::now(),
            &|_| true,
            &|_, _| {},
        );
        assert_eq!(up, Verdict::Swallow);
    }

    fn actions(link: bool) -> (JobActions, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel();
        let tx_post = tx.clone();
        let tx_toggle = Mutex::new(tx);
        let actions = JobActions {
            is_link: Box::new(move |_, _| link),
            post: Box::new(move |down, _, _| {
                tx_post
                    .send(if down { "down" } else { "up" }.into())
                    .unwrap()
            }),
            toggle: Arc::new(move || {
                tx_toggle.lock().unwrap().send("toggle".into()).unwrap();
            }),
        };
        (actions, rx)
    }

    #[test]
    fn worker_toggles_for_a_non_link() {
        let gate = MiddleClickGate::new();
        let (mut a, rx) = actions(false);
        let generation = lookup(gate.on_down(Instant::now(), 1.0, 1.0));
        run_job(
            &gate,
            Job::Lookup {
                generation,
                x: 1.0,
                y: 1.0,
            },
            &mut a,
        );
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), "toggle");
    }

    #[test]
    fn worker_replays_down_then_up_for_a_link() {
        let gate = MiddleClickGate::new();
        let (mut a, rx) = actions(true);
        let generation = lookup(gate.on_down(Instant::now(), 1.0, 1.0));
        run_job(
            &gate,
            Job::Lookup {
                generation,
                x: 1.0,
                y: 1.0,
            },
            &mut a,
        );
        assert_eq!(rx.try_recv().unwrap(), "down");
        assert!(rx.try_recv().is_err(), "release has not arrived yet");
        assert_eq!(
            gate.on_up(),
            UpAction::SwallowAndReplayUp { x: 1.0, y: 1.0 }
        );
        run_job(&gate, Job::ReplayUp { x: 1.0, y: 1.0 }, &mut a);
        assert_eq!(rx.try_recv().unwrap(), "up");
    }

    #[test]
    fn worker_ignores_a_stale_lookup() {
        let gate = MiddleClickGate::new();
        let (mut a, rx) = actions(true);
        let generation = lookup(gate.on_down(Instant::now(), 1.0, 1.0));
        gate.reset();
        run_job(
            &gate,
            Job::Lookup {
                generation,
                x: 1.0,
                y: 1.0,
            },
            &mut a,
        );
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }
}
