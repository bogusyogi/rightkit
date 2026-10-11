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

HTTP comes from `rightkit-http`. With feature `cli`:

- `provider::cli::CliProvider`: a provider over an installed, signed-in assistant CLI. See "Command-line assistant provider".

## When to use it

- Use it when an app brings the user's own API key and calls a model directly.
- Use it when calls must fail over between routes on retryable errors.

Do not use it to store keys. Keys come from a `CredentialSource`, and production keys live in `rightkit-secrets`.

## Cargo features

- `cli` (default off): `provider::cli`, which adds the optional dependency `rightkit-process`.

Always-on dependencies: `rightkit-http`, `serde`, `serde_json`.

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

## Command-line assistant provider (feature `cli`)

`CliProvider` drafts text by running a tool the user already installed and signed in to (Claude Code `claude -p`, OpenAI `codex exec`) as a child process. The app stores no API key; the tool authenticates itself. It implements `Provider`, so `FailoverChain` and callers treat it like any other route. It is text completion only: tool use and forced-tool output return `CliError::Unsupported`. `stream` is buffered and delivers the answer as one `StreamEvent::Text`.

```rust,ignore
use rightkit_llm::provider::cli::{resolve_program, CliProvider, CliSpec};

// The host resolves the program once. The provider never searches PATH.
let claude = resolve_program("claude", &std::env::var_os("PATH").unwrap()).unwrap();
let provider = CliProvider::new(CliSpec::claude_code(claude), "/abs/path/to/empty/scratch")?;
let reply = provider.complete(&request)?; // reply.usage_known == false unless the tool reported tokens
```

- `CliSpec` is data: `program` (absolute), `args`, `model_args` (`{model}` is substituted only for a plain token), `prompt` (`Stdin` or `ArgAfterTerminator`), `output` (`Text` or `Json(JsonOutput)` with RFC 6901 pointers for the answer, error flag, model, token counts and cost), `env_allow`, `env_set`, `not_signed_in_patterns`, `not_signed_in_exit_codes`, `limits`.
- `CliLimits`: wall-clock timeout, idle (no output) timeout, max stdout bytes, max stderr bytes, max prompt bytes.
- Usage: when the tool reports token counts they are returned and `usage_known` is true; otherwise `usage_known` is false and `UsageReport::Unknown` is returned by `complete_detailed`. Never zero. Every response from this route has `Metering::Subscription`, and `PriceTable::cost_of` returns `None` for it. A cost the tool reports is in `CliCompletion::reported_cost_usd`, informational only.
- `CliProvider::attempt` returns a `CliAttempt` whose `attempt` is a failover-style `Attempt` (`route` is the provider id, `reason` is `ok` or the error text), for failed runs too.
- `CliError` maps to `LlmError::Cli`: `NotInstalled`, `NotSignedIn`, `Timeout`, `IdleTimeout`, `OutputTooLarge`, `BadOutput`, `NonZeroExit`, `ToolReportedError`, `Spawn`, `Kill`, plus the non-retryable `InvalidSpec`, `InvalidRequest`, `Unsupported`. All but those three are failover triggers.
- Presets: `claude_code` and `codex_exec`. Their doc comments say which flags were checked against the installed tools' `--help` output and which behaviors were not exercised (no model call was made).

### Security notes

