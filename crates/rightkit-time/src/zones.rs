//! Local wall time to UTC for a small set of named zones, and whole-second UTC instants.
//!
//! LIMIT: there is no timezone database here, so four zones are written out as explicit offset
//! tables: `UTC`, `Asia/Kolkata` (fixed +05:30), `Europe/London` and `America/New_York`. The two
//! DST zones carry their published transitions for 2026 and 2027 only. Any other zone is
//! `timezone_unsupported`; a DST zone asked about outside 2026 and 2027 is `time_out_of_table`.
use std::time::{SystemTime, UNIX_EPOCH};

/// An error: a stable machine-readable code and a human-readable message.
pub type ZoneError = (&'static str, String);

const DAY: i64 = 86_400;

/// Seconds since the epoch of a UTC civil time.
fn utc(year: i64, month: u32, day: u32, hour: i64) -> i64 {
    crate::days_from_civil(year, month, day) * DAY + hour * 3_600
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let (y, m, d) = crate::civil_from_days(z);
    (y, m as i64, d as i64)
}

/// A zone's offset history: `initial` until the first transition, then each `(utc instant, offset
/// from that instant)`. `years` bounds the local years the table is valid for (`None`: all years).
#[derive(Clone)]
struct Zone {
    name: &'static str,
    initial: i64,
    transitions: Vec<(i64, i64)>,
    years: Option<(i64, i64)>,
}

pub const SUPPORTED_ZONES: &str = "UTC, Asia/Kolkata, Europe/London, America/New_York";

fn zone(name: &str) -> Result<Zone, ZoneError> {
    match name {
        "UTC" => Ok(Zone {
            name: "UTC",
            initial: 0,
            transitions: vec![],
            years: None,
        }),
        "Asia/Kolkata" => Ok(Zone {
            name: "Asia/Kolkata",
            initial: 19_800,
            transitions: vec![],
            years: None,
        }),
        // EU rules: last Sunday of March and of October, both 01:00 UTC (2026: 29 Mar, 25 Oct; 2027: 28 Mar, 31 Oct).
        "Europe/London" => Ok(Zone {
            name: "Europe/London",
            initial: 0,
            transitions: vec![
                (utc(2026, 3, 29, 1), 3_600),
                (utc(2026, 10, 25, 1), 0),
                (utc(2027, 3, 28, 1), 3_600),
                (utc(2027, 10, 31, 1), 0),
            ],
            years: Some((2026, 2027)),
        }),
        // US rules: second Sunday of March (07:00 UTC) to first Sunday of November (06:00 UTC).
        "America/New_York" => Ok(Zone {
            name: "America/New_York",
            initial: -18_000,
            transitions: vec![
                (utc(2026, 3, 8, 7), -14_400),
                (utc(2026, 11, 1, 6), -18_000),
                (utc(2027, 3, 14, 7), -14_400),
                (utc(2027, 11, 7, 6), -18_000),
            ],
            years: Some((2026, 2027)),
        }),
        other => Err((
            "timezone_unsupported",
            format!(
                "timezone {other} has no offset table in this build; supported: {SUPPORTED_ZONES}"
            ),
        )),
    }
}

/// Which of two occurrences of an ambiguous local time the caller means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fold {
    /// The first occurrence (the earlier UTC instant).
    Earlier,
    /// The second occurrence (the later UTC instant).
    Later,
}

/// Parse `earlier` or `later`. Absent is `None`.
pub fn parse_fold(raw: Option<&str>) -> Result<Option<Fold>, ZoneError> {
    match raw {
        None => Ok(None),
        Some("earlier") => Ok(Some(Fold::Earlier)),
        Some("later") => Ok(Some(Fold::Later)),
        Some(other) => Err((
            "bad_input",
            format!("fold must be earlier or later, got {other}"),
        )),
    }
}

/// `YYYY-MM-DDTHH:MM` or `YYYY-MM-DDTHH:MM:SS` with no zone, as seconds on the naive wall clock.
/// Checked by `crate::DateTime::parse_rfc3339` with a `Z` appended; fractions, a lowercase `T`
/// and leap seconds are refused here because the cache stores whole-second wall times.
fn parse_naive(body: &str) -> Option<i64> {
    let b = body.as_bytes();
    if b.len() < 16
        || !body.is_ascii()
        || b[10] != b'T'
        || body.contains(['.', 'Z', 'z', '+'])
        || b[11..].contains(&b'-')
    {
        return None;
    }
    let full = if b.len() == 16 {
        format!("{body}:00Z")
    } else {
        format!("{body}Z")
    };
    let t = crate::DateTime::parse_rfc3339(&full)?;
    (t.second <= 59).then(|| t.unix_seconds())
}

/// Parse a local wall time as entered. Refuses anything with a zone suffix.
pub fn parse_local(raw: &str, what: &str) -> Result<i64, ZoneError> {
    parse_naive(raw).ok_or_else(|| {
        (
            "bad_input",
            format!(
                "{what} must be YYYY-MM-DDTHH:MM or YYYY-MM-DDTHH:MM:SS with no zone, got {raw}"
            ),
        )
    })
}

