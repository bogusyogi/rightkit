use crate::timezone::time_zone_offset_minutes_at;

use crate::model::UtcMinute;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalMinuteResolution {
    Gap,
    Unique(i64),
    Overlap { earlier: i64, later: i64 },
}

/// Resolve one nominal local epoch-minute against IANA offsets. Scanning the
/// bounded UTC neighbourhood makes DST gaps/overlaps explicit instead of
/// silently picking whichever offset happens to be current.
pub fn resolve_local_epoch_minute(
    time_zone: &str,
    local_epoch_minute: i64,
) -> Result<LocalMinuteResolution, String> {
    let time_zone = normalize_time_zone(time_zone)?;
    let mut matches = Vec::new();
    for utc in (local_epoch_minute - 1_440)..=(local_epoch_minute + 1_440) {
        if utc + i64::from(time_zone_offset_minutes_at(&time_zone, utc)?) == local_epoch_minute {
            matches.push(utc);
        }
    }
    Ok(match matches.as_slice() {
        [] => LocalMinuteResolution::Gap,
        [only] => LocalMinuteResolution::Unique(*only),
        many => LocalMinuteResolution::Overlap {
            earlier: many[0],
            later: many[many.len().saturating_sub(1)],
        },
    })
}

pub fn current_epoch_minute() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 60) as i64)
        .unwrap_or_default()
}

pub fn current_epoch_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

pub fn next_due_epoch_minute_in_time_zone(
    expr: &str,
    time_zone: &str,
    after_epoch_minute: i64,
    search_limit_minutes: i64,
) -> Result<Option<i64>, String> {
    if search_limit_minutes <= 0 {
        return Ok(None);
    }
    let time_zone = normalize_time_zone(time_zone)?;
    for offset in 1..=search_limit_minutes {
        let candidate = after_epoch_minute + offset;
        if cron_matches_in_time_zone(expr, candidate, &time_zone)? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

pub fn normalize_time_zone(time_zone: &str) -> Result<String, String> {
    crate::timezone::normalize_time_zone(time_zone)
}

pub fn local_epoch_minute(time_zone: &str, utc_epoch_minute: i64) -> Result<i64, String> {
    let time_zone = normalize_time_zone(time_zone)?;
    Ok(utc_epoch_minute + i64::from(time_zone_offset_minutes_at(&time_zone, utc_epoch_minute)?))
}

pub fn cron_matches_in_time_zone(
    expr: &str,
    epoch_minute: i64,
    time_zone: &str,
) -> Result<bool, String> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return Err("cron expression must have exactly 5 fields".to_string());
    }
    let local_epoch_minute =
        epoch_minute + time_zone_offset_minutes_at(time_zone, epoch_minute)? as i64;
    let t = utc_minute(local_epoch_minute);
    Ok(field_matches(parts[0], 0, 59, t.minute)?
        && field_matches(parts[1], 0, 23, t.hour)?
        && field_matches(parts[2], 1, 31, t.day)?
        && field_matches(parts[3], 1, 12, t.month)?
        && field_matches(parts[4], 0, 6, t.weekday)?)
}

pub fn default_time_zone() -> String {
    "UTC".to_string()
}

