// rightkit-time consumed by the exact registry pin in Cargo.toml, outside the workspace.
//
// Proves the whole public surface of the time owner, with fixed known values and no clock:
// days_from_civil and civil_from_days (epoch, pre-epoch, a leap day, and a round trip over a wide
// sweep); is_leap_year, days_in_month (including 0 for an invalid month), weekday, and the Date
// type (new refuses impossible dates, add_days crosses year boundaries, ordering); DateTime
// parse_rfc3339 (fractions, numeric offsets, lowercase T and Z, second 60 accepted as written),
// format_rfc3339 (fraction trimming, Z for offset 0, an inexpressible offset normalised to the
// same instant), unix_seconds, from_unix, valid_offset_secs, and a parse/format round trip over an
// instant and offset grid; the rejection list for malformed RFC 3339 text; format_http_date and
// parse_http_date for the RFC 7231 IMF-fixdate, including the refusal of a wrong weekday and of
// the obsolete forms. Public API only.
// Does not prove: named time zones and daylight saving (the crate has none; callers pass a fixed
// offset), the system clock (the crate never reads it), or anything outside the crate's public API.
// State: in-memory values only. No network, no files, no process spawn, no clock, no user data.
use rightkit_time::{
    civil_from_days, days_from_civil, days_in_month, format_http_date, is_leap_year,
    parse_http_date, valid_offset_secs, weekday, Date, DateTime,
};

fn civil_days() {
    assert_eq!(days_from_civil(1970, 1, 1), 0, "epoch day is zero");
    assert_eq!(days_from_civil(2000, 3, 1), 11_017, "known day count");
    assert_eq!(days_from_civil(1969, 12, 31), -1, "day before the epoch");
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(civil_from_days(-1), (1969, 12, 31));
    assert_eq!(civil_from_days(11_017), (2000, 3, 1));
    assert_eq!(
        civil_from_days(days_from_civil(2024, 2, 29)),
        (2024, 2, 29),
        "a leap day survives the round trip"
    );
    for z in (-800_000..800_000).step_by(997) {
        let (y, m, d) = civil_from_days(z);
        assert_eq!(days_from_civil(y, m, d), z, "civil round trip at day {z}");
    }
}

fn calendar_rules() {
    assert!(
        is_leap_year(2000) && is_leap_year(2024),
        "divisible by 4 or 400"
    );
    assert!(
        !is_leap_year(1900) && !is_leap_year(2026),
        "century and common years"
    );
    assert_eq!(days_in_month(2024, 2), 29);
    assert_eq!(days_in_month(2026, 2), 28);
    assert_eq!(days_in_month(2026, 1), 31);
    assert_eq!(days_in_month(2026, 4), 30);
    assert_eq!(days_in_month(2026, 0), 0, "month 0 is invalid");
    assert_eq!(days_in_month(2026, 13), 0, "month 13 is invalid");
    assert_eq!(weekday(1970, 1, 1), 4, "the epoch was a Thursday");
    assert_eq!(weekday(1994, 11, 6), 0, "0 is Sunday");
    assert_eq!(weekday(2000, 1, 1), 6, "2000-01-01 was a Saturday");
}

fn dates() {
    assert!(Date::new(2026, 2, 29).is_none(), "2026 is not a leap year");
    assert!(Date::new(2026, 4, 31).is_none(), "April has 30 days");
    assert!(Date::new(2026, 13, 1).is_none(), "month 13");
    assert!(Date::new(2026, 1, 0).is_none(), "day 0");
    let leap = Date::new(2024, 2, 29).expect("a real leap day");
    assert_eq!((leap.year, leap.month, leap.day), (2024, 2, 29));
    assert_eq!(leap.add_days(1), Date::new(2024, 3, 1).unwrap());
    assert_eq!(leap.add_days(366), Date::new(2025, 3, 1).unwrap());

    let end = Date::new(2026, 12, 31).unwrap();
    assert_eq!(
        end.add_days(1),
        Date::new(2027, 1, 1).unwrap(),
        "year rollover"
    );
    assert_eq!(
        end.add_days(-365),
        Date::new(2025, 12, 31).unwrap(),
        "back a year"
    );
    assert_eq!(end.add_days(0), end, "zero days is the same date");

    let epoch = Date::new(1970, 1, 1).unwrap();
    assert_eq!(epoch.days_since_epoch(), 0);
    assert_eq!(Date::from_days(0), epoch);
    assert_eq!(Date::from_days(-1), Date::new(1969, 12, 31).unwrap());
    assert_eq!(epoch.weekday(), 4);
    assert_eq!(Date::new(1994, 11, 6).unwrap().weekday(), 0);
    assert!(
        epoch < end && end > leap && leap > epoch,
        "dates order chronologically"
    );
    let copy = leap;
    assert_eq!(copy, leap, "Date is Copy and comparable");
}

