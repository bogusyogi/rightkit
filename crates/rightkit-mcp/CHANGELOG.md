# Changelog

## 0.5.3

- New default-on feature `advanced-http` gates `tokio`, `rightkit-http[async]`, `http` and
  `futures-util` (the `ClientExtensions` HTTP transport: streamable HTTP with extensions and
  legacy SSE). Default builds are unchanged. Stdio-only consumers can build with
  `default-features = false`; plain blocking HTTP (`ureq`) and the server host stay available,
  and `McpClient::connect_with_extensions` on an `ServerSpec::Http` server returns
  `McpError::Config` without the feature.
- `CancellationToken` no longer uses `tokio::sync::Notify`: it is a runtime-agnostic waker list
  built on std. API and semantics (permanent, wakes every waiter) are unchanged.
- `auth::Secret` is now a re-export of `rightkit_http::Secret` (one redacted-secret type for the
  net crates). `Debug` prints `Secret([redacted])`; `Display`, `PartialEq`, `Eq`,
  `is_empty` and `From<String>` are added.
- Fingerprints hash with `rightkit_fs::digest` instead of a direct `sha2` dependency; output is
  unchanged.
- Package: ship `LICENSE-MIT` / `LICENSE-APACHE` (declared `MIT OR Apache-2.0`) and `CHANGELOG.md`.
- JSON-lines stdio framing still duplicates `rightkit-framed-sidecar`; commented, to be
  converged on that owner crate later.

## 0.5.2

- Internal: the HTTP/SSE client transport now runs on `rightkit-http` (`AsyncClient::stream_any`,
  one attempt, no redirects) instead of a direct `reqwest` dependency; `tokio` no longer enables
  `net`. Wire behaviour, cancellation and response bounds are unchanged (User-Agent is now
  `rightkit-mcp/<version>`).

## 0.5.1
- Security: the stdio server host no longer reads unbounded input lines. Each input frame is
  capped by `ServerHost::with_max_stdio_frame_bytes` (default `DEFAULT_MAX_STDIO_FRAME_BYTES`,
  8 MiB, the HTTP body cap). An oversize line is drained to its terminator and answered with a
  JSON-RPC Invalid Request error (`-32600`, id `null`); serving continues, including under
  `StdioInputMode::FailFast`. Invalid UTF-8 still ends serving with `InvalidData`.

## 0.5.0 — staged, unpublished

- Breaking (0.x minor): new `McpError::RandomUnavailable` variant; exhaustive matches need one arm.
- Security: a caller-supplied empty loopback token is refused instead of accepting empty bearers.

- Security: the loopback HTTP server's default bearer token no longer ignores RNG failures.
  A failed or all-zero random fill now fails closed: `ServerHost::serve_http` returns the new
  `McpError::RandomUnavailable` and binds no listener. Previously the discarded error left an
  all-zero token that anyone on loopback could guess.
- HTTP session ids use the same fail-closed generator. If none can be generated, `initialize`
  returns HTTP 500 and issues no session.
- Generated tokens are checked to be 48 hex characters and not all zeros.
- Adds `McpError::RandomUnavailable(String)`. Exhaustive `match` on `McpError` needs one new arm.

## 0.4.0 — staged, unpublished

- Bind the stdio launch configuration by value into the approval fingerprint: environment
  variable values now count, not only names. A changed `PATH` or `NODE_OPTIONS` is `Pending`.
- Fingerprint canonical form is version 2. All 0.3.x approvals are invalid; re-approve each
  workspace server once. Only the SHA-256 digest is stored; env values never reach the file.
- `PendingApproval` still exposes env variable names only.
