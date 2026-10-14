# rightkit-llm

Brand-neutral bring-your-own-key (BYOK) LLM provider clients for Right Suite apps. It provides Anthropic Messages and OpenAI-compatible chat clients with streaming, tool use, forced-tool structured output, route failover with a circuit breaker, pricing, and prompt-cache breakpoints.

Catalog capability: `llm-providers`. Release versions are tracked in [INDEX.md](../../INDEX.md) and `CHANGELOG.md`.

## What it owns

- `anthropic::AnthropicProvider`: the Anthropic Messages API.
- `openai::OpenAiCompatProvider`: chat completions for OpenAI and compatible endpoints (OpenRouter, Groq, DeepSeek, Together, Mistral, xAI, the Gemini endpoint, NVIDIA NIM, Ollama, LM Studio). `presets` lists the known endpoints. It also lists models (`list_models`) and checks a key (`validate_credential`).
- `failover::FailoverChain`, `Route`, `FailoverPolicy`, `FailoverOn`: route failover, a first-token timeout, and a circuit breaker. `FailoverEvent` reports each switch. The default trigger is retryable errors only; `FailoverOn::AnyError` and `FailoverOn::custom` widen it.
- `pricing::PriceTable`: per-model prices.
- `cache`: prompt-cache breakpoint planning (`plan_breakpoints`, `CacheBreakpointPlanner`) and a `CacheLedger`.
- `credentials`: `CredentialSource`, with `StaticCredentials` and `SecretStoreCredentials`. `SecretStoreCredentials` takes any `SecretStore`. The source comment names `rightkit-credentials` as the production binding (`src/credentials.rs:10`), but `rightkit-credentials` has no `SecretStore` implementation in this repository.
- `types`, `error`: `ChatRequest`, `Message`, `StreamEvent`, `LlmError`.
- `raw`: opt-in capture of the provider's raw response bytes, status and request id for non-streaming calls (`RawExchange`, `RawHandle`). See "Raw response capture".

HTTP comes from `rightkit-http`. With feature `cli`:

- `provider::cli::CliProvider`: a provider over an installed, signed-in assistant CLI. See "Command-line assistant provider".

## When to use it

- Use it when an app brings the user's own API key and calls a model directly.
- Use it when calls must fail over between routes on retryable errors.

Do not use it to store keys. Keys come from a `CredentialSource`, and production keys live in `rightkit-credentials`.

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
- Windows: `src/provider/cli.rs` has `cfg(windows)` branches (line 145), but the tests (`tests/cli.rs`, `#![cfg(all(feature = "cli", unix))]`) use a `/bin/sh` fake CLI and run on Unix only. The Windows branches were not built or run here.

## Pass-through body fields

`ChatRequest::with_extra(key, value)` adds a top-level field to the body the crate builds, for provider-specific switches such as `reasoning_effort`, `include_reasoning`, or OpenRouter's `provider` object. It applies to both HTTP providers.

The rule:

- A key the crate does not set for the request is added.
- A key the crate sets (for example `model`, `messages`, `max_tokens`, `tools`, `stream`) makes the call fail with `LlmError::InvalidRequest` before anything is sent. Nothing is silently replaced.
- `ChatRequest::with_extra_override(key, value)` replaces such a field on purpose. Use it only when the replacement is intended.
- Merging touches only the top level; nested objects are not merged.

## Retry, truncation and cancellation controls

- `ChatRequest::with_retry(CallRetry)` sets the attempt policy for one call. `CallRetry::Client` (default) keeps the client's `ClientConfig::retry`. `CallRetry::Disabled` (also `without_retries()`) makes one attempt. `CallRetry::Policy(p)` replaces the client's policy for that call. The providers still allow replay after 429 and 5xx, as before. For Anthropic, a `Policy` must list `529` in its `retry_statuses` itself.
- `ChatRequest::reject_truncated_output()` returns `LlmError::Truncated { output_tokens }` when the provider stops at its output limit, instead of a normal answer with `StopReason::MaxTokens`. It applies to streams too. The default is unchanged.

## Key validation and model list

`OpenAiCompatProvider::list_models()` calls `GET {base}/models` and returns the model ids in server order. `validate_credential()` makes the same call and returns `Ok(())`. A rejected key (HTTP 401 or 403) returns `LlmError::Auth { status, message }`; the credential is removed from `message`. The NVIDIA NIM preset (`nvidia`) uses `https://integrate.api.nvidia.com/v1`, taken from NVIDIA's public API documentation.

