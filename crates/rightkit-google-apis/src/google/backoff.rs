// Gmail retry policy, expressed as rightkit-http's `RetryPolicy`. The retry loop,
// jitter, and Retry-After parsing belong to rightkit-http; this file only fixes
// The numbers: 4 attempts, 250ms doubling to 8s, 429 plus every 5xx, and a
// server Retry-After honored up to 30s.
use rightkit_http::RetryPolicy;
use std::time::Duration;

pub fn gmail_retry_policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 4,
        initial_backoff: Duration::from_millis(250),
        max_backoff: Duration::from_secs(8),
        multiplier: 2.0,
        jitter: 0.2,
        retry_statuses: std::iter::once(429).chain(500..600).collect(),
        honor_retry_after: true,
        max_retry_after: Duration::from_secs(30),
        ..RetryPolicy::default()
    }
}
