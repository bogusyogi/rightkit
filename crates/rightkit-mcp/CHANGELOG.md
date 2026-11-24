# Changelog

## 0.10.1

- Depends on `rightkit-sandbox` 0.7.0. `ProcessLaunchAdapter`, `ProcessLaunchContext` and `ManagedChild` in the public launcher APIs (`load_with_process_launcher`, `LspMcpBridge::with_process_launcher`, `with_launch_context`) are now the 0.7.0 types; hosts must depend on the same `rightkit-sandbox` version.

## 0.10.0 (2026-11-16)

Breaking for hosts that relied on the implied OS keyring: the feature `agent_mcp` no longer enables `agent_mcp-os-keyring`. A host that wants stored MCP OAuth tokens in the OS credential store names `agent_mcp-os-keyring` itself (it also enables `rightkit-llm/agent_providers-os-keyring`); without it there are no stored OAuth tokens to attach or refresh and the OAuth login functions are not built. The `rightkit-sandbox` dependency now builds with `default-features = false` and only `execution`. Requires `rightkit-llm` 0.7.0 and `rightkit-agent-config` 0.1.3.

## 0.9.1

- Requires `rightkit-llm` 0.6.0 for the `agent_mcp` credential-name defaults (0.9.0 required llm 0.5.0, which cannot share a build with the 0.6 host settings). No other change.

## 0.9.0

`agent_mcp` now carries CodeRight's `crates/mcp` (feature `agent_mcp`), so CodeRight can delete its own copy. Breaking for dependents: `StdioConfig`, `HttpConfig` and `SseConfig` gain public fields (`options`, and on `HttpConfig` the bearer and header fields), `McpConfigFile` gains `http_bearer` and its `Debug` output is redacted, so a struct literal of any of them no longer compiles; the serde shapes are unchanged for existing files. The agent MCP tests are not carried over (owner rule D7).

New types and functions, with CodeRight's serde names and defaults:

- `McpServerOptions` (flattened into every server entry, all optional and omitted when default): `startup_timeout_sec`, `tool_timeout_sec` (seconds; non-positive, non-finite or out-of-range values are ignored with a warning), `required` (default `false`), `enabled_tools` (exact names or `*` globs; `[]` means no tools), `disabled_tools` (wins over `enabled_tools`), `expose` (`McpExpose`: `direct`, `deferred` (default), `hidden`). `McpServerConfig::options()`, `McpServerOptions::tool_filter()`.
- `HttpConfig`: `bearer_token_env_var` (the variable holding the bearer; wins over everything else), `bearer_token_command` (`BearerTokenCommand`, a string split on whitespace or an argument array; first output line is the token; user config only, run directly with no shell, at most 10 s and 16 KiB, needs feature `agent_mcp-host-env`), `http_headers` (alias `headers`) and `env_http_headers`. Only `Authorization: Bearer` is honoured; any other header is a load error. `resolve_http_bearer`, `resolve_http_bearer_with_command`.
- `McpConfigFile::from_json_lenient` skips a server entry that does not parse (one message each) and infers a missing `type` (`command` is stdio, `url` is http); the strict `Deserialize` infers `type` as well. `McpConfigFile::http_bearer` is filled by the loader and never written to a file. `HttpConfig`, `BearerTokenCommand` and `McpConfigFile` print no secrets in `Debug`.
- `PlaceholderScope` (`Repository`, `User`) and `expand_server_env_in_scope`: `${VAR}` and `${VAR:-default}` now also expand in `command`, `args` and `url`, not only `env`. Licence and device names and the host's daemon secrets never expand; a repository-supplied entry additionally cannot read a credential name, an entry from the operator's own file (user `mcp.json`, `<PREFIX>_MCP_CONFIG`) can. `expand_server_env` and `expand_config_env` keep the strict repository rule. Bug fix taken from CodeRight: a user-scope file that replaces a workspace entry of the same name removes that entry from the workspace trust gate.
- `McpPromptDef`, `McpTransport::list_prompts` / `get_prompt` (defaults: no prompts), `McpManager::list_prompts` / `get_prompt`.
- `McpManager`: `register_server_options`, `required_server_errors`, `note_required_failure` (a `required` server that fails to start, is gated or has a stale script is reported), per-server tool filter and timeouts, `direct_tools`, `wait_for_server`, `is_listed_read_only`; `limits::MCP_EXPLICIT_STARTUP_BOUND` (10 s, a call or a named listing waits longer for a cold server than discovery does) and `limits::MCP_OAUTH_REFRESH_BOUND` (10 s). Kept as before: `connection_states`, `default_mcp_servers`.
- Transport: `HttpTransport::with_configuration_error`, `with_timeouts` on every transport; `initialize` offers `2025-06-18` as well (`MCP_CLIENT_VERSIONS`); a failed connect is not retried for 30 s; a closed connection is reconnected on the next call; a managed server's stderr is drained (a full pipe no longer stalls it); `tools/list` paging stops at 64 pages (the tools read so far are kept) and fails on a repeated cursor; image, audio, resource and unknown content blocks become a short text line; an elicitation request without `mode` is a form.
- `lsp_bridge`: frames are parsed from a buffer kept on the bridge (a read cancelled by a timeout no longer desyncs the stream), header sizes are bounded, diagnostics wait up to 10 s for the first publish and report a broken read as an error, a framing error tears the bridge down and `workspace/configuration` and `workspace/applyEdit` requests from the server get replies.

