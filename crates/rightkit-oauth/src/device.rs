//! RFC 8628 device authorization grant, ported from CodeRight's device-code engine
//! (`subscription_auth/device_code.rs`) to this crate's blocking client.
//!
//! [`OAuthClient::begin_device`] returns the code the user types and the page they open;
//! [`OAuthClient::finish_device`] polls the token endpoint at the server's interval, honours
//! `slow_down` (the server's new interval, else +5 s per RFC 8628 §3.5), and ends on approval,
//! denial, expiry, or cancellation. The clock is injectable so the timing is testable.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rightkit_http::{Request, Secret};

use crate::{parse_tokens, OAuthClient, OAuthError, TokenSet};

/// RFC 8628 §3.2: an omitted `interval` means 5 seconds.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);
/// RFC 8628 §3.5: `slow_down` adds 5 seconds when the server names no new interval.
pub const SLOW_DOWN_INCREMENT: Duration = Duration::from_secs(5);
/// Never poll faster than this, whatever the server says.
pub const MINIMUM_INTERVAL: Duration = Duration::from_secs(1);
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// What to show the user while the device flow waits for approval.
#[derive(Clone)]
pub struct DeviceAuthorization {
    /// The short code the user enters on the verification page.
    pub user_code: String,
    /// The page the user opens (show it, or open it in a browser).
    pub verification_uri: String,
    /// The same page with the code pre-filled, when the provider offers one.
    pub verification_uri_complete: Option<String>,
    /// How long the codes stay valid.
    pub expires_in: Duration,
    /// The polling interval the server asked for.
    pub interval: Duration,
    device_code: Secret,
}

impl fmt::Debug for DeviceAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceAuthorization")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("verification_uri_complete", &self.verification_uri_complete)
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .field("device_code", &"[redacted]")
            .finish()
    }
}

/// One poll of the token endpoint.
#[derive(Debug)]
pub enum DevicePoll {
    /// The user has not approved yet.
    Pending,
    /// Poll more slowly; `interval` is the server's new minimum when it names one.
    SlowDown { interval: Option<Duration> },
    /// Approved.
    Complete(TokenSet),
}

/// Time source for the polling loop.
pub trait Clock: Send + Sync {
    /// Monotonic time since an arbitrary fixed origin.
    fn now(&self) -> Duration;
    fn sleep(&self, duration: Duration);
}

/// Wall-clock time and real sleeps.
pub struct SystemClock(Instant);

impl Default for SystemClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// How [`OAuthClient::finish_device`] waits.
#[derive(Clone)]
pub struct DeviceWait {
    /// Set to `true` from any thread to stop waiting with [`OAuthError::Cancelled`].
    pub cancel: Arc<AtomicBool>,
    pub clock: Arc<dyn Clock>,
}

impl Default for DeviceWait {
    fn default() -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            clock: Arc::new(SystemClock::default()),
        }
    }
}

