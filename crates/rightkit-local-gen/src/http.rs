//! The one outbound HTTP policy for local runtimes and providers, on `rightkit-http`.
//!
//! Every provider, studio and local-runtime call goes through [`Http`], so the rules that
//! matter for money and safety live in one place:
//!
//! - **No automatic retries.** A submit that reached a paid provider must never be re-sent
//!   by the transport; callers classify `retryable` themselves and the job layer decides.
//! - **Non-2xx responses are returned, not raised**, because callers read provider error
//!   bodies ([`Http::send`] is `send_any`).
//! - **Redirects are followed** (10 hops), as model hosts and result CDNs redirect.
//! - **Public addresses only**, except literal loopback hosts (the bundled local runtimes
//!   and the e2e mock servers) and hosts a caller names with [`Http::for_host`] (a rented
//!   worker the user configured).
//! - **Proxy environment variables are honoured** (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`).
//! - **One time budget per call**: `total` bounds the whole exchange.

use std::cell::Cell;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// No download helper is re-exported or wrapped here: models are downloaded through
// rightkit-models, and rightkit-http publishes without its downloader until a later version.
use rightkit_http::{AddressPolicy, AttemptDelivery, Client, ClientConfig, ProxyMode};
pub use rightkit_http::{
    CancelToken, HttpError, Method, Multipart, Request, Response, RetryPolicy,
};

/// Hosts that may reach loopback addresses.
const LOOPBACK_HOSTS: [&str; 3] = ["127.0.0.1", "localhost", "::1"];
const MAX_REDIRECTS: u32 = 10;

#[derive(Clone)]
pub struct Http {
    client: Client,
}

impl Http {
    /// `connect` bounds connection setup; `total` bounds the whole call (headers and body).
    pub fn new(connect: Duration, total: Duration) -> Self {
        Self::build(connect, total, policy())
    }

    /// As [`Http::new`], and `host` (exact match) may resolve to any address class. Use it
    /// only for a destination the user configured, such as a rented worker.
    pub fn for_host(connect: Duration, total: Duration, host: &str) -> Self {
        Self::build(
            connect,
            total,
            policy().allow_if(vec![host.to_string()], |_| true),
        )
    }

    /// Per-phase budgets for short local probes: `response` bounds time to headers and
    /// `body` bounds the body, inside `total`.
    pub fn with_phases(
        connect: Duration,
        total: Duration,
        response: Duration,
        body: Duration,
    ) -> Self {
        let mut config = config(connect, total, policy());
        config.response_timeout = Some(response);
        config.recv_body_timeout = Some(body);
        Self {
            client: Client::new(config),
        }
    }

    fn build(connect: Duration, total: Duration, policy: AddressPolicy) -> Self {
        Self {
            client: Client::new(config(connect, total, policy)),
        }
    }

    /// Send once and return the response whatever its status. The call's delivery evidence
    /// (the monotonic maximum over every transport hop, redirects included) is kept for
    /// [`take_delivery`] on this thread.
    pub fn send(&self, req: Request) -> Result<Response, HttpError> {
        let seen = Arc::new(Mutex::new(None::<Delivery>));
        let sink = Arc::clone(&seen);
        // A per-call clone carries the observer, so concurrent calls never share a record.
        let client = self.client.clone().on_attempt_outcome(move |o| {
            let d = Delivery::from(o.delivery);
            let mut slot = sink.lock().unwrap_or_else(|p| p.into_inner());
            *slot = Some(slot.map_or(d, |prev| prev.max(d)));
        });
        let result = client.send_any(&once(req));
        let observed = *seen.lock().unwrap_or_else(|p| p.into_inner());
        LAST_DELIVERY.with(|c| c.set(observed));
        result
    }

    /// Send once and return a body reader; a non-2xx status is an error here.
    pub fn stream(&self, req: Request) -> Result<rightkit_http::StreamResponse, HttpError> {
        self.client.stream(&once(req))
    }
}

/// What one call's transport hops proved about delivery, ordered by how much may have happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Delivery {
    /// Every observed hop failed before any byte left.
    NotSent,
    /// A hop may have reached the server (post-connect failure, timeout, mid-flight cancel).
    Uncertain,
    /// A hop got a reply.
    Responded,
}

impl From<AttemptDelivery> for Delivery {
    fn from(d: AttemptDelivery) -> Self {
        match d {
            AttemptDelivery::NotSent => Self::NotSent,
            AttemptDelivery::Responded => Self::Responded,
            // Uncertain and any later variant: assume the request may have arrived.
            _ => Self::Uncertain,
        }
    }
}

thread_local! {
    static LAST_DELIVERY: Cell<Option<Delivery>> = const { Cell::new(None) };
}

/// Take (and clear) the delivery evidence of the last [`Http::send`] on this thread. `None`
/// means no transport hop was observed (a refusal before anything was sent).
pub fn take_delivery() -> Option<Delivery> {
    LAST_DELIVERY.with(|c| c.take())
}

fn policy() -> AddressPolicy {
    AddressPolicy::public_web().allow_if(
        LOOPBACK_HOSTS.iter().map(|h| h.to_string()).collect(),
        |ip| ip.is_loopback(),
    )
}

fn config(connect: Duration, total: Duration, policy: AddressPolicy) -> ClientConfig {
    ClientConfig {
        connect_timeout: Some(connect),
        request_timeout: total,
        response_timeout: Some(total),
        max_redirects: MAX_REDIRECTS,
        retry: RetryPolicy::none(),
        address_policy: policy,
        // A user behind a proxy keeps working; rightkit-http still vets every destination.
        proxy: ProxyMode::Environment,
        ..ClientConfig::default()
    }
}

/// Pin single-attempt delivery on the request itself, whatever the client default becomes.
fn once(req: Request) -> Request {
    req.retry(RetryPolicy::none()).retry_unsafe_statuses(false)
}

/// The media type of a response, lower-cased and without parameters.
pub fn content_type(resp: &Response) -> Option<String> {
    resp.header("content-type").map(|s| {
        s.split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
    })
}

/// Whether sending the same request again could succeed. A refused destination, a request
/// that could not be built and an over-limit or undecodable response fail the same way
/// every time; connection failures and timeouts may not.
pub fn retryable(err: &HttpError) -> bool {
    matches!(
        err,
        HttpError::Transport { .. } | HttpError::Timeout { .. } | HttpError::Stream(_)
    )
}
