//! Minimal timestamp helpers (no chrono) so Cortex stays self-contained. ISO-8601 UTC seconds.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch.
pub fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// UTC `YYYY-MM-DDTHH:MM:SSZ` for now, through the time owner.
pub fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    rightkit_time::DateTime::from_unix(secs, 0)
        .map(|at| at.format_rfc3339())
        .unwrap_or_default()
}
