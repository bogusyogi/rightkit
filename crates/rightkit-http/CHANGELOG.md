# Changelog

## Unreleased

- Tests: a refused connection is now produced by a loopback socket that is bound, never listens and is held until the
  assertions are done (`tests/common/refused.rs`, exclusive address use on Windows). The eight places in six test
  files that bound a listener, read its port and dropped it assumed the freed port stayed closed; on the Windows CI
  leg another test's server was given that port, and `tests/phases.rs` `connection_refused_is_the_connect_phase` got a
  302 and `a_connect_failure_on_the_first_request_stays_retryable_for_post` got a 200. No product change.
- **Fixed (behaviour change, fifth read finding 11):** `ProxyMode::System` follows the Windows proxy settings exactly or not at all. A bypass entry that is not a plain host (`<local>`, a scheme-limited `https://host`, an inner wildcard) used to be dropped while the proxy stayed in use, so traffic configured as direct went through the proxy; a PAC script or automatic detection was ignored. All of these, and different routes for HTTP and HTTPS, now fail closed with `HttpError::SystemProxyUnsupported { element }` (class `Invalid`, never retried), which names the first unsupported element; nothing is sent. This replaces `HttpError::SystemProxyPerScheme`, which was never published. Machines with `<local>` or automatic detection set get the error: an app catches it and uses `Environment` or `Explicit`. Per-request routing is a later version.

Fixes from the fifth read (docs/audits/2026-10-11-astra-http-fifth-read.md, findings 9, 10 and the remainder of 5; finding 11 is the Windows system-proxy work and is not changed here). No published version contained `download_to_path` (0.2.5 had no download module), so none of this changes published behaviour. It is a reduction of the unreleased download code: the earlier rounds added mechanisms, this one removes them. The lock, partial-file and `416` statements in the third read, fourth read, 0.3.0 and 0.2.6 entries below describe states of the unreleased tree and are superseded by the entries here.

