# rightkit-http

Shared blocking HTTP client for Right Suite applications. It provides timeouts, bounded bodies, retry and backoff that honours `Retry-After` (never retrying unsafe methods), streaming responses, WHATWG Server-Sent Events decoding, redacted secrets, and tamper-evident wrapping of untrusted web text before it reaches a model.

Catalog capability: `http-client`. Published to crates.io at 0.2.5 (registry record); the source is 0.3.0, which [INDEX.md](../../INDEX.md) flags as a mismatch.

## What it owns

- `Client`, `ClientConfig`, `Request`, `Response`, `StreamResponse`: timeouts, bounded bodies, redirect policy, streaming reads, and `get_json`.
- `RetryPolicy`: retry and backoff. Retries respect `Retry-After`. Non-idempotent methods are not retried after transport failures.
- `Multipart`: `multipart/form-data` bodies with exact framing; `Request::multipart` sets the body and `Content-Type`.
- `netpolicy` / `AddressPolicy`: the outbound address policy (SSRF guard) that every request and redirect hop passes; `ClientConfig::first_party` pins a host set.
- `CancelToken`: cooperative cancellation for buffered calls and streams (`HttpError::Cancelled`).
- `Request::deadline`, `Request::secret_header` and `ClientConfig::redact_values`: a per-call budget, and secrets kept out of error text.
- `AsyncClient` (feature `async`): `send`, `send_any`, `stream`, `get_json` with identical timeout, retry/backoff, redirect, user-agent, proxy (off by default; `ProxyMode` opts in), address policy and size-limit behaviour. `stream` returns a `BodyStream` of `Bytes` chunks or an `SseStream` (`.sse()`, `.sse_with_limits(max_line, max_event)`), bounded to 1 MiB per line and 4 MiB per event by default.
- `sse`: `SseDecoder` (`new`, `with_limits`), `SseReader`, `SseEvent`. WHATWG-compliant Server-Sent Events.
- `untrusted`: `wrap_untrusted_web_text` for external text, with boundary markers and injection-signal reporting; `wrap_untrusted_web_text_coderight` is the opt-in variant that emits CodeRight's `<coderight-untrusted-context>` envelope (differences: `content_hash` is the body SHA-256, closing-tag text in the body is escaped).
- `secret`: a redacted `Secret`, and the `SecretStore` trait. `MemorySecretStore` is provided for tests. `src/secret.rs` says `rightkit-credentials` supplies the production implementation, but `rightkit-credentials` has no `SecretStore` implementation and does not depend on this crate.
- `testing` (feature): an in-process mock server that records exact requests.

## Not in this release

File downloads (`download_to_path`) are not part of 0.3.0 and are planned for 0.3.1.

## When to use it

- Use it for blocking HTTP calls from Right Suite apps where timeouts, retries and bounded bodies must be consistent.
- Use it for SSE streams and for wrapping web text before model input.

The default client is blocking (`ureq` with `rustls`). For async code enable the `async` feature: `AsyncClient` on `reqwest` with `rustls`, run inside a tokio runtime.

## Cargo features

No feature is on by default (0.3.0). Name `gzip` and `untrusted` to get what 0.2.x built.

| Feature | Default | Pulls in | Enables |
|---|---|---|---|
| `gzip` | no | `ureq/gzip` | Automatic `Accept-Encoding: gzip` and transparent decoding for buffered responses and stream readers. Matches ureq's default. |
| `untrusted` | no | `rightkit-fs` (SHA-256 digest) | `wrap_untrusted_web_text` and `WrapOptions` (`Cargo.toml:15-18`, `src/lib.rs:30-31`). |
| `async` | no | `reqwest` (rustls, no native-tls), `tokio` (time, net), `futures-util`, `bytes`, `rustls`, `rustls-platform-verifier`, `webpki-roots` | `AsyncClient`, `AsyncStreamResponse`, `BodyStream`, `SseStream`. Same `ClientConfig`, `Request`, `Response`, `HttpError` and policy as the blocking client. With `gzip`, also enables `reqwest/gzip`. |
| `testing` | no | none | The `testing` module: `MockServer`, `MockResponse`, `MockBody`, `RecordedRequest`, `loopback_policy`, `loopback_config`. |

Default features are empty since 0.3.0. Name what you use, e.g. `features = ["gzip", "untrusted"]`; with no features:

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