Bug and safety fixes taken from CodeRight, which change behaviour for every host: the pool identity of a stdio server includes the launcher, launch root and sandbox settings and the identity of an HTTP server its resolved bearer and timeouts (a session never leases a process another session started with other confinement); stored OAuth tokens are refreshed side by side, each bounded by 10 s; the served-invocation audit keeps the newest 1024 rows; the trust presets `local_process`, `local_dev` and `remote_full` (written by `mcp add`/`install`) count as `trusted` for the per-server tool policy, where they were read as untrusted before.

Host settings (each once at start-up, before any config is parsed; `false` means one was already set). Defaults are RightKit's and unchanged; CodeRight's calls follow:

- Workspace state folder and `<PREFIX>_MCP_CONFIG`, `<PREFIX>_DISABLE_OS_KEYCHAIN`, `<PREFIX>_DISABLE_DEFAULT_MCP`: from `rightkit_agent_config::set_state_dir_name` (default `.rightkit`) and `set_env_prefix` (default `RIGHTKIT`). CodeRight: `set_state_dir_name(".coderight")`, `set_env_prefix("CODERIGHT")`. Requires `rightkit-agent-config` 0.1.2.
- `set_config_dir_segment` (default `rightkit-agent`): unchanged from 0.8.0. CodeRight: `set_config_dir_segment("coderight")`.
- `set_client_name` / `client_name` (`DEFAULT_CLIENT_NAME` = `rightkit-agent`): `clientInfo.name` and the name of the MCP client, the default OAuth `clientId` of a server entry that names none, and the command named in the "run `<name> trust`" hint. CodeRight: `set_client_name("coderight")`.
- `set_client_version` / `client_version` (default: this crate's version): `clientInfo.version` of the `initialize` request. CodeRight sent `0.1.0`: `set_client_version("0.1.0")`.
- `set_keyring_service` / `keyring_service` (`DEFAULT_KEYRING_SERVICE` = `rightkit-agent`): the OS keychain service of stored OAuth tokens; tokens stored under another name are not found. CodeRight: `set_keyring_service("coderight")`.
- `set_credential_env_names(fn(&str) -> bool)` (default `rightkit_llm::agent_providers::host_credentials::is_credential_env_name`): the credential names a repository entry may not read. CodeRight: `set_credential_env_names(coderight_providers::host_credentials::is_credential_env_name)`.
- `set_never_expanded_env_names(fn(&str) -> bool)` (default: `rightkit_agent_config::licence_env::is_licence_env_name` or a `DAEMON_SECRET_NAMES` entry of `rightkit_llm`): names no scope may read. CodeRight: a function returning `coderight_config::licence_env::is_licence_env_name(name) || coderight_providers::host_credentials::DAEMON_SECRET_NAMES.contains(&name)`.
- Dependencies: feature `agent_mcp` adds `rightkit-llm` 0.5.0 (`agent_providers`, as `rightkit-hooks` already does) for the credential-name rules.

Not changed, by design: the error text of an unsupported HTTP header still names `rightkit-mcp 0.7.2` (CodeRight's literal); `MCP_CLIENT_VERSIONS` is not re-exported (as in CodeRight).

## 0.8.0

- Depends on `rightkit-agent-engine-protocol` 0.4.0 (additive events and fields, see its changelog). Breaking for dependents: the crate exposes protocol types in its public API, so those types are now the 0.4 ones.
- `agent_mcp::set_config_dir_segment` / `config_dir_segment` (default `rightkit-agent`, `DEFAULT_CONFIG_DIR_SEGMENT`): the folder under `~/.config` that holds the user's `mcp.json` and the persisted trust decisions `mcp_trust.json` (CodeRight passes `coderight`). Part of the sync with CodeRight's `crates/mcp`, which is not finished: the remaining differences are recorded for the next pass, and nothing else in `agent_mcp` changed.

## 0.7.7

- Breaking for dependents: a dependency requirement was raised to a semver-breaking version.

## 0.7.6 (unpublished)

- Requires `rightkit-agent-engine-protocol` 0.2.0, whose approval records carry the approvals core's `Binding` (mandatory digest) instead of an optional `payload_digest`. Decision 4 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.

## 0.7.5

- Publishes the source changes made since 0.7.4 was published under the same version number (src/agent_mcp/config.rs, src/agent_mcp/manager.rs, src/agent_mcp/mod.rs, src/agent_mcp/oauth.rs, src/agent_mcp/transport.rs, src/agent_mcp/trust_gate.rs).

## 0.7.4 (unpublished)

- Removed `tests/e2e.rs` and the `tempfile` dev-dependency: it ran the crate's own client against the crate's own example server with a fixed-token auth stand-in, not a real installed app or external service (Adrian, 2026-11-03: "My decision was no unit tests. Only e2e."). `examples/example_server.rs` stays as a documented example.
- Issue #1245: `McpManager::connection_states()` (feature `agent_mcp`) reports each registered server as an `EngineSource` (`Connected { tools }`, `Starting`, `Failed`) from the manager's own state: the last completed tool listing and any listing still running. It never starts a server, lists, or calls a tool, and keeps no error text. A never-listed server is `Starting` (lazy connect). `Disabled` and `NeedsAuth` are not reported because the manager has no such state. New `engine_source_status(&[EngineSource])` classifies engine-held sources without a trust store.
- Issue #421: no change to the server host; per-version batch policy and the bounded call already ship. `ToolProvider` is served from a `Toolbox` by `rightkit-agent-tools` `ToolboxProvider`.
- Issue #708: `TrustStore::try_load_migrating` gives a `records` document that does not parse as a native store to `migrate` (for example another store's camelCase records). Native files load exactly as before; when `migrate` returns `None` the native parse error is returned as `InvalidData`.
- Issue #1245: new `session_status` module: `session_source_status(configured, discovery, trust)` classifies each configured tool source for one session as `ready`, `no-tools`, `not-mounted`, `failed`, `needs-auth`, `pending`, `disabled`, `discovery-unavailable` or `discovery-failed` from the engine's own `SessionDiscovery`. Trust is checked first; no server is started, no tool is called, and no engine error text is carried.

## 0.7.3 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.7.2 (unreleased)

- Feature `agent_mcp`: agent MCP client manager, server config and tool bridging moved from CodeRight `coderight-mcp`.
- `agent_mcp` sync: new `limits` module (startup, call, request and elicitation limits; replaces `DEFAULT_MCP_STARTUP_BOUND` / `DEFAULT_MCP_CALL_DEADLINE`); OAuth tokens record their issuing `token_url` and attach or refresh only for the origin that issued them (`url_origin`, `McpOAuthTokens::issued_for_server`, `may_refresh_with`); stdio servers whose script is gone are not spawned (`stdio_unreachable_reason`); `approve_workspace_stdio` and `pending_workspace_stdio` in the trust gate. Removed: the built-in default context7 server (`default_mcp_servers`, `RIGHTKIT_DISABLE_DEFAULT_MCP`).

## 0.7.1 (unpublished)

- Dependency: `ureq` 3.4 -> 3.5 (minimum version); no source change. (rightkit#1284)

## 0.7.0 (2026-10-17)

- **New: `ServerHost::with_request_transform`** (rightkit#283 item 2). Like `with_response_transform`, but the closure gets a `TransformContext` whose `request()` is the request frame it answers (one item of a batch), for per-request rewrites such as `_meta` or protocol-version gating. `TransformInfo` keeps its two fields (`method`, `transport`), so existing closures and destructuring still compile. `TransformContext`'s `Debug` omits the request contents. Not built or run locally.

- **Fixed: strict validation before cancellation.** Concurrent stdio now validates a `notifications/cancelled` frame before acting on it, so a frame Strict rejects (for example `id: null`) gets `-32600` and cancels nothing.

- **Fixed: `LeasedPool<K, dyn Trait>`.** `LeasedPool` and `Lease` accept unsized values (`V: ?Sized`), so `Arc<dyn McpTransport>` can be pooled.

- **New: strict request validation** (rightkit#283 item 3). `ServerHost::with_request_validation(RequestValidation::Strict)` replies `-32600 Invalid Request` (with the request's `id` when it is a string or number, else `null`) to a non-object frame, an object without a string `method`, an `id` that is not a string or number (`null` stays valid only with `IdPolicy::NullResponse`), `params` that is present but not an object, and an empty batch. Each batch item is validated on its own; no handler runs for an invalid frame; all transports and `dispatch*` apply it. A frame with no `method` is invalid even if it looks like a client response, because a server host never sends requests. Default `RequestValidation::Lenient` keeps the old silent drop. Held on the host, not in `ServerOptions`, so no struct gains a field. Not built or run locally.

- **New: whole-number progress** (rightkit#283 item 4). `CallHandle::notify_progress_number(serde_json::Number, Option<Number>, Option<&str>)` writes `1`, not `1.0`. `notify_progress(f64, ..)` is unchanged. Not built or run locally.

- **New: batch policy per protocol version** (rightkit#421). `ServerHost::with_batch_policy_for_version("2025-06-18", BatchPolicy::Reject)` (repeatable) overrides `ServerOptions::batch_policy` for connections that negotiated that version: on stdio the version answered by `initialize`, on HTTP the `MCP-Protocol-Version` header, for `dispatch*` `RequestContext::metadata["protocolVersion"]`. An unknown or unlisted version, and a batch before `initialize`, use the global policy. Not built or run locally.

- **New: `LeasedPool<K, V>`** (rightkit#549). A generic pool of `Arc<V>` for any key: `acquire(key, create) -> Lease<V>` with refcounted leases, `current_epoch` / `bump_epoch` / `bump_epoch_on_reload` / `is_current`, `evict_idle(min_idle)`, `remove`, `snapshot()` (`EntrySnapshot`, `EntryState`), single-flight connect (concurrent first use shares one `create`; waiters share its value or error), `LeasedPoolConfig { reconnect_cooldown, cache_lifetime }`, and a value connected across an epoch bump is returned but not cached. A panicking `create` releases the key. `ConnectionPool` is unchanged. Sync (`Condvar`); an async leader/waiter variant is not provided. Not built or run locally.

- **New: trust gate helpers** (rightkit#550). `TrustStore::gate(&ServerSource, name, &ServerSpec)` and `gate_stdio(workspace, name, &ServerSpec)` decide before spawn (a non-stdio spec is `Pending` through `gate_stdio`); `credential_write_allowed(&ServerSource, name, &ServerSpec)` is true only for a user server or an exact `Approved` workspace record; `pending_approval(..)` gives what to show; `approve_with_fingerprint` / `deny_with_fingerprint` / `decide_with_fingerprint` bind a decision to a caller-supplied fingerprint (empty is refused); `TrustStore::try_load_migrating(path, migrate)` loads a file in another format through a hook returning `Vec<ImportedTrust>` (`None` is `InvalidData`, never an empty store; nothing is written until `save`, which stays atomic). Imported foreign fingerprints match only through `decide_with_fingerprint`; `decide` and the pool still compare the RightKit v2 fingerprint, so they read `Pending` (fail closed). Not built or run locally.

## 0.7.0

- **New: `ClientExtensions::allow_loopback`** (rightkit#165). 0.6.0 put HTTP MCP on the public-web address policy, which refuses loopback, and the only override (`address_policy`) is behind `testing`, so a user-configured `http://localhost` MCP server was refused in production. Setting `allow_loopback` lets that connection reach loopback addresses when the endpoint host is literally `localhost`, `127.0.0.1` or `[::1]`. A name that resolves to loopback, and private, link-local and metadata addresses, stay refused. Off by default, so behaviour is unchanged unless a host opts in. **Breaking only for struct literals** that list every `ClientExtensions` field without `..Default::default()`; hence 0.7.0.

- **New: handlers can return pre-serialised JSON** (rightkit#174). `RawJson` (validated once by `RawJson::new` / `from_slice` / `from_serialize`, or `From<Box<serde_json::value::RawValue>>`) holds exact JSON text. `ToolResult::raw_json(raw)` puts it in both the text block and `structuredContent`; `RawJson::to_value()` gives a stand-in `Value` for any other payload a handler builds (`ToolProvider::call_tool`, `AsyncRequestHandler`, `structured`). The server host writes every reply (stdio line, HTTP body, response-size check) through the new `to_wire_string`, which emits each `RawJson` byte for byte: key order, number spelling (`1.0` vs `1`, `1E2`) and unicode escapes are not touched. Replies without a `RawJson` are identical to before. A caller that serialises `dispatch*` output itself must use `to_wire_string` instead of `Value::to_string`. `serde_json` gains its `raw_value` feature. Additive; defaults unchanged. Not built or run locally.

- **New: progress and cancel notifications during tool calls** (rightkit#238). `ServerHost::serve_stdio_concurrent[_on]` serves stdio with a reader thread that keeps reading while a request runs: it answers `ping` at once, turns `notifications/cancelled` (`params.requestId`) into a signal on that in-flight request, and queues the rest for in-order handling. A handler receives a request-scoped `CallHandle` through the new `ToolProvider::call_tool_with_handle` / `call_with_handle` (defaults delegate to `call_tool` / `call`, so existing providers are unchanged): `progress_token()` (the request's `params._meta.progressToken`), `is_cancelled()` / `cancellation()` (a `CancellationToken`), `notify_progress(progress, total, message)` (emits `notifications/progress` with that token; `false` and nothing sent without a token) and `notify(method, params)`. Per the MCP spec a cancelled request gets no reply; `ServerHost::with_cancelled_response(CancelledResponse::Send)` replies anyway. On HTTP and `dispatch*` the handle is inert (never cancelled, notifications return `false`). The blocking `serve_stdio*` loops are unchanged. Not built or run locally.

- **New: control over the initialize result** (rightkit#238). `ServerHost::with_initialize_overrides(InitializeOverrides { server_title, server_info, instructions, capabilities, default_protocol_version })` adjusts the generated result without replacing negotiation (`ServerOptions::initialize_result` still replaces everything and wins). `capabilities` and `server_info` are deep-merged over the generated values (for example `tools.listChanged: false`, `resources {subscribe, listChanged}` even with no resources), `server_title` sets `serverInfo.title`, and `default_protocol_version` is answered when the client's version is not negotiable (default stays `PROTOCOL_VERSION`; Ember sets `2025-06-18`). Not built or run locally.

- **New: response hook** (rightkit#238). `ServerHost::with_response_hook(|&ResponseInfo { method, response, transport }| ..)` sees every outgoing response (results, errors, parse errors, batch rejection; one call per response frame; none for notifications or replies suppressed for a cancelled request) read only, for logging or metrics. It cannot change the reply, and a panic in it is caught. Not built or run locally.

- **New: response transform** (rightkit#238 item 3). `ServerHost::with_response_transform(|&TransformInfo { method, transport }, result: &mut Value| ..)` rewrites the `result` of a successful response before it is observed and written, for example to add Ember's `resultType`, `ttlMs` and `cacheScope`. It gets only the result value (a working copy), so `id` and `jsonrpc` cannot be touched and framing cannot break; error replies, parse errors and notifications skip it. It runs before the read-only response hook, which therefore sees the transformed reply, and applies per item in a batch. A panic falls back to the untransformed result (no partial edits). Opt-in; replies are unchanged without it. Not built or run locally.

- **Changed: unknown resource URI is JSON-RPC -32002** (rightkit#238 item 4). New `McpError::ResourceNotFound(uri)`, served as code `-32002`, message `Resource not found`, `data: {"uri": ..}` per the MCP resources spec. The default `ToolProvider::read_resource` returns it, and a provider that still returns `ToolNotFound` from `resources/read` gets the same wire error. Unknown tools and prompts keep `-32602`. Additive variant (`McpError` consumers with an exhaustive match need an arm). Not built or run locally.

- **New: client-side server notifications** (rightkit#243). The client reader used to drop every server-initiated notification, so `notifications/tools/list_changed` (and resources and prompts `list_changed`, `progress`, `message`) never reached the app. `ClientOptions::on_notification(|&ServerNotification { server, method, params }| ..)` (field `on_notification: Option<NotificationHandler>`) delivers each notification (a message with a `method` and no `id`) in arrival order, on stdio, plain streamable HTTP, `advanced-http` streamable HTTP and legacy SSE. A notification that arrives while a request waits for its response is delivered and is never taken for the response; server-to-client requests (`ping`, `elicitation/create`) are handled as before. The callback runs on the reader (stdio, legacy SSE) or on the calling thread (HTTP), so it should only forward the value (for example to a channel) and must not call back into the same client; a panic in it is caught. With no subscriber they are ignored, exactly as before. New exported types `ServerNotification` and `NotificationHandler`. `ClientOptions` gains a field, so **breaking only for struct literals** that omit `..Default::default()`. Not built or run locally.

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
