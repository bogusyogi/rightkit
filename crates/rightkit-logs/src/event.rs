use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const EVENT_SCHEMA_VERSION: u16 = 1;

/// Largest batch a host should write in one call. [`cap_batch`] keeps at most this many records.
pub const DEFAULT_BATCH_CAP: usize = 500;

/// Event name of the record that [`cap_batch`] puts first when it dropped records.
pub const LOGS_DROPPED_EVENT: &str = "logs_dropped";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    pub schema_version: u16,
    pub ts_ms: i64,
    pub level: Level,
    pub event: String,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

impl EventRecord {
    pub fn new(ts_ms: i64, level: Level, event: impl Into<String>) -> Self {
        Self {
            schema_version: EVENT_SCHEMA_VERSION,
            ts_ms,
            level,
            event: event.into(),
            fields: Map::new(),
        }
    }

    pub fn with_field(mut self, key: impl Into<String>, value: Value) -> Self {
        let key = key.into();
        if !matches!(key.as_str(), "schema_version" | "ts_ms" | "level" | "event") {
            self.fields.insert(key, value);
        }
        self
    }
}

/// Caps a batch at `max` records (at least 1), dropping the oldest. When any were dropped, the
/// result starts with one [`LOGS_DROPPED_EVENT`] warning record whose `dropped` field counts them,
/// so the gap shows in the log. The record takes the timestamp of the oldest kept event, and it
/// counts toward `max`: the result never holds more than `max` records. Records are kept in their
/// original order.
pub fn cap_batch(mut events: Vec<EventRecord>, max: usize) -> Vec<EventRecord> {
    let max = max.max(1);
    if events.len() <= max {
        return events;
    }
    let keep = max - 1;
    let dropped = events.len() - keep;
    let newest_ts = events.last().map_or(0, |event| event.ts_ms);
    let kept = events.split_off(dropped);
    let ts_ms = kept.first().map_or(newest_ts, |event| event.ts_ms);
    let record = EventRecord::new(ts_ms, Level::Warn, LOGS_DROPPED_EVENT)
        .with_field("dropped", Value::from(dropped as u64));
    let mut out = Vec::with_capacity(max);
    out.push(record);
    out.extend(kept);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(count: i64) -> Vec<EventRecord> {
        (0..count)
            .map(|n| EventRecord::new(n, Level::Info, format!("e{n}")))
            .collect()
    }

    #[test]
    fn a_batch_within_the_cap_is_unchanged() {
        let out = cap_batch(batch(3), 3);
        assert_eq!(out, batch(3));
    }

    #[test]
    fn oldest_records_are_dropped_and_a_warning_record_leads() {
        let out = cap_batch(batch(5), 3);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].event, LOGS_DROPPED_EVENT);
        assert_eq!(out[0].level, Level::Warn);
        assert_eq!(out[0].ts_ms, 3);
        assert_eq!(
            out[0].fields.get("dropped").and_then(Value::as_u64),
            Some(3)
        );
        assert_eq!(out[1].event, "e3");
        assert_eq!(out[2].event, "e4");
    }

    #[test]
    fn a_cap_of_one_keeps_only_the_warning_record() {
        let out = cap_batch(batch(4), 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, LOGS_DROPPED_EVENT);
        assert_eq!(
            out[0].fields.get("dropped").and_then(Value::as_u64),
            Some(4)
        );
    }
}
