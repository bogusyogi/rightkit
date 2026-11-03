//! Per-event-class sampling and retention for a JSONL log (issue #1278).
//!
//! A high-rate event (hundreds per hour) otherwise decides how far back a whole log reaches.
//! [`ClassedLog`] writes through to a [`JsonlStore`] and applies, by the record's `event` name:
//!
//! * [`ClassRule::counts_above`]: beyond a per-minute allowance the records of that class are not
//!   written; each minute that suppressed any is closed with one count line
//!   `{"event": "<event>", "rolled": true, "minute_ms": .., "count": .., "written": ..}`.
//!   An allowance of 0 turns the class into per-minute counts only ([`ClassRule::counts_only`]).
//! * [`ClassedLog::prune`]: a per-class maximum age, so rare events can be kept for days while the
//!   default class is trimmed sooner.
//!
//! Records carry `ts_ms` (as [`crate::EventRecord`] does); one without it uses the clock.

use crate::JsonlStore;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MINUTE_MS: i64 = 60_000;

/// Sampling of one event class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassRule {
    pub event: String,
    /// Records of this class written per minute before the rest are only counted.
    pub allowance_per_minute: u32,
}

impl ClassRule {
    /// Write at most `allowance_per_minute` records of `event` per minute, count the rest.
    pub fn counts_above(event: impl Into<String>, allowance_per_minute: u32) -> Self {
        Self {
            event: event.into(),
            allowance_per_minute,
        }
    }

    /// Never write the records of `event`; write one count line per minute instead.
    pub fn counts_only(event: impl Into<String>) -> Self {
        Self::counts_above(event, 0)
    }
}

/// Maximum age of one class (or of every class without its own entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassRetention {
    /// `None` is the default class: every event without its own entry.
    pub event: Option<String>,
    pub max_age: Duration,
}

#[derive(Default)]
struct Window {
    minute: i64,
    total: u64,
    written: u64,
}

/// A [`JsonlStore`] front with per-class sampling and retention.
pub struct ClassedLog {
    store: Arc<JsonlStore>,
    rules: BTreeMap<String, u32>,
    retention: Vec<ClassRetention>,
    windows: Mutex<BTreeMap<String, Window>>,
}

impl ClassedLog {
    pub fn new(
        store: Arc<JsonlStore>,
        rules: Vec<ClassRule>,
        retention: Vec<ClassRetention>,
    ) -> Self {
        Self {
            store,
            rules: rules
                .into_iter()
                .map(|rule| (rule.event, rule.allowance_per_minute))
                .collect(),
            retention,
            windows: Mutex::new(BTreeMap::new()),
        }
    }

    fn count_line(event: &str, window: &Window) -> Value {
        json!({
            "schema_version": 1,
            "ts_ms": window.minute * MINUTE_MS,
            "level": "info",
            "event": event,
            "rolled": true,
            "minute_ms": window.minute * MINUTE_MS,
            "count": window.total,
            "written": window.written,
        })
    }

    /// Append `value`, or count it when its class is over its allowance this minute. Returns true
    /// when a line was written for it.
    pub fn append(&self, value: &Value) -> io::Result<bool> {
        let event = value.get("event").and_then(Value::as_str).unwrap_or("");
        let Some(&allowance) = self.rules.get(event) else {
            self.store.append_value(value)?;
            return Ok(true);
        };
        let ts = value
            .get("ts_ms")
            .and_then(Value::as_i64)
            .unwrap_or_else(crate::now_ms);
        let minute = ts.div_euclid(MINUTE_MS);
        let (closed, write_it) = {
            let mut windows = self.windows.lock().unwrap_or_else(|p| p.into_inner());
            let window = windows.entry(event.to_string()).or_insert_with(|| Window {
                minute,
                ..Window::default()
            });
            let mut closed = None;
            if minute > window.minute {
                if window.total > window.written {
                    closed = Some(Self::count_line(event, window));
                }
                *window = Window {
                    minute,
                    ..Window::default()
                };
            }
            window.total += 1;
            let write_it = window.written < u64::from(allowance);
            if write_it {
                window.written += 1;
            }
            (closed, write_it)
        };
        if let Some(line) = closed {
            self.store.append_value(&line)?;
        }
        if write_it {
            self.store.append_value(value)?;
        }
        Ok(write_it)
    }

    /// Write the count line of every open minute that suppressed records. Call before an export
    /// or at shutdown.
    pub fn flush_counts(&self) -> io::Result<()> {
        let lines: Vec<Value> = {
            let mut windows = self.windows.lock().unwrap_or_else(|p| p.into_inner());
            let lines = windows
                .iter()
                .filter(|(_, window)| window.total > window.written)
                .map(|(event, window)| Self::count_line(event, window))
                .collect();
            windows.clear();
            lines
        };
        for line in lines {
            self.store.append_value(&line)?;
        }
        Ok(())
    }

    /// Remove records older than their class's maximum age (the current file and every rotated
    /// predecessor). `now_ms` is the reference time. A record without `ts_ms`, or of a class with
    /// no retention entry and no default, is kept.
    pub fn prune(&self, now_ms: i64) -> io::Result<crate::RetainReport> {
        let default = self
            .retention
            .iter()
            .find(|entry| entry.event.is_none())
            .map(|entry| entry.max_age);
        self.store.retain(|value| {
            let event = value.get("event").and_then(Value::as_str).unwrap_or("");
            let max_age = self
                .retention
                .iter()
                .find(|entry| entry.event.as_deref() == Some(event))
                .map(|entry| entry.max_age)
                .or(default);
            let (Some(max_age), Some(ts)) = (max_age, value.get("ts_ms").and_then(Value::as_i64))
            else {
                return true;
            };
            let age_ms = i64::try_from(max_age.as_millis()).unwrap_or(i64::MAX);
            ts >= now_ms.saturating_sub(age_ms)
        })
    }

    /// The store behind this log.
    pub fn store(&self) -> &Arc<JsonlStore> {
        &self.store
    }
}
