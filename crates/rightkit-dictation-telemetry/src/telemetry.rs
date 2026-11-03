use rightkit_transcript_library::delivery::{DeliveryRecord, ForegroundTarget};
use serde::Serialize;
use serde_json::json;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};

pub mod utterance_timeline;

// ---------------------------------------------------------------------------
// Per-event cost. Two caches:
//
// 1. `DIAGNOSTICS_ENABLED_CACHE`: an atomic mirror of the host's
//  `settings::diagnostics_enabled()`, avoiding a clone of the ENTIRE
//  `SettingsBlob` under the global settings lock for every single event.
//  States: 0 = unknown (consult the provider), 1 = off, 2 = on. The host
//  calls `notify_settings_changed()` after every persisted mutation
//  (`settings::mutate_and_save`), and `install_settings_provider` resets it,
//  so the cache can never go stale across a real toggle — gating semantics
//  are preserved exactly.
// ---------------------------------------------------------------------------
static DIAGNOSTICS_ENABLED_CACHE: AtomicU8 = AtomicU8::new(0);
/// Bumped on every invalidation. A reader that loaded settings BEFORE an
/// invalidation must not publish that stale answer AFTER it, or a user who
/// just turned diagnostics off keeps being logged until the next settings
/// change.
static DIAGNOSTICS_CACHE_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Invalidate the cached diagnostics-enabled flag. Called by the host's
/// settings layer after every successful mutation and by
/// `install_settings_provider` (a fresh provider may report differently).
pub fn notify_settings_changed() {
    DIAGNOSTICS_CACHE_GENERATION.fetch_add(1, Ordering::AcqRel);
    DIAGNOSTICS_ENABLED_CACHE.store(0, Ordering::Release);
}

fn diagnostics_enabled_cached() -> bool {
    match DIAGNOSTICS_ENABLED_CACHE.load(Ordering::Acquire) {
        1 => false,
        2 => true,
        _ => resolve_diagnostics_enabled(crate::settings::diagnostics_enabled),
    }
}

/// Read the setting and cache it ONLY if no invalidation happened meanwhile.
/// The answer returned is always the one just read; only publication is
/// conditional, so a racing reader can never pin a stale value.
fn resolve_diagnostics_enabled(read: impl FnOnce() -> bool) -> bool {
    let generation = DIAGNOSTICS_CACHE_GENERATION.load(Ordering::Acquire);
    let enabled = read();
    if DIAGNOSTICS_CACHE_GENERATION.load(Ordering::Acquire) == generation {
        DIAGNOSTICS_ENABLED_CACHE.store(if enabled { 2 } else { 1 }, Ordering::Release);
        // An invalidation can land between the check and the store; undo our
        // publication if it did, leaving the cache empty for the next reader.
        if DIAGNOSTICS_CACHE_GENERATION.load(Ordering::Acquire) != generation {
            DIAGNOSTICS_ENABLED_CACHE.store(0, Ordering::Release);
        }
    }
    enabled
}

// ALL telemetry emitters use
// `append_or_stderr`, not a bare `let _ = append(..)`. Emission is gated,
// redacted, and handed to one bounded background writer so producer paths
// never wait on disk, stderr, or a file lock.
pub fn log_state(state: &impl Serialize) {
    let state_value = rightkit_dictation_core::redact_diagnostic_event(json!(state));
    let level = if state_value.get("state").and_then(|value| value.as_str()) == Some("error") {
        "error"
    } else {
        "info"
    };
    append_or_stderr(json!({
        "event": "state_changed",
        "level": level,
        "state": state_value,
        "ts_ms": rightkit_transcript_library::delivery::now_ms()
    }));
}

