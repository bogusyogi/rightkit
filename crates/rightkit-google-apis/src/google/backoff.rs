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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_rate_limit_and_every_server_error_only() {
        let p = gmail_retry_policy();
        assert!(p.is_retryable_status(429));
        assert!(p.is_retryable_status(500));
        assert!(p.is_retryable_status(503));
        assert!(p.is_retryable_status(599));
        assert!(!p.is_retryable_status(401));
        assert!(!p.is_retryable_status(403));
        assert!(!p.is_retryable_status(404));
    }

    #[test]
    fn retry_after_is_honored_and_capped_at_30s() {
        let p = gmail_retry_policy();
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(5))),
            Duration::from_secs(5)
        );
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(120))),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn backoff_saturates_at_8s_with_jitter_below_it() {
        let p = gmail_retry_policy();
        let far = p.delay(20, None);
        assert!(far <= Duration::from_secs(8), "{far:?}");
        assert!(far >= Duration::from_millis(6400), "{far:?}");
    }
}
