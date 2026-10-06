/// Deterministic exponential backoff merged with server hints.
#[derive(Debug, Clone)]
pub struct BackoffPolicy {
    pub base_secs: f64,
    pub max_secs: f64,
    /// Minimum delay after a pass that finished with pending work
    /// (iOS: 30s).
    pub pending_floor_secs: f64,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            base_secs: 15.0,
            max_secs: 900.0,
            pending_floor_secs: 30.0,
        }
    }
}

impl BackoffPolicy {
    /// Delay after the `failures`-th consecutive failure (>= 1): never less
    /// than the server hint, capped at `max_secs` unless the hint is larger.
    pub fn delay_secs(&self, failures: u32, hint: Option<f64>) -> f64 {
        let exp = self.base_secs * 2f64.powi(failures.saturating_sub(1).min(20) as i32);
        let policy = exp.min(self.max_secs);
        match hint {
            Some(h) if h.is_finite() && h > policy => h,
            _ => policy,
        }
    }
}
