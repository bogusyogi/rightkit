pub fn normalize_time_zone(time_zone: &str) -> Result<String, String> {
    let trimmed = time_zone.trim();
    if trimmed.eq_ignore_ascii_case("UTC") || trimmed.eq_ignore_ascii_case("Z") {
        return Ok("UTC".to_string());
    }
    if let Some(name) = canonical_iana_time_zone(trimmed) {
        return Ok(name.to_string());
    }
    let rest = trimmed
        .strip_prefix("UTC")
        .or_else(|| trimmed.strip_prefix("utc"))
        .ok_or_else(|| {
            "time zone must be UTC, UTC±HH[:MM], or a supported IANA name".to_string()
        })?;
    let (sign, rest) = match rest.as_bytes().first().copied() {
        Some(b'+') => (1_i32, &rest[1..]),
        Some(b'-') => (-1_i32, &rest[1..]),
        _ => {
            return Err("time zone must be UTC, UTC±HH[:MM], or a supported IANA name".to_string())
        }
    };
    let (hour_part, minute_part) = rest.split_once(':').unwrap_or((rest, "0"));
    let hours = hour_part
        .parse::<u8>()
        .map_err(|_| "time zone hour offset must be a number".to_string())?;
    let minutes = minute_part
        .parse::<u8>()
        .map_err(|_| "time zone minute offset must be a number".to_string())?;
    if minutes > 59 {
        return Err("time zone minute offset must be between 0 and 59".to_string());
    }
    let total = sign * (hours as i32 * 60 + minutes as i32);
    if !(-14 * 60..=14 * 60).contains(&total) {
        return Err("time zone offset must be between UTC-14:00 and UTC+14:00".to_string());
    }
    let sign_char = if total < 0 { '-' } else { '+' };
    let abs = total.abs();
    Ok(format!("UTC{sign_char}{:02}:{:02}", abs / 60, abs % 60))
}

pub fn time_zone_offset_minutes_at(time_zone: &str, epoch_minute: i64) -> Result<i32, String> {
    let normalized = normalize_time_zone(time_zone)?;
    if normalized == "UTC" {
        return Ok(0);
    }
    if normalized.starts_with("UTC+") || normalized.starts_with("UTC-") {
        let sign = if normalized.as_bytes()[3] == b'-' {
            -1
        } else {
            1
        };
        let (hours, minutes) = parse_offset_parts(&normalized[4..])?;
        let total = sign * (hours as i32 * 60 + minutes as i32);
        validate_offset_minutes(total)?;
        return Ok(total);
    }
    iana_time_zone_offset_minutes_at(&normalized, epoch_minute)
}

fn parse_offset_parts(rest: &str) -> Result<(u8, u8), String> {
    if rest.is_empty() {
        return Err("time zone offset is missing".to_string());
    }
    let (hour_part, minute_part) = rest.split_once(':').unwrap_or((rest, "0"));
    let hours = hour_part
        .parse::<u8>()
        .map_err(|_| "time zone hour offset must be a number".to_string())?;
    let minutes = minute_part
        .parse::<u8>()
        .map_err(|_| "time zone minute offset must be a number".to_string())?;
    if minutes > 59 {
        return Err("time zone minute offset must be between 0 and 59".to_string());
    }
    Ok((hours, minutes))
}

fn validate_offset_minutes(total: i32) -> Result<(), String> {
    if (-14 * 60..=14 * 60).contains(&total) {
        Ok(())
    } else {
        Err("time zone offset must be between UTC-14:00 and UTC+14:00".to_string())
    }
}

fn canonical_iana_time_zone(input: &str) -> Option<&'static str> {
    match input.to_ascii_lowercase().as_str() {
        "america/new_york" | "us/eastern" => Some("America/New_York"),
        "america/chicago" | "us/central" => Some("America/Chicago"),
        "america/denver" | "us/mountain" => Some("America/Denver"),
        "america/los_angeles" | "us/pacific" => Some("America/Los_Angeles"),
        "europe/london" => Some("Europe/London"),
        "europe/berlin" => Some("Europe/Berlin"),
        "europe/paris" => Some("Europe/Paris"),
        "asia/kolkata" | "asia/calcutta" => Some("Asia/Kolkata"),
        "asia/tokyo" => Some("Asia/Tokyo"),
        "asia/singapore" => Some("Asia/Singapore"),
        "asia/dubai" => Some("Asia/Dubai"),
        "australia/sydney" => Some("Australia/Sydney"),
        _ => None,
    }
}

