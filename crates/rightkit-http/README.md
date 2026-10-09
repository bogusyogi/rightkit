# rightkit-http

Shared blocking HTTP client for Right Suite applications. It provides timeouts, bounded bodies, retry and backoff that honours `Retry-After` (never retrying unsafe methods), streaming responses, WHATWG Server-Sent Events decoding, redacted secrets, and tamper-evident wrapping of untrusted web text before it reaches a model.

Catalog capability: `http-client`. Published to crates.io (see [INDEX.md](../../INDEX.md)).

## What it owns

- `Client`, `ClientConfig`, `Request`, `Response`, `StreamResponse`: timeouts, bounded bodies, redirect policy, streaming reads, and `get_json`.
- `RetryPolicy`: retry and backoff. Retries respect `Retry-After`. Non-idempotent methods are not retried.
- `AsyncClient` (feature `async`): `send`, `send_any`, `stream`, `get_json` with identical timeout, retry/backoff, redirect, user-agent, proxy (environment variables) and size-limit behaviour. `stream` returns a `BodyStream` of `Bytes` chunks or an `SseStream` (`.sse()`, `.sse_with_limits(max_line, max_event)`), bounded to 1 MiB per line and 4 MiB per event by default.
- `sse`: `SseDecoder` (`new`, `with_limits`), `SseReader`, `SseEvent`. WHATWG-compliant Server-Sent Events.
- `untrusted`: `wrap_untrusted_web_text` for external text, with boundary markers and injection-signal reporting.
- `secret`: a redacted `Secret`, and the `SecretStore` trait that `rightkit-secrets` implements. `MemorySecretStore` is provided for tests.
- `testing` (feature): an in-process mock server that records exact requests.

## When to use it

- Use it for blocking HTTP calls from Right Suite apps where timeouts, retries and bounded bodies must be consistent.
- Use it for SSE streams and for wrapping web text before model input.

The default client is blocking (`ureq` with `rustls`). For async code enable the `async` feature: `AsyncClient` on `reqwest` with `rustls`, run inside a tokio runtime.

## Cargo features

| Feature | Default | Pulls in | Enables |
|---|---|---|---|
| `gzip` | yes | `ureq/gzip` | Automatic `Accept-Encoding: gzip` and transparent decoding for buffered responses and stream readers. Matches ureq's default. |
| `async` | no | `reqwest` (rustls, no native-tls), `tokio` (time), `futures-util`, `bytes` | `AsyncClient`, `AsyncStreamResponse`, `BodyStream`, `SseStream`. Same `ClientConfig`, `Request`, `Response`, `HttpError` and policy as the blocking client. With `gzip`, also enables `reqwest/gzip`. |
| `testing` | no | none | The `testing` module: `MockServer`, `MockResponse`, `MockBody`, `RecordedRequest`. |

To opt out of gzip:

```toml
rightkit-http = { version = "<exact published version from INDEX.md>", default-features = false }
```

## Usage

Client configuration, from the crate README. Every field below exists in `ClientConfig`:

```rust
use std::time::Duration;
use rightkit_http::{Client, ClientConfig, RetryPolicy};

let client = Client::new(ClientConfig {
    connect_timeout: Some(Duration::from_millis(500)),
    response_timeout: Some(Duration::from_millis(500)),
    recv_body_timeout: Some(Duration::from_secs(3)),
    request_timeout: Duration::from_secs(5),
    max_redirects: 10,
    max_redirects_will_error: true,
    retry: RetryPolicy::none(),
    ..ClientConfig::default()
});
```

Request shape, from `tests/e2e.rs`: `Request::post(url).bearer(token).json(&value)?` then `client.send(&req)?`.

## Timeouts

- `connect_timeout` and `response_timeout` are `Option<Duration>`. The defaults are `Some(10 s)` for connect and `Some(60 s)` for response headers. Use `None` to disable a per-phase deadline. Disabling phase deadlines does not disable global deadlines.
- Buffered calls use `request_timeout` (60 s by default). Streams use `stream_timeout` (`None` by default).
- `recv_body_timeout` defaults to `None`. When set, it limits total body-receive time after headers arrive. Progress does not reset it.
- Buffered body timeouts return `HttpError::Timeout`. Stream readers keep the underlying `std::io::Error`. Body reads are never retried.
- Callers that passed bare durations to `connect_timeout` or `response_timeout` must wrap them in `Some(...)`. The change is recorded in CHANGELOG.md.

## Redirects

- `max_redirects` defaults to 0, so redirects are not followed. With 0, a 3xx response is always returned, whatever the error policy.
- `max_redirects_will_error` defaults to `true`, matching ureq. When a nonzero limit is exhausted, the call returns `HttpError::Transport { connect_phase: false, .. }`. That error is eligible for idempotent retries under the retry policy.
- Set it to `false` to receive the final 3xx through `send_any`.

## Platform support

Pure Rust over `ureq` with `rustls`. No platform-specific code in the crate. Certificate and TLS behaviour follows `rustls`.

## Tests

`tests/e2e.rs` and, with `--features async`, `tests/async_e2e.rs`: a real local TCP server. Assertions use exact requests seen on the wire and exact bytes received.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Version history: [CHANGELOG.md](CHANGELOG.md).
