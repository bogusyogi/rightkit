use rightkit_agent_tools::coding_tools::schedule_cron_tool::time_zone_offset_minutes_at;

use super::model::UtcMinute;

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

pub(super) fn current_epoch_millis() -> u128 {
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
    rightkit_agent_tools::coding_tools::schedule_cron_tool::normalize_time_zone(time_zone)
}

pub fn local_epoch_minute(time_zone: &str, utc_epoch_minute: i64) -> Result<i64, String> {
    let time_zone = normalize_time_zone(time_zone)?;
    Ok(utc_epoch_minute + i64::from(time_zone_offset_minutes_at(&time_zone, utc_epoch_minute)?))
}

pub(super) fn cron_matches_in_time_zone(
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

pub(super) fn default_time_zone() -> String {
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