impl fmt::Debug for DeviceWait {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceWait")
            .field("cancelled", &self.cancel.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

/// Longest slice of one sleep, so cancellation is noticed promptly.
const CANCEL_GRANULARITY: Duration = Duration::from_millis(250);

impl OAuthClient {
    /// Start a device flow: ask the provider for a user code and verification page.
    pub fn begin_device(&self) -> Result<DeviceAuthorization, OAuthError> {
        let url = self
            .config
            .device_authorization_url
            .as_deref()
            .ok_or_else(|| {
                OAuthError::InvalidConfig("provider has no device authorization endpoint".into())
            })?;
        let scope = self.config.scopes.join(" ");
        let mut pairs = vec![("client_id", self.config.client_id.as_str())];
        if !scope.is_empty() {
            pairs.push(("scope", scope.as_str()));
        }
        let response = self.http.send_any(
            &Request::post(url)
                .header("Accept", "application/json")
                .form(&pairs),
        )?;
        let value: serde_json::Value = response.json().unwrap_or_default();
        if let Some(error) = value.get("error").and_then(|e| e.as_str()) {
            return Err(provider_error(response.status, error, &value));
        }
        if !(200..300).contains(&response.status) {
            return Err(provider_error(
                response.status,
                "device_authorization_failed",
                &value,
            ));
        }
        let text = |key: &str| {
            value
                .get(key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        };
        let device_code = text("device_code")
            .ok_or_else(|| OAuthError::Callback("device response omitted device_code".into()))?;
        let user_code = text("user_code")
            .ok_or_else(|| OAuthError::Callback("device response omitted user_code".into()))?;
        // RFC 8628 names `verification_uri`; Google still sends `verification_url`.
        let verification_uri = text("verification_uri")
            .or_else(|| text("verification_url"))
            .ok_or_else(|| {
                OAuthError::Callback("device response omitted verification_uri".into())
            })?;
        let seconds = |key: &str| {
            value
                .get(key)
                .and_then(|v| v.as_u64())
                .map(Duration::from_secs)
        };
        Ok(DeviceAuthorization {
            user_code: user_code.to_string(),
            verification_uri: verification_uri.to_string(),
            verification_uri_complete: text("verification_uri_complete").map(String::from),
            expires_in: seconds("expires_in").unwrap_or(Duration::from_secs(900)),
            interval: seconds("interval")
                .unwrap_or(DEFAULT_INTERVAL)
                .max(MINIMUM_INTERVAL),
            device_code: Secret::new(device_code),
        })
    }

    /// Poll the token endpoint once.
    pub fn poll_device(
        &self,
        authorization: &DeviceAuthorization,
    ) -> Result<DevicePoll, OAuthError> {
        let mut pairs: Vec<(&str, &str)> = vec![
            ("grant_type", DEVICE_GRANT),
            ("device_code", authorization.device_code.expose()),
            ("client_id", self.config.client_id.as_str()),
        ];
        if let Some(secret) = &self.config.client_secret {
            pairs.push(("client_secret", secret.expose()));
        }
        let request = Request::post(&self.config.token_url)
            .header("Accept", "application/json")
            .secret_form(&pairs, crate::SECRET_FORM_FIELDS);
        let response = self.http.send_any(&request)?;
        let value: serde_json::Value = response.json().unwrap_or_default();
        // GitHub answers pending polls with HTTP 200 and an `error`; RFC servers use 400.
        match value.get("error").and_then(|e| e.as_str()) {
            Some("authorization_pending") => Ok(DevicePoll::Pending),
            Some("slow_down") => Ok(DevicePoll::SlowDown {
                interval: value
                    .get("interval")
                    .and_then(|v| v.as_u64())
                    .filter(|s| *s > 0)
                    .map(Duration::from_secs),
            }),
            Some("expired_token") => Err(OAuthError::DeviceCodeExpired),
            Some("access_denied") => Err(OAuthError::AccessDenied),
            Some(error) => Err(crate::scrub_provider(
                &request,
                provider_error(response.status, error, &value),
            )),
            None if (200..300).contains(&response.status) => {
                parse_tokens(&value, None).map(DevicePoll::Complete)
            }
            None => Err(crate::scrub_provider(
                &request,
                provider_error(response.status, "token_request_failed", &value),
            )),
        }
    }

    /// Poll until the user approves, denies, the code expires, or `wait.cancel` is set.
    /// The first poll waits one interval: the code cannot be approved before it is shown.
    pub fn finish_device(
        &self,
        authorization: &DeviceAuthorization,
        wait: &DeviceWait,
    ) -> Result<TokenSet, OAuthError> {
        let clock = wait.clock.as_ref();
        let deadline = clock.now() + authorization.expires_in;
        let mut interval = authorization.interval.max(MINIMUM_INTERVAL);
        let mut slowed = false;
        loop {
            let remaining = deadline.saturating_sub(clock.now());
            if remaining.is_zero() {
                return Err(if slowed {
                    OAuthError::Callback(
                        "device code expired after slow_down responses; check the system clock"
                            .into(),
                    )
                } else {
                    OAuthError::DeviceCodeExpired
                });
            }
            sleep_cancellable(clock, interval.min(remaining), &wait.cancel)?;
            match self.poll_device(authorization)? {
                DevicePoll::Complete(tokens) => return Ok(tokens),
                DevicePoll::Pending => {}
                DevicePoll::SlowDown { interval: server } => {
                    slowed = true;
                    interval = server
                        .unwrap_or(interval + SLOW_DOWN_INCREMENT)
                        .max(MINIMUM_INTERVAL);
                }
            }
        }
    }
}

fn sleep_cancellable(
    clock: &dyn Clock,
    total: Duration,
    cancel: &AtomicBool,
) -> Result<(), OAuthError> {
    let end = clock.now() + total;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(OAuthError::Cancelled);
        }
        let left = end.saturating_sub(clock.now());
        if left.is_zero() {
            return Ok(());
        }
        clock.sleep(left.min(CANCEL_GRANULARITY));
    }
}

fn provider_error(status: u16, error: &str, value: &serde_json::Value) -> OAuthError {
    OAuthError::Provider {
        status,
        error: error.to_string(),
        description: value
            .get("error_description")
            .and_then(|d| d.as_str())
            .map(String::from),
    }
}
