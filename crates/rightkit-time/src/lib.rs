//! Civil date math, RFC 3339 and HTTP-date formatting. std only.
//!
//! Also: [`zones`] (local wall time to UTC for a few tabled zones) and [`stamp`] (system-clock
//! UTC stamps for logs and file names).

pub mod clock;
pub mod stamp;
pub mod zones;

pub use clock::{unix_millis_now, unix_millis_with, Clock, SystemClock};

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
