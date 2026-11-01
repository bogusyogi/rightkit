//! Bounded frame scheduler: priority-ordered queue with deadline expiry,
//! generation invalidation and backpressure.
//!
//! Rewritten from Clypra `frame_scheduler.rs`/`frame_deadline.rs` ideas:
//! requests enter a max-heap ordered by deadline, expired realtime requests
//! are dropped (missed deadline = useless work), and `advance` to a new
//! snapshot identity cancels everything queued for the old one. The queue is
//! bounded; under overflow the lowest-priority requests are dropped first —
//! never silently growing memory or serving out-of-scope work.

use std::collections::BinaryHeap;

use crate::native::identity::{FramePriority, FrameRequest};
use crate::native::snapshot::SnapshotIdentity;

/// A queued request, ordered by its deadline (earliest = greatest).
#[derive(Debug)]
struct Queued {
    request: FrameRequest,
    seq: u64,
}

impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        self.request.deadline == other.request.deadline && self.seq == other.seq
    }
}
impl Eq for Queued {}
impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Deadline order, then FIFO within equal deadlines.
        self.request
            .deadline
            .cmp(&other.request.deadline)
            .then(other.seq.cmp(&self.seq))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopOutcome {
    /// Returned a live request.
    Ready,
    /// Queue empty.
    Empty,
}

#[derive(Debug)]
pub enum SchedulerError {
    /// Request targets a snapshot the scheduler no longer serves.
    StaleIdentity,
}

impl std::fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleIdentity => f.write_str("request targets a superseded snapshot identity"),
        }
    }
}
impl std::error::Error for SchedulerError {}

/// Statistics for delivery reports — no telemetry side channel.
#[derive(Debug, Clone, Copy, Default)]
pub struct SchedulerStats {
    pub queued: usize,
    pub dropped_expired: u64,
    pub dropped_overflow: u64,
    pub cancelled: u64,
}

/// Bounded priority queue of frame requests for one current snapshot.
pub struct FrameScheduler {
    heap: BinaryHeap<Queued>,
    seq: u64,
    /// Maximum queued requests; overflow drops lowest-priority entries.
    capacity: usize,
    current: SnapshotIdentity,
    stats: SchedulerStats,
    /// Frame indices dropped as expired since the last `take_dropped` call —
    /// callers that dedup requests elsewhere must release them there too.
    dropped: Vec<i64>,
}

impl FrameScheduler {
    pub fn new(capacity: usize, current: SnapshotIdentity) -> Self {
        Self {
            heap: BinaryHeap::new(),
            seq: 0,
            capacity: capacity.max(1),
            current,
            stats: SchedulerStats::default(),
            dropped: Vec::new(),
        }
    }

    pub fn stats(&self) -> SchedulerStats {
        self.stats
    }

    /// Switch the served snapshot. Everything queued for the old identity is
    /// cancelled; afterwards stale requests are rejected at `request` and can
    /// never reach a producer.
    pub fn advance(&mut self, next: SnapshotIdentity) {
        self.stats.cancelled += self.heap.len() as u64;
        self.heap.clear();
        self.current = next;
    }

    /// Queue a frame request. Rejects stale-identity requests; enforces the
    /// capacity bound by dropping the lowest-priority queued request.
    pub fn request(&mut self, request: FrameRequest) -> Result<(), SchedulerError> {
        if request.key.identity != self.current {
            return Err(SchedulerError::StaleIdentity);
        }
        if self.heap.len() >= self.capacity {
            // Backpressure: drop among the lowest-priority tier first, and the
            // least urgent (latest deadline) within it. If the incoming request
            // is not strictly higher priority than that tier, it loses.
            let min_priority = self.heap.iter().map(|q| q.request.deadline.priority).min();
            match min_priority {
                Some(p) if p < request.deadline.priority => {
                    let mut items: Vec<Queued> = self.heap.drain().collect();
                    if let Some((idx, _)) = items
                        .iter()
                        .enumerate()
                        .filter(|(_, q)| q.request.deadline.priority == p)
                        .min_by(|(_, a), (_, b)| a.request.deadline.cmp(&b.request.deadline))
                    {
                        items.remove(idx);
                    }
                    self.heap = items.into_iter().collect();
                    self.stats.dropped_overflow += 1;
                }
                _ => {
                    self.stats.dropped_overflow += 1;
                    return Ok(());
                }
            }
        }
        self.seq += 1;
        self.heap.push(Queued {
            request,
            seq: self.seq,
        });
        self.stats.queued = self.heap.len();
        Ok(())
    }

    /// Pop the most urgent live request. Expired *realtime* requests are
    /// dropped; interactive/background requests never expire for being slow.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<FrameRequest> {
        while let Some(queued) = self.heap.pop() {
            let expired = queued.request.deadline.is_expired();
            if expired && queued.request.deadline.priority == FramePriority::Realtime {
                self.stats.dropped_expired += 1;
                self.dropped.push(queued.request.key.frame);
                continue;
            }
            self.stats.queued = self.heap.len();
            return Some(queued.request);
        }
        self.stats.queued = 0;
        None
    }

    /// Frame indices dropped as expired since the last call.
    pub fn take_dropped(&mut self) -> Vec<i64> {
        std::mem::take(&mut self.dropped)
    }

    /// Drop everything queued — hard cancellation without switching identity.
    pub fn cancel_all(&mut self) {
        self.stats.cancelled += self.heap.len() as u64;
        self.heap.clear();
        self.dropped.clear();
        self.stats.queued = 0;
    }

    pub fn pending(&self) -> usize {
        self.heap.len()
    }
}