## Retries and unsafe methods

- Status-code retries (429, 5xx and the other `retry_statuses`) apply to idempotent methods only. **Since 0.3.0 POST and PATCH are not replayed by default**, so a single-use OAuth code exchange is never sent twice; `Request::retry_unsafe_statuses(true)` opts one request in. Connect-phase transport failures are still retried for every method, because nothing reached the server.
- `.retry(RetryPolicy::none())` already disables every retry, including unsafe-status retries on POST and PATCH. `RetryPolicy::none()` has `max_attempts = 1`, so one request is made and no backoff sleep runs. Sub-second probes are therefore safe to call with it. The override and the policy are independent: `retry_unsafe_statuses(true)` with `RetryPolicy::none()` still makes exactly one request.
- `Request::deadline(total)` bounds all attempts and the retry sleeps between them. When the next sleep would pass the deadline, the last outcome is returned and no sleep runs. A retry sleep is also clamped to what the deadline has left after the failed response's body was drained, in both clients, so a stalled error body cannot push the call past its deadline by a full backoff. `Request::timeout` and `ClientConfig::request_timeout` stay per-attempt, so a 300 to 600 s call budget with a 60 s attempt timeout is `deadline(300 s)` plus the default `timeout`.

## Cancellation

`Request::cancel(CancelToken)` cancels the call. It is checked before each attempt, during retry sleeps (woken every 50 ms), and before each body read. A cancelled buffered call or retry returns `HttpError::Cancelled`. A stream returns an `io::Error` for which `rightkit_http::is_cancelled_io` is true. The blocking client cannot interrupt a socket wait for response headers or inside a body read: the first is bounded by `response_timeout`, the second only by `recv_body_timeout` or the stream or request timeout, when one is set. The async `BodyStream` (and `SseStream` on top of it) is different: when its request carries a `CancelToken` it wakes itself every 25 ms while pending, so `cancel()` ends a stream whose server has sent headers and then gone quiet, with no timeout configured, in at most about 25 ms (`HttpError::Cancelled`, then the stream ends).

## Secrets in errors

- No error, `RetryEvent` or `Debug` output echoes a URL's query string or userinfo. URLs render as `scheme://host[:port]/path?[redacted]`, and `scheme://...` text inside lower-layer messages is rewritten the same way. A `Status` body is the server's text and is only scrubbed of secret values.
- `Request::secret_header(name, value)` sends the header and records its value. `Request::bearer` does the same for the token. Recorded values never appear in `HttpError` text, in `RetryEvent` URLs or reasons, in stream I/O errors, or in `Request`'s `Debug` output. `Request`'s `Debug` prints the body length rather than its bytes.
- `ClientConfig::redact_values(vec![...])` scrubs configured strings in the same places. Each replaced value appears as `[redacted]`.

## Multipart

`Multipart::new()` (random boundary) or `Multipart::with_boundary(b)` (pinned). Add parts with `text`, `file_bytes` and `file_path`. Pass the form to `Request::multipart`, or call `into_body` and `content_type` yourself. The boundary must be 1 to 70 characters from ASCII letters, digits, `-`, `_`, `.` and `+`. `into_body` fails if the boundary occurs inside a part.

## Timeouts

- `connect_timeout` and `response_timeout` are `Option<Duration>`. The defaults are `Some(10 s)` for connect and `Some(60 s)` for response headers. Use `None` to disable a per-phase deadline. Disabling phase deadlines does not disable global deadlines.
- Buffered calls use `request_timeout` (60 s by default). Streams use `Request::timeout` when set, otherwise `stream_timeout` (`None` by default).
- `recv_body_timeout` defaults to `None`. When set, it limits total body-receive time after headers arrive. Progress does not reset it.
- Buffered body timeouts return `HttpError::Timeout`. Stream readers keep the underlying `std::io::Error`. Body reads are never retried.
- Callers that passed bare durations to `connect_timeout` or `response_timeout` must wrap them in `Some(...)`. The change is recorded in CHANGELOG.md.

## Migrating from a plain ureq agent

`ClientConfig::legacy_ureq_parity(user_agent)` sets the five options a plain `ureq` agent uses: `ProxyMode::Environment`, `TrustRoots::Bundled`, `max_redirects` 10, `RetryPolicy::none()` and the given user agent. Every other field keeps its default. It does not enable private-address resolution; opt into that separately with `AddressPolicy::allow_private_resolution`.

