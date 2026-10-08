# rightkit-http

Shared blocking HTTP client for Right Suite applications. It provides timeouts, bounded bodies, retry/backoff that honors `Retry-After` (never retrying unsafe methods), streaming responses, WHATWG Server-Sent Events decoding, redacted secrets, and tamper-evident wrapping of untrusted web text before it reaches a model.

The `testing` feature enables an in-process mock server that records exact requests.

## 0.2.0 migration

`connect_timeout` & `response_timeout` now take `Option<Duration>`: wrap existing
durations in `Some(...)`, or use `None` to disable per-phase deadlines. Defaults
remain `Some(10s)` for connect & `Some(60s)` for response headers. Buffered calls
still use `request_timeout` (60s by default); streams use `stream_timeout`
(`None` by default). Disabling phase deadlines does not disable global deadlines.

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

`recv_body_timeout` defaults to `None`. When set, it limits total body-receive
time after headers arrive; progress does not reset its budget. Buffered body
timeouts return `HttpError::Timeout`; stream readers retain `std::io::Error`
with underlying ureq timeout. Body reads are never retried.

`max_redirects_will_error` defaults to `true`, matching ureq. Exhausting a nonzero
redirect limit returns `HttpError::Transport { connect_phase: false, .. }`, eligible
for idempotent retries under existing retry policy. Set it to `false` to receive
final 3xx through `send_any`. `max_redirects` still defaults to 0 (no following);
with 0, 3xx is always returned regardless of error policy.

`gzip` is enabled by default & forwards `ureq/gzip`: automatic
`Accept-Encoding: gzip` negotiation & transparent decoding for buffered responses
& stream readers. Default-on preserves ureq's default behavior for migrating
callers, without adding direct dependencies. Opt out with:

```toml
rightkit-http = { version = "0.2.0", default-features = false }
```

Publish through the RightKit release workflow.
