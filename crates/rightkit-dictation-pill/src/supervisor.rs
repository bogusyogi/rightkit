//! Supervised Windows lifecycle: explicit installation enables restart on dictation start.
//!
//! Shutdown invalidates an admission epoch before calling the driver. Every supervised installer
//! checks that epoch under the driver's lifecycle lock before admission, including after waits.
//! A stale restart therefore cannot create a generation after deliberate shutdown has completed.
//!
//! One restart worker owns one bounded pending state event. Newer mode events replace older ones;
//! Voice updates never replace a mode. Replay and ordinary enqueue share a short state lock, so a
//! delayed Recording cannot overtake Processing, cancellation status, Idle or Hide.
//!
//! Lock order is STATE -> driver LIFE -> command queue. Driver admission only reads EPOCH and
//! never takes STATE. Slow ensure/shutdown, thread spawning and diagnostics run outside STATE;
//! driver enqueue defers diagnostics so reentrant host sinks never run under this lock.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use crate::config;
use crate::driver::{self, Install};
use crate::{PillEvent, Visibility};

// Even epochs disable restart; odd epochs enable it. Every explicit lifecycle request invalidates
// older admissions. Epoch changes are serialized by STATE, while driver admission reads atomically.
static EPOCH: AtomicU64 = AtomicU64::new(0);
static STATE: Mutex<State> = Mutex::new(State::new());

struct Pending {
    event: PillEvent,
    vis: Visibility,
    revision: u64,
}

struct State {
    restarting: bool,
    revision: u64,
    pending: Option<Pending>,
}

impl State {
    const fn new() -> Self {
        Self {
            restarting: false,
            revision: 0,
            pending: None,
        }
    }

    fn retain(&mut self, event: PillEvent, vis: Visibility) {
        self.revision = self.revision.wrapping_add(1);
        self.pending = Some(Pending {
            event,
            vis,
            revision: self.revision,
        });
    }

    fn release(&mut self) {
        self.pending = None;
        self.restarting = false;
    }

    // A failed attempt never retries its own event. A newer start received during that attempt
    // is a distinct request, and must not be stranded when worker ownership is released.
    fn newer_start(&self, attempted: u64) -> bool {
        self.pending.as_ref().is_some_and(|pending| {
            pending.revision != attempted && starts_dictation(&pending.event)
        })
    }
}

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|error| error.into_inner())
}

fn starts_dictation(event: &PillEvent) -> bool {
    matches!(event, PillEvent::Starting | PillEvent::Recording)
}

fn enabled(epoch: u64) -> bool {
    epoch & 1 == 1
}

fn change_epoch(wanted: bool) -> u64 {
    // Caller owns STATE. Keeping request identity separate from pending-event revision also
    // prevents a stale worker from replaying an event into a later explicit installation.
    let old = EPOCH.load(Ordering::SeqCst);
    let epoch = old.wrapping_add(if enabled(old) == wanted { 2 } else { 1 });
    EPOCH.store(epoch, Ordering::SeqCst);
    epoch
}

/// Explicitly install (bounded), enabling future restart. An overlapping shutdown invalidates
/// this call's admission, including an admission delayed behind another installer.
pub fn ensure_installed_supervised() -> Install {
    let epoch = {
        let mut state = state();
        state.pending = None;
        change_epoch(true)
    };
    let install = driver::ensure_installed_if(|| EPOCH.load(Ordering::SeqCst) == epoch);
    if !install.is_running() {
        config::report(
            "ensure_installed_failed",
            &format!("result={}", install.code()),
        );
    }
    install
}

/// Deliberately stop (bounded). Older supervised installers cannot admit another generation.
pub fn shutdown_supervised() -> bool {
    {
        let mut state = state();
        change_epoch(false);
        state.pending = None;
    }
    driver::shutdown()
}

