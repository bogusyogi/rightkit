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
