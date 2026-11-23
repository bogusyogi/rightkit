//! Coordination between a delivery's background clipboard restore and the next delivery, bound to
//! one delivery by a generation number and to the operating system's clipboard change count.
//!
//! Rules:
//!
//! * Every delivery takes a [`Generation`] from [`begin_delivery`]. Generations only grow.
//! * this delivery's own clipboard write (the staged transcript) is recorded with the generation and
//!   the OS change count read right after the write ([`record_stage`]). The write is owned by
//!   the newest recorded generation only.
//! * A restore worker may restore the prior clipboard only if (a) its generation still owns the
//!   recorded write, and (b) the OS change count still matches what was recorded
//!   ([`restore_verdict`]). Anything else leaves the clipboard alone and the caller reports why.
//! * Flags are per generation. A worker finishing, timing out or being waited on can only touch
//!   its own generation's state; the supersede signal is a high-water mark that is never cleared
//!   by a timeout, so no waiter can wipe another generation's state.
//!
//! About the change count on Windows: delayed rendering of our own staged text (the target
//! reading the clipboard during paste) is the one write allowed to advance the sequence number.
//! So on Windows an advance is accepted only while the clipboard window still reports that it
//! owns the clipboard (no `WM_DESTROYCLIPBOARD` seen), and `rightkit-automation`'s
//! `restore_if_unchanged` then re-verifies owner and exact rendered sequence under
//! `OpenClipboard`. On macOS promised data provision does not change `changeCount`, so equality
//! is required exactly. See [`decide_restore`].

use std::collections::BTreeSet;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use rightkit_transcript_library::delivery::elapsed_ms;

/// One delivery's identity for clipboard ownership. Copy; compare only through this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Generation(u64);

impl Generation {
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Whether a restore may run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreVerdict {
    Proceed,
    /// A newer delivery wrote the clipboard (or this one never did): the worker is stale.
    StaleGeneration,
    /// The OS change count or ownership shows someone else wrote since this delivery's write.
    ClipboardChanged,
    /// The change count could not be read: leave the clipboard alone.
    CountUnreadable,
}

impl RestoreVerdict {
    pub fn code(self) -> &'static str {
        match self {
            RestoreVerdict::Proceed => "proceed",
            RestoreVerdict::StaleGeneration => "stale_generation",
            RestoreVerdict::ClipboardChanged => "clipboard_changed",
            RestoreVerdict::CountUnreadable => "count_unreadable",
        }
    }
}

/// Pure restore decision.
///
/// * `owner`: the generation that owns the recorded write, with its recorded change count.
/// * `own_render_advances`: true where our own render may advance the count (Windows).
/// * `ownership_lost`: the clipboard owner window was told another writer took over.
pub fn decide_restore(
    generation: Generation,
    owner: Option<(Generation, u64)>,
    now_count: Option<u64>,
    ownership_lost: bool,
    own_render_advances: bool,
) -> RestoreVerdict {
    let Some((owner_generation, recorded)) = owner else {
        return RestoreVerdict::StaleGeneration;
    };
    if owner_generation != generation {
        return RestoreVerdict::StaleGeneration;
    }
    let Some(now) = now_count else {
        return RestoreVerdict::CountUnreadable;
    };
    if now == recorded {
        return if ownership_lost {
            RestoreVerdict::ClipboardChanged
        } else {
            RestoreVerdict::Proceed
        };
    }
    if own_render_advances && now > recorded && !ownership_lost {
        return RestoreVerdict::Proceed;
    }
    RestoreVerdict::ClipboardChanged
}

struct Inner {
    next: u64,
    /// The newest recorded this delivery clipboard write.
    owner: Option<(Generation, u64)>,
    /// Generations whose background restore is still running.
    pending: BTreeSet<u64>,
    /// Every generation below this has been superseded by a newer delivery. Monotonic.
    superseded_below: u64,
}

