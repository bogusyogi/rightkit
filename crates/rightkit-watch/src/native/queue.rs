//! The bounded record queue behind `TypedWatcher::watch_with`.
//!
//! One mutex, one deque, one condvar. When a push finds the deque full, the backlog is cleared and
//! replaced by a single gap marker that the next receive turns into `WatchEvent::Overflow` with
//! `raw_kind: None`. Memory is therefore bounded at `capacity` records plus that one marker.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvError, RecvTimeoutError, TryRecvError};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::{WatchEvent, WatchEventRecord, WatchMetrics};

/// A queued record with the private FSEvents event id it advances the cursor to.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) record: WatchEventRecord,
    pub(crate) id: Option<u64>,
}

impl Entry {
    pub(crate) fn new(record: WatchEventRecord, id: Option<u64>) -> Self {
        Self { record, id }
    }

    fn is_error(&self) -> bool {
        matches!(self.record.event, WatchEvent::Error(_))
    }
}

#[derive(Default)]
struct Inner {
    items: VecDeque<Entry>,
    /// A clear happened; the next receive yields one `Overflow` before anything queued.
    gap: bool,
    /// Highest event id among everything the gap swallowed.
    gap_id: u64,
    /// The producer is finished: receives drain, then report disconnect.
    closed: bool,
    /// The receiving handle is gone: pushes report failure so the producer can stop.
    receiver_gone: bool,
    last_id: Option<u64>,
    queued: u64,
    received: u64,
    overflow: u64,
    error: u64,
    dropped: u64,
    /// Records counted in `queued` that a clear removed before anyone received them.
    evicted: u64,
}

pub(crate) struct BoundedQueue {
    inner: Mutex<Inner>,
    ready: Condvar,
    capacity: usize,
    replaying: AtomicBool,
}

impl BoundedQueue {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            ready: Condvar::new(),
            capacity: capacity.max(1),
            replaying: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panicking producer must not wedge the consumer: the state is plain data.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn set_replaying(&self, replaying: bool) {
        self.replaying.store(replaying, Ordering::Release);
    }

    pub(crate) fn is_replaying(&self) -> bool {
        self.replaying.load(Ordering::Acquire)
    }

