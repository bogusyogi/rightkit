//! The owner thread's command queue: bounded, with a side channel that is never dropped.
//!
//! Commands from the host (events, `show_*`, getters that need the owner) and the pointer events
//! the overlays report go through [`CmdQueue::push`], which refuses a command once
//! [`COMMAND_CAPACITY`] are waiting (the caller reports the drop once per burst, see
//! [`Push::Dropped`]). Three things never go through that bounded list, so a full queue can never
//! lose them:
//!
//! * [`Control::Shutdown`]: a flag. `shutdown` and a failed start must always reach the owner.
//! * [`Control::Fault`]: the first painter fault, kept until the owner takes it.
//! * [`Control::PumpIdle`]: one bit per surface (a painter says its frame pump can stop); it
//!   coalesces, so three bits are all it can ever hold.
//!
//! Two kinds of item also bypass the capacity ([`CmdQueue::push_reserved`]) because dropping them
//! would make the screen untrue: the result of host work that ran on a worker thread (each live
//! worker posts one result, and the number of live workers is capped process-wide by
//! `Limits::max_worker_threads`; a dropped "copy succeeded" would show a failure for a copy that
//! worked) and the single `Closed` each overlay forwarder reports when its window ended (three at
//! most per generation).
//!
//! The owner takes control items first (shutdown, then fault, then pump bits), then commands in
//! arrival order. Platform-free pure logic apart from the clock.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::clock::after_duration;
use crate::regions::Surface;

/// Commands that may wait for the owner at once. A burst beyond this is dropped, not queued.
pub(crate) const COMMAND_CAPACITY: usize = 1024;

/// Items that bypass the capacity and the drop policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Control {
    /// Destroy the overlays and exit.
    Shutdown,
    /// A painter panicked on an overlay thread: destroy this generation. `kind` is the payload's
    /// type, never its text.
    Fault {
        surface: Surface,
        kind: &'static str,
    },
    /// A painter finished its last frame: the frame pump of `Surface` may stop.
    PumpIdle(Surface),
}

/// What [`CmdQueue::push`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Push {
    Queued,
    /// The queue was full and the command was dropped. `first_of_burst` is true for the first drop
    /// since the queue last drained to half its capacity: report that one, count the rest.
    Dropped {
        first_of_burst: bool,
    },
    /// The owner has ended; nothing will ever read this queue.
    Closed,
}

/// What [`CmdQueue::next`] found.
#[derive(Debug)]
pub(crate) enum Next<T> {
    Control(Control),
    Item(T),
    Timeout,
    Closed,
}

struct State<T> {
    items: VecDeque<T>,
    shutdown: bool,
    fault: Option<(Surface, &'static str)>,
    pump_idle: [bool; 3],
    closed: bool,
    in_burst: bool,
}

pub(crate) struct CmdQueue<T> {
    state: Mutex<State<T>>,
    wake: Condvar,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn surface_at(index: usize) -> Surface {
    match index {
        0 => Surface::Pill,
        1 => Surface::Hint,
        _ => Surface::Clip,
    }
}

impl<T> CmdQueue<T> {
    pub(crate) fn new() -> CmdQueue<T> {
        CmdQueue {
            state: Mutex::new(State {
                items: VecDeque::new(),
                shutdown: false,
                fault: None,
                pump_idle: [false; 3],
                closed: false,
                in_burst: false,
            }),
            wake: Condvar::new(),
        }
    }

    /// Queue a command unless [`COMMAND_CAPACITY`] are already waiting.
    pub(crate) fn push(&self, item: T) -> Push {
        let mut state = lock(&self.state);
        if state.closed {
            return Push::Closed;
        }
        if state.items.len() >= COMMAND_CAPACITY {
            let first_of_burst = !state.in_burst;
            state.in_burst = true;
            return Push::Dropped { first_of_burst };
        }
        state.items.push_back(item);
        drop(state);
        self.wake.notify_one();
        Push::Queued
    }

    /// Queue the result of a host action that ran on a worker thread, or a forwarder's `Closed`.
    /// Not subject to the capacity (see the module docs); `false` once the owner has ended.
    pub(crate) fn push_reserved(&self, item: T) -> bool {
        let mut state = lock(&self.state);
        if state.closed {
            return false;
        }
        state.items.push_back(item);
        drop(state);
        self.wake.notify_one();
        true
    }

    /// Ask the owner to stop. Never dropped; idempotent.
    pub(crate) fn post_shutdown(&self) {
        lock(&self.state).shutdown = true;
        self.wake.notify_one();
    }

    /// Report a painter fault. The first one is kept; never dropped.
    pub(crate) fn post_fault(&self, surface: Surface, kind: &'static str) {
        {
            let mut state = lock(&self.state);
            if state.fault.is_none() {
                state.fault = Some((surface, kind));
            }
        }
        self.wake.notify_one();
    }

    /// A painter went idle: the frame pump of `surface` can stop. Coalesces; never dropped.
    pub(crate) fn post_pump_idle(&self, surface: Surface) {
        lock(&self.state).pump_idle[surface as usize] = true;
        self.wake.notify_one();
    }

    /// The owner ended: refuse everything from now on and forget what was waiting.
    pub(crate) fn close(&self) {
        {
            let mut state = lock(&self.state);
            state.closed = true;
            state.items.clear();
        }
        self.wake.notify_all();
    }

    /// Wait for the next item. `timeout` of `None` blocks until something arrives or the queue
    /// closes (the owner idles with no timer); `Some(Duration::ZERO)` only polls.
    pub(crate) fn next(&self, timeout: Option<Duration>) -> Next<T> {
        let deadline = timeout.map(|bound| after_duration(Instant::now(), bound));
        let mut state = lock(&self.state);
        loop {
            if state.shutdown {
                return Next::Control(Control::Shutdown);
            }
            if let Some((surface, kind)) = state.fault.take() {
                return Next::Control(Control::Fault { surface, kind });
            }
            if let Some(index) = state.pump_idle.iter().position(|pending| *pending) {
                state.pump_idle[index] = false;
                return Next::Control(Control::PumpIdle(surface_at(index)));
            }
            if let Some(item) = state.items.pop_front() {
                if state.items.len() <= COMMAND_CAPACITY / 2 {
                    state.in_burst = false;
                }
                return Next::Item(item);
            }
            if state.closed {
                return Next::Closed;
            }
            state = match deadline {
                None => self.wake.wait(state).unwrap_or_else(|e| e.into_inner()),
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Next::Timeout;
                    }
                    self.wake
                        .wait_timeout(state, deadline - now)
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
            };
        }
    }
}
