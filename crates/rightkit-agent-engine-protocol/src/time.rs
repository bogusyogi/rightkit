use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    epoch_seconds_to_iso(secs)
}

/// RFC 3339 UTC text (`YYYY-MM-DDTHH:MM:SSZ`) for `secs` since the Unix epoch.
fn epoch_seconds_to_iso(secs: u64) -> String {
    i64::try_from(secs)
        .ok()
        .and_then(|secs| rightkit_time::DateTime::from_unix(secs, 0))
        .map(|dt| dt.format_rfc3339())
        .unwrap_or_default()
}
