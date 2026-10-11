//! `rightkit-http`: the shared blocking HTTP client for Right Suite apps.
//!
//! - [`Client`]: timeouts, bounded bodies, retry/backoff honoring `Retry-After`,
//!   retry-safety rules for non-idempotent methods, streaming responses.
//! - [`Client::download_to_path`]: resumable download to a file with SHA-256 and
//!   length verification ([`DownloadOptions`], [`DownloadReport`]).
//! - [`Multipart`]: `multipart/form-data` bodies with exact framing.
//! - [`netpolicy`]: the outbound address policy (SSRF guard) every request passes: resolve once,
//!   classify every address, connect only to checked addresses, on every redirect hop.
//! - [`CancelToken`]: cooperative cancellation for calls, streams and downloads.
//! - [`sse`]: WHATWG-compliant Server-Sent Events decoding.
//! - `untrusted` (feature, default on): tamper-evident wrapping of external text for model input.
//! - [`secret`]: redacted [`Secret`] and the [`SecretStore`] interface that
//!   `rightkit-secrets` implements.
//! - `async_client` (feature `async`): [`AsyncClient`], the same policy layer on
//!   reqwest/tokio, with a body [`BodyStream`] and an async [`SseStream`].
//! - `testing` (feature): in-process mock server that records exact requests.

#[cfg(feature = "async")]
pub mod async_client;
pub mod cancel;
pub mod client;
pub mod download;
pub mod error;
pub mod multipart;
pub mod netpolicy;
mod redirect;
pub mod retry;
pub mod secret;
pub mod sse;
mod system_proxy;
mod trust;
#[cfg(feature = "untrusted")]
pub mod untrusted;
mod urlredact;
mod wire;

#[cfg(feature = "testing")]
pub mod testing;

pub use cancel::{is_cancelled_io, CancelToken};
pub use client::{
    form_encode, percent_decode, percent_encode, Client, ClientConfig, Method, ProxyDiagnostic,
    ProxyMode, Request, Response, ResponseHead, RetryEvent, StreamResponse, TrustRoots,
};
pub use download::{DownloadError, DownloadOptions, DownloadProgress, DownloadReport};
pub use error::{ErrorClass, HttpError, Phase};
pub use multipart::Multipart;
pub use netpolicy::{
    parse_host_ip, parse_legacy_ipv4, AddressClass, AddressPolicy, HostIpKind, IpNet, ParsedHostIp,
};
pub use retry::{parse_retry_after, RetryPolicy};
pub use secret::{MemorySecretStore, Secret, SecretError, SecretStore};
pub use sse::{SseDecoder, SseEvent, SseReader};
#[cfg(feature = "untrusted")]
pub use untrusted::{wrap_untrusted_web_text, wrap_untrusted_web_text_with, WrapOptions};

#[cfg(feature = "async")]
pub use async_client::{AsyncClient, AsyncStreamResponse, BodyStream, SseStream};
