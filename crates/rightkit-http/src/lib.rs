//! `rightkit-http`: the shared blocking HTTP client for Right Suite apps.
//!
//! - [`Client`]: timeouts, bounded bodies, retry/backoff honoring `Retry-After`,
//!   retry-safety rules for non-idempotent methods, streaming responses.
//! - [`Multipart`]: `multipart/form-data` bodies with exact framing.
//! - [`netpolicy`]: the outbound address policy (SSRF guard) every request passes: resolve once,
//!   classify every address, connect only to checked addresses, on every redirect hop.
//! - [`CancelToken`]: cooperative cancellation for calls and streams.
//! - [`sse`]: WHATWG-compliant Server-Sent Events decoding.
//! - `untrusted` (feature, off by default): tamper-evident wrapping of external text for model input.
//! - [`secret`]: redacted [`Secret`], the [`SecretStore`] interface and the in-memory
//!   [`MemorySecretStore`] (no production implementation ships in this crate).
//! - [`ApprovedOrigins`], [`check_redirect_target`] and [`RedirectRefusal`]: the redirect origin
//!   allowlist (`Request::approved_redirect_origins`), its typed error
//!   ([`HttpError::RedirectRefused`]) and a string-level guard for other transports.
//! - `async_client` (feature `async`): `AsyncClient`, the same policy layer on
//!   reqwest/tokio, with a body `BodyStream` and an async `SseStream`.
//! - `testing` (feature): in-process mock server that records exact requests.

#[cfg(feature = "async")]
pub mod async_client;
pub mod cancel;
pub mod client;
pub mod error;
mod idle;
pub mod multipart;
pub mod netpolicy;
pub mod quota;
mod redirect;
pub mod retry;
pub mod secret;
pub mod sse;
mod system_proxy;
mod trust;
#[cfg(feature = "untrusted")]
pub mod untrusted;
mod urlredact;

#[cfg(feature = "testing")]
pub mod testing;

pub use cancel::{is_cancelled_io, CancelToken};
pub use client::{
    form_encode, percent_decode, percent_decode_component, percent_encode,
    percent_encode_component, AttemptDelivery, AttemptEvent, AttemptOutcome, Client, ClientConfig,
    Method, ProxyDiagnostic, ProxyMode, ProxyModeUsed, Request, Response, ResponseHead, RetryEvent,
    StreamResponse, TrustRoots,
};
pub use error::{ErrorClass, HttpError, Phase};
pub use multipart::{Multipart, MultipartRelated};
pub use netpolicy::{
    check_redirect_target, parse_host_ip, parse_legacy_ipv4, validate_public_https_url,
    AddressClass, AddressPolicy, ApprovedOrigins, CredentialScope, HostIpKind, IpNet, ParsedHostIp,
    RedirectRefusal, ResolveError, UrlPolicyError,
};
pub use quota::{defer_in, remaining_in, QuotaSchedule, QuotaStore};
pub use retry::{
    parse_rate_limit_reset, parse_rate_limit_reset_at, parse_retry_after, parse_retry_after_at,
    RetryPolicy,
};
pub use secret::{MemorySecretStore, Secret, SecretError, SecretStore};
pub use sse::{EofPolicy, SseDecoder, SseEvent, SseReader};
#[cfg(feature = "untrusted")]
pub use untrusted::{
    wrap_untrusted_web_text, wrap_untrusted_web_text_coderight, wrap_untrusted_web_text_with,
    WrapOptions,
};

#[cfg(feature = "async")]
pub use async_client::{AsyncClient, AsyncStreamResponse, BodyStream, SseStream};
