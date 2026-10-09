//! `rightkit-http`: the shared blocking HTTP client for Right Suite apps.
//!
//! - [`Client`]: timeouts, bounded bodies, retry/backoff honoring `Retry-After`,
//!   retry-safety rules for non-idempotent methods, streaming responses.
//! - [`sse`]: WHATWG-compliant Server-Sent Events decoding.
//! - [`untrusted`]: tamper-evident wrapping of external text for model input.
//! - [`secret`]: redacted [`Secret`] and the [`SecretStore`] interface that
//!   `rightkit-secrets` implements.
//! - `async_client` (feature `async`): [`AsyncClient`], the same policy layer on
//!   reqwest/tokio, with a body [`BodyStream`] and an async [`SseStream`].
//! - `testing` (feature): in-process mock server that records exact requests.

#[cfg(feature = "async")]
pub mod async_client;
pub mod client;
pub mod error;
pub mod retry;
pub mod secret;
pub mod sse;
pub mod untrusted;

#[cfg(feature = "testing")]
pub mod testing;

pub use client::{
    form_encode, percent_decode, percent_encode, Client, ClientConfig, Method, Request, Response,
    RetryEvent, StreamResponse,
};
pub use error::HttpError;
pub use retry::{parse_retry_after, RetryPolicy};
pub use secret::{MemorySecretStore, Secret, SecretError, SecretStore};
pub use sse::{SseDecoder, SseEvent, SseReader};
pub use untrusted::{wrap_untrusted_web_text, wrap_untrusted_web_text_with, WrapOptions};

#[cfg(feature = "async")]
pub use async_client::{AsyncClient, AsyncStreamResponse, BodyStream, SseStream};