- **Changed (behaviour change, finding 9):** a download no longer creates anything beside the destination. The claim (`<key>.lock`) and the partial file (`<key>.part`) live in one hidden directory, `.rk-download`, in the destination's parent (created owner-only on Unix; a file or link at that path is refused), where `<key>` is the lowercase hex SHA-256 of the destination's file name (raw bytes on Unix, UTF-16LE on Windows). No destination name in the parent can be one of those files, so a different spelling of a destination (a case or Unicode alias such as `out.locK`, which APFS equates with `out.lock`) can no longer replace another download's lock or partial file. The claim file holds the destination's exact file name; it is read after the lock is taken, and a partial file is kept only when the record matches (a first claim, or any other content, deletes the partial file and records the name). This replaces name-derived identity with a hash and a verified record, and removes the ASCII-folded `.lock`/`.part` suffix check: **names ending in `.lock` or `.part` are ordinary destinations again.** Refused instead, before any request or file access, on every platform: a destination whose file name or any directory in its parent path is `.rk-download` (simple Unicode case fold, so `.RK-DOWNLOAD` and a Kelvin-sign spelling are refused too), and, as before, a name ending in a dot or space; on Windows a name with `:`, a device name or `~<digit>`. Claim and partial file are opened without following links as before, and a claim file with more than one hard link is refused like a partial file. Where the claim is held the second call still fails at once with `DownloadError::Io` ("another download of this destination is in progress"); different destinations in one directory still download side by side. Limits: two spellings that one filesystem treats as one file are two independent downloads with separate partial files (not excluded from each other; the later rename wins); one claim file stays in `.rk-download` per destination name; `.rk-download` and the destination must be on one filesystem; the directory is not hidden on Windows.
- **Changed (behaviour change):** anything that looked for `<dest>.part` or `<dest>.lock` beside the destination (to delete a leftover partial file, to show resume state) must look in `.rk-download` instead. There is no public helper for the path; the layout is documented in the README and the module docs. No crate in this repository does this: `rightkit-models` has its own downloader and does not call `download_to_path`.
- **Fixed (behaviour change, finding 10):** a `416` never publishes saved bytes. Every `416` to a ranged request restarts from zero (the prefix is discarded, validators are forgotten, the next request has no `Range`), whatever its `Content-Range` says. The audit's sequence (a chunked `200` with ETag `"v1"` and no total cut after `AAAA`; the ranged retry answered `416 bytes */4` with ETag `"v2"`) used to publish the stale `AAAA` and now ends with the bytes of the plain request. A `416` to a request with no `Range` is an error. Cost: a complete partial file proven by `expected_sha256` is no longer verified on a `416` without fetching; it is downloaded again.
- **Removed:** the `416`-as-completion branch (total equals the local length), the once-only `416` restart flag, the `416` resource and total comparisons, `DestLock`, `lock_path`, `part_path` and the `.lock`/`.part` suffix refusal, with their tests and state-table rows.
- **Tests:** the unit test `the_same_file_check_notices_a_replaced_part` panics, instead of returning success, when its rename fixture fails (finding 5). Partial files are planted in `.rk-download` with a claim record (`tests/common/mod.rs`). New: `tests/fifth_read.rs`, and unit tests for the layout, the claim record, the owner-only directory and the refused names in `src/download.rs`.
- **Not changed:** no public API signature changed (`download_to_path`, `DownloadOptions`, `DownloadReport`, `DownloadProgress`, `DownloadError`); no dependency or feature was added, and the std file lock is kept (`rightkit-fs`'s lock is behind its `lock` feature and would make `download_to_path` pull `rightkit-fs` into HTTP-only consumers such as `rightkit-models`).

- **Fixed (behaviour change, fourth read findings 7 and 8):** `ProxyMode::System` no longer sends plain HTTP to the endpoint the system configures for HTTPS, and no longer widens a bypass entry limited to one scheme. The static proxy string is read per scheme as Windows reads it (`http=`, `https=`, a bare entry for the rest, direct otherwise). One endpoint for both schemes is used as before. Settings that route the two schemes differently (two endpoints, or a proxy for one scheme only) cannot be followed by a client with one proxy, so the client now fails closed with the new typed `HttpError::SystemProxyUnsupported` (first named `SystemProxyPerScheme`, see above) ("Windows proxy settings route HTTP and HTTPS differently; this version supports one proxy for both", class `Invalid`, never retried) instead of using the HTTPS endpoint for everything. A bypass entry limited to one scheme (`https://host`) is dropped, so that host goes through the proxy for both schemes instead of bypassing it for both. Per-request routing by scheme is not implemented.
- **Added:** `ProxyMode::System`: the proxy the operating system is configured with for the current user. On Windows it reads the static proxy and bypass list of the system proxy settings (`WinHttpGetIEProxyConfigForCurrentUser`); an `http`/`https` proxy is used and the bypass list becomes the `NO_PROXY` list where it can be expressed (`<local>` and patterns such as `10.*` are dropped, so those hosts go through the proxy). A proxy auto-config script and auto-detection are **not** evaluated. With no static proxy, and on other systems, it behaves as `ProxyMode::Environment`. The default is unchanged (`ProxyMode::None`). `ProxyMode` gained a variant: add an arm to any exhaustive `match`.

Fixes from the fourth read (docs/audits/2026-10-11-astra-http-fourth-read.md, findings 2 and 6; 7 and 8 are the Windows system-proxy work and are not changed here):

- **Fixed (behaviour change):** destination names that Windows normalizes onto a reserved name are refused on every platform before any request or filesystem access: a trailing dot or space (`out.lock.` reached `out.lock` on Windows and replaced a held lock). On Windows, names with `:` (alternate data stream), a device name (`NUL`, `COM1`, `con.txt`) or `~<digit>` (8.3 alias such as `OUT~1.LOC`) are refused too. A destination with no file name (`..`, a root) is refused. Same class, swept: the lock and partial names are the only ones derived from the destination. (Superseded by the fifth read entries above: those files are no longer beside the destination, and only `.rk-download` is reserved.)
- **Fixed (behaviour change):** `download_to_path` no longer publishes bytes from an earlier representation without a strong validator. A saved prefix is extended only when this call saw a strong ETag (`If-Range`) or `expected_sha256` is set; otherwise it is discarded and the download restarts without `Range`. A `.part` left by an earlier call therefore needs `expected_sha256` to be resumed (validators are not stored on disk). A `206` whose ETag, `Last-Modified`, resource or total contradicts the saved ones restarts, and the saved total is no longer overwritten by a `206` without a total. A `416` whose total contradicts the saved total (the same-URL shrink: `Content-Range: bytes */4` after a declared length of 10) restarts instead of verifying and publishing the old prefix. (Every `416` restarts now: see the fifth read entries above.) Weak ETags are no longer sent as `If-Range`. The full decision table is in the module docs of `src/download.rs`; tests per row in `tests/fourth_read.rs`.
- **Tests:** existing resume tests that start from a `.part` of an earlier call now set `expected_sha256`.

Fixes from the third read (docs/audits/2026-10-11-astra-http-third-read.md):

- **Fixed:** `download_to_path` refuses a `.part` that has more than one hard link (Unix `nlink`, Windows link count) before any write or truncation, so a hard link to another file is never rewritten on resume.
- **Fixed (behaviour change):** destination file names ending in `.lock` or `.part` (ASCII case-insensitive) are refused with `DownloadError::Io` before any filesystem access or request; they collided with the internal lock and partial files and let one download replace another's active lock. (Superseded: the internal files moved to `.rk-download` and these names are accepted again; see the fifth read entries above.)
- **Fixed (behaviour change):** the resource-change check now compares the full final URL including the query (fragment only dropped) and also applies to `416`, so a reconnect that lands on a different resource restarts instead of publishing the saved prefix. A redirect that re-signs its query on every request now restarts the download rather than resuming.
- **Added:** `Request::secret_form(pairs, secret_names)`, `Request::redact_value(value)` and `Request::scrub(text)`. Values registered through them are scrubbed from error text and `Debug` like secret headers. `rightkit-oauth` must adopt them to stop echoing submitted tokens (the OAuth side of audit finding 4 lives in that crate and is not changed here).
- **Tests:** the Windows link and swap tests now fail, instead of skipping, when the symlink or junction fixture cannot be created. Regression tests in `tests/third_read.rs`.

- **Fixed:** a bodiless GET, HEAD or DELETE no longer sends `Content-Length: 0` (the blocking client's ureq framed every request with a body; the async client now matches it). POST, PUT and PATCH with an empty body still send `Content-Length: 0`.

- **Breaking:** `HttpError`, `Phase`, `ErrorClass` and `DownloadError` are `#[non_exhaustive]`: add a fallback arm to any exhaustive `match`. Later variants are then not breaking.

- **HTTP 408 is now `ErrorClass::Timeout` (behaviour change).** `HttpError::Status { status: 408 }` previously
  classified as `ErrorClass::Client`; it now classifies as `ErrorClass::Timeout`, matching its meaning (the
  server timed out waiting for the request). HTTP 407 remains `ErrorClass::Auth`. Callers that map
  `ErrorClass::Client` to a non-retryable error, or that match 408 explicitly, must review their mapping.
- **`ClientConfig::legacy_ureq_parity(user_agent)`** (additive) returns the settings of a plain `ureq`
  agent: environment proxies, `TrustRoots::Bundled`, 10 redirects, `RetryPolicy::none()` and the given user
  agent. All other fields keep their defaults, and private-address resolution stays off.

## 0.3.0

This is the 0.2.6 work renumbered: it carries source-visible breaks (`HttpError` variants and shapes, new
`ClientConfig` fields, the default address policy), so sibling crates' requirements on `rightkit-http`
need updating. Fixes from the adversarial read (docs/audits/2026-10-11-astra-rightkit-http-0.2.6.md):

- **Default trust source changes (behaviour change).** Both clients now verify servers against the
  operating system's trust store by default (`ClientConfig::trust = TrustRoots::Platform`), where 0.2.x
  trusted only the bundled Mozilla roots. Enterprise and TLS-inspection roots installed on the machine
  are now honoured, and a certificate chaining only to a bundled root that the machine has removed is
  no longer trusted. `TrustRoots::Bundled` restores the old behaviour. Blocking client: ureq
  `platform-verifier` (`rustls-platform-verifier` 0.7, the OS verifier). Async client: the same
  verifier installed in reqwest 0.12 (feature `rustls-tls`, unchanged) with `use_preconfigured_tls`; no
  second reqwest major. `verifier_kind()` on both clients reports `"platform"` or `"bundled"`. Verification
  cannot be disabled. New dependencies: `rustls-platform-verifier` (ureq, and directly under `async`), `rustls` 0.23
  (direct, `async` only, `ring`), and the OS crates the verifier pulls in.
- **`ClientConfig::https_only`** (default `false`, so existing behaviour is preserved) refuses a non-https
  initial URL and every redirect hop to a non-https URL, in both clients and in downloads, with the new
  `HttpError::HttpsRequired { host, scheme, redirect }` (`ErrorClass::Invalid`, never retried). Callers that
  match `HttpError` exhaustively need an arm for it. `ClientConfig` gains `trust` and `https_only`:
  struct-literal construction without `..Default::default()` needs both.
- **Retry default changes (behaviour change).** `Request::retry_unsafe_statuses` now defaults to `false`:
  POST and PATCH are not replayed after 429/5xx unless the request opts in. A replayed single-use OAuth code
  exchange is harmful. Callers that relied on POST status retries (LLM calls) must opt in per request.
- README documents the defaults of no redirects, quiet-stream timeouts (no idle cut after headers) and
  lossy UTF-8 text decoding.
- **Second independent read** (docs/audits/2026-10-11-astra-rightkit-http-0.3.0.md):
  - Async TLS: both trust choices now install an explicit rustls `ClientConfig` through
    `use_preconfigured_tls`, so `Bundled` is exactly the Mozilla roots even if another crate enables
    reqwest `default-tls` or `rustls-tls-native-roots`; ALPN advertises `http/1.1` only (no `h2`,
    no hyper-util panic without reqwest's `http2` feature). New direct dependencies under `async`:
    `webpki-roots` (already in the lock through ureq). The blocking client names the rustls provider
    and root source explicitly too.
  - Proxy routing: one matcher decides proxy-or-direct per hop for both policy and transport; the
    async client holds a direct and a proxied reqwest client and no longer lets reqwest read the
    environment or apply its own `NO_PROXY` rules (they differ for wildcard entries).
  - Downloads: whole-resource completion against the advertised total; the wire `Content-Encoding`
    is read before ureq decodes, so chunked gzip is refused; a `Content-Range` ending at `u64::MAX`
    is rejected and arithmetic on header integers is checked or saturating.
  - Async retries: the drain of a retryable response and the retry sleep are clamped to the attempt
    deadline, the request deadline and `recv_body_timeout`.
  - `Debug` of `Response` and `ResponseHead` scrubs configured and per-request secrets.
  - `testing::add_bundled_test_root` (feature `testing`) lets a test trust a throwaway CA under
    `TrustRoots::Bundled`; new dev-dependencies `rcgen` and `rustls` (both already in the lock).
- **Third independent read** (docs/audits/2026-10-11-astra-rightkit-http-second-read.md; fixes on
  the 0.3.0 source, which that review ruled not publishable until these landed):
  - Downloads, Windows: `.part` is opened with `FILE_FLAG_OPEN_REPARSE_POINT` and the opened handle
    is refused if it is a symlink, junction or other reparse point, before any write or truncate,
    so a link swapped in between the path check and the open is never followed. The path-versus-handle
    check before the rename now compares volume serial and file index on Windows (it compared nothing
    there before). The destination lock file is opened the same way.
  - Downloads, concurrency (behaviour change): each download takes an exclusive OS lock (`flock` /
    `LockFileEx`, through `std::fs::File::try_lock`; no new dependency) on `<dest>.lock` before it
    opens `.part` and holds it until the rename or cleanup. A call that finds the lock held fails
    at once with `DownloadError::Io` and sends no request; it does not wait. `<dest>.lock` is a
    zero-byte file that stays beside `dest` after the download (deleting it would let two holders lock
    different files). A filesystem that cannot lock now fails the download instead of running
    unlocked. Completion also compares the byte count with the real length of the file, so a writer
    that ignored the lock cannot make a 200-byte file pass `expected_bytes = 100`: that is
    `DownloadError::LengthMismatch` and the `.part` is deleted.
  - Downloads, wire sniffer (behaviour change): "head not observed" is no longer read as "no
    `Content-Encoding`". A download whose response head was not observed is refused with
    `HttpError::Stream`. The sniffer accepts the head syntax `ureq` accepts: blank lines before the
    status line, bare `\n` line endings, several `Content-Encoding` lines (any non-`identity` coding
    refuses the download), and a final head already buffered behind a 1xx head.
  - Async: a `BodyStream` (and `SseStream`) whose request has a `CancelToken` re-arms a 25 ms timer
    while it is pending, so `cancel()` now ends a stream that is waiting on a server that sent
    headers and then nothing. Before, only socket activity polled it again.
  - Blocking client: the retry sleep is clamped to what `Request::deadline` has left after the failed
    response was drained, as the async client already did, so a stalled error body no longer adds a
    full backoff past the deadline. `RetryEvent::delay` still reports the unclamped delay.
  - Redirects: with `https_only` set, a redirect to `http` is now reported as `HttpsRequired { redirect:
    true }` in both clients. The address policy's always-on HTTPS-to-HTTP refusal used to run first
    and report `AddressDenied`, which hid the rule the caller had set and contradicted the
    documented behaviour (CI: `tls_local` `*_https_only_refuses_an_https_to_http_redirect`). The
    redirect is refused either way and nothing is sent to the `http` URL.
  - Tests: `tests/second_read.rs`, `tests/second_read_async.rs`, unit tests in `src/download.rs` and
    `src/wire.rs`, and a TLS download in `tests/tls_local.rs`. `rightkit-oauth`'s
    `token_post_is_not_replayed_after_a_5xx` now uses `testing::loopback_config()` (the default
    policy refused its mock, so it never reached the retry path) and asserts the 503 and one request.
  - API: no public signature changed. `DownloadError` gains no variant, so the busy-destination
    refusal is a `DownloadError::Io` whose text says a download is in progress.
- **Not done:** `ProxyMode::System` (OS proxy settings, gap ledger row 110 item 3) is not in this release: it needs per-scheme proxy selection per request, which the single-proxy client does not have.

- **Security.** `Retry-After` values too large for a `Duration` no longer panic either client (also
  present in published 0.2.5); they saturate and then meet `max_retry_after`. Legacy IPv4 spellings
  (`127.1`, `0177.0.0.1`, `0x7f.0.0.1`, `2130706433`) are canonicalised the way `inet_aton` does before
  classification, in both clients, through proxies and on redirects. The local-use NAT64 space
  `64:ff9b:1::/48` (RFC 8215) is inspected for embedded IPv4 in every RFC 6052 layout
  (`netpolicy::embedded_v4s`). Cloud metadata is now an unconditional block that no hook can allow:
  link-local, `fd00:ec2::/32`, `100.100.100.200`, `168.63.129.16`, `192.0.0.192`, and IPv6 forms that embed
  them. The trusted proxy endpoint is exempt only for the real proxy connection (host, port and a call
  that goes through the proxy); `NO_PROXY` destinations and any other port get the full check. An `http`
  proxy named by host is pinned by `AsyncClient` to the address it resolved at construction; an `https`
  proxy named by host is vetted like a destination there. HTTPS to HTTP redirects are refused by default
  (`AddressPolicy::allow_insecure_redirects` opts in; `allows_redirect` exposes the rule).
- **Host spelling.** `parse_host_ip` and `parse_legacy_ipv4` are public, with `ParsedHostIp`,
  `HostIpKind` and an `is_canonical` flag, so other transports can guard with the same parse (the
  `inet_aton` rules: one to four parts, decimal, octal or hex, the last part filling the rest). The public-web
  policy now refuses every non-canonical IPv4 host spelling (`127.1`, `0177.0.0.1`, `0x7f.0.0.1`,
  `2130706433`, `1.1.1.1.`) outright with `AddressDenied`, in both clients, through proxies and on
  redirects, instead of normalising it: parsers disagree about these spellings and a mismatch is an SSRF
  bypass. `AddressPolicy::allow_legacy_ipv4()` opts out (the address classes still apply). No legitimate
  URL spells a host this way, so refusing by default breaks nothing real.
- **Proxy.** `Client::proxy_diagnostic()` and `AsyncClient::proxy_diagnostic()` report the proxy
  endpoint (no credentials), whether it came from the environment, and that it is exempt from the
  destination class check only because the caller or environment configured it. A destination denied
  through a proxy says that only names and literals can be checked.
- **Downloads.** `.part` is created exclusively or opened with `O_NOFOLLOW`; an existing link is removed,
  never followed. The URL policy runs before the filesystem is touched. The file is fsynced, hashed
  through the same handle and checked against the path just before the rename, and the directory is
  fsynced. The resume validator belongs to the resource that supplied the prefix; a reconnect that lands
  on another resource restarts. `Content-Range` is parsed strictly and must agree with `Content-Length`.
  Bodies past `expected_bytes`, the declared range or the declared length stop the download. Responses
  whose framing shows transparent decoding are refused. Error bodies are read with cancellation.
- **Redaction.** `Request`'s `Debug` hides credential-named headers and every recorded secret inside any
  header value. `Response` and `ResponseHead` `Debug` redact `Set-Cookie`, credential headers and URL
  headers and print the body length only. `DownloadError` and JSON decode errors are scrubbed, and JSON
  errors name only the category and position, never body text.
- **Async parity.** `AsyncClient` now honours `Request::cancel`, `Request::deadline`,
  `Request::max_response_bytes` and `Request::headers_only`, like `Client`.
- **Timeouts.** Name lookup for blocking calls runs under the connect timeout (default 10 s), including
  streams with no global timeout. Retry and error-body drains stop on cancellation.
- **Not changed.** `ureq` logs the full URL only at TRACE, which its documentation describes as
  unredacted wire logging; at DEBUG it prints the URL without its query. The crate cannot intercept the
  global `log` filter, so do not enable TRACE for a process that carries secrets in URLs.


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
  `DownloadError`. Bytes go to a partial file (`<dest>.part` in this entry's tree; now in `.rk-download`),
  which is fsynced and renamed on success. Resume uses
  `Range` with `If-Range` on the first ETag seen in the call. `206` appends, `200` restarts, and a
  `416` restarts (an earlier tree verified the file on a `416` whose total equalled the local length). Mid-body failures reconnect up
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
