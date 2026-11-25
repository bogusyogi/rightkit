//! Every IANA zone, from a bundled time zone database (feature `iana-full`).
//!
//! This is the one file that depends on a time zone database crate (`jiff`). Without the feature the
//! scheduler knows `UTC`, a fixed `UTC+HH:MM` offset and the eleven named zones written out in
//! `timezone.rs`. Adding `jiff` to RightKit's dependency allowlist is an open owner decision; the crate is
//! in the workspace lock already (through other dependencies) but is allowlisted only for one app.

use jiff::tz::TimeZone;
use jiff::Timestamp;

/// The database's own spelling of `name`, or `None` when it is not an IANA zone.
pub(crate) fn canonical(name: &str) -> Option<String> {
    let zone = TimeZone::get(name).ok()?;
    zone.iana_name().map(str::to_owned)
}

/// The zone's offset from UTC, in minutes, at the UTC minute `epoch_minute`.
pub(crate) fn offset_minutes_at(name: &str, epoch_minute: i64) -> Result<i32, String> {
    let zone = TimeZone::get(name).map_err(|_| format!("unsupported IANA time zone: {name}"))?;
    let seconds = epoch_minute
        .checked_mul(60)
        .ok_or_else(|| "time is out of range".to_string())?;
    let at = Timestamp::from_second(seconds).map_err(|_| "time is out of range".to_string())?;
    Ok(zone.to_offset(at).seconds() / 60)
}
