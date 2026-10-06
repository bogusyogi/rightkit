//! Diagnostics host for `@rightkit/logs`: renderer events -> redacted JSONL; bundle collection.
//! Command names are the fixed standard names of `@rightkit/logs`/`@rightkit/tauri`.

use rightkit_logs::{
    collect_diagnostics as collect_shared, BundleMetadata, CollectionPolicy, DiagnosticsBundle, EventRecord,
    JsonlStore, Level, LogSource, RedactionPolicy,
};
use serde_json::{json, Value};
use std::path::PathBuf;

const MAX_RENDERER_LOG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_BATCH_EVENTS: usize = 500;
const LOG_FAMILIES: [(&str, &str); 3] =
    [("shell", "rightapp"), ("engine", "rightapp-engine"), ("renderer", "rightkit-renderer")];

fn logs_dir() -> PathBuf {
    crate::app_data_root().join("logs")
}

#[tauri::command]
pub fn rightkit_logs_write(events: Vec<Value>) -> Result<(), String> {
    let store = JsonlStore::new(logs_dir().join("rightkit-renderer.events.jsonl"), MAX_RENDERER_LOG_BYTES);
    let dropped = events.len().saturating_sub(MAX_BATCH_EVENTS);
    if dropped > 0 {
        store
            .append(&EventRecord::new(0, Level::Warn, "logs_dropped").with_field("dropped_count", json!(dropped)))
            .map_err(|e| format!("write diagnostic log: {e}"))?;
    }
    for mut event in events.into_iter().skip(dropped) {
        RedactionPolicy::default().redact_value(&mut event);
        let event: EventRecord = serde_json::from_value(event).map_err(|e| format!("invalid diagnostic event: {e}"))?;
        store.append(&event).map_err(|e| format!("write diagnostic log: {e}"))?;
    }
    Ok(())
}

#[tauri::command]
pub fn rightkit_logs_collect() -> DiagnosticsBundle {
    let dir = logs_dir();
    let sources: Vec<LogSource> = LOG_FAMILIES
        .iter()
        .map(|(name, prefix)| LogSource::new(*name, dir.join(format!("{prefix}.current.jsonl"))))
        .collect();
    let policy = CollectionPolicy { include_rotated: false, ..CollectionPolicy::default() };
    collect_shared(
        &sources,
        BundleMetadata {
            app_name: "RightApp".into(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            build: if cfg!(debug_assertions) { "debug" } else { "release" }.into(),
            os: std::env::consts::OS.into(),
            settings: json!({}),
        },
        &policy,
    )
}

#[tauri::command]
pub fn rightkit_logs_clear() -> Result<(), String> {
    for (_, prefix) in LOG_FAMILIES {
        let path = logs_dir().join(format!("{prefix}.current.jsonl"));
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("clear diagnostic logs: {e}")),
        }
    }
    Ok(())
}