pub fn log_delivery(record: &DeliveryRecord, session_id: Option<&str>, send_enter: Option<bool>) {
    let diagnostic_outcome = match &record.outcome {
        rightkit_transcript_library::delivery::DeliveryOutcome::Pasted => {
            json!({ "kind": "paste_keystroke_sent" })
        }
        outcome => json!(outcome),
    };
    let focus_target_kind = record
        .target
        .foreground_target
        .as_ref()
        .map(|target| match target {
            ForegroundTarget::WindowHandle { .. } => "window_handle",
            ForegroundTarget::ProcessId { .. } => "process_id",
        });
    append_or_stderr(json!({
        "event": "delivery_recorded",
        "session_id": session_id,
        "delivery_id": record.delivery_id,
        "outcome": diagnostic_outcome,
        "method": record.method,
        "elapsed_ms": record.elapsed_ms,
        "delivery_timing": record.delivery_timing,
        "recording_ms": record.recording_ms,
        "pre_polish_chars": record.raw_transcript.as_deref().map(|value| value.chars().count()),
        "pre_polish_words": record.raw_transcript.as_deref().map(|value| value.split_whitespace().count()),
        "delivered_chars": record.transcript.chars().count(),
        "delivered_words": record.transcript.split_whitespace().count(),
        "send_enter": send_enter,
        "focus": {
            "target_kind": focus_target_kind,
            "text_input": record.target.focused_text_input,
            "elevated": record.target.is_elevated
        },
        "ts_ms": rightkit_transcript_library::delivery::now_ms()
    }));
}

pub fn log_input_trigger(source: &str) {
    utterance_timeline::note_trigger(source);
    append_or_stderr(json!({
        "event": "input_trigger",
        "level": "info",
        "source": source,
        "ts_ms": rightkit_transcript_library::delivery::now_ms()
    }));
}

pub fn log_transcript_final(
    session_id: &str,
    text: &str,
    raw_text: Option<&str>,
    recording_ms: Option<u64>,
) {
    append_or_stderr(json!({
        "event": "transcript_final_received",
        "level": "info",
        "session_id": session_id,
        "recording_ms": recording_ms,
        "text_chars": text.chars().count(),
        "text_words": text.split_whitespace().count(),
        "raw_text_chars": raw_text.map(|value| value.chars().count()),
        "raw_text_words": raw_text.map(|value| value.split_whitespace().count()),
        "ts_ms": rightkit_transcript_library::delivery::now_ms()
    }));
}

pub fn log_message(event: &str, message: &str) {
    // Enqueue only; the writer owns disk failure handling.
    append_or_stderr(json!({
        "event": event,
        "level": inferred_level(event),
        "message": message,
        "ts_ms": rightkit_transcript_library::delivery::now_ms()
    }));
}

/// Clipboard diagnostics must remain content-free. Character count is enough
/// to distinguish empty/stale requests without persisting transcript text.
pub fn log_clipboard_copy(text_chars: usize) {
    log_clipboard_copy_to(&log_path(), diagnostics_enabled_cached(), text_chars);
}

/// Path-parameterized clipboard event for privacy regression coverage.
pub fn log_clipboard_copy_to(path: &Path, enabled: bool, text_chars: usize) {
    append_if_enabled_to(
        path,
        enabled,
        json!({
            "event": "copy_text_to_clipboard",
            "level": "info",
            "text_chars": text_chars,
            "ts_ms": rightkit_transcript_library::delivery::now_ms()
        }),
    );
}

pub fn log_engine_diagnostics(
    schema_name: &str,
    session_id: Option<&str>,
    request_id: &str,
    trace_id: &str,
    diagnostics: &serde_json::Value,
) {
    append_or_stderr(json!({
        "event": "engine_diagnostics",
        "level": "debug",
        "schema_name": schema_name,
        "session_id": session_id,
        "request_id": request_id,
        "trace_id": trace_id,
        "diagnostics": diagnostics,
        "ts_ms": rightkit_transcript_library::delivery::now_ms()
    }));
}

/// Telemetry is best-effort: it can never
/// crash the app. All emitters route through one gated, redacted, bounded
/// background writer; disk failures stay isolated to that worker.
pub fn append_or_stderr(value: serde_json::Value) {
    if !diagnostics_enabled_cached() {
        return;
    }
    // The writer resolves events.jsonl on its own thread: callers include the UI thread,
    // which must never wait on the app-data folder lookup.
    let value = rightkit_dictation_core::redact_diagnostic_event(normalize_event(value));
    crate::telemetry_writer::append_events(value);
}

