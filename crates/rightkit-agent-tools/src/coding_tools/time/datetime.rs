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

/// Resolve a timezone name to a UTC offset in seconds.
/// Covers IANA names for the most common zones; falls back to UTC.
fn tz_offset(tz: &str) -> Result<(i64, &'static str), String> {
    // Normalise: strip quotes/whitespace.
    let tz = tz.trim().trim_matches('"');
    match tz {
        "UTC" | "Etc/UTC" | "Z"                          => Ok((0, "UTC")),
        "GMT" | "Etc/GMT"                                 => Ok((0, "GMT")),
        // Americas
        "America/New_York" | "US/Eastern"                => Ok((-5 * 3600, "EST")),
        "America/Chicago" | "US/Central"                 => Ok((-6 * 3600, "CST")),
        "America/Denver" | "US/Mountain"                 => Ok((-7 * 3600, "MST")),
        "America/Los_Angeles" | "US/Pacific"             => Ok((-8 * 3600, "PST")),
        "America/Anchorage"                               => Ok((-9 * 3600, "AKST")),
        "Pacific/Honolulu" | "US/Hawaii"                 => Ok((-10 * 3600, "HST")),
        "America/Sao_Paulo"                              => Ok((-3 * 3600, "BRT")),
        "America/Argentina/Buenos_Aires"                 => Ok((-3 * 3600, "ART")),
        "America/Toronto"                                 => Ok((-5 * 3600, "EST")),
        "America/Vancouver"                               => Ok((-8 * 3600, "PST")),
        "America/Mexico_City"                             => Ok((-6 * 3600, "CST")),
        // Europe
        "Europe/London" | "GB"                           => Ok((0, "GMT")),
        "Europe/Paris" | "Europe/Berlin" |
        "Europe/Rome" | "Europe/Madrid" |
        "Europe/Amsterdam" | "Europe/Brussels"           => Ok((1 * 3600, "CET")),
        "Europe/Helsinki" | "Europe/Kyiv" |
        "Europe/Bucharest" | "Europe/Athens"             => Ok((2 * 3600, "EET")),
        "Europe/Moscow"                                   => Ok((3 * 3600, "MSK")),
        "Europe/Istanbul"                                 => Ok((3 * 3600, "TRT")),
        // Middle East / Africa
        "Asia/Dubai" | "Asia/Muscat"                     => Ok((4 * 3600, "GST")),
        "Asia/Karachi"                                    => Ok((5 * 3600, "PKT")),
        "Asia/Kolkata" | "Asia/Calcutta"                 => Ok((19800, "IST")), // +5:30
        "Asia/Dhaka"                                     => Ok((6 * 3600, "BST")),
        "Africa/Cairo"                                    => Ok((2 * 3600, "EET")),
        "Africa/Johannesburg"                             => Ok((2 * 3600, "SAST")),
        "Africa/Lagos"                                    => Ok((1 * 3600, "WAT")),
        // Asia / Pacific
        "Asia/Bangkok" | "Asia/Ho_Chi_Minh"              => Ok((7 * 3600, "ICT")),
        "Asia/Shanghai" | "Asia/Chongqing" |
        "Asia/Hong_Kong" | "Asia/Singapore" |
        "Asia/Taipei" | "Asia/Kuala_Lumpur"              => Ok((8 * 3600, "CST")),
        "Asia/Tokyo"                                     => Ok((9 * 3600, "JST")),
        "Asia/Seoul"                                     => Ok((9 * 3600, "KST")),
        "Australia/Sydney" | "Australia/Melbourne"       => Ok((10 * 3600, "AEST")),
        "Australia/Brisbane"                             => Ok((10 * 3600, "AEST")),
        "Australia/Adelaide"                             => Ok((37800, "ACST")), // +10:30
        "Pacific/Auckland"                               => Ok((12 * 3600, "NZST")),
        "Pacific/Fiji"                                   => Ok((12 * 3600, "FJT")),
        // Offset literals like "UTC+5:30" or "+05:30"
        s if s.starts_with("UTC+") || s.starts_with("UTC-") ||
             s.starts_with('+') || s.starts_with('-') => {
            let rest = s.trim_start_matches("UTC");
            parse_offset_literal(rest)
                .map(|secs| (secs, "custom"))
                .ok_or_else(|| format!("cannot parse offset '{s}'"))
        }
        other => Err(format!(
            "unknown timezone '{other}'; use an IANA name like 'America/New_York', 'Europe/London', 'Asia/Tokyo', or 'UTC'"
        )),
    }
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
