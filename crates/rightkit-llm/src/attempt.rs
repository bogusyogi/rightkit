//! Per physical attempt observer and separate retry budgets (Orthic-Labs/rightkit#658).
//!
//! [`observe_attempts`] feeds an [`AttemptNotice`] to the observer for every physical HTTP
//! attempt a provider's client makes, including the terminal one, with a [`SubmissionReceipt`]
//! that says whether the request reached the server. A request whose send outcome is unknown
//! ([`SubmissionReceipt::SubmissionUnknown`]) must not be blindly replayed. [`separate_budget_retry`]
//! gives connection failures and HTTP status retries their own budgets.
//!
//! The observer runs on the calling thread (async: the polling task), so it should not block.
//! The notice carries the sanitized URL that `rightkit-http` reports: no query, userinfo, header
//! value or body.

use std::time::Duration;

use rightkit_http::{AttemptDelivery, AttemptOutcome, Client, ErrorClass, RetryPolicy};

/// What became of one physical attempt's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionReceipt {
    /// The server answered, with a status. The request reached it.
    Submitted,
    /// Nothing reached the server: the failure was in the connect phase, or a policy refused the
    /// destination before anything was sent. Replaying the request is safe.
    Rejected,
    /// The request may have reached the server (a failure after sending, a timeout after
    /// sending, a cancellation mid-flight). Replay could repeat a side effect or a charge.
    SubmissionUnknown,
}

/// One physical attempt that finished, as the observer receives it.
#[derive(Debug, Clone)]
pub struct AttemptNotice<'a> {
    /// Sanitized URL of the hop, as `rightkit-http` reports it.
    pub url: &'a str,
    /// Attempt that finished (1 = the first try).
    pub attempt: u32,
    /// Redirect hop within this attempt (0 = the initial request).
    pub redirect_hop: u32,
    /// Response status when the server answered.
    pub status: Option<u16>,
    pub receipt: SubmissionReceipt,
    /// Typed class of a failure without a response.
    pub error_class: Option<ErrorClass>,
    /// Delay before the next attempt. `Some` exactly when another attempt will run.
    pub retry_in: Option<Duration>,
}

impl AttemptNotice<'_> {
    /// The retry notice for this attempt, `retrying in N s (reason)`, when another attempt
    /// follows; `None` otherwise. `N` rounds up to whole seconds.
    pub fn retry_notice(&self) -> Option<String> {
        let delay = self.retry_in?;
        let seconds = delay.as_secs() + u64::from(delay.subsec_nanos() > 0);
        let reason = match (self.status, self.error_class) {
            (Some(status), _) => format!("HTTP {status}"),
            (None, Some(class)) => format!("{class:?}"),
            (None, None) => "transport failure".to_owned(),
        };
        Some(format!("retrying in {seconds} s ({reason})"))
    }
}

fn receipt_of(delivery: AttemptDelivery) -> SubmissionReceipt {
    match delivery {
        AttemptDelivery::Responded => SubmissionReceipt::Submitted,
        AttemptDelivery::NotSent => SubmissionReceipt::Rejected,
        AttemptDelivery::Uncertain => SubmissionReceipt::SubmissionUnknown,
    }
}

/// Install `observer` as the client's per-attempt outcome observer. It replaces any observer
/// already attached to `client`.
pub fn observe_attempts(
    client: Client,
    observer: impl Fn(&AttemptNotice<'_>) + Send + Sync + 'static,
) -> Client {
    client.on_attempt_outcome(move |outcome: &AttemptOutcome| {
        observer(&AttemptNotice {
            url: &outcome.url,
            attempt: outcome.attempt,
            redirect_hop: outcome.redirect_hop,
            status: outcome.status,
            receipt: receipt_of(outcome.delivery),
            error_class: outcome.error_class,
            retry_in: outcome.next_delay,
        });
    })
}

/// A retry policy with separate budgets (#658): a connection or transport failure may end at most
/// `transport_attempts` attempts, on a 5 s backoff ladder; a retryable status may end at most
/// `status_attempts` attempts, and a server `Retry-After` is honoured. Each budget is at least 1
/// (the first failure of that kind is final). `max_attempts` is their sum, so total physical
/// attempts stay finite however the failure kinds alternate. The retryable statuses are the
/// [`RetryPolicy::default`] set; add a provider's own status (for example `529`) to
/// `retry_statuses` yourself.
pub fn separate_budget_retry(transport_attempts: u32, status_attempts: u32) -> RetryPolicy {
    let transport_attempts = transport_attempts.max(1);
    let status_attempts = status_attempts.max(1);
    RetryPolicy {
        max_attempts: transport_attempts.saturating_add(status_attempts),
        initial_backoff: Duration::from_secs(5),
        max_transport_attempts: Some(transport_attempts),
        max_status_attempts: Some(status_attempts),
        ..RetryPolicy::default()
    }
}
