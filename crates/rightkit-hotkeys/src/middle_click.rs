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