## Error classes

`HttpError::class()` returns an `ErrorClass`, one coarse class per error, for mapping to an app's own error type or retry set.

| `HttpError` | `ErrorClass` |
|---|---|
| `Status` 401, 403, 407 | `Auth` |
| `Status` 408 | `Timeout` (HTTP 408 Request Timeout; was `Client` before Unreleased, see CHANGELOG) |
| `Status` 429 | `RateLimit { retry_after }` (the parsed `Retry-After`, or `None`) |
| `Status` 500 to 599 | `Server` |
| `Status`, any other (other 4xx, an unfollowed 3xx) | `Client` |
| `Timeout` | `Timeout` |
| `Transport` | `Transport` |
| `Cancelled` | `Cancelled` |
| `AddressDenied` | `AddressDenied` |
| `InvalidRequest`, `Json`, `Stream`, `BodyTooLarge` | `Invalid` |

## Address policy

Every request, and every redirect hop, passes an outbound address policy before a socket opens. `ClientConfig::address_policy` defaults to public-web: the host is resolved once by this crate, every resolved address is classified, and the connection goes only to the addresses that were checked (no second lookup). One denied address denies the host. The client returns `HttpError::AddressDenied { host, addr, class, reason }`, which is never retried.

Denied by default: loopback, RFC 1918, link-local (including `169.254.169.254`), CGNAT `100.64/10`, multicast, reserved, unspecified and broadcast addresses, IPv6 `fc00::/7` and `fe80::/10`, `localhost` and `*.localhost`, cloud metadata host names, and names that resolve to no address. IPv4-mapped, IPv4-compatible, NAT64 `64:ff9b::/96` and 6to4 `2002::/16` addresses take the class of the embedded IPv4 address. The classification mirrors `rightkit-browser`'s `policy.rs`; `netpolicy::classify_ip` is the shared entry.

```rust
use rightkit_http::{AddressPolicy, Client, ClientConfig, IpNet};

let policy = AddressPolicy::default()
    // Extra denials, with your own error text. Both also see the IPv4 inside an IPv6 form.
    .deny_ranges(vec!["203.0.113.0/24".parse::<IpNet>().unwrap()])
    .deny_if(|ip| ip.is_ipv6().then(|| "IPv6 is not allowed here".to_string()))
    // An explicit escape for named hosts only: a local model server.
    .allow_if(vec!["127.0.0.1".into()], |ip| ip.is_loopback());
let client = Client::new(ClientConfig::default().address_policy(policy));
```

