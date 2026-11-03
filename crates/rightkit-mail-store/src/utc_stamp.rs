//! Wall-clock UTC stamps for backup names and the migration log. The civil math and the RFC 3339
//! text are `rightkit-time`'s; this module only reads the system clock.
use rightkit_time::DateTime;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// `2026-10-20T12:00:00Z`.
pub(crate) fn rfc3339_secs() -> String {
    DateTime::from_unix(now_secs(), 0)
        .expect("offset 0 is always valid")
        .format_rfc3339()
}

/// `20261020T120000Z`: the basic form used in file names.
pub(crate) fn compact() -> String {
    rfc3339_secs().replace(['-', ':'], "")
}