/// Apply an event without waiting for startup or shutdown. While restart is in flight, retain
/// only the newest bounded mode event; high-frequency Voice updates remain independent.
pub fn apply_event_supervised(event: &PillEvent, vis: Visibility) {
    // Bound user-provided labels before taking STATE; retained memory never grows with event rate.
    let event = driver::bounded_event(event);
    let (spawn, dispatch) = {
        let mut state = state();
        if matches!(event, PillEvent::Voice { .. }) {
            (false, Some(driver::apply_event_deferred(&event, vis)))
        } else if enabled(EPOCH.load(Ordering::SeqCst))
            && ((state.restarting && state.pending.is_some())
                || (starts_dictation(&event) && !driver::is_running()))
        {
            state.retain(event, vis);
            let spawn = !state.restarting;
            state.restarting = true;
            (spawn, None)
        } else {
            (false, Some(driver::apply_event_deferred(&event, vis)))
        }
    };
    if let Some(dispatch) = dispatch {
        dispatch.report();
    }
    if spawn {
        restart_off_thread();
    }
}

fn restart_off_thread() {
    let name = config::try_get().map_or_else(
        || "pill-restart".to_string(),
        |config| format!("{}-restart", config.thread_name),
    );
    if std::thread::Builder::new()
        .name(name)
        .spawn(restart)
        .is_err()
    {
        state().release();
        config::report("restart_failed", "reason=spawn_failed");
    }
}

fn restart() {
    loop {
        let (epoch, attempted) = {
            let mut state = state();
            let epoch = EPOCH.load(Ordering::SeqCst);
            if !enabled(epoch) || state.pending.is_none() {
                state.release();
                return;
            }
            (epoch, state.pending.as_ref().unwrap().revision)
        };
        let install = driver::ensure_installed_if(|| EPOCH.load(Ordering::SeqCst) == epoch);
        // Reports may synchronously reenter any supervised entry point. Ownership remains held,
        // but STATE does not: new events can replace pending, and shutdown can invalidate epoch.
        if install.is_running() {
            config::report("restart", "result=ok");
        } else {
            config::report("restart_failed", &format!("result={}", install.code()));
        }
        let dispatch = {
            let mut state = state();
            let current = EPOCH.load(Ordering::SeqCst);
            if current != epoch {
                // An explicit install may have enabled a newer epoch while this worker was
                // finishing. Hand it the same single worker only if a new start requested it.
                if enabled(current) && state.pending.is_some() {
                    continue;
                }
                state.release();
                return;
            }
            if !install.is_running() {
                if state.newer_start(attempted) {
                    continue;
                }
                state.release();
                return;
            }
            let dispatch = state
                .pending
                .take()
                .map(|pending| driver::apply_event_deferred(&pending.event, pending.vis));
            // This release and replay enqueue are atomic with respect to every incoming mode
            // event. No caller can stash an event between taking pending and dropping ownership.
            state.release();
            dispatch
        };
        if let Some(dispatch) = dispatch {
            dispatch.report();
        }
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_event_replaces_pending_recording() {
        let mut state = State::new();
        state.retain(PillEvent::Recording, Visibility::Show);
        let attempted = state.revision;
        state.retain(
            PillEvent::Processing { cancellable: false },
            Visibility::Hide,
        );
        assert!(!state.newer_start(attempted));
        let pending = state.pending.unwrap();
        assert_eq!(pending.event, PillEvent::Processing { cancellable: false });
        assert_eq!(pending.vis, Visibility::Hide);
    }

    #[test]
    fn only_new_start_requests_retry_after_failure() {
        let mut state = State::new();
        state.retain(PillEvent::Starting, Visibility::Show);
        let attempted = state.revision;
        assert!(!state.newer_start(attempted));
        state.retain(PillEvent::Recording, Visibility::Keep);
        assert!(state.newer_start(attempted));
        state.retain(PillEvent::Idle, Visibility::Keep);
        assert!(!state.newer_start(attempted));
    }

    #[test]
    fn release_clears_event_and_worker_ownership_together() {
        let mut state = State::new();
        state.restarting = true;
        state.retain(PillEvent::Starting, Visibility::Show);
        state.release();
        assert!(!state.restarting);
        assert!(state.pending.is_none());
        state.retain(PillEvent::Recording, Visibility::Show);
        assert!(!state.restarting);
        assert!(state.pending.is_some());
    }
}