- `deny_ranges` and `deny_if` always win over `allow_if`. Link-local and metadata destinations cannot be allowed by any hook.
- `allow_if(hosts, pred)` applies only to the exactly listed hosts (ASCII case-insensitive, one trailing dot ignored) and only to addresses for which `pred` is true.
- `Client::first_party(hosts)` / `ClientConfig::first_party(hosts)` pin the allowed host set: exact host match, HTTPS only, on every redirect hop. The resolved addresses of those hosts are classified with the public-web classes too, so a listed name that resolves to a private address is refused. An on-premises server opts in per host with `AddressPolicy::allow_private_resolution(host)`; link-local and metadata destinations, `deny_ranges` and `deny_if` still apply. Use it for license checks and model downloads.
- `AddressPolicy::with_resolver` replaces the system resolver (offline tests, pinned DNS).
- Proxies: see [Proxies](#proxies). The default is no proxy, and environment variables are ignored. Through a proxy only literals and names can be checked.
- Tests and consumers that talk to `testing::MockServer` use `testing::loopback_config()` or `testing::loopback_policy()`, which list the loopback hosts through `allow_if`.

## Proxies

`ClientConfig::proxy` takes a `ProxyMode`: `None` (default), `Environment` (`ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY` in that order, either case, plus `NO_PROXY`) or `Explicit("http://10.0.0.5:3128")`. Only `http` and `https` proxies are accepted. `ProxyMode::None` stays the default, and environment variables are ignored unless `Environment` is chosen. `System` uses the operating system's proxy for the current user: on Windows the static proxy and bypass list of the system proxy settings with nothing configured, and on other systems, it behaves as `Environment`.

Limits of `System` in this version: on a default Windows install, where "Automatically detect settings" is on and nothing else is configured, `System` does not fail: it uses the environment proxy variables (`ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY`) when set and a direct connection otherwise. `Client::proxy_mode_used()` / `AsyncClient::proxy_mode_used()` reports which (`ProxyModeUsed::SystemFallbackEnvironment` or `SystemFallbackDirect`, with the reason `automatic detection only; no static proxy or PAC configured`; `System`, `Environment` and `Direct` otherwise). With a static proxy the detection flag is ignored and the static proxy is followed. There is no other fallback inside the client: it follows the Windows settings exactly or not at all. When they hold a proxy this client cannot follow exactly, `Client::try_new` returns `HttpError::SystemProxyUnsupported { element }` (and every request of a client built with `Client::new` fails with it), `element` names the first such thing, and nothing is sent. No bypass entry is ever dropped while the proxy stays in use, and no scheme is sent to the other scheme's proxy. What is refused:

- **Different routes for HTTP and HTTPS.** The static proxy string is read per scheme as Windows reads it (`http=`, `https=`, a bare `host:port` for the rest). The same proxy for both is used; two proxies, or a proxy for one scheme only, are refused.
- **A bypass entry that is not a plain host.** A host name, an address and `*.example.com` are followed. `<local>` (names without a dot, set by "Don't use the proxy server for local addresses"), an entry limited to one scheme (`https://host`) and a wildcard elsewhere (`10.*`, `*`) are refused.
- **A proxy auto-config script.** It chooses a proxy per URL.

Many Windows machines also have `<local>` set next to a static proxy. An app that offers `System` catches `SystemProxyUnsupported` and chooses `ProxyMode::None` or an explicit proxy itself.
**Proxy mode is weaker than a direct connection by construction.** The proxy resolves the destination name itself, so this crate cannot see or pin the address a proxied connection reaches. What it enforces is name- and literal-based:

- A proxy on a private address (`10.x`, `192.168.x`) is accepted: the address policy governs the destination, and the endpoint is exempt from the class check only because the caller or the environment configured it, only for the real proxy connection (its host, its port, a call that goes through it). `Client::proxy_diagnostic()` reports the endpoint (never credentials). **DNS-based private destinations cannot be verified through a proxy**: only literals and the name list are checked.
- The proxy endpoint is trusted: it is user or administrator configuration and is often on a private address. It gets no address-class denial. Proxy credentials in the URL never appear in `Debug` output or errors.
- IP-literal destinations (and `localhost`) are classified like any address: `169.254.169.254`, RFC 1918, loopback and the rest are refused.
- Hostnames are refused when they are `localhost`, `*.localhost`, `*.local`, `*.internal` or single-label, unless an `allow_if` rule lists the host. Cloud metadata names and the first-party host pin apply too. `deny_ranges` and `deny_if` apply to literals only, because a hostname's address is unknown.
- Only literals are classified, and only `localhost`, `*.localhost`, `*.local`, `*.internal` and single-label names are refused. A public-looking hostname that resolves to a private address at the proxy is not caught, and `deny_ranges` and `deny_if` never see it. Use a direct connection when that matters.
- Every redirect hop is vetted the same way.
- Hosts that match `NO_PROXY` connect directly and get the full direct-connection check (resolve once, classify, connect to the checked address).
- A first-party client uses the proxy too, unless `ClientConfig::proxy_bypass_first_party(true)` sends it direct.
- A proxy is trusted: it sees destinations and can forward anywhere.

## Headers only

`Client::send_headers(&req)` returns `ResponseHead { status, headers, final_url, hops, attempts }` for any status without reading the body (3xx when redirects are off, 4xx, and 5xx after retries). The unread body is dropped, which closes that connection. Redirects, the address policy on every hop, retries and cancellation behave as for `send_any`. `Request::headers_only()` makes `send` and `send_any` skip the body in the same way. `Client::stream_any` returns any status with its headers and a streaming body; `Client::stream` still turns non-2xx into `HttpError::Status`.

## Error phases

`HttpError::Timeout { message, phase }` and `HttpError::Transport { message, connect_phase, phase }` carry a `Phase`, also available as `HttpError::phase()`:

| `Phase` | When |
|---|---|
| `Connect` | Name lookup, TCP connect, TLS handshake. `connect_phase` is true exactly here. |
| `Headers` | Sending the request and waiting for the response headers, including the request-wide budget. |
| `Body` | Reading the response body, including streams. |
| `Redirect` | Any failure on a redirect hop after the first request, and an exhausted redirect limit. |

## TLS trust and `https_only`

- `ClientConfig::trust` is `TrustRoots::Platform` (default) or `TrustRoots::Bundled`. **From 0.3.0 the default is the operating system's trust store**, so roots an enterprise or an inspection proxy installed on the machine are honoured, as curl and browsers honour them. Before 0.3.0 only Mozilla's bundled roots were trusted. Both clients name their verifier explicitly, so no other crate's Cargo features can change it: the blocking client sets ureq's TLS provider and `RootCerts` (`PlatformVerifier` or `WebPki`); the async client installs a complete rustls `ClientConfig` with `use_preconfigured_tls` in **both** branches (`rustls-platform-verifier` for `Platform`, the `webpki-roots` Mozilla set and nothing else for `Bundled`). reqwest stays 0.12.28 with feature `rustls-tls`; no second reqwest major is needed. `Client::verifier_kind()` and `AsyncClient::verifier_kind()` return `"platform"` or `"bundled"`; the async value reports the configuration that was installed. `Bundled` ignores the machine's roots. There is no option that disables verification.
- The async client advertises **HTTP/1.1 only** in ALPN. reqwest's `http2` feature is not enabled here, so a server that selected `h2` would otherwise panic hyper-util; the client cannot negotiate it, whatever other crates enable.
- `ClientConfig::https_only` (default `false`) refuses a non-`https` initial URL and any redirect hop to a non-`https` URL with `HttpError::HttpsRequired { host, scheme, redirect }` (class `Invalid`), before anything is sent to that URL. It applies to both clients. It is separate from the address policy's HTTPS-to-HTTP redirect rule, which stays on; when a redirect to `http` trips both, `HttpsRequired` is the error reported.

## Defaults worth knowing

- **Routing is decided once per hop.** Whether a request goes through the proxy is decided by one matcher (`ureq`'s `NO_PROXY` rules, wildcards included) that also drives address vetting, and the async client keeps a direct and a proxied reqwest client and picks one per hop, with reqwest's own environment and `NO_PROXY` handling switched off. A host the policy treats as direct is always vetted at the resolver; the same holds after redirects.
- **`Debug` of `Response` and `ResponseHead` hides every recorded secret** (`redact_values`, `secret_header` values, URL secrets) as well as credential-named headers.

- **Redirects: none by default** (`max_redirects = 0`). A 3xx comes back as the response. Set `ClientConfig::max_redirects` (and `max_redirects_will_error`) to follow them; each hop passes the address policy and strips credentials on a new origin.
- **Retries: idempotent methods only.** POST and PATCH are never replayed after a 429/5xx unless the request sets `retry_unsafe_statuses(true)`. A connect-phase failure (nothing reached the server) is still retried for every method.
- **Streams and quiet SSE.** `ClientConfig::response_timeout` (default 60 s) is the time to the response *headers* (first byte), not an idle timeout. Once headers arrive, a stream body has no idle cut unless `recv_body_timeout` (a total body budget, default none) or `stream_timeout` / `Request::timeout` (total lifetime, default none) is set. A server that sends headers and then stays quiet is therefore not cut; one that takes over 60 s to send headers is, so raise or clear `response_timeout` (`None`) for such servers. There is no per-read idle timeout; use `Request::cancel` or `deadline` to bound a stalled stream.
- **Text decoding is lossy UTF-8.** `Response::text()` and error bodies decode as UTF-8 and replace invalid bytes; the `Content-Type` charset is not honoured. For another charset, take `Response::body` bytes and decode them yourself.

## Hard blocks and spellings

- **Cloud metadata cannot be allowed by any hook.** Link-local (which holds `169.254.169.254`), `fd00:ec2::/32` (AWS IPv6 metadata and DNS), `100.100.100.200` (Alibaba), `168.63.129.16` (Azure) and `192.0.0.192`, plus IPv6 forms that embed them, are refused whatever `allow_if`, `allow_private_resolution` or the proxy mode say. Cloud metadata host names are refused too.
- **Legacy IPv4 spellings** such as `127.1`, `0177.0.0.1`, `0x7f.0.0.1` and `2130706433` are read the way `inet_aton` reads them before any check, in both clients, through proxies and on redirects.
- IPv6 forms that embed IPv4 take the embedded class: mapped, compatible, NAT64 `64:ff9b::/96`, 6to4, and the local-use NAT64 space `64:ff9b:1::/48` in each RFC 6052 layout.
- **Non-canonical IPv4 spellings are refused** by default (`AddressPolicy::allow_legacy_ipv4()` opts out): parsers disagree about `0177.0.0.1`, so normalising it would be an SSRF bypass. `parse_host_ip(host)` returns `Option<ParsedHostIp>` (the address, `kind` and `is_canonical`) for callers guarding other transports; `parse_legacy_ipv4` reads the `inet_aton` forms (one to four parts, decimal/octal/hex, the last part fills the remaining bytes).
- **HTTPS to HTTP redirects are refused** by the address policy. `AddressPolicy::allow_insecure_redirects()` opts in.
- Logging: the vendored `ureq` 3.4.0 documentation describes its `TRACE` level as wire-level debug output that is "NOT REDACTED" (`ureq` 3.4.0, `src/lib.rs:449`). This crate cannot intercept it. Do not enable TRACE for a process that carries secrets in URLs.

## Vetting for other transports

`AddressPolicy::resolve_vetted(host, port) -> Result<Vec<SocketAddr>, HttpError>` resolves a host once and applies exactly the rules the clients use: the address classes, `deny_ranges`, `deny_if`, `allow_if`, the first-party pin and its private-resolution opt-outs, metadata names, and the denial of names with no address. It returns the same `HttpError::AddressDenied`. A WebSocket, raw TCP or gRPC transport calls it and **must connect to the returned addresses, not look the name up again**. A transport that cannot pin its connection leaves a DNS-rebinding window between the check and its own lookup. The host is treated as reached over HTTPS; requiring TLS under a first-party policy is the caller's job.

## Redirects

- `max_redirects` defaults to 0, so redirects are not followed. With 0, a 3xx response is always returned, whatever the error policy.
- The crate follows redirects itself, one hop at a time, so the address policy and header stripping run on every hop. Rules match ureq: 301/302/303 and other 3xx (not 304) become `GET` unless the method is `GET` or `HEAD`; 307/308 are not followed for `POST`, `PUT`, `PATCH` and `DELETE` (the 3xx comes back); a 3xx without `Location` comes back.
- On a cross-origin hop (scheme, host or port differs) `Authorization`, `Cookie`, headers named like credentials (`authorization`, `authentication`, `cookie`, `token`, `secret`, `api-key`, `apikey`, `password`) and any header holding a recorded secret value are dropped. A same-origin hop keeps them.
- `max_redirects_will_error` defaults to `true`, matching ureq. When a nonzero limit is exhausted, the call returns `HttpError::Transport { connect_phase: false, .. }`. That error is eligible for idempotent retries under the retry policy.
- Set it to `false` to receive the final 3xx through `send_any`.

## Platform support

Pure Rust over `ureq` with `rustls`, with platform-specific code in `src/system_proxy.rs` (Windows system proxy settings). Certificate and TLS behaviour follows `rustls`; the default roots are the operating system's (`TrustRoots::Platform`).

## Tests

`tests/e2e.rs` and, with `--features async`, `tests/async_e2e.rs`: a real local TCP server. Assertions use exact requests seen on the wire and exact bytes received. `tests/second_read.rs` (and `tests/second_read_async.rs` with `--features async`) pin the retry-sleep clamp. `tests/request_policy.rs` covers per-request overrides, cancellation, redaction and the deadline. `tests/address_policy.rs` (and `tests/async_policy.rs` with `--features async`) cover the address policy: resolver denial per class, embedded IPv4 forms, redirects, cross-origin stripping, first-party mode, `deny_ranges`, `deny_if` and URL redaction. `tests/send_headers.rs`, `tests/proxy.rs` and `tests/phases.rs` cover headers-only calls, proxies and error phases. `tests/classify_parity.rs` cross-checks the classification against `rightkit-browser`'s source.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Version history: [CHANGELOG.md](CHANGELOG.md).
