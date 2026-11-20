//! Delayed work with a generation token: a callback that runs after a delay, a settle sequence
//! that repeats a main-thread action at fixed delays (single flight), and a "report only when the
//! value changed" gate for diagnostics.
//!
//! A [`Generation`] is the host's `static` cancellation token. Every `schedule` takes a new
//! generation and a delayed callback runs only if its generation is still current when its delay
//! ends (checked and run under one lock), so a cancelled or superseded delay does nothing.
//! [`Generation::retire`] invalidates everything scheduled so far and refuses new work (call it at
//! shutdown); [`Generation::restart`] opens the next lifecycle and moves the epoch, so nothing
//! scheduled before it can run. Threads are not joined: a cancelled thread wakes once, sees the
//! newer generation and ends.
//!
//! Moved from HeardRight `pill_overlays.rs` (the delayed hint threads) and `pill/commands.rs`
//! (`AUX_REPOSITION_SETTLE_ACTIVE`, `schedule_visible_pill_aux_reposition`,
//! `LAST_LOGGED_PILL_HITBOX`). Every delay, count and name is the host's.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// The longest delay a callback or a settle step may have.
pub const MAX_DELAY: Duration = Duration::from_secs(60);
/// The most steps a settle sequence runs.
pub const MAX_SETTLE_STEPS: usize = 16;

struct GenState {
    /// The generation a callback must hold to run. Every `schedule`, `cancel`, `retire` and
    /// `restart` moves it, so a callback taken before any of them is stale.
    current: u64,
    /// A retired lifecycle accepts no new work (until `restart`).
    retired: bool,
}

/// A cancellation token for delayed work (declare as a `static`).
///
/// The check "is this callback still current" and the callback's effect happen under one lock
/// (`run_if_current`), so a `cancel`, `retire` or `restart` can never fall between the check and
/// the effect: when one of them returns, no stale callback is running or will run. The rules that
/// follow: a callback must not call `schedule`, `cancel`, `retire` or `restart` on the token that
/// runs it (the lock is not reentrant), and a thread that calls `cancel`, `retire` or `restart`
/// waits for a callback already running, so a host whose callback waits for that thread must call
/// from another one.
pub struct Generation(Mutex<GenState>);

impl Default for Generation {
    fn default() -> Self {
        Self::new()
    }
}

impl Generation {
    pub const fn new() -> Generation {
        Generation(Mutex::new(GenState {
            current: 0,
            retired: false,
        }))
    }

    fn lock(&self) -> MutexGuard<'_, GenState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Invalidate every delay scheduled so far. New work is still accepted.
    pub fn cancel(&self) {
        let mut state = self.lock();
        state.current = state.current.wrapping_add(1);
    }

    /// End this lifecycle (call it at shutdown): invalidate every delay scheduled so far and
    /// refuse new work (`schedule` returns `false`) until [`Generation::restart`].
    pub fn retire(&self) {
        let mut state = self.lock();
        state.current = state.current.wrapping_add(1);
        state.retired = true;
    }

    /// Begin a new lifecycle (call it when the pill is installed again): the epoch moves, so no
    /// callback taken in the previous lifecycle can run, and new work is accepted.
    pub fn restart(&self) {
        let mut state = self.lock();
        state.current = state.current.wrapping_add(1);
        state.retired = false;
    }

    /// Whether this lifecycle has been retired and not restarted.
    pub fn is_retired(&self) -> bool {
        self.lock().retired
    }

    /// Take a new generation; `None` in a retired lifecycle.
    fn next(&self) -> Option<u64> {
        let mut state = self.lock();
        if state.retired {
            return None;
        }
        state.current = state.current.wrapping_add(1);
        Some(state.current)
    }

    /// Take a new generation unless the lifecycle is retired or the sequence recorded in
    /// `running` (a generation of this token, `0` for none) is still current. On success
    /// `running` holds the new generation.
    fn begin_unless_running(&self, running: &AtomicU64) -> Option<u64> {
        let mut state = self.lock();
        if state.retired {
            return None;
        }
        let active = running.load(Ordering::SeqCst);
        if active != 0 && active == state.current {
            return None;
        }
        state.current = state.current.wrapping_add(1);
        running.store(state.current, Ordering::SeqCst);
        Some(state.current)
    }

    fn is_current(&self, generation: u64) -> bool {
        let state = self.lock();
        !state.retired && state.current == generation
    }

