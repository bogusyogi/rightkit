//! A generation-stamped event queue with one scheduled drain. Producers enqueue from any thread;
//! the first enqueue after a drain reports `schedule_drain` so the host schedules exactly one
//! drain on the thread that applies events (a UI thread, say). Each event carries the
//! generation it was enqueued at, so a delayed follow-up ("go back to idle if nothing newer
//! arrived") is accepted only while its generation is still the latest
//! ([`DispatchQueue::enqueue_if_current`]). Platform-free; the lock is never held while an event
//! is dispatched.
//!
//! Bounds. The queue holds at most its capacity ([`DispatchQueue::with_capacity`], default
//! [`DEFAULT_CAPACITY`], at most [`MAX_CAPACITY`]): when full, the oldest event is dropped and the
//! result says so (`dropped`), because the newest events decide what the user sees. A producer
//! that emits at a high rate (an audio level, say) uses
//! [`DispatchQueue::enqueue_replacing_last`], which overwrites a still-pending event of the same
//! kind instead of growing the queue.
//!
//! A dispatch that panics does not end the queue: [`drain`] catches the panic, counts it in the
//! returned [`DrainReport`] and carries on with the next event, so `drain_scheduled` never stays
//! set with nothing left to clear it.

use std::collections::VecDeque;
use std::ops::{Deref, DerefMut};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, MutexGuard};

/// Capacity of a queue built with `Default`.
pub const DEFAULT_CAPACITY: usize = 1024;
/// Largest capacity [`DispatchQueue::with_capacity`] accepts (larger asks are clamped).
pub const MAX_CAPACITY: usize = 65_536;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuedEvent<T> {
    pub generation: u64,
    pub payload: T,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnqueueResult {
    pub generation: u64,
    pub schedule_drain: bool,
    /// The queue was full and its oldest event was dropped to make room.
    pub dropped: bool,
}

/// What one [`drain`] did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrainReport {
    /// Events handed to the dispatch function.
    pub dispatched: u32,
    /// Dispatches that panicked (caught; the drain went on).
    pub panicked: u32,
}

pub struct DispatchQueue<T> {
    generation: u64,
    pending: VecDeque<QueuedEvent<T>>,
    drain_scheduled: bool,
    capacity: usize,
}

impl<T> Default for DispatchQueue<T> {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }
}

impl<T> DispatchQueue<T> {
    /// A queue holding at most `capacity` pending events, clamped to `1..=`[`MAX_CAPACITY`].
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            generation: 0,
            pending: VecDeque::new(),
            drain_scheduled: false,
            capacity: capacity.clamp(1, MAX_CAPACITY),
        }
    }

    pub fn enqueue(&mut self, event: T) -> EnqueueResult {
        self.generation = self.generation.wrapping_add(1);
        let mut dropped = false;
        while self.pending.len() >= self.capacity {
            self.pending.pop_front();
            dropped = true;
        }
        self.pending.push_back(QueuedEvent {
            generation: self.generation,
            payload: event,
        });
        let schedule_drain = !self.drain_scheduled;
        self.drain_scheduled = true;
        EnqueueResult {
            generation: self.generation,
            schedule_drain,
            dropped,
        }
    }

    /// Enqueue `event`, replacing the newest pending event instead when `replaces(last, event)`
    /// says they are the same kind (an amplitude update replacing an amplitude update). The
    /// replacement takes the new generation, exactly as an enqueue would.
    pub fn enqueue_replacing_last(
        &mut self,
        event: T,
        replaces: impl Fn(&T, &T) -> bool,
    ) -> EnqueueResult {
        if self
            .pending
            .back()
            .is_some_and(|last| replaces(&last.payload, &event))
        {
            self.pending.pop_back();
        }
        self.enqueue(event)
    }

    pub fn enqueue_if_current(
        &mut self,
        expected_generation: u64,
        event: T,
    ) -> Option<EnqueueResult> {
        if self.generation != expected_generation {
            return None;
        }
        Some(self.enqueue(event))
    }

    fn take_next(&mut self) -> Option<QueuedEvent<T>> {
        let event = self.pending.pop_front();
        if event.is_none() {
            self.drain_scheduled = false;
        }
        event
    }

    pub fn mark_drain_unscheduled(&mut self) {
        self.drain_scheduled = false;
    }

    /// Events waiting for a drain.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

pub struct DispatchQueueGuard<'a, T> {
    guard: MutexGuard<'a, DispatchQueue<T>>,
}