/// Gate an ordinary diagnostic event before any directory/file operation.
/// Kept path-parameterized for a real no-file integration test.
pub fn append_if_enabled_to(path: &std::path::Path, enabled: bool, value: serde_json::Value) {
    if !enabled {
        return;
    }
    append_or_stderr_to(path, value);
}

/// Testable core of `append_or_stderr`. It never performs file I/O on caller
/// thread. `pub` so integration tests can drive unwritable and stalled paths.
pub fn append_or_stderr_to(path: &std::path::Path, value: serde_json::Value) {
    let value = rightkit_dictation_core::redact_diagnostic_event(normalize_event(value));
    crate::telemetry_writer::append(path, value);
}

fn normalize_event(mut value: serde_json::Value) -> serde_json::Value {
    let Some(object) = value.as_object_mut() else {
        return json!({
            "schema_version": 1,
            "ts_ms": rightkit_transcript_library::delivery::now_ms(),
            "level": "warn",
            "event": "non_object_telemetry",
            "value": value
        });
    };
    object.insert("schema_version".to_string(), json!(1));
    object
        .entry("ts_ms")
        .or_insert_with(|| json!(rightkit_transcript_library::delivery::now_ms()));
    if !object.contains_key("level") {
        let level = object
            .get("event")
            .and_then(|event| event.as_str())
            .map(inferred_level)
            .unwrap_or("info");
        object.insert("level".to_string(), json!(level));
    }
    value
}

fn inferred_level(event: &str) -> &'static str {
    let event = event.to_ascii_lowercase();
    if event == "error"
        || event.ends_with("_error")
        || event.ends_with("_failed")
        || event.ends_with("_failure")
    {
        "error"
    } else if event.ends_with("_warning") || event.ends_with("_warn") || event.ends_with("_retry") {
        "warn"
    } else {
        "info"
    }
}

pub fn log_path() -> PathBuf {
    crate::app_paths::app_data_file("events.jsonl")
}

pub fn engine_log_path() -> PathBuf {
    crate::app_paths::app_data_file("engine-events.jsonl")
}

pub fn engine_command_log_path() -> PathBuf {
    crate::app_paths::app_data_file("engine-command-events.jsonl")
}

/// Fixed-script setup results remain local unless first route fails, but they
/// belong in user-requested diagnostics export & deletion alongside app logs.
pub fn onboarding_calibration_log_path() -> PathBuf {
    crate::app_paths::app_data_file("onboarding-calibration.jsonl")
}

/// Setup route evidence is always retained locally. It contains fixed-passage
/// metrics only; generic dictation does not enter this ledger.
pub fn log_onboarding_calibration(value: serde_json::Value) {
    append_or_stderr_to(&onboarding_calibration_log_path(), value);
}

/// Copy retained redacted log generations in this process's writer order.
/// Producers keep enqueueing; separate processes are not paused by this barrier.
/// Returned names are relative to `destination` for archive manifests.
pub fn snapshot_log_files(destination: &Path) -> Result<Vec<String>, String> {
    crate::telemetry_writer::snapshot_log_files(
        destination,
        &[
            log_path(),
            engine_log_path(),
            engine_command_log_path(),
            onboarding_calibration_log_path(),
        ],
    )
}

pub fn clear_local_logs() -> Result<(), String> {
    crate::telemetry_writer::clear_local_logs(&[
        log_path(),
        engine_log_path(),
        engine_command_log_path(),
        onboarding_calibration_log_path(),
    ])
}

/// Wait until this process's queued telemetry writes have been attempted.
/// Best-effort append failures remain local; this is not an fsync guarantee.
/// Emitters never call this; it is for explicit diagnostics collection,
/// clearing, shutdown, and deterministic tests.
pub fn flush_pending_logs() -> Result<(), String> {
    crate::telemetry_writer::flush_pending_logs()
}
