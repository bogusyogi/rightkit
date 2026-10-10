# Changelog

## 0.2.6

Mostly additive. Call sites that use builders compile unchanged; source-level changes for code that
builds structs by hand or matches `HttpError` exhaustively, and the new default address policy, are listed
below.

- Add `Request::put`, `Request::delete`, `Request::set_header` (replaces every header with the same
  name, ASCII case-insensitive, keeping the first position), `Request::secret_header`, and
  `Request::multipart`.
- Add per-request overrides: `Request::retry_unsafe_statuses(bool)`, `Request::max_response_bytes(usize)`,
  `Request::deadline(Duration)` (budget for all attempts and retry sleeps together; the per-attempt
  `timeout` is unchanged), and `Request::cancel(CancelToken)`.
- `Client::stream` now honours `Request::timeout` as its total budget. Before, it ignored it and used
  `ClientConfig::stream_timeout` only.
- Add `CancelToken` and `is_cancelled_io`. Cancellation is checked between attempts, during retry
  sleeps, and before every body read in buffered calls, streams, and downloads. A cancelled call
  returns `HttpError::Cancelled`. Cancellation cannot interrupt a socket wait for response headers.
- Add `ClientConfig::redact_values(Vec<String>)`. Configured values and each request's secret values
  are replaced with `[redacted]` in error messages, `RetryEvent` URLs and reasons, stream I/O error
  text, and `Debug` output. `Request`'s `Debug` no longer prints the body bytes, only their length.
- Add `Client::download_to_path` with `DownloadOptions`, `DownloadProgress`, `DownloadReport`, and
  `DownloadError`. Bytes go to `<dest>.part`, which is fsynced and renamed on success. Resume uses
  `Range` with `If-Range` on the first ETag seen in the call. `206` appends, `200` restarts, and a
  `416` whose total equals the local length verifies without fetching. Mid-body failures reconnect up
  to `max_reconnects`. Hash or length mismatches delete `.part`. Cancellation keeps it. Requests send
  `Accept-Encoding: identity` unless the caller sets one.
- Add `Multipart` (`new`, `with_boundary`, `text`, `file_bytes`, `file_path`, `content_type`,
  `into_body`) with CRLF framing, `Content-Disposition` on every part, `Content-Type` only on file
  parts, and `"`, CR, and LF replaced with `_` in names and filenames. `into_body` fails if the
  boundary occurs inside a part.
- Add `HttpError::Cancelled`. Exhaustive `match` expressions on `HttpError` need a new arm.
- Add the `sha2` 0.11 dependency for download hashing. It is already in the lockfile.
- Add an outbound address policy (SSRF guard), on by default, for `Client` and `AsyncClient`.
  The host of the request and of every redirect hop is resolved once by the crate's own resolver,
  every resolved address is classified, and the connection is made only to the checked addresses
  (ureq: a custom `Resolver` whose result `TcpConnector` connects to; reqwest: a custom DNS
  `Resolve`), so there is no second lookup between check and connect. Denied by default:
  loopback, RFC 1918, link-local including `169.254.169.254`, CGNAT `100.64/10`, multicast,
  reserved, unspecified and broadcast addresses, IPv6 ULA `fc00::/7` and link-local `fe80::/10`,
  IPv4-mapped, IPv4-compatible, NAT64 `64:ff9b::/96` and 6to4 `2002::/16` forms by the class of
  the embedded IPv4 address, `localhost` and `*.localhost`, cloud metadata host names, and any
  name that resolves to no address. One denied address among several denies the host.
  **Behaviour change:** calls to loopback or LAN servers, including the `testing` `MockServer`,
  now fail with `HttpError::AddressDenied` until the host is listed with `allow_if`.
- Add `netpolicy` (`AddressClass`, `classify_ip`, `embedded_v4`, `IpNet`, `AddressPolicy`). The
  classification mirrors `rightkit-browser`'s `policy.rs`; `tests/classify_parity.rs` reads that
  source and checks every address its tests expect.
- Add `AddressPolicy` builders: `deny_ranges(Vec<IpNet>)`, `deny_if(Fn(&IpAddr) -> Option<String>)`
  (the text becomes the error `reason`), `allow_if(hosts, Fn(&IpAddr) -> bool)` (listed hosts only),
  and `with_resolver` (offline tests, pinned DNS). Custom denials beat `allow_if`. Link-local and
  metadata destinations cannot be allowed by any hook. IPv6 forms are also tested by their
  embedded IPv4 address against `deny_ranges` and `deny_if`.
- Add `ClientConfig::first_party(hosts)`, `Client::first_party(hosts)` and
  `AsyncClient::first_party(hosts)`: only the listed hosts (exact match), only over HTTPS, on every
  redirect hop; resolved addresses are classified with the public-web classes unless the host is opted out with `allow_private_resolution`.
- Add `AddressPolicy::resolve_vetted(host, port) -> Result<Vec<SocketAddr>, HttpError>`: resolve once and
  vet with exactly the client rules, so other transports apply the identical policy and pin to the
  returned addresses. A transport that cannot pin leaves a DNS-rebinding window.
