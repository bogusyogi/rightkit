//! The APNs request as a plain description the host's transport sends.

use super::{PhoneAlert, PushError};

pub const MIN_DEVICE_TOKEN_BYTES: usize = 32;
pub const MAX_DEVICE_TOKEN_BYTES: usize = 100;

/// An APNs device token: the hex string the phone reports. `Debug` hides it.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceToken(String);

impl DeviceToken {
    /// Accepts the hex string (either case, stored lowercase) of 32 to 100 bytes.
    pub fn from_hex(hex: &str) -> Result<Self, PushError> {
        let hex = hex.trim();
        let ok = hex.len().is_multiple_of(2)
            && (MIN_DEVICE_TOKEN_BYTES * 2..=MAX_DEVICE_TOKEN_BYTES * 2).contains(&hex.len())
            && hex.bytes().all(|b| b.is_ascii_hexdigit());
        if !ok {
            return Err(PushError::Invalid("device token"));
        }
        Ok(Self(hex.to_ascii_lowercase()))
    }

    /// The hex string, for persistence. Do not log it.
    pub fn expose_hex(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for DeviceToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceToken(<redacted>)")
    }
}

/// Which APNs endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApnsEnvironment {
    Production,
    Sandbox,
}

impl ApnsEnvironment {
    pub fn host(self) -> &'static str {
        match self {
            Self::Production => "api.push.apple.com",
            Self::Sandbox => "api.sandbox.push.apple.com",
        }
    }
}

/// `apns-priority`: 10 sends immediately, 5 lets the system batch for power.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Immediate,
    Background,
}

/// What to send: an HTTP/2 `POST` to `https://{host}{path}` with `headers` (lowercase names, as
/// HTTP/2 requires) and `body`. The transport adds only what the protocol layer owns (`:method`,
/// `:scheme`, `:authority`, `content-length`).
#[derive(Clone, PartialEq, Eq)]
pub struct PushRequest {
    pub host: &'static str,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for PushRequest {
    /// The authorization header and the device token in the path are not printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("PushRequest")
            .field("host", &self.host)
            .field("path", &"/3/device/<redacted>")
            .field("headers", &names)
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// Build the request for one alert. `provider_jwt` is the token from
/// [`ProviderTokenCache::token`](super::ProviderTokenCache::token); `topic` is the app's bundle id.
pub fn build_request(
    env: ApnsEnvironment,
    topic: &str,
    token: &DeviceToken,
    provider_jwt: &str,
    alert: &PhoneAlert,
    priority: Priority,
) -> Result<PushRequest, PushError> {
    if topic.is_empty()
        || topic.len() > 255
        || !topic
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return Err(PushError::Invalid("topic"));
    }
    // A JWT is three base64url parts; anything else (notably CR or LF) could split a header.
    if provider_jwt.is_empty()
        || !provider_jwt
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(PushError::Invalid("provider token"));
    }
    let priority = match priority {
        Priority::Immediate => "10",
        Priority::Background => "5",
    };
    Ok(PushRequest {
        host: env.host(),
        path: format!("/3/device/{}", token.expose_hex()),
        headers: vec![
            ("authorization".into(), format!("bearer {provider_jwt}")),
            ("apns-topic".into(), topic.into()),
            ("apns-push-type".into(), "alert".into()),
            ("apns-priority".into(), priority.into()),
            ("content-type".into(), "application/json".into()),
        ],
        body: alert.payload_json().into_bytes(),
    })
}
