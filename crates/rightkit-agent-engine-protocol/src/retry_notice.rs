//! Provider retry notices: a plain-data hook so a provider adapter can tell
//! its caller "I am backing off and retrying" without owning any session
//! state. The engine installs a notifier on the request metadata and turns
//! each notice into a `providerRetrying` protocol event.

use std::fmt;
use std::sync::Arc;

/// One pending provider retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryNotice {
    /// Short provider label (route host).
    pub provider: String,
    /// Why the previous attempt failed: `rate limited`, `HTTP 503`,
    /// `connection error`, `stream interrupted`.
    pub reason: String,
    /// The retry about to run (1-based).
    pub attempt: u32,
    /// Retries the ladder allows.
    pub max_attempts: u32,
    /// Backoff the adapter sleeps before the retry.
    pub delay_ms: u64,
}

type NotifyFn = dyn Fn(RetryNotice) + Send + Sync;

/// Cloneable, optional retry sink. Default is a no-op.
#[derive(Clone, Default)]
pub struct RetryNotifier(Option<Arc<NotifyFn>>);

impl RetryNotifier {
    pub fn new(f: impl Fn(RetryNotice) + Send + Sync + 'static) -> Self {
        Self(Some(Arc::new(f)))
    }

    pub fn notify(&self, notice: RetryNotice) {
        if let Some(f) = &self.0 {
            f(notice);
        }
    }
}

impl fmt::Debug for RetryNotifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() {
            "RetryNotifier(set)"
        } else {
            "RetryNotifier(none)"
        })
    }
}

impl PartialEq for RetryNotifier {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Eq for RetryNotifier {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn notifier_delivers_and_default_is_silent() {
        RetryNotifier::default().notify(RetryNotice {
            provider: "p".into(),
            reason: "r".into(),
            attempt: 1,
            max_attempts: 3,
            delay_ms: 10,
        });
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let n = RetryNotifier::new(move |notice| sink.lock().unwrap().push(notice.attempt));
        n.notify(RetryNotice {
            provider: "p".into(),
            reason: "r".into(),
            attempt: 2,
            max_attempts: 3,
            delay_ms: 10,
        });
        assert_eq!(*seen.lock().unwrap(), vec![2]);
        assert_eq!(n.clone(), n);
    }
}