    /// Run `work` only if `generation` is still current, with the check and the effect under the
    /// same lock. Returns whether it ran.
    fn run_if_current(&self, generation: u64, work: impl FnOnce()) -> bool {
        let state = self.lock();
        if state.retired || state.current != generation {
            return false;
        }
        work();
        drop(state);
        true
    }

    /// Run `work` on a new thread after `delay` (at most [`MAX_DELAY`]) unless a later `schedule`,
    /// a `cancel`, a `retire` or a `restart` happened first. Returns whether the thread was
    /// started (`false` in a retired lifecycle).
    pub fn schedule(&'static self, delay: Duration, work: impl FnOnce() + Send + 'static) -> bool {
        let Some(generation) = self.next() else {
            return false;
        };
        let delay = delay.min(MAX_DELAY);
        std::thread::Builder::new()
            .name("pill-delayed".to_string())
            .spawn(move || {
                std::thread::sleep(delay);
                self.run_if_current(generation, work);
            })
            .is_ok()
    }
}

/// Repeats a main-thread action at fixed delays, one sequence at a time (declare as a `static`).
pub struct Settle {
    /// The generation of the sequence that is running, `0` when none is.
    running: AtomicU64,
    generation: Generation,
}

impl Default for Settle {
    fn default() -> Self {
        Self::new()
    }
}

impl Settle {
    pub const fn new() -> Settle {
        Settle {
            running: AtomicU64::new(0),
            generation: Generation::new(),
        }
    }

    /// Invalidate the running sequence: its remaining steps do not run, and a step already queued
    /// on the main thread checks this again when it runs there. A new sequence may start at once.
    pub fn cancel(&self) {
        self.generation.cancel();
    }

    /// End this lifecycle (call it at shutdown): like `cancel`, and `schedule` starts nothing
    /// until `restart`.
    pub fn retire(&self) {
        self.generation.retire();
    }

    /// Begin a new lifecycle: nothing queued before it can run, and `schedule` starts sequences.
    pub fn restart(&self) {
        self.generation.restart();
    }

    /// Start a sequence unless `needed()` is false, one is already running, or the lifecycle is
    /// retired: after each of `delays_ms` (at most [`MAX_SETTLE_STEPS`] entries, each at most
    /// [`MAX_DELAY`]) call `on_main(action)` with the host's main-thread scheduler (whose result is
    /// ignored, as before). The queued closure re-checks the sequence's generation on the main
    /// thread, under the lock, when it runs, so a `cancel`, `retire` or `restart` that came after
    /// the step was queued stops it. `action` must not call this token's methods.
    pub fn schedule(
        &'static self,
        delays_ms: &'static [u64],
        needed: impl FnOnce() -> bool,
        on_main: impl Fn(Box<dyn FnOnce() + Send + 'static>) + Send + 'static,
        action: impl Fn() + Send + Sync + 'static,
    ) {
        if !needed() {
            return;
        }
        let Some(generation) = self.generation.begin_unless_running(&self.running) else {
            return;
        };
        let action = Arc::new(action);
        let spawned = std::thread::Builder::new()
            .name("pill-settle".to_string())
            .spawn(move || {
                for delay_ms in delays_ms.iter().take(MAX_SETTLE_STEPS) {
                    std::thread::sleep(Duration::from_millis(*delay_ms).min(MAX_DELAY));
                    if !self.generation.is_current(generation) {
                        break;
                    }
                    let step = Arc::clone(&action);
                    on_main(Box::new(move || {
                        self.generation.run_if_current(generation, || step());
                    }));
                }
                let _ = self.running.compare_exchange(
                    generation,
                    0,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
            });
        if spawned.is_err() {
            let _ =
                self.running
                    .compare_exchange(generation, 0, Ordering::SeqCst, Ordering::SeqCst);
        }
    }
}

/// Remembers the last value reported so a repeat is not reported again (declare as a `static`).
pub struct LastValue<T: Copy + PartialEq>(Mutex<Option<T>>);

impl<T: Copy + PartialEq> Default for LastValue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + PartialEq> LastValue<T> {
    pub const fn new() -> LastValue<T> {
        LastValue(Mutex::new(None))
    }

    /// Store `next` and return whether it differs from the stored value.
    pub fn changed(&self, next: T) -> bool {
        let mut last = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if *last == Some(next) {
            false
        } else {
            *last = Some(next);
            true
        }
    }
}