- First-party hosts are classified with the public-web classes on their resolved addresses. A listed host
  that resolves to a private address is refused unless `AddressPolicy::allow_private_resolution(host)`
  opts it out (on-premises servers). Link-local, metadata, `deny_ranges` and `deny_if` still apply.
- Add `ClientConfig::address_policy`, `ClientConfig::proxy` and `ClientConfig::proxy_bypass_first_party`
  (public fields with builder methods), `ProxyMode::{None, Environment, Explicit(url)}` and
  `Client::try_new`. **Behaviour change:** environment proxies are no longer honoured by default
  (`ProxyMode::None`); set `ProxyMode::Environment` (`ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY`,
  `NO_PROXY`, either case) or `ProxyMode::Explicit(url)` (`http`/`https` only) to use one. The proxy
  endpoint is trusted (no address-class denial, often a private address). Every destination is still
  vetted: IP literals and `localhost` by class, hostnames by name rules (`localhost`, `*.localhost`,
  `*.local`, `*.internal` and single-label names refused unless `allow_if` lists them), metadata names,
  the first-party pin, and every redirect hop. The proxy resolves hostnames itself, so their addresses
  cannot be pinned: the guarantee through a proxy is name- and literal-based, weaker than a direct
  connection. Hosts matching `NO_PROXY` connect directly with the full check.
  `proxy_bypass_first_party(true)` skips the proxy for a first-party client. Proxy credentials never
  appear in `Debug` or errors.
- Add `HttpError::AddressDenied { host, addr, class, reason }`. `addr` and `class` are `Option`
  because some denials concern a name or scheme, not an address. It is never retried and its text is
  scrubbed like every other error. Exhaustive `match` expressions need another arm.
- Redirects are now followed by the crate, hop by hop, in both clients; ureq and reqwest never follow
  one. `max_redirects` and `max_redirects_will_error` keep their meaning. Rules match ureq's
  (301/302/303 and other 3xx become `GET` except `GET`/`HEAD`; 307/308 are not followed for
  `POST`/`PUT`/`PATCH`/`DELETE`; a 3xx without `Location` is returned). On a cross-origin hop
  (scheme, host or port differs) `Authorization`, `Cookie`, any header whose name contains
  `authorization`, `authentication`, `cookie`, `token`, `secret`, `api-key`, `apikey` or `password`,
  and any header holding a recorded secret value are dropped. Same-origin hops keep them (ureq
  dropped `Authorization` and `Cookie` on every hop).
- URLs are never echoed with their query string or userinfo. `RetryEvent::url`, `Request`'s `Debug`
  and every error and event that mentions a URL render `scheme://host[:port]/path?[redacted]`; any
  `scheme://...` text inside a message from a lower layer is rewritten the same way, and the query and
  userinfo of the request URL join the scrub list. This covers `InvalidRequest`, `Transport`,
  `Timeout`, `Stream` and `AddressDenied`; a `Status` body is the server's text and is only scrubbed
  of secret values.
- `Client::send_headers(req) -> ResponseHead { status, headers, final_url, hops, attempts }` and
  `Request::headers_only()` return status and headers for any status (3xx with redirects off, 4xx,
  5xx after retries) without reading the body, with the same per-hop address policy; the unread body is
  dropped, which closes that connection. `Client::stream_any` returns any status with its headers and
  streaming body, like `AsyncClient::stream_any`. `Client::stream` still turns non-2xx into
  `HttpError::Status`.
- `HttpError::Timeout` is now `Timeout { message, phase }` and `HttpError::Transport` gains `phase`, with
  `Phase::{Connect, Headers, Body, Redirect}` and `HttpError::phase()`. `connect_phase` stays and is
  true exactly for `Phase::Connect`. `Redirect` marks failures on a redirect hop after the first request
  and an exhausted redirect limit. **Source-level change:** patterns `HttpError::Timeout(_)` become
  `HttpError::Timeout { .. }`, and exhaustive `Transport { .. }` patterns need `..` or `phase`.
- `AsyncClient::stream` and `stream_any` honour `Request::timeout` as the total stream budget, like
  `Client::stream`. `AsyncClient` also applies secret scrubbing and URL redaction to its errors and
  retry events, as the blocking client does.
- `testing::loopback_policy()` and `testing::loopback_config()` allow the `MockServer` through
  `allow_if` for consumer tests. The `async` feature now enables `tokio/net`.
- Add `HttpError::class() -> ErrorClass` with `ErrorClass::{Auth, RateLimit { retry_after }, Server,
  Client, Timeout, Transport, Cancelled, AddressDenied, Invalid}`, so apps map their own error classes
  and retry sets from one typed helper. The table is in the README.
- Source-level changes for code that builds structs by hand: `Request` gains private fields, so
  external struct literals of `Request` no longer compile; use `Request::new`/`get`/`post`/`put`/`delete`
  and the builder methods. `ClientConfig` gains the public fields `redact_values`, `address_policy`, `proxy` and
  `proxy_bypass_first_party`, so literals must use `..ClientConfig::default()`, as the README already shows.


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
