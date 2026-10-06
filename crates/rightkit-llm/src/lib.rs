//! `rightkit-llm`: BYOK LLM provider clients for Right Suite apps.
//!
//! - [`anthropic::AnthropicProvider`] (Messages API) and
//!   [`openai::OpenAiCompatProvider`] (chat completions: OpenAI, OpenRouter, Groq,
//!   DeepSeek, Together, Mistral, xAI, Gemini endpoint, Ollama, LM Studio).
//! - Streaming with cancellation, tool use, forced-tool structured output.
//! - [`failover::FailoverChain`]: retryable-only route failover, first-token
//!   timeout, circuit breaker.
//! - [`pricing::PriceTable`] and [`cache`] (prompt-cache breakpoints + ledger).
//! - Credentials come from a [`credentials::CredentialSource`]; production wiring
//!   uses `rightkit-secrets` through the `SecretStore` trait.

pub mod anthropic;
pub mod cache;
pub mod credentials;
pub mod error;
pub mod failover;
pub mod openai;
pub mod presets;
pub mod pricing;
pub mod provider;
pub mod types;

pub use credentials::{CredentialSource, SecretStoreCredentials, StaticCredentials};
pub use error::{Attempt, LlmError};
pub use failover::{FailoverChain, FailoverEvent, FailoverPolicy, Route};
pub use pricing::{ModelPrice, PriceTable};
pub use provider::Provider;
pub use types::*;