## Failover trigger

`FailoverChain::fail_over_on(FailoverOn)` chooses which errors move a call to the next route:

- `FailoverOn::Retryable` (default): `LlmError::is_failover_trigger`, as before.
- `FailoverOn::AnyError`: every error except `Cancelled`, `MidStream` and `InvalidRequest`. Auth errors and rejected output fail over. The three excluded errors are never worth a second route: the caller stopped the call, output already reached the caller, or the request itself is wrong.
- `FailoverOn::custom(f)`: your predicate.

Under every policy, a route that has already delivered a token is never failed over.

## Request and response observer

`with_observer(f)` on `OpenAiCompatProvider` or `AnthropicProvider` calls `f` with a `CallRecord` once for each call that reaches the network. The record holds the provider id, whether the call streamed, the exact JSON body sent (after extra fields are merged), and either the parsed `ChatResponse` (with usage) or the `LlmError`. The credential and auth headers are never part of it, and error text has the credential removed. The callback runs on the calling thread, so keep it quick. A call rejected before sending (a missing credential or a body collision) is not reported. Use it for an on-device payload ledger; the body contains the prompt, so store it accordingly. For the provider's response bytes, turn on raw capture (next section).

## Raw response capture

`with_raw_capture(max_body_bytes)` on `OpenAiCompatProvider` or `AnthropicProvider` keeps the provider's raw answer to a non-streaming `complete()` call. It is off by default; with it off nothing is retained and every result is unchanged. When on, each call that got an HTTP response produces one `RawExchange`, shared as a `RawHandle` (an `Arc`):

- `CallRecord::raw` hands it to the observer, for every such call, whether the call then succeeded or failed (a 429 or a malformed 200 included). It is `None` when no response arrived (a transport failure or a timeout).
- `LlmError::Truncated` and `LlmError::Malformed` come back wrapped as `LlmError::Captured { error, raw }`, and `err.raw()` returns the same handle the observer got (`Arc::ptr_eq` holds). Other errors are not wrapped; a non-2xx answer stays `LlmError::Status`, and the observer has its body.

`RawExchange` holds `status`, `request_id`, `attempts`, `body` (the retained bytes), `received_bytes`, `truncated` and `credential_removed`. The cap is yours: `body` is at most `max_body_bytes`, `truncated` says bytes were left out, and `0` keeps status and request id only. The request id is the `x-request-id` header, else `request-id` (Anthropic); any other header is not read.

What the body can contain: the model's output, which usually repeats or derives from the user's text; tool-call arguments; token usage; provider metadata; on an error status, the provider's error text. The crate keeps no copy of its own once the call returns, so the app owns that data from then on, including storing, rotating and scrubbing it. `Debug` for `RawExchange` prints sizes only, never the body.

