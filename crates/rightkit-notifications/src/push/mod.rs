//! Phone push (APNs), the parts that need no network (RK-4).
//!
//! Everything here is a pure function or a trait the host implements: the alert shape, the provider
//! token, the request description, the response classification and the device token registry. The
//! HTTP/2 connection itself is the host's [`PushTransport`]; it waits on the rightkit-http HTTP/2
//! decision and is not in this crate.
//!
//! Content rule: an alert carries a short title and an opaque item id and nothing else. There is no
//! body field, so message text can never reach Apple's servers through this crate.

mod alert;
#[cfg(feature = "apns-http")]
mod http;
mod registry;
mod request;
mod token;
mod transport;

pub use alert::{PhoneAlert, MAX_ITEM_ID_BYTES, MAX_TITLE_CHARS};
#[cfg(feature = "apns-http")]
pub use http::HttpPushTransport;
pub use registry::{DeviceTokenStore, MemoryTokenStore};
pub use request::{
    build_request, ApnsEnvironment, DeviceToken, Priority, PushRequest, MAX_DEVICE_TOKEN_BYTES,
    MIN_DEVICE_TOKEN_BYTES,
};
pub use token::{
    build_provider_jwt, ProviderTokenCache, MAX_TOKEN_AGE_SECS, MIN_REFRESH_INTERVAL_SECS,
    REFRESH_AFTER_SECS,
};
pub use transport::{
    backoff_hint, classify, send_alert, ApnsConfig, PushOutcome, PushResponse, PushTransport,
    MAX_BACKOFF_SECS,
};

/// Why a push step failed. No variant carries a key, a provider token or a device token.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error("alert title is empty")]
    TitleEmpty,
    #[error("alert title is longer than the cap")]
    TitleTooLong,
    #[error("alert title contains a control character")]
    TitleControl,
    #[error("alert item id must be 1 to 128 characters of A-Z a-z 0-9 . _ : -")]
    BadItemId,
    /// A configuration or request field is malformed (which one is named, never its value).
    #[error("invalid {0}")]
    Invalid(&'static str),
    #[error("signing the provider token failed")]
    Signing,
    #[error("no device token is registered for this device")]
    NoDeviceToken,
    /// The host's token store failed. The message is the host's; stores must not put tokens in it.
    #[error("device token store failed: {0}")]
    Store(String),
}
