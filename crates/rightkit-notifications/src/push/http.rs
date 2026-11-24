//! The APNs transport over `rightkit-http`'s async client (feature `apns-http`).
//!
//! This is the one file that depends on an HTTP stack. APNs speaks HTTP/2 only, and `rightkit-http`'s
//! client advertises HTTP/1.1 only today (its reqwest `http2` feature is off), so until the owner decides
//! to let `rightkit-http` enable HTTP/2 (which adds the `h2` crate to that crate's tree) a send through this
//! transport reaches APNs over HTTP/1.1 and APNs refuses it; the error comes back as "no answer" and
//! [`send_alert`](super::send_alert) reports a retry. Nothing else in this crate changes with the decision:
//! the choice is one feature line in `rightkit-http` and one in this crate's manifest.

use std::time::Duration;

use rightkit_http::{AsyncClient, ClientConfig, Request, RetryPolicy};

use super::{PushError, PushRequest, PushResponse, PushTransport};

/// Longest wait for one APNs answer.
const SEND_TIMEOUT: Duration = Duration::from_secs(20);

/// Sends alerts through one shared client and a private single-thread runtime, so the blocking
/// [`PushTransport::send`] can be called from any thread that is not itself inside a tokio runtime.
pub struct HttpPushTransport {
    client: AsyncClient,
    runtime: tokio::runtime::Runtime,
}

impl HttpPushTransport {
    /// No retries and no redirects here: [`send_alert`](super::send_alert) classifies the answer and the
    /// caller owns the retry loop, so a replayed request never happens behind its back.
    pub fn new() -> Result<Self, PushError> {
        let config = ClientConfig {
            retry: RetryPolicy::none(),
            max_redirects: 0,
            ..ClientConfig::default()
        };
        let client = AsyncClient::new(config).map_err(|_| PushError::Invalid("http client"))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| PushError::Invalid("runtime"))?;
        Ok(Self { client, runtime })
    }
}

impl PushTransport for HttpPushTransport {
    fn send(&self, request: &PushRequest) -> Result<PushResponse, String> {
        let mut http = Request::post(format!("https://{}{}", request.host, request.path));
        for (name, value) in &request.headers {
            http = if name == "authorization" {
                http.secret_header(name.clone(), value.clone())
            } else {
                http.header(name.clone(), value.clone())
            };
        }
        http.body = request.body.clone();
        http.timeout = Some(SEND_TIMEOUT);
        http.retry = Some(RetryPolicy::none());
        // The error text is dropped on purpose: it could name the URL, which holds the device token.
        let response = self
            .runtime
            .block_on(self.client.send_any(&http))
            .map_err(|_| "no answer from APNs".to_string())?;
        Ok(PushResponse::from_parts(
            response.status,
            &response.body,
            response.header("retry-after"),
        ))
    }
}
