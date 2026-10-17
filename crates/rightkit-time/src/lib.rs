//! Civil date math, RFC 3339 and HTTP-date formatting. std only.

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant `days_from_civil`).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`] (Hinnant `civil_from_days`).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Days in month `m` (1-12) of year `y`; 0 for an invalid month.
pub fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(y) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Day of week, 0 = Sunday.
pub fn weekday(y: i64, m: u32, d: u32) -> u32 {
    (days_from_civil(y, m, d) + 4).rem_euclid(7) as u32
}

/// A civil date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub year: i64,
    pub month: u32,
    pub day: u32,
}

impl Date {
    /// `None` when the month or day is out of range.
    pub fn new(year: i64, month: u32, day: u32) -> Option<Date> {
        if day == 0 || day > days_in_month(year, month) {
            return None;
        }
        Some(Date { year, month, day })
    }

    pub fn days_since_epoch(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }

    pub fn from_days(z: i64) -> Date {
        let (year, month, day) = civil_from_days(z);
        Date { year, month, day }
    }

    pub fn add_days(&self, n: i64) -> Date {
        Date::from_days(self.days_since_epoch() + n)
    }

    pub fn weekday(&self) -> u32 {
        weekday(self.year, self.month, self.day)
    }
}

/// A civil date and time with a fixed UTC offset in seconds (east positive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub nanos: u32,
    pub offset_secs: i32,
}

/// True when `offset_secs` is a whole number of minutes within +-23:59, the only offsets
/// RFC 3339 can express.
pub fn valid_offset_secs(offset_secs: i32) -> bool {
    offset_secs % 60 == 0 && offset_secs.unsigned_abs() <= 23 * 3600 + 59 * 60
}

fn digits(b: &[u8]) -> Option<u32> {
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    b.iter().try_fold(0u32, |a, d| {
        a.checked_mul(10)?.checked_add((d - b'0') as u32)
    })
}

impl DateTime {
    /// Parse RFC 3339: `YYYY-MM-DDTHH:MM:SS[.frac](Z|+HH:MM|-HH:MM)`. `T` and `Z` may be
    /// lowercase. Second 60 is accepted as written. `None` on any deviation.
    pub fn parse_rfc3339(s: &str) -> Option<DateTime> {
        let b = s.as_bytes();
        if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't') {
            return None;
        }
        if b[13] != b':' || b[16] != b':' {
            return None;
        }
        let year = digits(&b[0..4])? as i64;
        let month = digits(&b[5..7])?;
        let day = digits(&b[8..10])?;
        let hour = digits(&b[11..13])?;
        let minute = digits(&b[14..16])?;
        let second = digits(&b[17..19])?;
        if !(1..=12).contains(&month)
            || day == 0
            || day > days_in_month(year, month)
            || hour > 23
            || minute > 59
            || second > 60
        {
            return None;
        }
        let mut i = 19;
        let mut nanos = 0u32;
        if b[i] == b'.' {
            let start = i + 1;
            let mut end = start;
            while end < b.len() && b[end].is_ascii_digit() {
                end += 1;
            }
            if end == start {
                return None;
            }
            let mut scaled = 0u32;
            for k in 0..9 {
                scaled = scaled * 10
                    + b.get(start + k)
                        .filter(|_| start + k < end)
                        .map_or(0, |d| (d - b'0') as u32);
            }
            nanos = scaled;
            i = end;
        }
        let rest = b.get(i..)?;
        let offset_secs = match rest {
            [b'Z'] | [b'z'] => 0,
            [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] => {
                let h = digits(&[*h1, *h2])?;
                let m = digits(&[*m1, *m2])?;
                if h > 23 || m > 59 {
                    return None;
                }
                let v = (h * 3600 + m * 60) as i32;
                if *sign == b'-' {
                    -v
                } else {
                    v
                }
            }
            _ => return None,
        };
        Some(DateTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
            nanos,
            offset_secs,
        })
    }

    /// Format as RFC 3339; fractional seconds appear only when `nanos != 0` (trailing
    /// zeros trimmed). Offset 0 is written `Z`.
    ///
    /// An `offset_secs` RFC 3339 cannot express (see [`valid_offset_secs`]) is normalized to
    /// UTC `Z` with the same instant, so the output always parses back to the same Unix second.
    pub fn format_rfc3339(&self) -> String {
        if !valid_offset_secs(self.offset_secs) {
            let utc =
                DateTime::from_unix(self.unix_seconds(), 0).expect("offset 0 is always valid");
            return DateTime {
                nanos: self.nanos,
                ..utc
            }
            .format_rfc3339();
        }
        let mut s = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        );
        if self.nanos != 0 {
            let frac = format!("{:09}", self.nanos);
            s.push('.');
            s.push_str(frac.trim_end_matches('0'));
        }
        if self.offset_secs == 0 {
            s.push('Z');
        } else {
            let sign = if self.offset_secs < 0 { '-' } else { '+' };
            let a = self.offset_secs.unsigned_abs();
            s.push_str(&format!("{sign}{:02}:{:02}", a / 3600, a % 3600 / 60));
        }
        s
    }

    /// Unix seconds (UTC), ignoring `nanos`.
    pub fn unix_seconds(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day) * 86_400
            + self.hour as i64 * 3600
            + self.minute as i64 * 60
            + self.second as i64
            - self.offset_secs as i64
    }

    /// The civil time of `unix_secs` as seen at `offset_secs` east of UTC. `None` when the
    /// offset is not whole minutes within +-23:59 (see [`valid_offset_secs`]).
    pub fn from_unix(unix_secs: i64, offset_secs: i32) -> Option<DateTime> {
        if !valid_offset_secs(offset_secs) {
            return None;
        }
        let local = unix_secs + offset_secs as i64;
        let (year, month, day) = civil_from_days(local.div_euclid(86_400));
        let sod = local.rem_euclid(86_400) as u32;
        Some(DateTime {
            year,
            month,
            day,
            hour: sod / 3600,
            minute: sod % 3600 / 60,
            second: sod % 60,
            nanos: 0,
            offset_secs,
        })
    }
}

/// Format Unix seconds as an RFC 7231 IMF-fixdate, e.g. `Sun, 06 Nov 1994 08:49:37 GMT`.
pub fn format_http_date(unix_secs: i64) -> String {
    let t = DateTime::from_unix(unix_secs, 0).expect("offset 0 is always valid");
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        DAYS[weekday(t.year, t.month, t.day) as usize],
        t.day,
        MONTHS[t.month as usize - 1],
        t.year,
        t.hour,
        t.minute,
        t.second
    )
}

/// Parse an IMF-fixdate into Unix seconds; `None` for other forms or inconsistent weekdays.
pub fn parse_http_date(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 29 || &b[3..5] != b", " || b[7] != b' ' || b[11] != b' ' || b[16] != b' ' {
        return None;
    }
    if b[19] != b':' || b[22] != b':' || &b[25..] != b" GMT" {
        return None;
    }
    let wd = DAYS.iter().position(|d| d.as_bytes() == &b[0..3])? as u32;
    let day = digits(&b[5..7])?;
    let month = MONTHS.iter().position(|m| m.as_bytes() == &b[8..11])? as u32 + 1;
    let year = digits(&b[12..16])? as i64;
    let (h, mi, sec) = (
        digits(&b[17..19])?,
        digits(&b[20..22])?,
        digits(&b[23..25])?,
    );
    if day == 0 || day > days_in_month(year, month) || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    if weekday(year, month, day) != wd {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + (h * 3600 + mi * 60 + sec) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip_and_known_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        for z in (-800_000..800_000).step_by(997) {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
    }

    #[test]
    fn leap_and_month_lengths() {
        assert!(is_leap_year(2000) && is_leap_year(2024));
        assert!(!is_leap_year(1900) && !is_leap_year(2026));
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2026, 13), 0);
        assert!(Date::new(2026, 2, 29).is_none());
    }

    #[test]
    fn date_add_days_and_weekday() {
        let d = Date::new(2026, 12, 31).unwrap();
        assert_eq!(d.add_days(1), Date::new(2027, 1, 1).unwrap());
        assert_eq!(d.add_days(-365), Date::new(2025, 12, 31).unwrap());
        assert_eq!(weekday(1970, 1, 1), 4);
        assert_eq!(weekday(1994, 11, 6), 0);
    }

    #[test]
    fn rfc3339_examples() {
        let t = DateTime::parse_rfc3339("1985-04-12T23:20:50.52Z").unwrap();
        assert_eq!(
            (t.year, t.month, t.day, t.hour, t.minute, t.second),
            (1985, 4, 12, 23, 20, 50)
        );
        assert_eq!(t.nanos, 520_000_000);
        assert_eq!(t.format_rfc3339(), "1985-04-12T23:20:50.52Z");
        let t = DateTime::parse_rfc3339("1996-12-19T16:39:57-08:00").unwrap();
        assert_eq!(t.offset_secs, -8 * 3600);
        assert_eq!(t.format_rfc3339(), "1996-12-19T16:39:57-08:00");
        assert_eq!(t.unix_seconds(), 851_042_397);
        assert!(DateTime::parse_rfc3339("1990-12-31T23:59:60Z").is_some());
        assert_eq!(
            DateTime::parse_rfc3339("1937-01-01T12:00:27.87+00:20")
                .unwrap()
                .offset_secs,
            1200
        );
    }

    #[test]
    fn rfc3339_rejects_bad_input() {
        for bad in [
            "",
            "2026-02-29T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-01-01 00:00:00Z",
            "2026-01-01T24:00:00Z",
            "2026-01-01T00:00:00",
            "2026-01-01T00:00:00+0100",
            "2026-01-01T00:00:00.Z",
            "2026-01-01T00:00:00Zx",
        ] {
            assert!(DateTime::parse_rfc3339(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn from_unix_matches_parse() {
        let t = DateTime::from_unix(851_042_397, -8 * 3600).unwrap();
        assert_eq!(t.format_rfc3339(), "1996-12-19T16:39:57-08:00");
        assert_eq!(
            DateTime::from_unix(0, 0).unwrap().format_rfc3339(),
            "1970-01-01T00:00:00Z"
        );
        assert_eq!(
            DateTime::from_unix(-1, 0).unwrap().format_rfc3339(),
            "1969-12-31T23:59:59Z"
        );
    }

    #[test]
    fn http_date_rfc7231() {
        assert_eq!(
            format_http_date(784_111_777),
            "Sun, 06 Nov 1994 08:49:37 GMT"
        );
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777)
        );
        assert_eq!(parse_http_date("Mon, 06 Nov 1994 08:49:37 GMT"), None);
        assert_eq!(parse_http_date("Sunday, 06-Nov-94 08:49:37 GMT"), None);
        assert_eq!(format_http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
    }

    #[test]
    fn rejects_second_resolution_and_out_of_range_offsets() {
        assert!(DateTime::from_unix(0, 1).is_none());
        assert!(DateTime::from_unix(0, -90).is_none());
        assert!(DateTime::from_unix(0, 24 * 3600).is_none());
        assert!(DateTime::from_unix(0, -24 * 3600).is_none());
        assert!(DateTime::from_unix(0, 23 * 3600 + 59 * 60).is_some());
        assert!(DateTime::from_unix(0, -(23 * 3600 + 59 * 60)).is_some());
        for bad in ["+24:00", "-24:00", "+00:60", "+23:60"] {
            let s = format!("2026-01-01T00:00:00{bad}");
            assert!(DateTime::parse_rfc3339(&s).is_none(), "{s}");
        }
        assert!(DateTime::parse_rfc3339("2026-01-01T00:00:00+23:59").is_some());
    }

    #[test]
    fn invalid_offset_formats_as_same_instant() {
        let mut t = DateTime::from_unix(1_000_000, 0).unwrap();
        t.offset_secs = 1;
        let f = t.format_rfc3339();
        assert_eq!(
            DateTime::parse_rfc3339(&f).unwrap().unix_seconds(),
            t.unix_seconds()
        );
    }

    #[test]
    fn rfc3339_round_trips_over_instant_offset_grid() {
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
                assert_eq!(p, t, "{f}");
                assert_eq!(p.unix_seconds(), u, "{f}");
            }
            u += 1_234_567_891;
        }
    }
}
