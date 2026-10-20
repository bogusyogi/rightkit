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
    /// Opt-in separate budget for transport/timeout failures: the most attempts that may
    /// end in such a failure (`1` = the first transport failure is final). `None` (the
    /// default) keeps the legacy single shared counter, where only `max_attempts` binds.
    /// When either budget is set, failures count only against their own budget, backoff
    /// advances per budget, alternating failures never reset either counter, and
    /// `max_attempts` remains the finite cap on total physical attempts.
    pub max_transport_attempts: Option<u32>,
    /// Opt-in separate budget for retryable-status failures; see
    /// [`RetryPolicy::max_transport_attempts`].
    pub max_status_attempts: Option<u32>,
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
            max_transport_attempts: None,
            max_status_attempts: None,
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

    /// True when either opt-in per-failure-kind budget is set; see
    /// [`RetryPolicy::max_transport_attempts`].
    pub fn separate_budgets(&self) -> bool {
        self.max_transport_attempts.is_some() || self.max_status_attempts.is_some()
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

/// Per-call failure counters behind [`RetryPolicy::max_transport_attempts`]. One instance
/// lives for a whole call across both clients' retry loops. A recorded failure returns the
/// backoff index when another attempt may run: the per-kind counter under separate
/// budgets, the physical attempt number under the legacy shared counter. The total
/// physical cap (`max_attempts`) always binds, so alternating failure kinds stay finite
/// and neither counter ever resets.
pub(crate) struct AttemptBudgets {
    transport: u32,
    status: u32,
}

impl AttemptBudgets {
    pub(crate) fn new() -> Self {
        Self {
            transport: 0,
            status: 0,
        }
    }

    fn may_retry(
        &self,
        policy: &RetryPolicy,
        attempt: u32,
        failures: u32,
        budget: Option<u32>,
    ) -> Option<u32> {
        if attempt >= policy.max_attempts.max(1) {
            return None;
        }
        if budget.is_some_and(|b| failures >= b) {
            return None;
        }
        Some(if policy.separate_budgets() {
            failures
        } else {
            attempt
        })
    }

    /// Record a transport/timeout failure of physical attempt `attempt` (1-based).
    pub(crate) fn transport_failed(&mut self, policy: &RetryPolicy, attempt: u32) -> Option<u32> {
        self.transport = self.transport.saturating_add(1);
        self.may_retry(
            policy,
            attempt,
            self.transport,
            policy.max_transport_attempts,
        )
    }

    /// Record a retryable-status failure of physical attempt `attempt` (1-based).
    pub(crate) fn status_failed(&mut self, policy: &RetryPolicy, attempt: u32) -> Option<u32> {
        self.status = self.status.saturating_add(1);
        self.may_retry(policy, attempt, self.status, policy.max_status_attempts)
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

/// Parse a rate-limit reset value (`X-RateLimit-Reset` and similar) into the wait it names,
/// measured from `now`. Accepted forms: whole Unix epoch seconds (`1760000000`) and an ISO-8601
/// instant with `T` (or a space) and either `Z` or a `±HH:MM` offset, fractional seconds
/// allowed (`2026-10-18T10:00:00.5Z`). Fractions finer than nanoseconds round up, so retries
/// never happen early. A reset already past is `Duration::ZERO`; any other text
/// is `None`. A bare small integer is read as epoch seconds, so a delta-seconds value is not
/// accepted here; use [`parse_retry_after_at`] for those.
pub fn parse_rate_limit_reset_at(value: &str, now: SystemTime) -> Option<Duration> {
    let v = value.trim();
    let at = if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
        Duration::from_secs(v.parse().ok()?)
    } else {
        parse_iso8601_instant(v)?
    };
    // Compare as exact durations since the epoch: a `SystemTime` on Windows has 100 ns
    // resolution, so building the reset instant as one would truncate a sub-100 ns fraction
    // and report a reset that is still ahead as already reached.
    let now = now.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    Some(at.saturating_sub(now))
}

/// [`parse_rate_limit_reset_at`] against the current clock.
pub fn parse_rate_limit_reset(value: &str) -> Option<Duration> {
    parse_rate_limit_reset_at(value, SystemTime::now())
}

/// `YYYY-MM-DD[T ]HH:MM:SS[.fraction](Z|±HH:MM)` as the exact time since the Unix epoch,
/// retaining nanoseconds and rounding finer fractions up to the next nanosecond.
fn parse_iso8601_instant(v: &str) -> Option<Duration> {
    let b = v.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let num = |s: Option<&str>| -> Option<i64> {
        let s = s?;
        if !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()) {
            s.parse().ok()
        } else {
            None
        }
    };
    if b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let year = num(v.get(0..4))?;
    let month = num(v.get(5..7))?;
    let day = num(v.get(8..10))?;
    let hour = num(v.get(11..13))?;
    let minute = num(v.get(14..16))?;
    let second = num(v.get(17..19))?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut i = 19;
    let mut nanos = 0u32;
    let mut fraction_digits = 0usize;
    let mut finer_nonzero = false;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while let Some(&digit) = b.get(i).filter(|c| c.is_ascii_digit()) {
            if fraction_digits < 9 {
                nanos = nanos * 10 + u32::from(digit - b'0');
            } else if digit != b'0' {
                finer_nonzero = true;
            }
            fraction_digits += 1;
            i += 1;
        }
        if i == start {
            return None;
        }
        for _ in fraction_digits..9 {
            nanos *= 10;
        }
        if finer_nonzero {
            if nanos == 999_999_999 {
                nanos = 0;
                // Carry the sub-nanosecond rounding into whole seconds below.
                fraction_digits = usize::MAX;
            } else {
                nanos += 1;
            }
        }
    }
    let offset: i64 = match *b.get(i)? {
        b'Z' | b'z' => {
            i += 1;
            0
        }
        sign @ (b'+' | b'-') => {
            if b.get(i + 3) != Some(&b':') {
                return None;
            }
            let oh = num(v.get(i + 1..i + 3))?;
            let om = num(v.get(i + 4..i + 6))?;
            i += 6;
            let secs = oh * 3600 + om * 60;
            if sign == b'-' {
                -secs
            } else {
                secs
            }
        }
        _ => return None,
    };
    if i != b.len() {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let mut secs = days * 86_400 + hour * 3_600 + minute * 60 + second - offset;
    if secs < 0 {
        return None;
    }
    if fraction_digits == usize::MAX {
        secs = secs.checked_add(1)?;
    }
    Some(Duration::new(secs as u64, nanos))
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
                if yy < 70 {
                    2000 + yy
                } else {
                    1900 + yy
                }
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
        assert_eq!(
            parse_retry_after_at("Sun, 32 Nov 1994 08:49:37 GMT", now),
            None
        );
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

    #[test]
    fn separate_budgets_do_not_steal_from_each_other_and_stay_finite() {
        let policy = RetryPolicy {
            max_attempts: 100,
            max_transport_attempts: Some(2),
            max_status_attempts: Some(3),
            ..RetryPolicy::default()
        };
        let mut b = AttemptBudgets::new();
        // Two transport failures exhaust only the transport budget.
        assert_eq!(b.transport_failed(&policy, 1), Some(1));
        assert_eq!(b.transport_failed(&policy, 2), None);
        // The status budget is untouched (and its backoff starts at 1, not 3).
        assert_eq!(b.status_failed(&policy, 3), Some(1));
        assert_eq!(b.status_failed(&policy, 4), Some(2));
        assert_eq!(b.status_failed(&policy, 5), None);
        // Alternation never resets a counter.
        assert_eq!(b.transport_failed(&policy, 6), None);
        // The total physical cap binds even with unbounded-looking kind budgets.
        let capped = RetryPolicy {
            max_attempts: 4,
            max_transport_attempts: Some(10),
            max_status_attempts: Some(10),
            ..RetryPolicy::default()
        };
        let mut b = AttemptBudgets::new();
        assert!(b.transport_failed(&capped, 1).is_some());
        assert!(b.status_failed(&capped, 2).is_some());
        assert!(b.transport_failed(&capped, 3).is_some());
        assert_eq!(b.status_failed(&capped, 4), None);
        // Legacy policy: one shared counter, backoff index is the attempt number.
        let legacy = RetryPolicy {
            max_attempts: 3,
            ..RetryPolicy::default()
        };
        assert!(!legacy.separate_budgets());
        let mut b = AttemptBudgets::new();
        assert_eq!(b.transport_failed(&legacy, 1), Some(1));
        assert_eq!(b.status_failed(&legacy, 2), Some(2));
        assert_eq!(b.transport_failed(&legacy, 3), None);
    }

    #[test]
    fn rate_limit_reset_epoch_and_iso_forms() {
        // 2026-10-18T10:00:00Z is 1792317600 seconds after the epoch.
        let now = UNIX_EPOCH + Duration::from_secs(1_792_317_600 - 90);
        let want = Some(Duration::from_secs(90));
        assert_eq!(parse_rate_limit_reset_at("1792317600", now), want);
        assert_eq!(parse_rate_limit_reset_at("2026-10-18T10:00:00Z", now), want);
        assert_eq!(
            parse_rate_limit_reset_at("2026-10-18T12:00:00+02:00", now),
            want
        );
        assert_eq!(
            parse_rate_limit_reset_at("2026-10-18 10:00:00.250Z", now),
            Some(Duration::from_millis(90_250))
        );
        let at_whole_second = UNIX_EPOCH + Duration::from_secs(1_792_317_600);
        assert_eq!(
            parse_rate_limit_reset_at("2026-10-18T10:00:00.5Z", at_whole_second),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            parse_rate_limit_reset_at("2026-10-18T10:00:00.0000000001Z", at_whole_second),
            Some(Duration::from_nanos(1))
        );
        // `now` itself is a SystemTime, so its precision is the host's: 1 ns on Unix, 100 ns on
        // Windows. A reset at or before `now` is zero at that precision.
        let tick = if cfg!(windows) { 100 } else { 1 };
        assert_eq!(
            parse_rate_limit_reset_at(
                "2026-10-18T10:00:00.0000000001Z",
                at_whole_second + Duration::from_nanos(tick)
            ),
            Some(Duration::ZERO)
        );
        assert_eq!(
            parse_rate_limit_reset_at("2026-10-18T10:00:00-02:00", now),
            Some(Duration::from_secs(90 + 7200))
        );
        assert_eq!(parse_rate_limit_reset_at("1000", now), Some(Duration::ZERO));
        assert_eq!(parse_rate_limit_reset_at("2026-13-18T10:00:00Z", now), None);
        assert_eq!(parse_rate_limit_reset_at("2026-10-18T10:00:00", now), None);
        assert_eq!(parse_rate_limit_reset_at("soon", now), None);
        assert_eq!(parse_rate_limit_reset_at("", now), None);
    }
}