/// Parse a UTC instant `YYYY-MM-DDTHH:MM:SSZ` (the form [`format_instant`] writes).
pub fn parse_instant(raw: &str, what: &str) -> Result<i64, ZoneError> {
    raw.strip_suffix('Z').and_then(parse_naive).ok_or_else(|| {
        (
            "bad_input",
            format!("{what} must be a UTC instant YYYY-MM-DDTHH:MM:SSZ, got {raw}"),
        )
    })
}

/// Format seconds since the epoch as `YYYY-MM-DDTHH:MM:SSZ`. Fixed width, so text order is time order.
pub fn format_instant(secs: i64) -> String {
    crate::DateTime::from_unix(secs, 0)
        .map(|t| t.format_rfc3339())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

/// Current time, or the `now` the caller passed (deterministic tests and replays).
pub fn now_secs(raw: Option<&str>) -> Result<i64, ZoneError> {
    match raw {
        Some(s) => parse_instant(s, "now"),
        None => Ok(SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)),
    }
}

/// The UTC instant of a local wall time in a zone.
///
/// The wall time is matched against each offset period: a candidate instant `u` is valid when
/// `u` falls inside its period and `u + offset` is the wall time. Zero candidates means the wall
/// time is skipped (`time_nonexistent`). Two mean it occurs twice (`time_ambiguous` unless `fold`
/// picks one). `fold` is ignored when the time occurs once.
pub fn to_utc(timezone: &str, local_raw: &str, fold: Option<Fold>) -> Result<i64, ZoneError> {
    let z = zone(timezone)?;
    let local = parse_local(local_raw, "local_time")?;
    if let Some((first, last)) = z.years {
        let (year, _, _) = civil_from_days(local.div_euclid(DAY));
        if year < first || year > last {
            return Err((
                "time_out_of_table",
                format!(
                    "{} has offset rules only for {first} and {last}; {local_raw} is outside them",
                    z.name
                ),
            ));
        }
    }
    // Periods as (start, end, offset): start None is unbounded before, end None unbounded after.
    let mut periods: Vec<(Option<i64>, Option<i64>, i64)> =
        Vec::with_capacity(z.transitions.len() + 1);
    let mut start = None;
    let mut offset = z.initial;
    for &(at, next) in &z.transitions {
        periods.push((start, Some(at), offset));
        start = Some(at);
        offset = next;
    }
    periods.push((start, None, offset));

    let mut candidates: Vec<i64> = periods
        .iter()
        .filter_map(|&(s, e, off)| {
            let u = local - off;
            let after_start = s.is_none_or(|s| u >= s);
            let before_end = e.is_none_or(|e| u < e);
            (after_start && before_end).then_some(u)
        })
        .collect();
    candidates.sort_unstable();
    match (candidates.as_slice(), fold) {
        ([], _) => Err((
            "time_nonexistent",
            format!(
                "{local_raw} does not exist in {} (the clocks skip it)",
                z.name
            ),
        )),
        ([only], _) => Ok(*only),
        ([first, ..], Some(Fold::Earlier)) => Ok(*first),
        ([.., last], Some(Fold::Later)) => Ok(*last),
        (_, None) => Err((
            "time_ambiguous",
            format!(
                "{local_raw} occurs twice in {} (clocks go back); pass fold earlier or later",
                z.name
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn london_spring_gap_and_autumn_fold() {
        assert_eq!(
            to_utc("Europe/London", "2026-03-29T01:30", None)
                .unwrap_err()
                .0,
            "time_nonexistent"
        );
        assert_eq!(
            to_utc("Europe/London", "2026-10-25T01:30", None)
                .unwrap_err()
                .0,
            "time_ambiguous"
        );
        let early = to_utc("Europe/London", "2026-10-25T01:30", Some(Fold::Earlier)).unwrap();
        let late = to_utc("Europe/London", "2026-10-25T01:30", Some(Fold::Later)).unwrap();
        assert_eq!(late - early, 3_600);
        assert_eq!(format_instant(early), "2026-10-25T00:30:00Z");
    }

    #[test]
    fn fixed_zones_and_refusals() {
        assert_eq!(
            to_utc("Asia/Kolkata", "2026-01-01T05:30", None).unwrap(),
            parse_instant("2026-01-01T00:00:00Z", "t").unwrap()
        );
        assert_eq!(
            to_utc("Mars/Base", "2026-01-01T00:00", None).unwrap_err().0,
            "timezone_unsupported"
        );
        assert_eq!(
            to_utc("America/New_York", "2030-01-01T00:00", None)
                .unwrap_err()
                .0,
            "time_out_of_table"
        );
        assert!(parse_local("2026-01-01T00:00Z", "t").is_err());
        assert_eq!(parse_fold(Some("later")).unwrap(), Some(Fold::Later));
        assert!(parse_fold(Some("x")).is_err());
    }
}
