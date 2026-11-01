//! One-line JSON lifecycle events on stderr, shaped by `rightkit_logs::EventRecord`.

use rightkit_logs::{EventRecord, Level};
use serde_json::Value;

/// Emit `value` (an object with an `"event"` name; every other key is a field) as one
/// `EventRecord` line. A `failed`/`skipped` stage and the timeout and oversize events are
/// warnings; everything else is info.
pub(crate) fn emit(value: Value) {
    let Value::Object(mut fields) = value else {
        return;
    };
    let Some(Value::String(event)) = fields.remove("event") else {
        return;
    };
    let warn = matches!(
        fields.get("stage").and_then(Value::as_str),
        Some("failed" | "skipped")
    ) || event.ends_with("_timeout")
        || event.ends_with("_oversized");
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default();
    let mut record = EventRecord::new(ts_ms, if warn { Level::Warn } else { Level::Info }, event);
    for (key, field) in fields {
        record = record.with_field(key, field);
    }
    if let Ok(line) = serde_json::to_string(&record) {
        stderr_line!("{line}");
    }
}
