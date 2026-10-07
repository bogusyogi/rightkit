use std::time::Duration;

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
        }
    }
}

impl RetryPolicy {
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
        Duration::from_secs_f64(shaved.max(0.0))
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

/// Parse a Retry-After header (delta-seconds only; HTTP-dates are ignored).
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let v = value.trim();
    v.parse::<f64>()
        .ok()
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map(Duration::from_secs_f64)
}
