//! A bounded, pollable log of recent service events, for clients that cannot hold a stream open
//! (a C ABI with no cancel entry point, for example). Every published event is appended with a
//! sequence number; a client asks for what is newer than the last number it saw, waiting a bounded
//! time ([`MAX_WAIT_MS`]) when nothing is.
//!
//! Numbers: a sequence number is unique and increasing within one process, and the first one is
//! the process's start time in microseconds, so a restarted service never reuses a number a client
//! saw from the one before. A cursor older than the oldest kept event, or newer than the newest, is
//! reported as `dropped`: the client re-reads its state and polls from `next`.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// Events kept; older ones are dropped first.
pub const MAX_EVENTS: usize = 512;
/// Serialized payload bytes kept in total (an event larger than this is replaced by a `{truncated, bytes}` marker).
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
/// The longest a poll waits for an event, in milliseconds. A larger request is clamped.
pub const MAX_WAIT_MS: u32 = 25_000;
/// One reply carries at most this many events and this many payload bytes (the newest unread
/// event past either limit is left for the next poll), so it always fits a frame.
const REPLY_EVENTS: usize = 256;
const REPLY_BYTES: usize = 8 * 1024 * 1024;

struct Entry {
    seq: u64,
    topic: String,
    payload: Arc<Value>,
    size: usize,
}

struct State {
    /// Contiguous sequence numbers, oldest first.
    entries: VecDeque<Entry>,
    bytes: usize,
    /// The number the next event gets.
    next_seq: u64,
    closed: bool,
}

/// One event a poll returns.
pub struct Polled {
    /// The event's sequence number.
    pub seq: u64,
    /// The event's topic.
    pub topic: String,
    /// The event's payload (a `{truncated, bytes}` marker when it was too large to keep).
    pub payload: Arc<Value>,
}

/// What a poll answers.
pub struct PollResult {
    /// The events, oldest first.
    pub events: Vec<Polled>,
    /// The cursor for the next poll: the last number this reply covers.
    pub next: u64,
    /// The cursor `after` is not one this log can continue from, so events may have been missed
    /// (or none were). `events` is empty and `next` is the newest number: re-read state, then
    /// poll from `next`.
    pub dropped: bool,
}

/// The bounded event log; share it behind an `Arc`.
pub struct EventLog {
    state: Mutex<State>,
    wake: Condvar,
}

impl Default for EventLog {
    fn default() -> EventLog {
        let start = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        EventLog {
            state: Mutex::new(State {
                entries: VecDeque::new(),
                bytes: 0,
                next_seq: start.saturating_add(1),
                closed: false,
            }),
            wake: Condvar::new(),
        }
    }
}

impl EventLog {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Appends one event and wakes every waiting poll.
    pub fn push(&self, topic: &str, payload: &Value) {
        let mut size = serde_json::to_vec(payload).map(|v| v.len()).unwrap_or(0);
        // An event larger than the whole bound is replaced by a marker so the bound is hard.
        let oversized = size > MAX_BYTES;
        let marker = serde_json::json!({ "truncated": true, "bytes": size });
        let payload = if oversized { &marker } else { payload };
        if oversized {
            size = serde_json::to_vec(payload).map(|v| v.len()).unwrap_or(0);
        }
        let mut state = self.lock();
        let seq = state.next_seq;
        state.next_seq = seq.saturating_add(1);
        state.bytes += size;
        state.entries.push_back(Entry {
            seq,
            topic: topic.to_string(),
            payload: Arc::new(payload.clone()),
            size,
        });
        while state.entries.len() > 1
            && (state.entries.len() > MAX_EVENTS || state.bytes > MAX_BYTES)
        {
            if let Some(old) = state.entries.pop_front() {
                state.bytes -= old.size;
            }
        }
        drop(state);
        self.wake.notify_all();
    }

    /// Ends every wait now and makes later polls answer at once (service shutdown).
    pub fn close(&self) {
        self.lock().closed = true;
        self.wake.notify_all();
    }

    /// The events with a number above `after` on one of `topics` (every topic when it is empty),
    /// waiting up to `wait` when there are none. Never waits when the cursor is `dropped`, and
    /// never busy-waits: a poll sleeps on the condition variable until an event or the deadline.
    pub fn poll(&self, after: u64, topics: &[String], wait: Duration) -> PollResult {
        let deadline = Instant::now() + wait;
        let mut state = self.lock();
        loop {
            let last = state.next_seq - 1;
            let oldest = state.entries.front().map_or(state.next_seq, |e| e.seq);
            if after > last || after.saturating_add(1) < oldest {
                return PollResult {
                    events: Vec::new(),
                    next: last,
                    dropped: true,
                };
            }
            // Numbers are contiguous, so the first unread entry is at a fixed offset.
            let start = ((after + 1 - oldest) as usize).min(state.entries.len());
            let mut events = Vec::new();
            let mut bytes = 0usize;
            let mut truncated = false;
            for entry in state.entries.range(start..) {
                if !topics.is_empty() && !topics.contains(&entry.topic) {
                    continue;
                }
                if !events.is_empty()
                    && (events.len() >= REPLY_EVENTS || bytes + entry.size > REPLY_BYTES)
                {
                    truncated = true;
                    break;
                }
                bytes += entry.size;
                events.push(Polled {
                    seq: entry.seq,
                    topic: entry.topic.clone(),
                    payload: Arc::clone(&entry.payload),
                });
            }
            if !events.is_empty() {
                let next = if truncated {
                    events.last().map_or(last, |e| e.seq)
                } else {
                    last
                };
                return PollResult {
                    events,
                    next,
                    dropped: false,
                };
            }
            // Nothing matched everything up to `last`: a later poll starts after it.
            let now = Instant::now();
            if state.closed || now >= deadline {
                return PollResult {
                    events,
                    next: last,
                    dropped: false,
                };
            }
            state = self
                .wake
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}