impl<T> Deref for DispatchQueueGuard<'_, T> {
    type Target = DispatchQueue<T>;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T> DerefMut for DispatchQueueGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl<T> Drop for DispatchQueueGuard<'_, T> {
    fn drop(&mut self) {}
}

pub fn lock<T>(queue: &Mutex<DispatchQueue<T>>) -> DispatchQueueGuard<'_, T> {
    let guard = queue.lock().unwrap_or_else(|error| error.into_inner());
    DispatchQueueGuard { guard }
}

/// Kept for callers that assert no queue lock is held while running UI work; the queue lock
/// is never held across a dispatch, so this has nothing to check.
pub fn assert_not_locked() {}

/// Dispatch every pending event in order, until the queue is empty (events enqueued by a dispatch
/// are drained in the same pass). A panicking dispatch is caught and counted; the drain continues.
pub fn drain<T>(
    queue: &Mutex<DispatchQueue<T>>,
    mut dispatch: impl FnMut(QueuedEvent<T>),
) -> DrainReport {
    let mut report = DrainReport::default();
    loop {
        let event = lock(queue).take_next();
        let Some(event) = event else {
            return report;
        };
        report.dispatched = report.dispatched.saturating_add(1);
        if catch_unwind(AssertUnwindSafe(|| dispatch(event))).is_err() {
            report.panicked = report.panicked.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(queue: &Mutex<DispatchQueue<&'static str>>) -> Vec<(u64, &'static str)> {
        let mut seen = Vec::new();
        drain(queue, |event| seen.push((event.generation, event.payload)));
        seen
    }

    #[test]
    fn the_first_enqueue_schedules_one_drain_and_later_ones_do_not() {
        let queue = Mutex::new(DispatchQueue::default());
        let first = lock(&queue).enqueue("a");
        let second = lock(&queue).enqueue("b");
        assert!(first.schedule_drain);
        assert!(!second.schedule_drain);
        assert_eq!((first.generation, second.generation), (1, 2));
        assert_eq!(collect(&queue), vec![(1, "a"), (2, "b")]);
        // The drain finished: the next enqueue schedules a new one.
        assert!(lock(&queue).enqueue("c").schedule_drain);
    }

    #[test]
    fn a_follow_up_is_accepted_only_while_its_generation_is_the_latest() {
        let queue = Mutex::new(DispatchQueue::default());
        let status = lock(&queue).enqueue("status").generation;
        assert!(lock(&queue).enqueue_if_current(status, "idle").is_some());
        // Something newer arrived after "status": a follow-up for it is dropped.
        let queue = Mutex::new(DispatchQueue::default());
        let status = lock(&queue).enqueue("status").generation;
        lock(&queue).enqueue("recording");
        assert!(lock(&queue).enqueue_if_current(status, "idle").is_none());
        assert_eq!(collect(&queue), vec![(1, "status"), (2, "recording")]);
    }

    #[test]
    fn a_failed_schedule_can_be_retried() {
        let queue = Mutex::new(DispatchQueue::default());
        assert!(lock(&queue).enqueue("a").schedule_drain);
        // The host could not schedule the drain.
        lock(&queue).mark_drain_unscheduled();
        assert!(lock(&queue).enqueue("b").schedule_drain);
    }

    #[test]
    fn the_queue_lock_is_free_while_an_event_is_dispatched() {
        let queue = Mutex::new(DispatchQueue::default());
        lock(&queue).enqueue("a");
        let mut dispatched = 0;
        drain(&queue, |_| {
            dispatched += 1;
            if dispatched == 1 {
                // A dispatch that enqueues again (an event handler raising another event) must
                // not deadlock, and the new event is drained in the same pass.
                let again = lock(&queue).enqueue("b");
                assert!(!again.schedule_drain);
            }
        });
        assert_eq!(dispatched, 2);
        assert!(lock(&queue).enqueue("c").schedule_drain);
    }

    #[test]
    fn a_poisoned_lock_is_recovered() {
        let queue = std::sync::Arc::new(Mutex::new(DispatchQueue::<&'static str>::default()));
        let poisoner = std::sync::Arc::clone(&queue);
        let _ = std::thread::spawn(move || {
            let _held = poisoner.lock().unwrap();
            panic!("poison");
        })
        .join();
        assert!(lock(&queue).enqueue("a").schedule_drain);
    }
}