/// The restore coordination state. The process uses one instance (the free functions below);
/// tests build their own.
pub struct RestoreState {
    inner: Mutex<Inner>,
    idle: Condvar,
}

impl RestoreState {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                next: 1,
                owner: None,
                pending: BTreeSet::new(),
                superseded_below: 0,
            }),
            idle: Condvar::new(),
        }
    }

    pub fn begin_delivery(&self) -> Generation {
        match self.inner.lock() {
            Ok(mut inner) => {
                let generation = Generation(inner.next);
                inner.next += 1;
                generation
            }
            // A poisoned lock yields a generation no state will ever own: it can never restore.
            Err(_) => Generation(u64::MAX),
        }
    }

    /// Record this delivery's own clipboard write: the OS change count read right after it.
    pub fn record_stage(&self, generation: Generation, change_count: u64) {
        if let Ok(mut inner) = self.inner.lock() {
            let newer = inner.owner.is_none_or(|(owner, _)| owner < generation);
            if newer {
                inner.owner = Some((generation, change_count));
            }
        }
    }

    pub fn mark_pending(&self, generation: Generation) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.pending.insert(generation.0);
        }
    }

    /// The generation's restore finished, was skipped, or could not be spawned. Touches only
    /// that generation.
    pub fn finish(&self, generation: Generation) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.pending.remove(&generation.0);
        }
        self.idle.notify_all();
    }

    /// Mark every older generation superseded and wait (bounded) until none of them has a
    /// restore pending. Returns the time spent. Never clears anything: a timeout leaves the
    /// older worker to find itself stale (`record_stage` of this delivery) or superseded.
    pub fn supersede_pending(&self, generation: Generation, max_wait: Duration) -> u64 {
        let started = Instant::now();
        let deadline = started + max_wait;
        let Ok(mut inner) = self.inner.lock() else {
            return elapsed_ms(started);
        };
        if inner.superseded_below < generation.0 {
            inner.superseded_below = generation.0;
        }
        while inner.pending.range(..generation.0).next().is_some() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self.idle.wait_timeout(inner, remaining) {
                Ok((next, _)) => inner = next,
                Err(_) => break,
            }
        }
        elapsed_ms(started)
    }

    /// True once a newer delivery is waiting for this generation's restore.
    pub fn is_superseded(&self, generation: Generation) -> bool {
        self.inner
            .lock()
            .map(|inner| generation.0 < inner.superseded_below)
            .unwrap_or(true)
    }

    pub fn restore_verdict(
        &self,
        generation: Generation,
        now_count: Option<u64>,
        ownership_lost: bool,
        own_render_advances: bool,
    ) -> RestoreVerdict {
        let owner = match self.inner.lock() {
            Ok(inner) => inner.owner,
            Err(_) => None,
        };
        decide_restore(
            generation,
            owner,
            now_count,
            ownership_lost,
            own_render_advances,
        )
    }
}

impl Default for RestoreState {
    fn default() -> Self {
        Self::new()
    }
}

static STATE: RestoreState = RestoreState::new();

/// Start a delivery's clipboard accounting. Call before waiting on the previous delivery.
pub fn begin_delivery() -> Generation {
    STATE.begin_delivery()
}

/// Record the delivery's own clipboard write right after it succeeded.
pub fn record_stage(generation: Generation, change_count: u64) {
    STATE.record_stage(generation, change_count);
}

/// Mark this generation's background restore pending (before it is spawned).
pub fn mark_pending(generation: Generation) {
    STATE.mark_pending(generation);
}

/// This generation's restore ended (also when spawning it failed, so deliveries never stall).
pub fn finish(generation: Generation) {
    STATE.finish(generation);
}

/// Wait (bounded) for older generations' background restores; returns the time spent.
pub fn supersede_pending(generation: Generation, max_wait: Duration) -> u64 {
    STATE.supersede_pending(generation, max_wait)
}