    /// Starting position reported by `cursor()` before any record has been received.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn seed_cursor(&self, id: u64) {
        self.lock().last_id = Some(id);
    }

    pub(crate) fn last_id(&self) -> Option<u64> {
        self.lock().last_id
    }

    /// No more records will arrive; receivers drain what is queued, then disconnect.
    pub(crate) fn close(&self) {
        self.lock().closed = true;
        self.ready.notify_all();
    }

    /// The consumer dropped its handle; further pushes fail.
    pub(crate) fn close_receiver(&self) {
        self.lock().receiver_gone = true;
    }

    /// Queues one batch under a single lock, so a consumer never sees a later record of the
    /// batch without the earlier ones. `coalesce_tail` merges a record into an identical unread
    /// tail instead of queueing a duplicate. Returns false once the consumer is gone.
    pub(crate) fn push_all(&self, entries: Vec<Entry>, coalesce_tail: bool) -> bool {
        if entries.is_empty() {
            return !self.lock().receiver_gone;
        }
        let mut inner = self.lock();
        if inner.receiver_gone {
            return false;
        }
        for entry in entries {
            self.push_one(&mut inner, entry, coalesce_tail);
        }
        drop(inner);
        self.ready.notify_all();
        true
    }

    fn push_one(&self, inner: &mut Inner, entry: Entry, coalesce_tail: bool) {
        if coalesce_tail {
            if let Some(tail) = inner.items.back_mut() {
                if matches!(entry.record.event, WatchEvent::Modify(_))
                    && tail.record == entry.record
                {
                    tail.id = tail.id.max(entry.id);
                    return;
                }
            }
        }
        // An overflow notice is itself a gap: when full it joins the existing one.
        let is_overflow = matches!(entry.record.event, WatchEvent::Overflow);
        if inner.items.len() >= self.capacity {
            let mut kept_error = None;
            let mut highest = entry.id.unwrap_or(0);
            let cleared = inner.items.len() as u64;
            for old in inner.items.drain(..) {
                highest = highest.max(old.id.unwrap_or(0));
                if old.is_error() && kept_error.is_none() {
                    kept_error = Some(old);
                }
            }
            inner.evicted += cleared;
            inner.dropped += cleared;
            if !inner.gap {
                inner.gap = true;
                inner.queued += 1;
                inner.overflow += 1;
            }
            inner.gap_id = inner.gap_id.max(highest);
            // A backend failure must survive a clear: the consumer has to learn the watch is dead.
            let incoming_is_error = entry.is_error();
            if let Some(error) = kept_error {
                inner.evicted -= 1;
                inner.dropped -= 1;
                inner.items.push_back(error);
            }
            if incoming_is_error {
                self.account(inner, &entry);
                inner.items.push_back(entry);
            } else if !is_overflow {
                inner.dropped += 1;
            }
            return;
        }
        self.account(inner, &entry);
        inner.items.push_back(entry);
    }

    fn account(&self, inner: &mut Inner, entry: &Entry) {
        inner.queued += 1;
        match entry.record.event {
            WatchEvent::Overflow => inner.overflow += 1,
            WatchEvent::Error(_) => inner.error += 1,
            _ => {}
        }
    }

    fn pop(&self, inner: &mut Inner) -> Option<Entry> {
        let entry = if inner.gap {
            inner.gap = false;
            let id = std::mem::take(&mut inner.gap_id);
            Some(Entry::new(
                WatchEventRecord {
                    event: WatchEvent::Overflow,
                    raw_kind: None,
                },
                (id != 0).then_some(id),
            ))
        } else {
            inner.items.pop_front()
        }?;
        inner.received += 1;
        if let Some(id) = entry.id {
            inner.last_id = Some(inner.last_id.map_or(id, |last| last.max(id)));
        }
        Some(entry)
    }

    pub(crate) fn recv(&self) -> Result<Entry, RecvError> {
        let mut inner = self.lock();
        loop {
            if let Some(entry) = self.pop(&mut inner) {
                return Ok(entry);
            }
            if inner.closed {
                return Err(RecvError);
            }
            inner = self.ready.wait(inner).unwrap_or_else(|p| p.into_inner());
        }
    }

    pub(crate) fn try_recv(&self) -> Result<Entry, TryRecvError> {
        let mut inner = self.lock();
        match self.pop(&mut inner) {
            Some(entry) => Ok(entry),
            None if inner.closed => Err(TryRecvError::Disconnected),
            None => Err(TryRecvError::Empty),
        }
    }

    pub(crate) fn recv_timeout(&self, timeout: Duration) -> Result<Entry, RecvTimeoutError> {
        // A timeout too large to add to `Instant` waits as long as the platform allows.
        let deadline = Instant::now().checked_add(timeout);
        let mut inner = self.lock();
        loop {
            if let Some(entry) = self.pop(&mut inner) {
                return Ok(entry);
            }
            if inner.closed {
                return Err(RecvTimeoutError::Disconnected);
            }
            let wait = match deadline {
                Some(deadline) => match deadline.checked_duration_since(Instant::now()) {
                    Some(left) if !left.is_zero() => left,
                    _ => return Err(RecvTimeoutError::Timeout),
                },
                None => Duration::from_secs(3600),
            };
            inner = self
                .ready
                .wait_timeout(inner, wait)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    pub(crate) fn metrics(&self) -> WatchMetrics {
        let inner = self.lock();
        WatchMetrics {
            queued_total: inner.queued,
            received_total: inner.received,
            queue_depth: inner.queued.saturating_sub(inner.received + inner.evicted),
            overflow_total: inner.overflow,
            error_total: inner.error,
            dropped_total: inner.dropped,
            queue_capacity: self.capacity as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WatchError;

    fn modify(name: &str, id: Option<u64>) -> Entry {
        Entry::new(
            WatchEventRecord {
                event: WatchEvent::Modify(name.into()),
                raw_kind: None,
            },
            id,
        )
    }

    #[test]
    fn full_queue_clears_and_reports_one_gap_first() {
        let queue = BoundedQueue::new(2);
        assert!(queue.push_all(vec![modify("a", Some(1)), modify("b", Some(2))], false));
        assert!(queue.push_all(vec![modify("c", Some(3))], false));
        let m = queue.metrics();
        assert_eq!((m.dropped_total, m.queue_depth), (3, 1), "{m:?}");
        let head = queue.try_recv().unwrap();
        assert_eq!(head.record.event, WatchEvent::Overflow);
        assert_eq!(head.record.raw_kind, None);
        assert_eq!(head.id, Some(3), "the gap carries the highest dropped id");
        assert!(queue.try_recv().is_err());
        assert!(queue.push_all(vec![modify("d", Some(4))], false));
        assert_eq!(
            queue.try_recv().unwrap().record.event,
            WatchEvent::Modify("d".into())
        );
        assert_eq!(queue.last_id(), Some(4));
        assert_eq!(queue.metrics().queue_depth, 0);
    }

    #[test]
    fn depth_never_exceeds_capacity_plus_the_gap() {
        let queue = BoundedQueue::new(8);
        for i in 0..1000 {
            queue.push_all(vec![modify("x", Some(i))], false);
            assert!(queue.metrics().queue_depth <= 9);
        }
    }

    #[test]
    fn errors_survive_a_clear() {
        let queue = BoundedQueue::new(1);
        let error = Entry::new(
            WatchEventRecord {
                event: WatchEvent::Error(WatchError::Backend {
                    message: "dead".into(),
                    paths: vec![],
                }),
                raw_kind: None,
            },
            None,
        );
        queue.push_all(vec![modify("a", None), error, modify("b", None)], false);
        assert_eq!(queue.try_recv().unwrap().record.event, WatchEvent::Overflow);
        assert!(matches!(
            queue.try_recv().unwrap().record.event,
            WatchEvent::Error(_)
        ));
        assert!(queue.try_recv().is_err());
    }

    #[test]
    fn identical_unread_tail_coalesces_only_when_asked() {
        let queue = BoundedQueue::new(8);
        queue.push_all(vec![modify("d", Some(1)), modify("d", Some(2))], true);
        assert_eq!(queue.metrics().queue_depth, 1);
        let entry = queue.try_recv().unwrap();
        assert_eq!(entry.id, Some(2));
        // Once read, an identical record is new information.
        queue.push_all(vec![modify("d", Some(3))], true);
        assert_eq!(queue.metrics().queue_depth, 1);
        let plain = BoundedQueue::new(8);
        plain.push_all(vec![modify("d", None), modify("d", None)], false);
        assert_eq!(plain.metrics().queue_depth, 2);
    }

    #[test]
    fn close_drains_then_disconnects() {
        let queue = BoundedQueue::new(4);
        queue.push_all(vec![modify("a", None)], false);
        queue.close();
        assert!(queue.recv().is_ok());
        assert!(queue.recv().is_err());
        assert_eq!(queue.try_recv().unwrap_err(), TryRecvError::Disconnected);
        assert_eq!(
            queue.recv_timeout(Duration::from_millis(1)).unwrap_err(),
            RecvTimeoutError::Disconnected
        );
    }
}