fn iana_time_zone_offset_minutes_at(zone: &str, epoch_minute: i64) -> Result<i32, String> {
    let year = utc_year(epoch_minute);
    match zone {
        "America/New_York" => Ok(us_offset(epoch_minute, year, -5 * 60)),
        "America/Chicago" => Ok(us_offset(epoch_minute, year, -6 * 60)),
        "America/Denver" => Ok(us_offset(epoch_minute, year, -7 * 60)),
        "America/Los_Angeles" => Ok(us_offset(epoch_minute, year, -8 * 60)),
        "Europe/London" => Ok(europe_offset(epoch_minute, year, 0)),
        "Europe/Berlin" | "Europe/Paris" => Ok(europe_offset(epoch_minute, year, 60)),
        "Asia/Kolkata" => Ok(5 * 60 + 30),
        "Asia/Tokyo" => Ok(9 * 60),
        "Asia/Singapore" => Ok(8 * 60),
        "Asia/Dubai" => Ok(4 * 60),
        "Australia/Sydney" => Ok(sydney_offset(epoch_minute, year)),
        _ => Err(format!("unsupported IANA time zone: {zone}")),
    }
}

fn us_offset(epoch_minute: i64, year: i32, standard_offset: i32) -> i32 {
    let daylight_offset = standard_offset + 60;
    let start_day = nth_weekday_of_month(year, 3, 0, 2);
    let end_day = nth_weekday_of_month(year, 11, 0, 1);
    let start_utc = epoch_minute_from_civil(year, 3, start_day, 2, 0) - standard_offset as i64;
    let end_utc = epoch_minute_from_civil(year, 11, end_day, 2, 0) - daylight_offset as i64;
    if epoch_minute >= start_utc && epoch_minute < end_utc {
        daylight_offset
    } else {
        standard_offset
    }
}

fn europe_offset(epoch_minute: i64, year: i32, standard_offset: i32) -> i32 {
    let start_day = last_weekday_of_month(year, 3, 0);
    let end_day = last_weekday_of_month(year, 10, 0);
    let start_utc = epoch_minute_from_civil(year, 3, start_day, 1, 0);
    let end_utc = epoch_minute_from_civil(year, 10, end_day, 1, 0);
    if epoch_minute >= start_utc && epoch_minute < end_utc {
        standard_offset + 60
    } else {
        standard_offset
    }
}

fn sydney_offset(epoch_minute: i64, year: i32) -> i32 {
    let start_day = nth_weekday_of_month(year, 10, 0, 1);
    let end_day = nth_weekday_of_month(year, 4, 0, 1);
    let start_utc = epoch_minute_from_civil(year, 10, start_day, 2, 0) - 10 * 60;
    let end_utc = epoch_minute_from_civil(year, 4, end_day, 3, 0) - 11 * 60;
    if epoch_minute >= start_utc || epoch_minute < end_utc {
        11 * 60
    } else {
        10 * 60
    }
}

fn utc_year(epoch_minute: i64) -> i32 {
    let days = epoch_minute.div_euclid(1440);
    let (year, _, _) = rightkit_time::civil_from_days(days);
    year as i32
}

fn nth_weekday_of_month(year: i32, month: u32, weekday: u8, nth: u8) -> u32 {
    let first_weekday = rightkit_time::weekday(i64::from(year), month, 1) as u8;
    let delta = (weekday as i32 - first_weekday as i32).rem_euclid(7) as u32;
    1 + delta + 7 * (nth as u32 - 1)
}

fn last_weekday_of_month(year: i32, month: u32, weekday: u8) -> u32 {
    let last = rightkit_time::days_in_month(i64::from(year), month);
    let last_weekday = rightkit_time::weekday(i64::from(year), month, last) as u8;
    let delta = (last_weekday as i32 - weekday as i32).rem_euclid(7) as u32;
    last - delta
}

fn epoch_minute_from_civil(year: i32, month: u32, day: u32, hour: u8, minute: u8) -> i64 {
    rightkit_time::days_from_civil(i64::from(year), month, day) * 1440
        + hour as i64 * 60
        + minute as i64
}