fn rfc3339_parse_and_format() {
    let t = DateTime::parse_rfc3339("1985-04-12T23:20:50.52Z").expect("fractional UTC");
    assert_eq!(
        (t.year, t.month, t.day, t.hour, t.minute, t.second),
        (1985, 4, 12, 23, 20, 50)
    );
    assert_eq!(t.nanos, 520_000_000, "the fraction scales to nanoseconds");
    assert_eq!(t.offset_secs, 0);
    assert_eq!(
        t.format_rfc3339(),
        "1985-04-12T23:20:50.52Z",
        "trailing zeros trimmed"
    );

    let t = DateTime::parse_rfc3339("1996-12-19T16:39:57-08:00").expect("negative offset");
    assert_eq!(t.offset_secs, -8 * 3600);
    assert_eq!(t.nanos, 0);
    assert_eq!(t.format_rfc3339(), "1996-12-19T16:39:57-08:00");
    assert_eq!(
        t.unix_seconds(),
        851_042_397,
        "offset is applied to the instant"
    );

    let t = DateTime::parse_rfc3339("1937-01-01T12:00:27.87+00:20").expect("minute offset");
    assert_eq!(t.offset_secs, 1200);
    assert_eq!(t.nanos, 870_000_000);

    let t = DateTime::parse_rfc3339("2026-01-02t03:04:05z").expect("lowercase t and z");
    assert_eq!(
        t.format_rfc3339(),
        "2026-01-02T03:04:05Z",
        "output is canonical upper case"
    );

    let t = DateTime::parse_rfc3339("2026-01-01T00:00:00.123456789123Z").expect("long fraction");
    assert_eq!(
        t.nanos, 123_456_789,
        "digits beyond nanoseconds are dropped"
    );
    assert_eq!(t.format_rfc3339(), "2026-01-01T00:00:00.123456789Z");

    let leap_second =
        DateTime::parse_rfc3339("1990-12-31T23:59:60Z").expect("second 60 as written");
    assert_eq!(leap_second.second, 60, "second 60 is not normalised");
    assert_eq!(leap_second.format_rfc3339(), "1990-12-31T23:59:60Z");

    assert!(DateTime::parse_rfc3339("2026-01-01T00:00:00+23:59").is_some());
    assert!(DateTime::parse_rfc3339("2026-01-01T00:00:00-23:59").is_some());
}

fn rfc3339_rejections() {
    for bad in [
        "",
        "2026-02-29T00:00:00Z",
        "2026-13-01T00:00:00Z",
        "2026-00-10T00:00:00Z",
        "2026-04-31T00:00:00Z",
        "2026-01-01 00:00:00Z",
        "2026-01-01T24:00:00Z",
        "2026-01-01T00:60:00Z",
        "2026-01-01T00:00:61Z",
        "2026-01-01T00:00:00",
        "2026-01-01T00:00:00+0100",
        "2026-01-01T00:00:00.Z",
        "2026-01-01T00:00:00Zx",
        "2026-01-01T00:00:00+24:00",
        "2026-01-01T00:00:00-24:00",
        "2026-01-01T00:00:00+00:60",
        "2026-01-01T00:00:00+23:60",
        "20260101T000000Z",
        "2026-01-0100:00:00Z",
    ] {
        assert!(
            DateTime::parse_rfc3339(bad).is_none(),
            "must refuse {bad:?}"
        );
    }
}

