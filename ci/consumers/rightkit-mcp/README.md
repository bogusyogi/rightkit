# rightkit-mcp registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-mcp` to the exact version in its
`Cargo.toml` with default features (stdio only, which is what pulse selects; coderight also selects
`advanced-http`, which this consumer does not prove, see below). It also depends on
`serde_json = "1"`, which is already in the crate's own dependency closure. It is built from the
sealed archive through the offline directory registry. The only platform-gated code builds a zero
`ExitStatus` for the in-memory child, so Windows, macOS and Linux run the same assertions.

The client is proven against a real `ServerHost` without a process or a socket: a `ProcessLauncher`
returns in-memory pipes served by the host on a thread, so `McpClient` speaks the real
newline-delimited JSON-RPC. It proves:

- Names and types: `sanitize_tool_name` and `parse_qualified_name`, `ToolDescriptor`, `ToolResult`,
  `Content` and `PromptMessage` wire shapes, `mutating_tools`, the protocol version constants,
  `McpError` text, `RawJson` and `to_wire_string` (bytes preserved), `McpEvent`, `Secret`, an
  `AuthProvider` implementation.
- `config::parse_mcp_json` with `${VAR}` and `${VAR:-default}` expansion, sorted entries and each
  refusal.
- Trust: `fingerprint` equals an independently computed SHA-256 of the canonical launch
  description, environment values included and never shown in a `PendingApproval`; `TrustStore`
  decide, approve, deny, gate, `gate_stdio`, `credential_write_allowed`, caller-supplied
  fingerprints, `save`, `try_load` (a corrupt file is an error, not an empty store) and
  `try_load_migrating`.
- `ServerHost` without a transport: `initialize` negotiation and overrides, `ping`, unknown
  methods, `tools/list` and `tools/call` under `ReadOnlyPolicy`, tool-level and RPC errors with
  data, resources, prompts, batches (`Process`, `Reject`, `Ignore`, per protocol version), id and
  initialized policies, strict request validation, response limits, the response hook and
  transform, `RawJson` passthrough, `dispatch_async` with an `AsyncRequestHandler`, content-free
  events.
- Stdio serving over in-memory buffers: replies per line, a parse error reply, an oversize line
  answered while serving continues, and `FailFast` ending with the original `InvalidData`.
- Optional provider hooks: `list_resource_templates` (opt-in), `read_resource_with_context` with
  `accessGrants` (string entries only, in order), `CallHandle::notify_progress_number` (a whole
  number keeps its integer spelling) and `CallHandle::notify` over `serve_stdio_concurrent_on` on
  in-memory buffers, the same handle inert under `dispatch`, and a custom `AdmissionHook` (the
  request it is given, `Deny`, and `expose` hiding a tool the hook is then never asked about).
- Client: connect through the trust gate (`NotApproved` and `Denied` launch nothing, and a changed
  environment value needs re-approval), the exact `LaunchRequest`, `list_tools`, `call_tool`,
  resources, prompts, `ping`, session ids, per-call options, a pre-cancelled call, cancelling a call
  in flight, progress notifications through `on_notification`, lifecycle events with the pid, and
  `close`; an empty protocol version list and a pre-cancelled connect are refused before any launch;
  `ConnectionPool` refuses unapproved and denied servers, lists them as skipped in `catalog`, and
  reports `Denied` events.
- Session-bound elicitation, against a small hand-written in-memory server that asks
  `elicitation/create` while a call is running: the default callback is bound to the connection's
  own session, a question tied to the call by `relatedRequestId` and an untied one on the only
  active call both reach it with the originating call's cancellation, `set_elicitation_handler`
  replaces it, a handler error becomes an error reply, and `clear_elicitation_handler` leaves the
  server with a refusal.
- The feature boundary: asking for HTTP client extensions (`ServerSpec::Http` with
  `ClientExtensions`) without `advanced-http` is a `Config` error naming the feature, before any socket.
- `LeasedPool`: single-flight `acquire`, epochs, idle eviction, cooldown after a failure, cache
  lifetime, `snapshot`. `EndpointOriginPolicy` resolution and `CancellationToken` are checked as
  pure logic.

It does not prove a real child process (`DefaultProcessLauncher`, `LaunchedProcess::from_child`),
the process-stdio entry points `serve_stdio`, `serve_stdio_io` and `serve_stdio_concurrent` (the
in-memory `*_on` variants are proven instead), streamable HTTP or legacy SSE clients, the
`advanced-http` feature, `ServerHost::serve_http` (it binds a loopback socket), an approved
workspace server inside a `ConnectionPool` (the pool spawns without a launcher), `exact-json-ids`,
`with_cancelled_response`, or `AuthProvider` on a live HTTP connection.

State: one small trust file under a per-process directory in the OS temp directory, removed at the
end. Worker threads serve the in-memory pipes and are joined by `close`. A watchdog thread ends the
process with a failure if the run exceeds 50 seconds. No network, no process spawn, no real user
data. Run only by the CI consumer lane.
