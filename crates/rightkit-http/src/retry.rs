use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Retry/backoff policy. `max_attempts` counts total tries (1 = no retry).
#[derive(Debug, Clone, PartialEq)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub multiplier: f64,
    /// Fraction (0.0..=1.0) of each delay randomly shaved off; 0 = deterministic.
    pub jitter: f64,
    pub retry_statuses: Vec<u16>,
    pub honor_retry_after: bool,
    /// Upper bound applied to a server-supplied Retry-After.
    pub max_retry_after: Duration,
    /// When a server-supplied Retry-After is longer than `max_retry_after`, stop retrying
    /// instead of sleeping the capped time and asking again too early. The call then returns
    /// the response (or `HttpError::Status` with the full, uncapped `retry_after`) so the
    /// caller can schedule the next try durably, for example with `QuotaSchedule`.
    /// Default `false` (the old behaviour: sleep the capped delay and retry).
    pub defer_over_cap: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(8),
            multiplier: 2.0,
            jitter: 0.2,
            retry_statuses: vec![408, 425, 429, 500, 502, 503, 504],
            honor_retry_after: true,
            max_retry_after: Duration::from_secs(30),
            defer_over_cap: false,
        }
    }
}

impl RetryPolicy {
    /// True when `retry_after` is honoured, `defer_over_cap` is set and the server asked
    /// for longer than `max_retry_after`: the retry loop should return, not sleep.
    pub fn defers(&self, retry_after: Option<Duration>) -> bool {
        self.defer_over_cap
            && self.honor_retry_after
            && retry_after.is_some_and(|ra| ra > self.max_retry_after)
    }

    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            ..Self::default()
        }
    }

    pub fn is_retryable_status(&self, status: u16) -> bool {
        self.retry_statuses.contains(&status)
    }

    /// Delay before retry number `retry` (1 = after the first failure).
    pub fn backoff(&self, retry: u32) -> Duration {
        let exp = retry.saturating_sub(1).min(32) as i32;
        let secs = self.initial_backoff.as_secs_f64() * self.multiplier.powi(exp);
        let capped = secs.min(self.max_backoff.as_secs_f64());
        let shaved = if self.jitter > 0.0 {
            capped * (1.0 - self.jitter.clamp(0.0, 1.0) * random_unit())
        } else {
            capped
        };
        Duration::try_from_secs_f64(shaved.max(0.0)).unwrap_or(self.max_backoff)
    }

    /// Delay for a given failure: the larger of backoff and a (capped) Retry-After.
    pub fn delay(&self, retry: u32, retry_after: Option<Duration>) -> Duration {
        let base = self.backoff(retry);
        match (self.honor_retry_after, retry_after) {
            (true, Some(ra)) => base.max(ra.min(self.max_retry_after)),
            _ => base,
        }
    }
}

fn random_unit() -> f64 {
    let mut b = [0u8; 4];
    if getrandom::fill(&mut b).is_err() {
        return 0.0;
    }
    u32::from_le_bytes(b) as f64 / u32::MAX as f64
}

/// Parse a Retry-After header: delta-seconds or an HTTP-date (RFC 9110 section 10.2.3).
/// A date is turned into a delay from the current clock; a date in the past is zero.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    parse_retry_after_at(value, SystemTime::now())
}

