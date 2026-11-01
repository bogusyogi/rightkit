//! Diagnostic events for every host process (shell, engine, job runner). One JSONL file under
//! `<app data>/logs/diagnostics.jsonl`, written through `rightkit_logs::JsonlStore` (rotation, `EventRecord` rows). stderr stays the engine's protocol channel; diagnostics never go there.

use rightkit_logs::{EventRecord, JsonlStore, Level};
use serde_json::Value;

const MAX_DIAG_BYTES: u64 = 5 * 1024 * 1024;

fn store() -> JsonlStore {
    JsonlStore::new(
        super::app_data_root()
            .join("logs")
            .join("diagnostics.jsonl"),
        MAX_DIAG_BYTES,
    )
    .with_cross_process_lock()
}

/// Append one diagnostic event. Never fails the caller: a write failure is counted by the store.
pub fn event(level: Level, name: &str, fields: &[(&str, Value)]) {
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0);
    let record = fields
        .iter()
        .fold(EventRecord::new(ts_ms, level, name), |r, (k, v)| {
            r.with_field(*k, v.clone())
        });
    store().append_best_effort(&record);
}

pub fn info(name: &str, fields: &[(&str, Value)]) {
    event(Level::Info, name, fields);
}

pub fn warn(name: &str, fields: &[(&str, Value)]) {
    event(Level::Warn, name, fields);
}

pub fn error(name: &str, fields: &[(&str, Value)]) {
    event(Level::Error, name, fields);
}
