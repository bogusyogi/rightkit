# Changelog

## 0.7.0

- **New: `ClientExtensions::allow_loopback`** (rightkit#165). 0.6.0 put HTTP MCP on the public-web address policy, which refuses loopback, and the only override (`address_policy`) is behind `testing`, so a user-configured `http://localhost` MCP server was refused in production. Setting `allow_loopback` lets that connection reach loopback addresses when the endpoint host is literally `localhost`, `127.0.0.1` or `[::1]`. A name that resolves to loopback, and private, link-local and metadata addresses, stay refused. Off by default, so behaviour is unchanged unless a host opts in. **Breaking only for struct literals** that list every `ClientExtensions` field without `..Default::default()`; hence 0.7.0.

- **New: handlers can return pre-serialised JSON** (rightkit#174). `RawJson` (validated once by `RawJson::new` / `from_slice` / `from_serialize`, or `From<Box<serde_json::value::RawValue>>`) holds exact JSON text. `ToolResult::raw_json(raw)` puts it in both the text block and `structuredContent`; `RawJson::to_value()` gives a stand-in `Value` for any other payload a handler builds (`ToolProvider::call_tool`, `AsyncRequestHandler`, `structured`). The server host writes every reply (stdio line, HTTP body, response-size check) through the new `to_wire_string`, which emits each `RawJson` byte for byte: key order, number spelling (`1.0` vs `1`, `1E2`) and unicode escapes are not touched. Replies without a `RawJson` are identical to before. A caller that serialises `dispatch*` output itself must use `to_wire_string` instead of `Value::to_string`. `serde_json` gains its `raw_value` feature. Additive; defaults unchanged. Not built or run locally.

- **New: progress and cancel notifications during tool calls** (rightkit#238). `ServerHost::serve_stdio_concurrent[_on]` serves stdio with a reader thread that keeps reading while a request runs: it answers `ping` at once, turns `notifications/cancelled` (`params.requestId`) into a signal on that in-flight request, and queues the rest for in-order handling. A handler receives a request-scoped `CallHandle` through the new `ToolProvider::call_tool_with_handle` / `call_with_handle` (defaults delegate to `call_tool` / `call`, so existing providers are unchanged): `progress_token()` (the request's `params._meta.progressToken`), `is_cancelled()` / `cancellation()` (a `CancellationToken`), `notify_progress(progress, total, message)` (emits `notifications/progress` with that token; `false` and nothing sent without a token) and `notify(method, params)`. Per the MCP spec a cancelled request gets no reply; `ServerHost::with_cancelled_response(CancelledResponse::Send)` replies anyway. On HTTP and `dispatch*` the handle is inert (never cancelled, notifications return `false`). The blocking `serve_stdio*` loops are unchanged. Not built or run locally.

- **New: control over the initialize result** (rightkit#238). `ServerHost::with_initialize_overrides(InitializeOverrides { server_title, server_info, instructions, capabilities, default_protocol_version })` adjusts the generated result without replacing negotiation (`ServerOptions::initialize_result` still replaces everything and wins). `capabilities` and `server_info` are deep-merged over the generated values (for example `tools.listChanged: false`, `resources {subscribe, listChanged}` even with no resources), `server_title` sets `serverInfo.title`, and `default_protocol_version` is answered when the client's version is not negotiable (default stays `PROTOCOL_VERSION`; Ember sets `2025-06-18`). Not built or run locally.

- **New: response hook** (rightkit#238). `ServerHost::with_response_hook(|&ResponseInfo { method, response, transport }| ..)` sees every outgoing response (results, errors, parse errors, batch rejection; one call per response frame; none for notifications or replies suppressed for a cancelled request) read only, for logging or metrics. It cannot change the reply, and a panic in it is caught. Not built or run locally.

## 0.6.0

- **Runtime budget declared as `held-resource`.** `[package.metadata.rightkit] continuous = "held-resource"` with a `resources` table: `McpClient` (child process, reader thread), `AdvancedHttp` (tokio runtime and SSE reader task, `advanced-http` only), `ConnectionPool` (bounded set of clients) and `ServerHost::serve_http` (loopback listener and accept thread). All are owned by a value the caller holds and end on `Drop`; nothing runs at link time. Metadata only; no code change.

- **BREAKING (0.6.0): default features are now empty (`default = []`).** 0.5.x built `advanced-http` by default. A consumer that connects with `ClientExtensions` over HTTP, or uses legacy SSE, must name `features = ["advanced-http"]`; without it `ServerSpec::Http` with `ClientExtensions` returns `McpError::Config`. Stdio and plain blocking HTTP need nothing. Naming `advanced-http` restores the 0.5.x dependency tree (tokio, http, futures-util, `rightkit-http/async`); the empty default drops them.

- **BREAKING (0.6.0): `McpClient::connect`, `connect_with_extensions` and `connect_with_extensions_cancellable` are trust gated.**
  Each now takes `source: &ServerSource, trust: &TrustStore` after the server name. A `ServerSource::Workspace`
  server that is not `Approved` in `trust` for exactly that spec returns `McpError::NotApproved` / `McpError::Denied`
  before anything is spawned or contacted; `ServerSource::User` is exempt, as in `ConnectionPool`. `ConnectionPool::get`
  keeps its own gate and uses a crate-private ungated constructor. Migration: pass `&ServerSource::User,
  &TrustStore::new()` for a user-configured server. Call sites in apps: CodeRight
  `engine/crates/mcp/src/transport.rs:130` and `engine/crates/engine/src/sentinel_client.rs:56`
  (`connect_with_extensions`). Not built or run locally (compile unverified, CI pending).
- **Protocol 2025-06-18 is accepted (99724405).** `SUPPORTED_VERSIONS` now lists `2025-06-18` first. `PROTOCOL_VERSION`,
  the default and the fallback, stays `2025-03-26`.
- **Opt-in mutating-tool check (aced383e).** `ToolDescriptor::is_mutating()` is true unless the tool is annotated
  `readOnlyHint: true` (matching `ReadOnlyPolicy`). `mutating_tools(&[ToolDescriptor]) -> Vec<String>` lists the
  mutating tool names for an adopter's test. Nothing in the server calls it, so existing providers are unchanged.
- **Trust store: `TrustStore::try_load`, and `save` is atomic.** `load` returned an empty store for a missing file and
  for any other failure: a file that could not be read (on Windows an open can be refused for a moment while another
  process replaces the file) or did not parse. The caller's next `save` then wrote that empty store over every
  recorded decision. `try_load` returns an empty store only for a missing file and an `io::Error` otherwise
  (`InvalidData` for a parse failure). `load` is deprecated and unchanged in behaviour, so nothing breaks; callers
  should move to `try_load` and decide what an error means for them. `save` now writes through
  `rightkit_fs::atomic_write` (temp file, flush, rename); it was a plain `fs::write`, which a reader or a crash could
  catch half written. The `rightkit-fs` dependency gains its `fs` feature. Not built or run locally.

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
