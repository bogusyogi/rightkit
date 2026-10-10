# Changelog

## 0.2.5

- New default-on feature `untrusted` gates the `untrusted` module and its only dependency,
  `rightkit-fs` (SHA-256). Default builds are unchanged; HTTP-only consumers can use
  `default-features = false, features = ["gzip"]` to drop `rightkit-fs`.
- `getrandom` 0.3 -> 0.4 (retry jitter only), matching `rightkit-logs`/`rightkit-pdf` and the
  newest major already in the lockfile.

## 0.2.4

- Internal: rename a private helper to satisfy clippy `wrong_self_convention` (no API change).

## 0.2.3

- Add `AsyncClient::stream_any`: streaming request returning any final status
  with its headers and streaming body (the caller inspects 401/404 etc. and
  bounds the body). Dropping the future or stream cancels the request.

## 0.2.2

- Add optional `async` feature: `AsyncClient` (reqwest + tokio, rustls only) with
  the same policy layer as `Client` (shared `ClientConfig`, `Request`, `Response`,
  `HttpError`): per-phase timeouts, retry/backoff with `Retry-After`, retry-safety
  for non-idempotent methods, redirect limit/credential rules, user agent,
  environment proxies, bounded bodies, URL-free error messages.
- Add `AsyncStreamResponse`/`BodyStream` (typed-error chunk stream with stream
  and body deadlines) and `SseStream` (`sse()`, `sse_with_limits()`).
- Add `SseDecoder::with_limits(max_line, max_event)`; `SseDecoder::new` is
  unchanged (events unbounded). Default features and the blocking API are unchanged.

## 0.2.0

- Breaking: `ClientConfig::connect_timeout` & `response_timeout` now use
  `Option<Duration>`. Wrap explicit durations in `Some`; `None` removes per-phase
  deadlines. Default connect/header deadlines remain 10s/60s.
- Add `ClientConfig::recv_body_timeout`, optional total body-phase deadline
  independent of connect, header & global deadlines. Default: `None`.
- Add `ClientConfig::max_redirects_will_error`, default `true` like ureq.
  Exhausted nonzero redirect limits map to `HttpError::Transport` with
  `connect_phase: false`, retaining idempotent retry behavior. `false` returns
  final 3xx; zero redirect limit retains no-follow behavior.
- Enable `gzip` feature by default, forwarding `ureq/gzip` for automatic
  negotiation & transparent decoding. Use `default-features = false` to opt out.
- Preserve typed `HttpError::Timeout` for ureq timeouts wrapped by body readers.
