//! Feed dates to Unix seconds: RFC 3339 (Atom, Dublin Core) through `rightkit-time`, and the
//! RFC 822 / 2822 form RSS uses (`Mon, 02 Jan 2006 15:04:05 +0000`).

use rightkit_time::{days_from_civil, days_in_month, valid_offset_secs, DateTime};

/// Unix seconds for a feed date in either form, or `None` when it reads as neither.
pub(crate) fn parse_unix(text: &str) -> Option<i64> {
    let text = text.trim();
    if let Some(dt) = DateTime::parse_rfc3339(text) {
        return Some(dt.unix_seconds());
    }
    parse_rfc822(text)
}

fn month_number(name: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let lower = name.get(..3)?.to_ascii_lowercase();
    MONTHS
        .iter()
        .position(|m| *m == lower)
        .map(|i| i as u32 + 1)
}

fn zone_offset_secs(zone: &str) -> Option<i32> {
    let upper = zone.to_ascii_uppercase();
    let named = match upper.as_str() {
        "GMT" | "UT" | "UTC" | "Z" => Some(0),
        "EST" => Some(-5),
        "EDT" => Some(-4),
        "CST" => Some(-6),
        "CDT" => Some(-5),
        "MST" => Some(-7),
        "MDT" => Some(-6),
        "PST" => Some(-8),
        "PDT" => Some(-7),
        _ => None,
    };
    if let Some(hours) = named {
        return Some(hours * 3600);
    }
    let sign = match upper.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = upper[1..].chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i32 = digits[..2].parse().ok()?;
    let minutes: i32 = digits[2..].parse().ok()?;
    if minutes > 59 {
        return None;
    }
    let secs = sign * (hours * 3600 + minutes * 60);
    valid_offset_secs(secs).then_some(secs)
}

fn parse_rfc822(text: &str) -> Option<i64> {
    // Drop an optional "Mon," weekday.
    let body = match text.split_once(',') {
        Some((day, rest)) if day.trim().len() <= 9 => rest.trim(),
        _ => text,
    };
    let mut parts = body.split_whitespace();
    let day: u32 = parts.next()?.parse().ok()?;
    let month = month_number(parts.next()?)?;
    let year_text = parts.next()?;
    let mut year: i64 = year_text.parse().ok()?;
    if year_text.len() == 2 {
        year += if year < 50 { 2000 } else { 1900 };
    } else if year_text.len() == 3 {
        year += 1900;
    }
    let clock = parts.next()?;
    let zone = parts.next().unwrap_or("GMT");
    let mut fields = clock.split(':');
    let hour: i64 = fields.next()?.parse().ok()?;
    let minute: i64 = fields.next()?.parse().ok()?;
    let second: i64 = match fields.next() {
        Some(s) => s.parse().ok()?,
        None => 0,
    };
    if fields.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    if day == 0 || day > days_in_month(year, month) {
        return None;
    }
    let offset = zone_offset_secs(zone)? as i64;
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}
