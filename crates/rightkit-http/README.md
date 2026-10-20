# rightkit-http

Shared blocking HTTP client for Right Suite applications. It provides timeouts, bounded bodies, retry and backoff that honours `Retry-After` (never retrying unsafe methods), streaming responses, WHATWG Server-Sent Events decoding, redacted secrets, and tamper-evident wrapping of untrusted web text before it reaches a model.

Catalog capability: `http-client`. Published to crates.io: 0.3.7 (see [CHANGELOG.md](CHANGELOG.md)); the catalogue record in [INDEX.md](../../INDEX.md) can lag the registry.

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
- `secret`: a redacted `Secret`, and the `SecretStore` trait (an interface only: `get`, `set`, `delete` keyed by service and account). The one implementation in this repository is `MemorySecretStore`, an in-memory map for tests and ephemeral sessions. There is no production (Keychain / Credential Manager) `SecretStore` implementation: `rightkit-credentials` has its own `Store` and `Secret` types, does not implement this trait and does not depend on this crate. An app that wants OS-backed storage behind this trait implements it over `rightkit-credentials` itself; consumers such as `rightkit-llm`'s `SecretStoreCredentials` accept any `SecretStore`.
- `ApprovedOrigins`, `Request::approved_redirect_origins`, `RedirectRefusal`, `HttpError::RedirectRefused` and `check_redirect_target`: the redirect origin allowlist, its typed error and a string-level guard for other transports (see [Redirects](#redirects)).
- `testing` (feature): an in-process mock server that records exact requests.

## Not in this crate

File downloads. `Client::download_to_path` was removed before publication and is in no published version (see the 0.3.0 entry in [CHANGELOG.md](CHANGELOG.md)). Copy a `StreamResponse` into a writer you own with `StreamResponse::copy_to(writer, max_bytes, progress)`.

## When to use it

- Use it for blocking HTTP calls from Right Suite apps where timeouts, retries and bounded bodies must be consistent.
- Use it for SSE streams and for wrapping web text before model input.

The default client is blocking (`ureq` with `rustls`). For async code enable the `async` feature: `AsyncClient` on `reqwest` with `rustls`, run inside a tokio runtime.

## Cargo features

No feature is on by default (since 0.3.0). Releases before 0.3.0 enabled `gzip` and `untrusted` by default; name them to get that build.

| Feature | Default | Pulls in | Enables |
|---|---|---|---|
| `gzip` | no | `ureq/gzip` | Automatic `Accept-Encoding: gzip` and transparent decoding for buffered responses and stream readers. Matches ureq's default. |
| `untrusted` | no | `rightkit-fs` (SHA-256 digest) | `wrap_untrusted_web_text` and `WrapOptions` (`Cargo.toml:15-18`, `src/lib.rs:30-31`). |
| `async` | no | `reqwest` 0.13 (`rustls-no-provider` and `stream`; no native-tls), `tokio` (time, net), `futures-util`, `bytes`, `rustls`, `rustls-platform-verifier`, `webpki-roots` | `AsyncClient`, `AsyncStreamResponse`, `BodyStream`, `SseStream`. Same `ClientConfig`, `Request`, `Response`, `HttpError` and policy as the blocking client. With `gzip`, also enables `reqwest/gzip`. |
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

`rightkit_http::validate_public_https_url` checks that a URL is plain public `https` before a request (rightkit#374). It reads the string only; the address guard still vets resolved hosts.


- No error, `RetryEvent` or `Debug` output echoes a URL's query string or userinfo. URLs render as `scheme://host[:port]/path?[redacted]`, and `scheme://...` text inside lower-layer messages is rewritten the same way. A `Status` body is the server's text and is only scrubbed of secret values.
- `Request::secret_header(name, value)` sends the header and records its value. `Request::bearer` does the same for the token. Recorded values never appear in `HttpError` text, in `RetryEvent` URLs or reasons, in stream I/O errors, or in `Request`'s `Debug` output. `Request`'s `Debug` prints the body length rather than its bytes.
- `ClientConfig::redact_values(vec![...])` scrubs configured strings in the same places. Each replaced value appears as `[redacted]`.

## Multipart

`Multipart::new()` (random boundary) or `Multipart::with_boundary(b)` (pinned). Add parts with `text`, `file_bytes` and `file_path`. Pass the form to `Request::multipart`, or call `into_body` and `content_type` yourself. The boundary must be 1 to 70 characters from ASCII letters, digits, `-`, `_`, `.` and `+`. `into_body` fails if the boundary occurs inside a part.

`MultipartRelated` is the `multipart/related` form (RFC 2387) for a metadata part followed by a media part, such as a Google-style upload: `MultipartRelated::new().part("application/json; charset=UTF-8", meta).part("message/rfc822", raw)`, then `Request::multipart_related(form)`. Each part carries only its own `Content-Type`.

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
| `InvalidRequest`, `Json`, `Stream`, `BodyTooLarge`, `HttpsRequired`, `RedirectRefused` | `Invalid` |

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

`ClientConfig::proxy` takes a `ProxyMode`: `None` (default), `Environment` (`ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY` in that order, either case, plus `NO_PROXY`) or `Explicit("http://10.0.0.5:3128")`. Only `http` and `https` proxies are supported; there is no SOCKS support in either client (this crate enables no SOCKS feature of `ureq` or `reqwest` and refuses the scheme itself). `Explicit` with any other scheme (`socks5://`, `socks4://`, `ftp://`) is refused as `HttpError::InvalidRequest` ("invalid proxy URL") from `Client::try_new` / `AsyncClient::new`, with nothing sent. `Environment` skips a proxy variable that names an unsupported scheme and uses the next one, and connects directly when none is usable, so an environment holding only `ALL_PROXY=socks5://...` is not proxied. `System` reads only `http=` and `https=` entries and a bare `host:port`; `socks=` and `ftp=` entries, and an entry with its own `socks5://` scheme, read as no entry. `ProxyMode::None` stays the default, and environment variables are ignored unless `Environment` is chosen. `System` uses the operating system's proxy for the current user: on Windows the static proxy and bypass list of the system proxy settings with nothing configured, and on other systems, it behaves as `Environment`.

Limits of `System` in this version: on a default Windows install, where "Automatically detect settings" is on and nothing else is configured, `System` does not fail: it uses the environment proxy variables (`ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY`) when set and a direct connection otherwise. `Client::proxy_mode_used()` / `AsyncClient::proxy_mode_used()` reports which (`ProxyModeUsed::SystemFallbackEnvironment` or `SystemFallbackDirect`, with the reason `automatic detection only; no static proxy or PAC configured`; `System`, `Environment` and `Direct` otherwise). With a static proxy the detection flag is ignored and the static proxy is followed. Since 0.3.2 `System` resolves the static proxy per request URL on Windows. The string is read per scheme as Windows reads it (`http=`, `https=`, a bare `host:port` for the rest), each request goes to its own scheme's endpoint (a scheme with none connects directly), and the bypass list is evaluated for that URL's host: names, addresses, `.suffix`, `*` wildcards anywhere (`10.*`), `<local>` (names without a dot, "Don't use the proxy server for local addresses") and scheme-limited entries (`https://host` bypasses `https` only). The decision is made for every hop, so a redirect is routed on its own. Nothing is cached: it is a string match. One proxy for both schemes with a bypass list of plain hosts is still held as one client-wide proxy; in the per-request case `proxy_diagnostic()` is `None`. What is still refused, as `HttpError::SystemProxyUnsupported { element }` from `Client::try_new` (and on every request of a client built with `Client::new`), with nothing sent:

- **A proxy auto-config script.** It chooses a proxy per URL; use `ProxyMode::SystemForUrl`.

**`ProxyMode::SystemForUrl("https://host/path")`** (0.3.2) supports the automatic and PAC modes: it asks WinHTTP's resolver (`WinHttpGetProxyForUrl`) for that URL, and otherwise applies the static proxy and bypass list exactly for that URL, so nothing is refused for being per-URL. The client uses the one resulting proxy (or none) for all its requests, so build one client per target host. A PAC script that cannot be evaluated is `SystemProxyUnsupported`; detection that finds nothing falls back to the static proxy, then the environment, then direct (reported by `proxy_mode_used()`).

Windows machines with `<local>` or per-scheme proxies next to a static proxy no longer need an app-side fallback: `System` follows them per request. Only a PAC script is still `SystemProxyUnsupported` under `System`. Per-request routing is covered by unit tests with injected settings and local fake proxies; it has not been run against a real Windows proxy configuration.
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

### Loopback and local-first policies

`AddressPolicy::allow_loopback()` admits loopback addresses for every host. `AddressPolicy::local_first()` admits loopback and RFC 1918 IPv4 (not CGNAT or IPv6 unique-local). `AddressPolicy::allow_if_any_host(pred)` is the general form of `allow_if` for every host. None of them admits link-local or metadata destinations, `deny_ranges` or `deny_if`. `AddressPolicy::refuse_mixed_loopback()` refuses a host whose answer mixes loopback with non-loopback addresses, the DNS-rebinding shape.

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

- `ClientConfig::trust` is `TrustRoots::Platform` (default) or `TrustRoots::Bundled`. **From 0.3.0 the default is the operating system's trust store**, so roots an enterprise or an inspection proxy installed on the machine are honoured, as curl and browsers honour them. Before 0.3.0 only Mozilla's bundled roots were trusted. Both clients name their verifier explicitly, so no other crate's Cargo features can change it: the blocking client sets ureq's TLS provider and `RootCerts` (`PlatformVerifier` or `WebPki`); the async client installs a complete rustls `ClientConfig` with `use_preconfigured_tls` in **both** branches (`rustls-platform-verifier` for `Platform`, the `webpki-roots` Mozilla set and nothing else for `Bundled`). The async client builds on reqwest 0.13 (`Cargo.toml`: `rustls-no-provider` and `stream`, default features off); rustls is 0.23 with the `ring` provider installed by this crate, and the workspace lock resolves reqwest 0.13.5. `Client::verifier_kind()` and `AsyncClient::verifier_kind()` return `"platform"` or `"bundled"`; the async value reports the configuration that was installed. `Bundled` ignores the machine's roots. There is no option that disables verification.
- The async client advertises **HTTP/1.1 only** in ALPN. reqwest's `http2` feature is not enabled here, so a server that selected `h2` would otherwise panic hyper-util; the client cannot negotiate it, whatever other crates enable.
- `ClientConfig::https_only` (default `false`) refuses a non-`https` initial URL and any redirect hop to a non-`https` URL with `HttpError::HttpsRequired { host, scheme, redirect }` (class `Invalid`), before anything is sent to that URL. It applies to both clients. It is separate from the address policy's HTTPS-to-HTTP redirect rule, which stays on; when a redirect to `http` trips both, `HttpsRequired` is the error reported.

## Defaults worth knowing

- **Routing is decided once per hop.** Whether a request goes through the proxy is decided by one matcher (`ureq`'s `NO_PROXY` rules, wildcards included) that also drives address vetting, and the async client keeps a direct and a proxied reqwest client and picks one per hop, with reqwest's own environment and `NO_PROXY` handling switched off. A host the policy treats as direct is always vetted at the resolver; the same holds after redirects.
- **`Debug` of `Response` and `ResponseHead` hides every recorded secret** (`redact_values`, `secret_header` values, URL secrets) as well as credential-named headers.

- **Redirects: none by default** (`max_redirects = 0`). A 3xx comes back as the response. Set `ClientConfig::max_redirects` (and `max_redirects_will_error`) to follow them; each hop passes the address policy and strips credentials on a new origin.
- **Retries: idempotent methods only.** POST and PATCH are never replayed after a 429/5xx unless the request sets `retry_unsafe_statuses(true)`. A connect-phase failure (nothing reached the server) is still retried for every method.
- **Streams and quiet SSE.** `ClientConfig::response_timeout` (default 60 s) is the time to the response *headers* (first byte), not an idle timeout. Once headers arrive, a stream body has no idle cut unless `recv_body_timeout` (a total body budget, default none) or `stream_timeout` / `Request::timeout` (total lifetime, default none) is set. A server that sends headers and then stays quiet is therefore not cut; one that takes over 60 s to send headers is, so raise or clear `response_timeout` (`None`) for such servers. For a per-read stall limit, set `ClientConfig::idle_timeout`: it applies to streamed bodies in both clients and restarts on every chunk. Use `Request::cancel` or `deadline` for an absolute bound.
- **Compression.** `Client::compression(false)` (and `AsyncClient::compression(false)`) sends `Accept-Encoding: identity` on each request that sets none, so an SSE or provider stream is not re-encoded by an intermediary. The default (`true`) keeps the transport's own negotiation.
- **Repeated headers and final URL.** `Response::header_all(name)` (also on `ResponseHead` and `StreamResponse`) yields every value in order, for example several `Set-Cookie` lines. `Response::final_url()` is the URL after the followed redirects. `Response::retry_after()` reads a numeric or HTTP-date `Retry-After`. `StreamResponse::copy_to(writer, max_bytes, progress)` copies a stream into a writer under a byte cap, reporting progress per chunk.
- **Server-Sent Events framing.** The default decoder follows WHATWG blank-line framing. `SseDecoder::eager()` delivers each `data:` line as its newline arrives, for a daemon that ends frames with a newline rather than a blank line. `SseDecoder::finish()` flushes a final frame that lacks its blank line, and `has_pending()` says whether a stream ended mid-event. `SseReader::eof_policy(EofPolicy::Deliver | Fail)` and `SseStream::eof_policy(...)` make the end-of-stream choice explicit (default `Drop`, the spec behaviour).
- **Text decoding is lossy UTF-8.** `Response::text()` and error bodies decode as UTF-8 and replace invalid bytes; the `Content-Type` charset is not honoured. For another charset, take `Response::body` bytes and decode them yourself.

## Hard blocks and spellings

- **Cloud metadata cannot be allowed by any hook.** Link-local (which holds `169.254.169.254`), `fd00:ec2::/32` (AWS IPv6 metadata and DNS), `100.100.100.200` (Alibaba), `168.63.129.16` (Azure) and `192.0.0.192`, plus IPv6 forms that embed them, are refused whatever `allow_if`, `allow_private_resolution` or the proxy mode say. Cloud metadata host names are refused too.
- **Legacy IPv4 spellings** such as `127.1`, `0177.0.0.1`, `0x7f.0.0.1` and `2130706433` are read the way `inet_aton` reads them before any check, in both clients, through proxies and on redirects.
- IPv6 forms that embed IPv4 take the embedded class: mapped, compatible, NAT64 `64:ff9b::/96`, 6to4, and the local-use NAT64 space `64:ff9b:1::/48` in each RFC 6052 layout.
- **Non-canonical IPv4 spellings are refused** by default (`AddressPolicy::allow_legacy_ipv4()` opts out): parsers disagree about `0177.0.0.1`, so normalising it would be an SSRF bypass. `parse_host_ip(host)` returns `Option<ParsedHostIp>` (the address, `kind` and `is_canonical`) for callers guarding other transports; `parse_legacy_ipv4` reads the `inet_aton` forms (one to four parts, decimal/octal/hex, the last part fills the remaining bytes).
- **HTTPS to HTTP redirects are refused** by the address policy. `AddressPolicy::allow_insecure_redirects()` opts in.
- Logging: the vendored `ureq` 3.4.0 documentation describes its `TRACE` level as wire-level debug output that is "NOT REDACTED" (`ureq` 3.4.0, `src/lib.rs:449`). This crate cannot intercept it. Do not enable TRACE for a process that carries secrets in URLs.

## Vetting for other transports

`AddressPolicy::resolve_vetted_async(host, port)` (feature `async`) is the same check for an async caller, on tokio without `spawn_blocking`. It returns `ResolveError::Private { addr }` for a private address that an approval could admit, `ResolveError::Blocked { addr, class, reason }` for anything that must never be admitted (link-local, metadata, a denied range, a name rule), and `ResolveError::Dns(HttpError)` for a failed or slow lookup. A `Private` answer is given only when every address of the answer is admitted or approvable, so a hard block is never hidden behind an approval.

`AddressPolicy::resolve_vetted(host, port) -> Result<Vec<SocketAddr>, HttpError>` resolves a host once and applies exactly the rules the clients use: the address classes, `deny_ranges`, `deny_if`, `allow_if`, the first-party pin and its private-resolution opt-outs, metadata names, and the denial of names with no address. It returns the same `HttpError::AddressDenied`. A WebSocket, raw TCP or gRPC transport calls it and **must connect to the returned addresses, not look the name up again**. A transport that cannot pin its connection leaves a DNS-rebinding window between the check and its own lookup. The host is treated as reached over HTTPS; requiring TLS under a first-party policy is the caller's job.

## Redirects

- `max_redirects` defaults to 0, so redirects are not followed. With 0, a 3xx response is always returned, whatever the error policy.
- The crate follows redirects itself, one hop at a time, so the address policy and header stripping run on every hop. Rules match ureq: 301/302/303 and other 3xx (not 304) become `GET` unless the method is `GET` or `HEAD`; 307/308 are not followed for `POST`, `PUT`, `PATCH` and `DELETE` (the 3xx comes back); a 3xx without `Location` comes back.
- On a cross-origin hop (scheme, host or port differs) `Authorization`, `Cookie`, headers named like credentials (`authorization`, `authentication`, `cookie`, `token`, `secret`, `api-key`, `apikey`, `password`) and any header holding a recorded secret value are dropped. A same-origin hop keeps them.
- **Approved origins (unreleased, after 0.3.6).** `Request::approved_redirect_origins(ApprovedOrigins)` confines a request's redirects to its own origin plus the listed origins (scheme, host and port, compared canonically). A hop anywhere else is refused before anything is sent to it, with `HttpError::RedirectRefused { host, reason: RedirectRefusal::UnapprovedOrigin }` (class `Invalid`, never retried); the error names the host only, never the path, query or userinfo. `ApprovedOrigins::new()` means the request's origin only; `ApprovedOrigins::parse(["https://downloads.example.com"])` adds origins (a plain origin per entry, no userinfo, path, query or fragment; one bad entry refuses the whole set). A request without a set behaves as before. The set decides where a redirect may go, not what it carries: a hop to an approved but different origin still drops credentials as described above, and the address policy and `https_only` still run on every hop.
- **Redirect target shape (unreleased, after 0.3.6).** On every hop, with or without a set, a target whose authority carries userinfo (`https://user:pw@host/`, `https://good.example@evil.example/`) is refused as `RedirectRefused` with `RedirectRefusal::Credentials`. A `Location` that is not an absolute http(s) URL after resolution keeps returning `HttpError::InvalidRequest("redirect with an unusable Location")`.
- **HTTPS to HTTP.** The clients refuse the hop through the address policy (`HttpError::AddressDenied`, reason "redirect from HTTPS to HTTP refused") unless `AddressPolicy::allow_insecure_redirects()` was set; with `https_only` the error is `HttpsRequired { redirect: true, .. }`. The new `RedirectRefused` is not used for this case, so existing matches on `AddressDenied` keep working.
- **For other transports.** `check_redirect_target(request_url, from_url, location, &ApprovedOrigins) -> Result<String, RedirectRefusal>` is the same shape and origin check on strings only, for a transport that follows redirects itself (it neither resolves hosts nor applies an `AddressPolicy`). It returns the absolute target, fragment dropped, or `RedirectRefusal::{Malformed, SchemeDowngrade, Credentials, UnapprovedOrigin}`; unlike the clients it has no insecure-redirect opt-in and refuses every `https` to `http` hop. Apply `AddressPolicy::resolve_vetted` to the target's host and connect to the returned addresses.
- `max_redirects_will_error` defaults to `true`, matching ureq. When a nonzero limit is exhausted, the call returns `HttpError::Transport { connect_phase: false, .. }`. That error is eligible for idempotent retries under the retry policy.
- Set it to `false` to receive the final 3xx through `send_any`.

## Platform support

Pure Rust over `ureq` with `rustls`, with platform-specific code in `src/system_proxy.rs` (Windows system proxy settings). Certificate and TLS behaviour follows `rustls`; the default roots are the operating system's (`TrustRoots::Platform`).

## Tests

`tests/e2e.rs` and, with `--features async`, `tests/async_e2e.rs`: a real local TCP server. Assertions use exact requests seen on the wire and exact bytes received. `tests/second_read.rs` (and `tests/second_read_async.rs` with `--features async`) pin the retry-sleep clamp. `tests/request_policy.rs` covers per-request overrides, cancellation, redaction and the deadline. `tests/address_policy.rs` (and `tests/async_policy.rs` with `--features async`) cover the address policy: resolver denial per class, embedded IPv4 forms, redirects, cross-origin stripping, first-party mode, `deny_ranges`, `deny_if` and URL redaction. `tests/redirect_guard.rs` (and `tests/tls_local.rs` for the HTTPS to HTTP hop) cover approved origins, the typed redirect error, the userinfo refusal and `check_redirect_target`. `tests/send_headers.rs`, `tests/proxy.rs` and `tests/phases.rs` cover headers-only calls, proxies and error phases. `tests/classify_parity.rs` cross-checks the classification against `rightkit-browser`'s source.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Version history: [CHANGELOG.md](CHANGELOG.md).