fn field_matches(field: &str, min: u8, max: u8, actual: u8) -> Result<bool, String> {
    for part in field.split(',') {
        if values_for_field(part, min, max)?.contains(&actual) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn values_for_field(field: &str, min: u8, max: u8) -> Result<Vec<u8>, String> {
    if let Some((base, step)) = field.split_once('/') {
        let step: usize = step
            .parse::<usize>()
            .map_err(|_| format!("invalid step value: {step}"))?;
        if step == 0 {
            return Err("cron step must be greater than zero".to_string());
        }
        return Ok(values_for_field(base, min, max)?
            .into_iter()
            .step_by(step)
            .collect());
    }
    if field == "*" {
        return Ok((min..=max).collect());
    }
    if let Some((start, end)) = field.split_once('-') {
        let start: u8 = start
            .parse()
            .map_err(|_| format!("invalid range start: {start}"))?;
        let end: u8 = end
            .parse()
            .map_err(|_| format!("invalid range end: {end}"))?;
        if start > end || start < min || end > max {
            return Err(format!("range {start}-{end} outside {min}-{max}"));
        }
        return Ok((start..=end).collect());
    }
    let value: u8 = field
        .parse()
        .map_err(|_| format!("invalid value: {field}"))?;
    if value < min || value > max {
        return Err(format!("value {value} outside {min}-{max}"));
    }
    Ok(vec![value])
}

fn utc_minute(epoch_minute: i64) -> UtcMinute {
    let days = epoch_minute.div_euclid(1440);
    let minute_of_day = epoch_minute.rem_euclid(1440);
    let (_, month, day) = rightkit_time::civil_from_days(days);
    UtcMinute {
        minute: (minute_of_day % 60) as u8,
        hour: (minute_of_day / 60) as u8,
        day: day as u8,
        month: month as u8,
        weekday: (days + 4).rem_euclid(7) as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_for_field_star_covers_full_range() {
        assert_eq!(values_for_field("*", 0, 4).unwrap(), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn values_for_field_range_is_inclusive() {
        assert_eq!(values_for_field("2-4", 0, 10).unwrap(), vec![2, 3, 4]);
    }

    #[test]
    fn values_for_field_range_out_of_bounds_errors() {
        assert!(values_for_field("2-20", 0, 10).is_err());
    }

    #[test]
    fn values_for_field_reversed_range_errors() {
        assert!(values_for_field("5-2", 0, 10).is_err());
    }

    #[test]
    fn values_for_field_single_value() {
        assert_eq!(values_for_field("7", 0, 59).unwrap(), vec![7]);
    }

    #[test]
    fn values_for_field_single_value_out_of_bounds_errors() {
        assert!(values_for_field("100", 0, 59).is_err());
    }

    #[test]
    fn values_for_field_step_applies_to_base_range() {
        assert_eq!(values_for_field("0-10/3", 0, 59).unwrap(), vec![0, 3, 6, 9]);
    }

    #[test]
    fn values_for_field_step_zero_errors() {
        assert!(values_for_field("*/0", 0, 59).is_err());
    }

    #[test]
    fn values_for_field_non_numeric_errors() {
        assert!(values_for_field("abc", 0, 59).is_err());
    }

    #[test]
    fn field_matches_comma_separated_list() {
        assert!(field_matches("1,3,5", 0, 59, 3).unwrap());
        assert!(!field_matches("1,3,5", 0, 59, 4).unwrap());
    }

    #[test]
    fn cron_matches_in_time_zone_requires_exactly_five_fields() {
        assert!(cron_matches_in_time_zone("* * * *", 0, "UTC").is_err());
        assert!(cron_matches_in_time_zone("* * * * * *", 0, "UTC").is_err());
    }

    #[test]
    fn cron_matches_in_time_zone_wildcard_matches_every_minute() {
        assert!(cron_matches_in_time_zone("* * * * *", 0, "UTC").unwrap());
        assert!(cron_matches_in_time_zone("* * * * *", 123_456, "UTC").unwrap());
    }

    #[test]
    fn cron_matches_in_time_zone_exact_minute_boundary() {
        // epoch minute 0 = 1970-01-01T00:00 UTC.
        assert!(cron_matches_in_time_zone("0 0 * * *", 0, "UTC").unwrap());
        assert!(!cron_matches_in_time_zone("1 0 * * *", 0, "UTC").unwrap());
    }

    #[test]
    fn next_due_epoch_minute_finds_next_matching_minute() {
        // "* * * * *" always matches, so the very next minute after `after`
        // is the answer.
        let next = next_due_epoch_minute_in_time_zone("* * * * *", "UTC", 100, 10)
            .unwrap()
            .unwrap();
        assert_eq!(next, 101);
    }

    #[test]
    fn next_due_epoch_minute_returns_none_when_search_limit_zero() {
        assert_eq!(
            next_due_epoch_minute_in_time_zone("* * * * *", "UTC", 0, 0).unwrap(),
            None
        );
    }

    #[test]
    fn next_due_epoch_minute_returns_none_when_search_limit_negative() {
        assert_eq!(
            next_due_epoch_minute_in_time_zone("* * * * *", "UTC", 0, -5).unwrap(),
            None
        );
    }

    #[test]
    fn next_due_epoch_minute_returns_none_when_nothing_matches_in_window() {
        // minute field "70" is out of range and every value_for_field call
        // errors, so the whole search errors rather than silently matching.
        assert!(next_due_epoch_minute_in_time_zone("70 * * * *", "UTC", 0, 5).is_err());
    }

    #[test]
    fn resolve_local_epoch_minute_unique_in_utc() {
        // UTC has no DST, so every local minute maps to exactly one UTC
        // minute (itself).
        let resolved = resolve_local_epoch_minute("UTC", 1_000_000).unwrap();
        assert_eq!(resolved, LocalMinuteResolution::Unique(1_000_000));
    }

    #[test]
    fn local_epoch_minute_round_trips_in_utc() {
        let converted = local_epoch_minute("UTC", 500_000).unwrap();
        assert_eq!(converted, 500_000);
    }

    #[test]
    fn normalize_time_zone_rejects_unknown_zone() {
        assert!(normalize_time_zone("Not/A_Real_Zone").is_err());
    }

    #[test]
    fn utc_minute_one_day_after_epoch_is_friday_the_second() {
        let t = utc_minute(1440);
        assert_eq!((t.month, t.day, t.weekday), (1, 2, 5));
    }

    #[test]
    fn utc_minute_finds_the_leap_day() {
        // Day 19_782 since the epoch is Thursday 2024-02-29; 01:02 is minute 62 of the day.
        let t = utc_minute(19_782 * 1440 + 62);
        assert_eq!(
            (t.month, t.day, t.weekday, t.hour, t.minute),
            (2, 29, 4, 1, 2)
        );
    }

    #[test]
    fn utc_minute_decomposes_epoch_zero() {
        let t = utc_minute(0);
        assert_eq!(t.minute, 0);
        assert_eq!(t.hour, 0);
        assert_eq!(t.day, 1);
        assert_eq!(t.month, 1);
        // 1970-01-01 was a Thursday; weekday convention here is 0-6 with
        // epoch day (days+4) rem 7.
        assert_eq!(t.weekday, 4);
    }

    #[test]
    fn utc_minute_decomposes_mid_day_offset() {
        // 90 minutes past epoch = 01:30 on 1970-01-01.
        let t = utc_minute(90);
        assert_eq!(t.hour, 1);
        assert_eq!(t.minute, 30);
        assert_eq!(t.day, 1);
    }
}