/// True while a newer delivery is waiting for this generation's restore.
pub fn is_superseded(generation: Generation) -> bool {
    STATE.is_superseded(generation)
}

/// May this generation restore the prior clipboard right now? Every restore calls this first.
pub fn restore_verdict(
    generation: Generation,
    now_count: Option<u64>,
    ownership_lost: bool,
    own_render_advances: bool,
) -> RestoreVerdict {
    STATE.restore_verdict(generation, now_count, ownership_lost, own_render_advances)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn begin(state: &RestoreState) -> Generation {
        state.begin_delivery()
    }

    #[test]
    fn a_fresh_unchanged_write_may_restore() {
        let state = RestoreState::new();
        let a = begin(&state);
        state.record_stage(a, 100);
        assert_eq!(
            state.restore_verdict(a, Some(100), false, false),
            RestoreVerdict::Proceed
        );
    }

    #[test]
    fn a_delivery_that_never_wrote_cannot_restore() {
        let state = RestoreState::new();
        let a = begin(&state);
        assert_eq!(
            state.restore_verdict(a, Some(100), false, false),
            RestoreVerdict::StaleGeneration
        );
    }

    #[test]
    fn a_stale_generation_cannot_restore_after_a_newer_write() {
        let state = RestoreState::new();
        let a = begin(&state);
        state.record_stage(a, 100);
        let b = begin(&state);
        state.record_stage(b, 105);
        // Worker A wakes up while the count happens to equal ITS recorded value.
        assert_eq!(
            state.restore_verdict(a, Some(100), false, false),
            RestoreVerdict::StaleGeneration
        );
        assert_eq!(
            state.restore_verdict(b, Some(105), false, false),
            RestoreVerdict::Proceed
        );
    }

    #[test]
    fn an_older_write_never_takes_ownership_back() {
        let state = RestoreState::new();
        let a = begin(&state);
        let b = begin(&state);
        state.record_stage(b, 9);
        state.record_stage(a, 7); // a late record from the older delivery
        assert_eq!(
            state.restore_verdict(b, Some(9), false, false),
            RestoreVerdict::Proceed
        );
        assert_eq!(
            state.restore_verdict(a, Some(7), false, false),
            RestoreVerdict::StaleGeneration
        );
    }

    #[test]
    fn a_user_copy_during_the_restore_window_is_left_alone() {
        let state = RestoreState::new();
        let a = begin(&state);
        state.record_stage(a, 100);
        // macOS: any change of the count is someone else's write.
        assert_eq!(
            state.restore_verdict(a, Some(101), false, false),
            RestoreVerdict::ClipboardChanged
        );
        // Windows: the count advanced and the clipboard window was told it lost ownership.
        assert_eq!(
            state.restore_verdict(a, Some(102), true, true),
            RestoreVerdict::ClipboardChanged
        );
        // Windows: ownership lost even though the count did not move is still a change.
        assert_eq!(
            state.restore_verdict(a, Some(100), true, true),
            RestoreVerdict::ClipboardChanged
        );
    }

    #[test]
    fn windows_accepts_its_own_render_advance_only_while_it_owns_the_clipboard() {
        let state = RestoreState::new();
        let a = begin(&state);
        state.record_stage(a, 100);
        assert_eq!(
            state.restore_verdict(a, Some(101), false, true),
            RestoreVerdict::Proceed
        );
        // The count going backwards is never ours.
        assert_eq!(
            state.restore_verdict(a, Some(99), false, true),
            RestoreVerdict::ClipboardChanged
        );
        // macOS rule: no advance is accepted.
        assert_eq!(
            state.restore_verdict(a, Some(101), false, false),
            RestoreVerdict::ClipboardChanged
        );
    }

    #[test]
    fn an_unreadable_count_leaves_the_clipboard_alone() {
        let state = RestoreState::new();
        let a = begin(&state);
        state.record_stage(a, 100);
        assert_eq!(
            state.restore_verdict(a, None, false, true),
            RestoreVerdict::CountUnreadable
        );
    }

    #[test]
    fn supersede_waits_only_for_older_generations_and_ignores_newer_ones() {
        let state = RestoreState::new();
        let a = begin(&state);
        let b = begin(&state);
        state.mark_pending(b); // newer delivery's own pending flag must not hold a waiter on `a`
        let waited = state.supersede_pending(a, Duration::from_millis(500));
        assert!(waited < 400, "waited {waited} ms for a newer generation");
        assert!(!state.is_superseded(b));
    }

    #[test]
    fn a_timed_out_waiter_does_not_clear_the_older_workers_pending_flag() {
        let state = RestoreState::new();
        let a = begin(&state);
        state.mark_pending(a);
        let b = begin(&state);
        let waited = state.supersede_pending(b, Duration::from_millis(30));
        assert!(waited >= 20);
        // The older worker is still pending and now sees itself superseded.
        assert!(state.is_superseded(a));
        assert!(!state.is_superseded(b));
        assert!(state.inner.lock().unwrap().pending.contains(&a.get()));
        // A third delivery still has to wait for it: the timeout cleared nothing.
        let c = begin(&state);
        let waited = state.supersede_pending(c, Duration::from_millis(30));
        assert!(waited >= 20);
        state.finish(a);
        assert!(state.supersede_pending(c, Duration::from_millis(30)) < 20);
    }

    #[test]
    fn a_newer_workers_finish_does_not_release_an_older_worker() {
        let state = RestoreState::new();
        let a = begin(&state);
        state.mark_pending(a);
        let b = begin(&state);
        state.mark_pending(b);
        state.finish(b);
        assert!(state.inner.lock().unwrap().pending.contains(&a.get()));
        assert!(!state.inner.lock().unwrap().pending.contains(&b.get()));
    }

    #[test]
    fn the_waiter_wakes_when_the_older_worker_finishes() {
        let state = Arc::new(RestoreState::new());
        let a = begin(&state);
        state.mark_pending(a);
        let b = begin(&state);
        let worker = {
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                state.finish(a);
            })
        };
        let waited = state.supersede_pending(b, Duration::from_millis(2_000));
        worker.join().unwrap();
        assert!(waited < 1_500, "waiter was not woken: {waited} ms");
    }

    #[test]
    fn two_overlapping_deliveries_only_the_newest_write_is_restored() {
        let state = RestoreState::new();
        // A stages and pastes; its restore is pending.
        let a = begin(&state);
        state.record_stage(a, 10);
        state.mark_pending(a);
        // B starts, waits, times out (A's worker is slow), snapshots and stages anyway.
        let b = begin(&state);
        state.supersede_pending(b, Duration::from_millis(10));
        state.record_stage(b, 12);
        state.mark_pending(b);
        // A's worker finally runs: stale even though nothing else touched the clipboard since B.
        assert_eq!(
            state.restore_verdict(a, Some(12), false, false),
            RestoreVerdict::StaleGeneration
        );
        state.finish(a);
        // B's worker restores, and may, since the count is still B's.
        assert_eq!(
            state.restore_verdict(b, Some(12), false, false),
            RestoreVerdict::Proceed
        );
        // After the user copies something, even B must not.
        assert_eq!(
            state.restore_verdict(b, Some(13), false, false),
            RestoreVerdict::ClipboardChanged
        );
        state.finish(b);
    }

    #[test]
    fn decide_restore_is_pure_and_checks_generation_first() {
        let a = Generation(1);
        let b = Generation(2);
        assert_eq!(
            decide_restore(a, Some((b, 5)), Some(5), false, false),
            RestoreVerdict::StaleGeneration
        );
        assert_eq!(
            decide_restore(a, None, Some(5), false, false),
            RestoreVerdict::StaleGeneration
        );
        assert_eq!(
            decide_restore(a, Some((a, 5)), Some(5), false, false),
            RestoreVerdict::Proceed
        );
    }
}
