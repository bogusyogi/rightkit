//! Scalar operator-value parsers (dates, relative durations, byte sizes).
//! Chrono-free port of MailRight `operator_parse.rs`.

/// Days since 1970-01-01 for a proleptic Gregorian civil date (Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn valid_day(y: i64, m: i64, d: i64) -> bool {
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=dim).contains(&d)
}

/// `YYYY-MM-DD` or `YYYY/MM/DD` -> epoch milliseconds at UTC midnight.
pub fn parse_date_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let sep = if s.contains('-') { '-' } else { '/' };
    let mut it = s.split(sep);
    let (y, m, d) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some()
        || y.len() != 4
        || !(1..=2).contains(&m.len())
        || !(1..=2).contains(&d.len())
    {
        return None;
    }
    let (y, m, d): (i64, i64, i64) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    valid_day(y, m, d).then(|| days_from_civil(y, m, d) * 86_400_000)
}

/// `Nd` / `Nm` (30 days) / `Ny` (365 days) -> the cutoff `now_ms - duration`.
pub fn parse_relative_ms(s: &str, now_ms: i64) -> Option<i64> {
    let s = s.trim();
    if s.len() < 2 || !s.is_char_boundary(s.len() - 1) {
        return None;
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: i64 = num.parse().ok()?;
    let day = 86_400_000i64;
    let span = match unit {
        "d" => n.checked_mul(day)?,
        "m" => n.checked_mul(30 * day)?,
        "y" => n.checked_mul(365 * day)?,
        _ => return None,
    };
    Some(now_ms - span)
}

/// Bytes with optional `k`/`m`/`g` suffix (decimal multiples, case-insensitive).
pub fn parse_size_bytes(s: &str) -> Option<i64> {
    let lower = s.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    let (num, mult) = if let Some(n) = lower.strip_suffix('k') {
        (n, 1_000.0)
    } else if let Some(n) = lower.strip_suffix('m') {
        (n, 1_000_000.0)
    } else if let Some(n) = lower.strip_suffix('g') {
        (n, 1_000_000_000.0)
    } else {
        (lower.as_str(), 1.0)
    };
    let n: f64 = num.trim().parse().ok()?;
    (n >= 0.0 && n.is_finite()).then(|| (n * mult).round() as i64)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
