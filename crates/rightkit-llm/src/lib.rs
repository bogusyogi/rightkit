//! `rightkit-llm`: BYOK LLM provider clients for Right Suite apps.
//!
//! - [`anthropic::AnthropicProvider`] (Messages API) and
//!   [`openai::OpenAiCompatProvider`] (chat completions: OpenAI, OpenRouter, Groq,
//!   DeepSeek, Together, Mistral, xAI, Gemini endpoint, Ollama, LM Studio).
//! - Streaming with cancellation, tool use, forced-tool structured output.
//! - [`failover::FailoverChain`]: retryable-only route failover by default (configurable
//!   with [`failover::FailoverOn`]), first-token timeout, circuit breaker.
//! - [`pricing::PriceTable`] and [`cache`] (prompt-cache breakpoints + ledger).
//! - Raw response capture for an on-device payload ledger: [`raw`], `with_raw_capture` on the
//!   HTTP providers, [`RawExchange`] on the observer's [`CallRecord`] and on `Truncated` and
//!   `Malformed` errors. Off by default; non-streaming calls only.
//! - `cli` feature: [`provider::cli::CliProvider`] drives an installed, signed-in
//!   assistant CLI (Claude Code, Codex) as a bounded child process; no API key.
//!   [`provider::cli::CancelHandle`] kills a running call's process tree mid-run.
//! - Credentials come from a [`credentials::CredentialSource`]; production wiring
//!   uses `rightkit-credentials` through the `SecretStore` trait.

pub mod anthropic;
pub mod cache;
pub mod credentials;
pub mod error;
pub mod failover;
pub mod openai;
pub mod presets;
pub mod pricing;
pub mod provider;
pub mod raw;
pub mod types;

pub use credentials::{CredentialSource, SecretStoreCredentials, StaticCredentials};
pub use error::{Attempt, LlmError};
pub use failover::{FailoverChain, FailoverEvent, FailoverOn, FailoverPolicy, Route};
pub use pricing::{ModelPrice, PriceTable};
pub use provider::Provider;
pub use raw::{RawExchange, RawHandle};
/// The HTTP client this crate's providers are built on, re-exported so a host can build a
/// [`Client`] (timeout, retry, proxy, redaction, address policy) without a direct
/// `rightkit-http` dependency.
pub use rightkit_http as http;
pub use rightkit_http::{AddressPolicy, Client, ClientConfig, HttpError, ProxyMode, RetryPolicy};
pub use types::*;
