# rightkit-llm

Brand-neutral bring-your-own-key (BYOK) LLM provider clients for Right Suite apps. It provides Anthropic Messages and OpenAI-compatible chat clients with streaming, tool use, forced-tool structured output, route failover with a circuit breaker, pricing, and prompt-cache breakpoints.

Catalog capability: `llm-providers`. Release versions are tracked in [INDEX.md](../../INDEX.md) and `CHANGELOG.md`.

## What it owns

- `anthropic::AnthropicProvider`: the Anthropic Messages API.
- `openai::OpenAiCompatProvider`: chat completions for OpenAI and compatible endpoints (OpenRouter, Groq, DeepSeek, Together, Mistral, xAI, the Gemini endpoint, Ollama, LM Studio). `presets` lists the known endpoints.
- `failover::FailoverChain`, `Route`, `FailoverPolicy`: retryable-only route failover, a first-token timeout, and a circuit breaker. `FailoverEvent` reports each switch.
- `pricing::PriceTable`: per-model prices.
- `cache`: prompt-cache breakpoint planning (`plan_breakpoints`, `CacheBreakpointPlanner`) and a `CacheLedger`.
- `credentials`: `CredentialSource`, with `StaticCredentials` and `SecretStoreCredentials`. Production wiring uses `rightkit-secrets` through the `SecretStore` trait.
- `types`, `error`: `ChatRequest`, `Message`, `StreamEvent`, `LlmError`.

HTTP comes from `rightkit-http`.

## When to use it

- Use it when an app brings the user's own API key and calls a model directly.
- Use it when calls must fail over between routes on retryable errors.

Do not use it to store keys. Keys come from a `CredentialSource`, and production keys live in `rightkit-secrets`.

## Cargo features

None. The crate has no `[features]` table. Dependencies: `rightkit-http`, `serde`, `serde_json`.

## Usage

Illustrative. These calls match `tests/e2e.rs`:

```rust
use std::sync::Arc;
use rightkit_http::{Client, ClientConfig, RetryPolicy};
use rightkit_llm::anthropic::AnthropicProvider;
use rightkit_llm::credentials::StaticCredentials;
use rightkit_llm::ChatRequest;

let client = Client::new(ClientConfig { retry: RetryPolicy::none(), ..ClientConfig::default() });
let creds = Arc::new(StaticCredentials::default().with("anthropic", "<key from the app's secret store>"));
let provider = AnthropicProvider::new("https://api.anthropic.com", client, creds);
let request = ChatRequest::simple("<model id>", "You are concise", "Find Rust");
```

Do not put a real key in source. Use the app's secret store through `SecretStoreCredentials`.

## Platform support

Pure Rust over `rightkit-http` (`ureq` with `rustls`). No platform-specific code.

## Tests

`tests/e2e.rs` runs both provider clients against a local mock server. It checks exact headers, body and parsed tool calls.

## Version and changes

Version: see [INDEX.md](../../INDEX.md). Changes are recorded in `CHANGELOG.md`.
