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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_iso_is_rfc3339_utc_seconds() {
        let stamp = now_iso();
        assert!(stamp.ends_with('Z'), "got {stamp:?}");
        let bytes = stamp.as_bytes();
        assert_eq!(bytes.len(), 20, "got {stamp:?}");
        assert_eq!(bytes[4], b'-');
        assert_eq!(bytes[7], b'-');
        assert_eq!(bytes[10], b'T');
        assert_eq!(bytes[13], b':');
        assert_eq!(bytes[16], b':');
        assert!(stamp.starts_with("20"), "got {stamp:?}");
    }

    #[test]
    fn now_millis_is_after_2020_and_non_decreasing() {
        let first = now_millis();
        let second = now_millis();
        assert!(first > 1_577_836_800_000, "got {first}");
        assert!(second >= first);
    }
}