fn instants_and_offsets() {
    assert_eq!(
        DateTime::from_unix(0, 0).unwrap().format_rfc3339(),
        "1970-01-01T00:00:00Z"
    );
    assert_eq!(
        DateTime::from_unix(-1, 0).unwrap().format_rfc3339(),
        "1969-12-31T23:59:59Z"
    );
    assert_eq!(
        DateTime::from_unix(851_042_397, -8 * 3600)
            .unwrap()
            .format_rfc3339(),
        "1996-12-19T16:39:57-08:00"
    );
    assert_eq!(
        DateTime::from_unix(0, 5 * 3600 + 30 * 60)
            .unwrap()
            .format_rfc3339(),
        "1970-01-01T05:30:00+05:30"
    );

    assert!(valid_offset_secs(0));
    assert!(valid_offset_secs(60) && valid_offset_secs(-60));
    assert!(valid_offset_secs(23 * 3600 + 59 * 60) && valid_offset_secs(-(23 * 3600 + 59 * 60)));
    assert!(
        !valid_offset_secs(1),
        "a second-resolution offset is inexpressible"
    );
    assert!(!valid_offset_secs(-90));
    assert!(!valid_offset_secs(24 * 3600) && !valid_offset_secs(-24 * 3600));
    assert!(DateTime::from_unix(0, 1).is_none());
    assert!(DateTime::from_unix(0, -90).is_none());
    assert!(DateTime::from_unix(0, 24 * 3600).is_none());
    assert!(DateTime::from_unix(0, 23 * 3600 + 59 * 60).is_some());

    // An offset RFC 3339 cannot express is written as UTC for the same instant, so the text
    // always parses back to the same Unix second.
    let mut t = DateTime::from_unix(1_000_000, 0).unwrap();
    t.offset_secs = 1;
    let text = t.format_rfc3339();
    assert!(text.ends_with('Z'), "normalised to UTC: {text}");
    assert_eq!(
        DateTime::parse_rfc3339(&text).unwrap().unix_seconds(),
        t.unix_seconds(),
        "same instant after normalisation"
    );

    // Parse/format round trip across an instant and offset grid (years 1 to 9999).
    let offsets = [
        0,
        60,
        -60,
        1200,
        -8 * 3600,
        5 * 3600 + 30 * 60,
        -(9 * 3600 + 30 * 60),
        23 * 3600 + 59 * 60,
        -(23 * 3600 + 59 * 60),
    ];
    let mut u = -62_135_596_800i64 + 86_400;
    while u < 253_402_300_799 - 86_400 {
        for &o in &offsets {
            let t = DateTime::from_unix(u, o).unwrap();
            let f = t.format_rfc3339();
            let p = DateTime::parse_rfc3339(&f).unwrap();
            assert_eq!(p, t, "round trip of {f}");
            assert_eq!(p.unix_seconds(), u, "instant of {f}");
        }
        u += 1_234_567_891;
    }
}

fn http_dates() {
    assert_eq!(
        format_http_date(784_111_777),
        "Sun, 06 Nov 1994 08:49:37 GMT"
    );
    assert_eq!(format_http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
    assert_eq!(format_http_date(-1), "Wed, 31 Dec 1969 23:59:59 GMT");
    assert_eq!(
        parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
        Some(784_111_777)
    );
    assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
    for secs in [0i64, 1, 86_399, 784_111_777, 1_900_000_000, 4_102_444_799] {
        assert_eq!(
            parse_http_date(&format_http_date(secs)),
            Some(secs),
            "IMF-fixdate round trip at {secs}"
        );
    }
    for bad in [
        "Mon, 06 Nov 1994 08:49:37 GMT",
        "Sunday, 06-Nov-94 08:49:37 GMT",
        "Sun Nov  6 08:49:37 1994",
        "Sun, 06 Nov 1994 08:49:37 UTC",
        "Sun, 06 Nov 1994 25:49:37 GMT",
        "Sun, 31 Nov 1994 08:49:37 GMT",
        "Sun, 06 Foo 1994 08:49:37 GMT",
        "",
    ] {
        assert_eq!(parse_http_date(bad), None, "must refuse {bad:?}");
    }
}

fn main() {
    civil_days();
    calendar_rules();
    dates();
    rfc3339_parse_and_format();
    rfc3339_rejections();
    instants_and_offsets();
    http_dates();
    println!("OK rightkit-time (registry consumer)");
}