Secrets: the request side is as before. The credential and the auth headers are never part of a `CallRecord` or a `RawExchange`. A provider can echo a credential in an error body, so the exact credential text (and the client's `redact_values`) is replaced with `[redacted]` in the retained bytes, and `credential_removed` is set when that happened; then `body` is not byte-identical to what arrived. A masked or partial key is not the credential text and is left alone.

Things to know:

- **Wrapped errors.** With capture on, match on `err.root()` (or use `err.is_truncated()` and `err.is_malformed()`), because `Truncated` and `Malformed` arrive inside `Captured`. Display, `status()` and `is_failover_trigger()` answer for the inner error, and `FailoverOn` policies (including `FailoverOn::custom`) are applied to it. A failover chain that moves on after such an error keeps only its text in `AllRoutesFailed`; the observer of the route that failed still had the handle.
- **Streams are not covered.** `stream()` reports `raw: None` and attaches nothing to its errors, with capture on or off. A stream has no single response body to keep.
- **Retries.** `rightkit-http` retries inside one call and returns the final response only, so `RawExchange` describes the final attempt (`attempts` counts them). For every attempt's own response, use `ChatRequest::without_retries()` and retry in the app; the observer then runs once per attempt.
- **Memory.** The transport still reads the whole response to parse it, bounded by the client's `max_response_bytes`, as before. Capture adds only the capped copy.

## Per-call options, errors and network

Request options on `ChatRequest` (all optional; the defaults are unchanged):

- `with_max_tokens(n)` sets the output limit, which is 1024 by default and always sent. `without_max_tokens()` leaves the field out of OpenAI-compatible bodies (`max_tokens` or `max_completion_tokens`, whichever the provider uses) so the provider applies its own limit. The Anthropic Messages API requires the field, so the Anthropic client still sends the value (1024 unless set) and ignores the omission.
- `with_temperature(t)` sends `temperature` only when set. `temperature` is also a public field.
- `with_timeout(d)` sets a total budget for one call. It applies `Request::timeout` and `Request::deadline` from `rightkit-http`, so it overrides the client's 60 s `request_timeout` and bounds retry attempts too. For a stream it caps the whole stream body. It does not raise `ClientConfig::response_timeout` (60 s), which still bounds the wait for response headers. An app whose model takes longer than 60 s to start answering must raise `response_timeout` on the client it builds.

Errors: a non-2xx provider answer is `LlmError::Status { status, message }`. `status` is the numeric code. `message` is the provider's `error.message` when the body is JSON (otherwise the body text), cut to 512 bytes, with the request's credential removed. `LlmError::status()` returns the code for this variant. The display text is `HTTP <code>: <message>`, so an app can keep its own wording by matching on `status`. Transport failures stay `LlmError::Http`, and timeouts are `LlmError::Http(HttpError::Timeout { .. })`.

Credentials: implement `credentials::CredentialSource` over the app's own store (one method, read on every call), or use `StaticCredentials` or `SecretStoreCredentials`. Keys are sent as a bearer token (OpenAI-compatible) or `x-api-key` (Anthropic) and registered as secrets in `rightkit-http`, so they are removed from its errors. `Debug` output of the providers, `StaticCredentials` and `ChatRequest` prints no key, and every error variant's text omits the key. `tests/call_options.rs` checks this with a sentinel key.

Network defaults (no change in this version):

- Redirects: `ClientConfig::max_redirects` is 0, so redirects are not followed and a 3xx answer surfaces as `LlmError::Status`. Raise it on the client to follow them. Credentials are never replayed to a different host.
- Proxy: `ClientConfig::proxy` defaults to `ProxyMode::None`, which ignores environment proxy variables. `ProxyMode::Environment` and `ProxyMode::Explicit(url)` come from `rightkit-http`. The operating system's proxy settings are read only when the client is built with `ProxyMode::System` (`rightkit-http`; on Windows it follows the static proxy settings exactly or fails closed, and elsewhere it behaves as `ProxyMode::Environment`).
- Timeouts: connect 10 s, whole request 60 s, response headers 60 s (`ClientConfig` defaults).

## Platform support

Pure Rust over `rightkit-http` (`ureq` with `rustls`). `provider::cli` has `cfg(windows)` and `cfg(unix)` branches (`src/provider/cli.rs:145` and `:362`) and inherits `rightkit-process` platform behavior.

## Tests

`tests/e2e.rs` runs both provider clients against a local mock server. It checks exact headers, body and parsed tool calls.

`tests/call_controls.rs` covers the extra body fields and their collision rule, per-call retry (including a call that makes one attempt), truncation as an error for both wires and for streams, the failover trigger, the NVIDIA preset, `GET /models` key checks, and the observer, all against the same mock server.

`tests/raw_capture.rs` covers raw capture against the same mock server: exact bytes, status and request id on success for both wires, the cap and its flag, the same handle on the observer and on `Truncated` and `Malformed`, an error status that stays `LlmError::Status` while the observer gets its body, the final attempt of a retried call, credential removal, no body in `Debug`, failover policies, and a stream that is not captured.

`tests/call_options.rs` covers the per-call options above against the same mock server: an omitted `max_tokens` field, `temperature` 0.3, a slow server that times out at a short per-call budget and succeeds within it, a 429 that yields `LlmError::Status`, a streamed non-2xx, and the sentinel-key redaction checks.

`tests/cli.rs` (feature `cli`, Unix) drives `CliProvider` against a fake CLI script: text and JSON output with usage, verbatim prompt delivery, wall and idle timeouts that kill the tree, stdout and stderr caps, non-zero exit with redaction, sign-in detection, malformed JSON, missing program, environment allow-list, working directory, and failover. No test calls a model or the network.

## Version and changes

Version: see [INDEX.md](../../INDEX.md). Changes are recorded in `CHANGELOG.md`.
