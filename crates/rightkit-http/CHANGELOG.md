# Changelog

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
