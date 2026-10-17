//! Structured lifecycle events: one JSON object per line in `events.jsonl`,
//! also retained in memory. Events never carry credentials, signatures,
//! tokens or request bodies.
//!
//! Every event carries a `seq`, counted from 1 for the life of one [`EventLog`] (one daemon
//! run). [`EventLog::since`] and [`EventLog::wait_since`] read the retained events after a
//! cursor, which is the live-update primitive an operator screen polls: remember the `next`
//! of the last page, ask for more. A cursor from an earlier run is recognised by its `epoch`
//! and answered with `reset: true` and the retained events from the start, so a screen
//! re-reads instead of waiting for a count that will never come.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

pub type Event = Value;

/// Events kept in memory; `events.jsonl` keeps every event.
const RETAIN: usize = 10_000;

/// Longest [`EventLog::wait_since`] blocks, whatever the caller asked.
pub const MAX_WAIT: Duration = Duration::from_secs(60);

struct Mem {
    events: VecDeque<Event>,
    /// `seq` of the next event.
    next_seq: u64,
}

pub struct EventLog {
    file: Mutex<Option<File>>,
    memory: Mutex<Mem>,
    arrived: Condvar,
    /// Names this run: a cursor is only meaningful against the epoch that issued it.
    epoch: String,
}

/// A page of events read after a cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct EventsPage {
    /// The run that produced these events; hand it back with `next`.
    pub epoch: String,
    /// The cursor the caller presented belonged to another run (or to events no longer
    /// retained): the page starts at the oldest retained event instead.
    pub reset: bool,
    pub events: Vec<Event>,
    /// Pass this as `after` on the next read. It is the `seq` of the last event returned, or
    /// the cursor given when nothing new arrived.
    pub next: u64,
}

/// daemon-api v1 `lifecycleEvents` name for an event, where one exists.
pub fn contract_name(event: &str) -> Option<&'static str> {
    Some(match event {
        "LISTENING" => "daemon_started",
        "SHUTDOWN" => "daemon_stopped",
        "ENROLLED" => "device_enrolled",
        "DEVICE_REVOKED" => "device_revoked",
        "UPLOAD_STARTED" => "upload_started",
        "UPLOAD_COMPLETE" => "upload_completed",
        "JOB_UPDATED" => "job_updated",
        _ => return None,
    })
}

fn new_epoch() -> String {
    let mut random = [0u8; 8];
    let _ = getrandom::fill(&mut random);
    random.iter().map(|b| format!("{b:02x}")).collect()
}

impl EventLog {
    fn with(file: Option<File>) -> Self {
        Self {
            file: Mutex::new(file),
            memory: Mutex::new(Mem {
                events: VecDeque::new(),
                next_seq: 1,
            }),
            arrived: Condvar::new(),
            epoch: new_epoch(),
        }
    }

