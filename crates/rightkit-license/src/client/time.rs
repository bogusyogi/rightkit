//! Time helpers of the native client: RFC 3339 to Unix seconds, and seconds to the wire's
//! milliseconds.

/// Unix seconds to Unix milliseconds, saturating.
pub(crate) fn to_ms(seconds: i64) -> i64 {
    seconds.saturating_mul(1000)
}

/// RFC 3339 `date-time` to unix seconds. Accepts `Z` or a numeric offset and optional
/// fractional seconds (ignored). Anything else is `None`.
pub(crate) fn parse_rfc3339(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let digits = |from: usize, len: usize| -> Option<i64> {
        let part = b.get(from..from + len)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(part).ok()?.parse().ok()
    };
    if b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (year, month, day) = (digits(0, 4)?, digits(5, 2)?, digits(8, 2)?);
    let (hour, minute, second) = (digits(11, 2)?, digits(14, 2)?, digits(17, 2)?);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if day < 1 || day > month_days || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut at = 19;
    if b[at] == b'.' {
        at += 1;
        let start = at;
        while at < b.len() && b[at].is_ascii_digit() {
            at += 1;
        }
        if at == start {
            return None;
        }
    }
    let offset = match b.get(at)? {
        b'Z' | b'z' if at + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if at + 6 == b.len() && b[at + 3] == b':' => {
            let (oh, om) = (digits(at + 1, 2)?, digits(at + 4, 2)?);
            if oh > 23 || om > 59 {
                return None;
            }
            let seconds = oh * 3600 + om * 60;
            if *sign == b'+' {
                seconds
            } else {
                -seconds
            }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_known_values() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2024-02-29T12:00:00Z"), Some(1_709_208_000));
        assert_eq!(
            parse_rfc3339("2026-10-16T02:00:00+02:00"),
            parse_rfc3339("2026-10-16T00:00:00Z")
        );
        assert_eq!(
            parse_rfc3339("2026-10-16T00:00:00.123Z"),
            Some(1_792_108_800)
        );
        assert_eq!(parse_rfc3339("2026-02-29T00:00:00Z"), None);
        assert_eq!(parse_rfc3339("garbage"), None);
    }

    #[test]
    fn milliseconds_saturate() {
        assert_eq!(to_ms(2), 2000);
        assert_eq!(to_ms(i64::MAX), i64::MAX);
    }
}