- No shell. The child is spawned with an argument vector. The prompt goes to stdin; if a tool has no stdin mode, `ArgAfterTerminator` puts it last, after `--`. A prompt such as `--help` or `$(cmd)` is never parsed as a flag or expanded. The model id becomes an argument only when it is a plain token that does not start with `-`.
- Program path: must be absolute and executable. Resolve a bare name once at start-up with `resolve_program`; there is no call-time PATH search.
- Environment: cleared, then only the names in `env_allow` (default `HOME`, `PATH`, plus the tool's config-dir variable) and `env_set` are given to the child. Names that look like credentials (`KEY`, `TOKEN`, `SECRET`, `PASSWORD`, `AUTH`, `COOKIE`, ...) are rejected when the spec is validated. The crate never reads, copies, logs or passes API keys or tokens, and never touches the tool's credential files; `credentials` is unused by this provider.
- Working directory: an empty, absolute scratch directory the host supplies. Do not point it at app data or a repository. Claude Code runs with all built-in tools disabled and no MCP servers. Codex has no tools-off switch, so the preset uses its read-only sandbox, which still allows reads outside the scratch directory; add an OS sandbox (`rightkit-sandbox`) when that matters.
- Bounds: any exceeded bound kills the child and its descendants through `rightkit-process` and returns a typed error; the child is reaped before the call returns. If the tree cannot be confirmed dead, the error is `CliError::Kill`.
- Output is untrusted data. It is size-capped, must be UTF-8, JSON is parsed strictly, and nothing in it is executed or used as a path. A non-empty answer is required.
- Errors include stderr only truncated to 512 bytes, with control characters removed and credential-shaped tokens redacted. Redaction is best effort.
- Not-signed-in detection runs only on a failed run (non-zero exit or the tool's JSON error flag), so an answer that mentions "not logged in" is never misread.
- Windows: the code paths compile for Windows, but the tests use a `/bin/sh` fake CLI and run on Unix only.

## Per-call options, errors and network

Request options on `ChatRequest` (all optional; the defaults are unchanged):

- `with_max_tokens(n)` sets the output limit, which is 1024 by default and always sent. `without_max_tokens()` leaves the field out of OpenAI-compatible bodies (`max_tokens` or `max_completion_tokens`, whichever the provider uses) so the provider applies its own limit. The Anthropic Messages API requires the field, so the Anthropic client still sends the value (1024 unless set) and ignores the omission.
- `with_temperature(t)` sends `temperature` only when set. `temperature` is also a public field.
- `with_timeout(d)` sets a total budget for one call. It applies `Request::timeout` and `Request::deadline` from `rightkit-http`, so it overrides the client's 60 s `request_timeout` and bounds retry attempts too. For a stream it caps the whole stream body. It does not raise `ClientConfig::response_timeout` (60 s), which still bounds the wait for response headers. An app whose model takes longer than 60 s to start answering must raise `response_timeout` on the client it builds.

Errors: a non-2xx provider answer is `LlmError::Status { status, message }`. `status` is the numeric code. `message` is the provider's `error.message` when the body is JSON (otherwise the body text), cut to 512 bytes, with the request's credential removed. `LlmError::status()` returns the code for this variant. The display text is `HTTP <code>: <message>`, so an app can keep its own wording by matching on `status`. Transport failures stay `LlmError::Http`, and timeouts are `LlmError::Http(HttpError::Timeout { .. })`.

Credentials: implement `credentials::CredentialSource` over the app's own store (one method, read on every call), or use `StaticCredentials` or `SecretStoreCredentials`. Keys are sent as a bearer token (OpenAI-compatible) or `x-api-key` (Anthropic) and registered as secrets in `rightkit-http`, so they are removed from its errors. `Debug` output of the providers, `StaticCredentials` and `ChatRequest` prints no key, and every error variant's text omits the key. `tests/call_options.rs` checks this with a sentinel key.

Network defaults (no change in this version):

- Redirects: `ClientConfig::max_redirects` is 0, so redirects are not followed and a 3xx answer surfaces as `LlmError::Status`. Raise it on the client to follow them. Credentials are never replayed to a different host.
- Proxy: `ClientConfig::proxy` defaults to `ProxyMode::None`, which ignores environment proxy variables. `ProxyMode::Environment` and `ProxyMode::Explicit(url)` come from `rightkit-http`. The operating system's proxy settings are not read: `rightkit-http` does not implement system proxy lookup yet, so this crate cannot use them either.
- Timeouts: connect 10 s, whole request 60 s, response headers 60 s (`ClientConfig` defaults).

## Platform support

Pure Rust over `rightkit-http` (`ureq` with `rustls`). No platform-specific code, except that `provider::cli` inherits `rightkit-process` platform behavior.

## Tests

`tests/e2e.rs` runs both provider clients against a local mock server. It checks exact headers, body and parsed tool calls.

`tests/call_options.rs` covers the per-call options above against the same mock server: an omitted `max_tokens` field, `temperature` 0.3, a slow server that times out at a short per-call budget and succeeds within it, a 429 that yields `LlmError::Status`, a streamed non-2xx, and the sentinel-key redaction checks.

`tests/cli.rs` (feature `cli`, Unix) drives `CliProvider` against a fake CLI script: text and JSON output with usage, verbatim prompt delivery, wall and idle timeouts that kill the tree, stdout and stderr caps, non-zero exit with redaction, sign-in detection, malformed JSON, missing program, environment allow-list, working directory, and failover. No test calls a model or the network.

## Version and changes

Version: see [INDEX.md](../../INDEX.md). Changes are recorded in `CHANGELOG.md`.
