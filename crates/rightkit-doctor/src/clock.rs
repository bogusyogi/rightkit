//! UTC timestamps and lifecycle lines for doctors that run long enough to need progress.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` for `time` (proleptic Gregorian, no runtime or calendar crate).
pub fn rfc3339_millis(time: SystemTime) -> String {
    let elapsed = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = elapsed.as_secs() as i64;
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    // Civil date from Unix days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096).div_euclid(365);
    let year = yoe + era * 400;
    let day_of_year = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let month = (5 * day_of_year + 2).div_euclid(153);
    let day = day_of_year - (153 * month + 2).div_euclid(5) + 1;
    let month = month + if month < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        day_seconds / 3_600,
        day_seconds % 3_600 / 60,
        day_seconds % 60,
        elapsed.subsec_millis()
    )
}

/// The current time as [`rfc3339_millis`].
pub fn now() -> String {
    rfc3339_millis(SystemTime::now())
}

/// A lifecycle record: `{"kind": kind, "phase": phase, "detail": detail, "at": now}`.
pub fn lifecycle_line(kind: &str, phase: &str, detail: Value) -> Value {
    json!({ "kind": kind, "phase": phase, "detail": detail, "at": now() })
}

/// Print a lifecycle record on stderr, one JSON object per line, so stdout stays the report.
pub fn emit_lifecycle(kind: &str, phase: &str, detail: Value) {
    eprintln!("{}", lifecycle_line(kind, phase, detail));
}