/// [`parse_retry_after`] against an explicit `now` (for tests and callers with their own clock).
pub fn parse_retry_after_at(value: &str, now: SystemTime) -> Option<Duration> {
    let v = value.trim();
    if let Ok(s) = v.parse::<f64>() {
        return Some(s)
            .filter(|s| s.is_finite() && *s >= 0.0)
            .map(|s| Duration::try_from_secs_f64(s).unwrap_or(Duration::MAX));
    }
    let at = parse_http_date(v)?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

fn month_number(m: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let m = m.get(..3)?.to_ascii_lowercase();
    MONTHS.iter().position(|x| *x == m).map(|i| i as i64 + 1)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// HTTP-date in the three RFC 9110 forms (IMF-fixdate, obsolete RFC 850, asctime), always GMT.
pub(crate) fn parse_http_date(v: &str) -> Option<SystemTime> {
    let t: Vec<&str> = v.split_whitespace().collect();
    let (day, mon, year, time) = match t.len() {
        6 if t[0].ends_with(',') && t[5].eq_ignore_ascii_case("GMT") => {
            (t[1], t[2], t[3].parse::<i64>().ok()?, t[4])
        }
        4 if t[0].ends_with(',') && t[3].eq_ignore_ascii_case("GMT") => {
            let mut p = t[1].split('-');
            let (d, m, y) = (p.next()?, p.next()?, p.next()?);
            if p.next().is_some() {
                return None;
            }
            let yy = y.parse::<i64>().ok()?;
            let year = if y.len() == 2 {
                if yy < 70 { 2000 + yy } else { 1900 + yy }
            } else {
                yy
            };
            (d, m, year, t[2])
        }
        5 => (t[2], t[1], t[4].parse::<i64>().ok()?, t[3]),
        _ => return None,
    };
    let day = day.parse::<i64>().ok()?;
    let month = month_number(mon)?;
    let mut hms = time.split(':');
    let h = hms.next()?.parse::<i64>().ok()?;
    let mi = hms.next()?.parse::<i64>().ok()?;
    let s = hms.next()?.parse::<i64>().ok()?;
    if hms.next().is_some()
        || !(1..=31).contains(&day)
        || !(0..24).contains(&h)
        || !(0..60).contains(&mi)
        || !(0..=60).contains(&s)
        || !(1970..=9999).contains(&year)
    {
        return None;
    }
    let secs = days_from_civil(year, month, day) * 86_400 + h * 3600 + mi * 60 + s;
    u64::try_from(secs)
        .ok()
        .map(|s| UNIX_EPOCH + Duration::from_secs(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huge_retry_after_saturates_instead_of_panicking() {
        for v in [
            "18446744073709551616",
            "1e300",
            "99999999999999999999999999999999999999",
            "1.7976931348623157e308",
        ] {
            assert_eq!(parse_retry_after(v), Some(Duration::MAX), "{v}");
        }
        assert_eq!(parse_retry_after("1.5"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_retry_after("-1"), None);
        assert_eq!(parse_retry_after("NaN"), None);
        assert_eq!(parse_retry_after("inf"), None);
    }

    #[test]
    fn delay_caps_a_saturated_retry_after() {
        let p = RetryPolicy {
            max_retry_after: Duration::from_secs(30),
            jitter: 0.0,
            ..RetryPolicy::default()
        };
        assert_eq!(p.delay(1, Some(Duration::MAX)), Duration::from_secs(30));
    }

    #[test]
    fn backoff_with_absurd_settings_does_not_panic() {
        let p = RetryPolicy {
            initial_backoff: Duration::MAX,
            max_backoff: Duration::MAX,
            multiplier: f64::MAX,
            jitter: 0.0,
            ..RetryPolicy::default()
        };
        let _ = p.backoff(40);
    }

    #[test]
    fn retry_after_http_date_forms() {
        let at = UNIX_EPOCH + Duration::from_secs(784_111_777); // Sun, 06 Nov 1994 08:49:37 GMT
        let now = at - Duration::from_secs(90);
        for v in [
            "Sun, 06 Nov 1994 08:49:37 GMT",
            "Sunday, 06-Nov-94 08:49:37 GMT",
            "Sun Nov  6 08:49:37 1994",
        ] {
            assert_eq!(
                parse_retry_after_at(v, now),
                Some(Duration::from_secs(90)),
                "{v}"
            );
        }
        // A date in the past is a zero delay; junk is None.
        assert_eq!(
            parse_retry_after_at("Sun, 06 Nov 1994 08:49:37 GMT", at + Duration::from_secs(5)),
            Some(Duration::ZERO)
        );
        assert_eq!(parse_retry_after_at("soon", now), None);
        assert_eq!(parse_retry_after_at("Sun, 32 Nov 1994 08:49:37 GMT", now), None);
    }

    #[test]
    fn retry_after_beyond_the_cap_is_kept_whole_and_defers_on_request() {
        assert_eq!(parse_retry_after("3600"), Some(Duration::from_secs(3600)));
        let mut p = RetryPolicy::default();
        assert!(!p.defers(Some(Duration::from_secs(3600))));
        p.defer_over_cap = true;
        assert!(p.defers(Some(Duration::from_secs(3600))));
        assert!(!p.defers(Some(Duration::from_secs(10))));
        assert!(!p.defers(None));
        p.honor_retry_after = false;
        assert!(!p.defers(Some(Duration::from_secs(3600))));
    }
}
