struct DateTime {
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    weekday: &'static str,
}

impl DateTime {
    fn format_iso(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    fn format_human(&self) -> String {
        format!(
            "{}, {:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
            self.weekday, self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Seconds since Unix epoch → UTC DateTime (civil math is `rightkit-time`'s).
fn from_unix(secs: i64) -> DateTime {
    let civil = rightkit_time::DateTime::from_unix(secs, 0).expect("offset 0 is always valid");
    // `rightkit_time::weekday` counts from Sunday = 0.
    let weekday_names = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    let weekday =
        weekday_names[rightkit_time::weekday(civil.year, civil.month, civil.day) as usize];
    DateTime {
        year: civil.year as i32,
        month: civil.month as u8,
        day: civil.day as u8,
        hour: civil.hour as u8,
        minute: civil.minute as u8,
        second: civil.second as u8,
        weekday,
    }
}

/// Apply a fixed UTC offset (seconds) — used for well-known timezone abbreviations.
fn apply_offset(unix: i64, offset_secs: i64, tz_label: &str) -> String {
    let local = from_unix(unix + offset_secs);
    let sign = if offset_secs >= 0 { "+" } else { "-" };
    let abs = offset_secs.abs();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02} ({})",
        local.year,
        local.month,
        local.day,
        local.hour,
        local.minute,
        local.second,
        abs / 3600,
        (abs % 3600) / 60,
        tz_label
    )
}

/// Daylight-saving rule family a zone follows. The workspace carries no time-zone database,
/// so the current rule of each family is kept here as data.
#[derive(Clone, Copy, PartialEq)]
enum DstRule {
    None,
    Us,
    Eu,
    Au,
    Nz,
    Egypt,
}

struct ZoneDef {
    std_secs: i64,
    std_label: &'static str,
    dst_label: &'static str,
    rule: DstRule,
}

const fn zone(
    std_secs: i64,
    std_label: &'static str,
    dst_label: &'static str,
    rule: DstRule,
) -> ZoneDef {
    ZoneDef {
        std_secs,
        std_label,
        dst_label,
        rule,
    }
}

const fn fixed(std_secs: i64, label: &'static str) -> ZoneDef {
    ZoneDef {
        std_secs,
        std_label: label,
        dst_label: label,
        rule: DstRule::None,
    }
}

fn zone_def(tz: &str) -> Option<ZoneDef> {
    use DstRule::{Au, Egypt, Eu, Nz, Us};
    Some(match tz {
        "UTC" | "Etc/UTC" | "Z" => fixed(0, "UTC"),
        "GMT" | "Etc/GMT" => fixed(0, "GMT"),
        // Americas
        "America/New_York" | "US/Eastern" | "America/Toronto" => zone(-5 * 3600, "EST", "EDT", Us),
        "America/Chicago" | "US/Central" => zone(-6 * 3600, "CST", "CDT", Us),
        "America/Denver" | "US/Mountain" => zone(-7 * 3600, "MST", "MDT", Us),
        "America/Los_Angeles" | "US/Pacific" | "America/Vancouver" => {
            zone(-8 * 3600, "PST", "PDT", Us)
        }
        "America/Anchorage" => zone(-9 * 3600, "AKST", "AKDT", Us),
        "Pacific/Honolulu" | "US/Hawaii" => fixed(-10 * 3600, "HST"),
        "America/Sao_Paulo" => fixed(-3 * 3600, "BRT"),
        "America/Argentina/Buenos_Aires" => fixed(-3 * 3600, "ART"),
        "America/Mexico_City" => fixed(-6 * 3600, "CST"),
        // Europe
        "Europe/London" | "GB" => zone(0, "GMT", "BST", Eu),
        "Europe/Paris" | "Europe/Berlin" | "Europe/Rome" | "Europe/Madrid" | "Europe/Amsterdam"
        | "Europe/Brussels" => zone(3600, "CET", "CEST", Eu),
        "Europe/Helsinki" | "Europe/Kyiv" | "Europe/Bucharest" | "Europe/Athens" => {
            zone(2 * 3600, "EET", "EEST", Eu)
        }
        "Europe/Moscow" => fixed(3 * 3600, "MSK"),
        "Europe/Istanbul" => fixed(3 * 3600, "TRT"),
        // Middle East / Africa
        "Asia/Dubai" | "Asia/Muscat" => fixed(4 * 3600, "GST"),
        "Asia/Karachi" => fixed(5 * 3600, "PKT"),
        "Asia/Kolkata" | "Asia/Calcutta" => fixed(19800, "IST"),
        "Asia/Dhaka" => fixed(6 * 3600, "BST"),
        "Africa/Cairo" => zone(2 * 3600, "EET", "EEST", Egypt),
        "Africa/Johannesburg" => fixed(2 * 3600, "SAST"),
        "Africa/Lagos" => fixed(3600, "WAT"),
        // Asia / Pacific
        "Asia/Bangkok" | "Asia/Ho_Chi_Minh" => fixed(7 * 3600, "ICT"),
        "Asia/Shanghai" | "Asia/Chongqing" | "Asia/Hong_Kong" | "Asia/Singapore"
        | "Asia/Taipei" | "Asia/Kuala_Lumpur" => fixed(8 * 3600, "CST"),
        "Asia/Tokyo" => fixed(9 * 3600, "JST"),
        "Asia/Seoul" => fixed(9 * 3600, "KST"),
        "Australia/Sydney" | "Australia/Melbourne" => zone(10 * 3600, "AEST", "AEDT", Au),
        "Australia/Brisbane" => fixed(10 * 3600, "AEST"),
        "Australia/Adelaide" => zone(37800, "ACST", "ACDT", Au),
        "Pacific/Auckland" => zone(12 * 3600, "NZST", "NZDT", Nz),
        "Pacific/Fiji" => fixed(12 * 3600, "FJT"),
        _ => return None,
    })
}

/// Day of the month of the `nth` (1-based) or, for `nth == 0`, the last `weekday` (0 = Sunday).
fn weekday_of_month(year: i64, month: u32, nth: u32, weekday: u32) -> u32 {
    if nth == 0 {
        let last = rightkit_time::days_in_month(year, month);
        last - (rightkit_time::weekday(year, month, last) + 7 - weekday) % 7
    } else {
        1 + (weekday + 7 - rightkit_time::weekday(year, month, 1)) % 7 + 7 * (nth - 1)
    }
}

fn utc_at(year: i64, month: u32, day: u32, secs_in_day: i64) -> i64 {
    rightkit_time::days_from_civil(year, month, day) * 86400 + secs_in_day
}

/// Whether daylight time is in force at `unix`, plus a note when the rule is only an approximation.
fn dst_in_force(def: &ZoneDef, unix: i64) -> (bool, Option<String>) {
    let year = rightkit_time::DateTime::from_unix(unix + def.std_secs, 0)
        .map(|d| d.year as i64)
        .unwrap_or(2026);
    let std = def.std_secs;
    let (active, valid_from, always_approx) = match def.rule {
        DstRule::None => return (false, None),
        DstRule::Us => {
            let start = utc_at(year, 3, weekday_of_month(year, 3, 2, 0), 2 * 3600) - std;
            let end = utc_at(year, 11, weekday_of_month(year, 11, 1, 0), 3600) - std;
            (unix >= start && unix < end, 2007, false)
        }
        DstRule::Eu => {
            let start = utc_at(year, 3, weekday_of_month(year, 3, 0, 0), 3600);
            let end = utc_at(year, 10, weekday_of_month(year, 10, 0, 0), 3600);
            (unix >= start && unix < end, 1996, false)
        }
        DstRule::Au => {
            let start = utc_at(year, 10, weekday_of_month(year, 10, 1, 0), 2 * 3600) - std;
            let end = utc_at(year, 4, weekday_of_month(year, 4, 1, 0), 2 * 3600) - std;
            (unix >= start || unix < end, 2008, false)
        }
        DstRule::Nz => {
            let start = utc_at(year, 9, weekday_of_month(year, 9, 0, 0), 2 * 3600) - std;
            let end = utc_at(year, 4, weekday_of_month(year, 4, 1, 0), 2 * 3600) - std;
            (unix >= start || unix < end, 2008, false)
        }
        DstRule::Egypt => {
            // Last Friday of April to last Thursday of October (rule since 2023, Ramadan changes ignored).
            let start = utc_at(year, 4, weekday_of_month(year, 4, 0, 5), 0) - std;
            let end = utc_at(year, 10, weekday_of_month(year, 10, 0, 4), 86400) - std;
            (unix >= start && unix < end, 2023, true)
        }
    };
    let note = if always_approx {
        Some("approximate: the daylight rule for this zone has changed often and is applied as of 2023".to_string())
    } else if year < valid_from {
        Some(format!(
            "approximate: the current daylight rule is applied to {year}"
        ))
    } else {
        None
    };
    (active, note)
}

/// Resolve a timezone name to its UTC offset (seconds) and label at the instant `unix`.
/// Zones with daylight saving follow their current rule; an approximated result says so in the label.
fn tz_offset(tz: &str, unix: i64) -> Result<(i64, String), String> {
    // Normalise: strip quotes/whitespace.
    let tz = tz.trim().trim_matches('"');
    if let Some(def) = zone_def(tz) {
        let (dst, note) = dst_in_force(&def, unix);
        let (offset, label) = if dst {
            (def.std_secs + 3600, def.dst_label)
        } else {
            (def.std_secs, def.std_label)
        };
        return Ok((
            offset,
            match note {
                Some(n) => format!("{label}; {n}"),
                None => label.to_string(),
            },
        ));
    }
    match tz {
        // Offset literals like "UTC+5:30" or "+05:30"
        s if s.starts_with("UTC+") || s.starts_with("UTC-") ||
             s.starts_with('+') || s.starts_with('-') => {
            let rest = s.trim_start_matches("UTC");
            parse_offset_literal(rest)
                .map(|secs| (secs, "custom".to_string()))
                .ok_or_else(|| format!("cannot parse offset '{s}'"))
        }
        other => Err(format!(
            "unknown timezone '{other}'; use an IANA name like 'America/New_York', 'Europe/London', 'Asia/Tokyo', or 'UTC'"
        )),
    }
}

/// Offset of a zone for a wall-clock reading (seconds as if the local time were UTC).
fn tz_offset_for_local(tz: &str, local: i64) -> Result<i64, String> {
    let (guess, _) = tz_offset(tz, local)?;
    tz_offset(tz, local - guess).map(|(offset, _)| offset)
}

fn parse_offset_literal(s: &str) -> Option<i64> {
    // e.g. "+05:30", "-08:00", "+5"
    let (sign, rest) = if let Some(r) = s.strip_prefix('-') {
        (-1i64, r)
    } else {
        (1, s.trim_start_matches('+'))
    };
    let parts: Vec<&str> = rest.splitn(2, ':').collect();
    let hours: i64 = parts.first()?.parse().ok()?;
    let minutes: i64 = parts.get(1).and_then(|m| m.parse().ok()).unwrap_or(0);
    Some(sign * (hours * 3600 + minutes * 60))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ── Operations ─────────────────────────────────────────────────────────────