    pub fn open(path: &Path) -> std::io::Result<Self> {
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self::with(Some(f)))
    }

    pub fn memory_only() -> Self {
        Self::with(None)
    }

    /// The epoch cursors of this log belong to.
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    pub fn emit(&self, now: u64, event: &str, fields: Value) {
        let mut obj = Map::new();
        obj.insert("ts".into(), json!(now));
        obj.insert("event".into(), json!(event));
        if let Some(name) = contract_name(event) {
            obj.insert("contractEvent".into(), json!(name));
        } else if event == "AUTH_REJECTED"
            && fields.get("class").and_then(Value::as_str) == Some("signed_webhook")
        {
            obj.insert("contractEvent".into(), json!("webhook_rejected"));
        }
        if let Value::Object(m) = fields {
            for (k, v) in m {
                obj.insert(k, v);
            }
        }
        // The sequence number is assigned and the file written under the memory lock, so the
        // file, the memory and `seq` all agree on the order of concurrent events. A caller's
        // own field named `seq` cannot replace it.
        let mut mem = self.memory.lock().unwrap_or_else(|e| e.into_inner());
        obj.insert("seq".into(), json!(mem.next_seq));
        let value = Value::Object(obj);
        if let Ok(mut guard) = self.file.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = writeln!(f, "{value}");
                let _ = f.flush();
            }
        }
        mem.next_seq += 1;
        if mem.events.len() >= RETAIN {
            mem.events.pop_front();
        }
        mem.events.push_back(value);
        drop(mem);
        self.arrived.notify_all();
    }

    pub fn snapshot(&self) -> Vec<Event> {
        self.memory
            .lock()
            .map(|m| m.events.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Events held in memory: at most 10,000, while the file keeps every event.
    pub fn retained(&self) -> usize {
        self.memory.lock().map(|m| m.events.len()).unwrap_or(0)
    }

    /// Up to `limit` retained events with `seq` greater than `after`. `epoch` is the one a
    /// previous page returned (`None` for a first read). A cursor from another run, or one older
    /// than the oldest retained event, gives `reset: true` and starts from the oldest retained.
    pub fn since(&self, epoch: Option<&str>, after: u64, limit: usize) -> EventsPage {
        let mem = self.memory.lock().unwrap_or_else(|e| e.into_inner());
        self.page(&mem, epoch, after, limit)
    }

    /// Like [`since`](Self::since), but when nothing newer than the cursor exists, waits up to
    /// `timeout` (at most [`MAX_WAIT`]) for an event to arrive. An empty page after the wait
    /// means nothing happened: ask again. This blocks the calling thread.
    pub fn wait_since(
        &self,
        epoch: Option<&str>,
        after: u64,
        limit: usize,
        timeout: Duration,
    ) -> EventsPage {
        let until = Instant::now() + timeout.min(MAX_WAIT);
        let mut mem = self.memory.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let page = self.page(&mem, epoch, after, limit);
            let left = until.saturating_duration_since(Instant::now());
            if !page.events.is_empty() || page.reset || left.is_zero() {
                return page;
            }
            mem = self
                .arrived
                .wait_timeout(mem, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    fn page(&self, mem: &Mem, epoch: Option<&str>, after: u64, limit: usize) -> EventsPage {
        let last_seq = mem.next_seq - 1;
        let first_retained = mem
            .events
            .front()
            .and_then(|e| e["seq"].as_u64())
            .unwrap_or(mem.next_seq);
        let foreign = epoch.is_some_and(|e| e != self.epoch);
        // `after` ahead of everything issued cannot be a cursor of this run.
        let reset = foreign || after > last_seq || after.saturating_add(1) < first_retained;
        let after = if reset { 0 } else { after };
        let events: Vec<Event> = mem
            .events
            .iter()
            .filter(|e| e["seq"].as_u64().is_some_and(|s| s > after))
            .take(limit.max(1))
            .cloned()
            .collect();
        let next = events
            .last()
            .and_then(|e| e["seq"].as_u64())
            .unwrap_or(after);
        EventsPage {
            epoch: self.epoch.clone(),
            reset,
            events,
            next,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_with(n: u64) -> EventLog {
        let log = EventLog::memory_only();
        for i in 0..n {
            log.emit(i, "X", json!({ "i": i }));
        }
        log
    }

    #[test]
    fn events_are_numbered_from_one_and_a_caller_field_cannot_replace_seq() {
        let log = EventLog::memory_only();
        log.emit(0, "A", json!({ "seq": 99 }));
        log.emit(0, "B", json!({}));
        let all = log.snapshot();
        assert_eq!(all[0]["seq"], 1);
        assert_eq!(all[1]["seq"], 2);
    }

    #[test]
    fn a_cursor_walks_the_log_without_gaps_or_repeats() {
        let log = log_with(5);
        let first = log.since(None, 0, 2);
        assert!(!first.reset);
        assert_eq!(first.events.len(), 2);
        assert_eq!(first.next, 2);
        let second = log.since(Some(&first.epoch), first.next, 10);
        assert!(!second.reset);
        assert_eq!(second.events.len(), 3);
        assert_eq!(second.events[0]["seq"], 3);
        assert_eq!(second.next, 5);
        let none = log.since(Some(&first.epoch), second.next, 10);
        assert!(none.events.is_empty());
        assert_eq!(none.next, 5);
    }

    #[test]
    fn a_cursor_from_another_run_or_ahead_of_this_one_resets() {
        let log = log_with(3);
        let foreign = log.since(Some("not-this-epoch"), 2, 10);
        assert!(foreign.reset);
        assert_eq!(foreign.events.len(), 3);
        let ahead = log.since(Some(log.epoch()), 50, 10);
        assert!(ahead.reset);
        assert_eq!(ahead.events.len(), 3);
        assert_eq!(ahead.next, 3);
    }

    #[test]
    fn a_cursor_older_than_the_retained_window_resets() {
        let log = log_with(RETAIN as u64 + 5);
        assert_eq!(log.retained(), RETAIN);
        let stale = log.since(Some(log.epoch()), 1, 3);
        assert!(stale.reset);
        assert_eq!(stale.events[0]["seq"], 6);
        // One event behind the oldest retained is still contiguous: no reset.
        let edge = log.since(Some(log.epoch()), 5, 3);
        assert!(!edge.reset);
        assert_eq!(edge.events[0]["seq"], 6);
    }

    #[test]
    fn wait_since_returns_at_once_when_events_wait_and_after_the_timeout_when_none() {
        let log = log_with(2);
        let now = Instant::now();
        let page = log.wait_since(None, 0, 10, Duration::from_secs(30));
        assert_eq!(page.events.len(), 2);
        assert!(now.elapsed() < Duration::from_secs(5));
        let quiet = log.wait_since(Some(log.epoch()), 2, 10, Duration::from_millis(30));
        assert!(quiet.events.is_empty());
        assert_eq!(quiet.next, 2);
    }

    #[test]
    fn wait_since_wakes_when_an_event_arrives() {
        let log = std::sync::Arc::new(log_with(1));
        let writer = log.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            writer.emit(9, "LATE", json!({}));
        });
        let page = log.wait_since(Some(log.epoch()), 1, 10, Duration::from_secs(10));
        handle.join().unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0]["event"], "LATE");
    }
}
