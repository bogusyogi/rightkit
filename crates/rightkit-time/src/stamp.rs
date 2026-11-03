//! Wall-clock UTC stamps for logs, backup names and reports, read from the system clock.
use crate::DateTime;
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> (i64, u32) {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as i64, d.subsec_nanos())
}

fn at(secs: i64) -> DateTime {
    DateTime::from_unix(secs, 0).expect("offset 0 is always valid")
}

/// `2026-10-20T12:00:00Z`.
pub fn rfc3339_secs() -> String {
    at(now().0).format_rfc3339()
}

/// `2026-10-20T12:00:00.123Z`: always three fraction digits, so log lines sort and align.
pub fn rfc3339_millis() -> String {
    let (secs, nanos) = now();
    let whole = at(secs).format_rfc3339();
    format!("{}.{:03}Z", whole.trim_end_matches('Z'), nanos / 1_000_000)
}

/// RFC 3339 with the clock's sub-second part (trailing zeros trimmed), for report fields.
pub fn rfc3339_precise() -> String {
    let (secs, nanos) = now();
    DateTime { nanos, ..at(secs) }.format_rfc3339()
}

/// `20261020T120000Z`: the basic form used in file names.
pub fn compact() -> String {
    rfc3339_secs().replace(['-', ':'], "")
}
