# rightkit-mcp registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-mcp` to the exact version in its
`Cargo.toml` with default features (stdio only, which is what pulse selects; coderight also selects
`advanced-http`, which this consumer does not prove, see below). It also depends on
`serde_json = "1"`, which is already in the crate's own dependency closure. It is built from the
sealed archive through the offline directory registry. Windows, macOS and Linux run the same assertions.

The server host and the trust gate are proven through the public API without a process or a socket.
There is no fake process launcher: the client side needs a real child process and is not exercised here.
It proves:

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
- Client, pure parts only: `ConnectionPool` refuses unapproved and denied servers, lists them as
  skipped in `catalog`, and reports `Denied` events.
- The feature boundary: asking for HTTP client extensions (`ServerSpec::Http` with
  `ClientExtensions`) without `advanced-http` is a `Config` error naming the feature, before any socket.
- `LeasedPool`: single-flight `acquire`, epochs, idle eviction, cooldown after a failure, cache
  lifetime, `snapshot`. `EndpointOriginPolicy` resolution and `CancellationToken` are checked as
  pure logic.

It does not prove `McpClient` against a server (connect, `list_tools`, `call_tool`, cancellation in
flight, progress, elicitation, lifecycle events with the pid), a real child process
(`DefaultProcessLauncher`, `LaunchedProcess::from_child`),
the process-stdio entry points `serve_stdio`, `serve_stdio_io` and `serve_stdio_concurrent` (the
in-memory `*_on` variants are proven instead), streamable HTTP or legacy SSE clients, the
`advanced-http` feature, `ServerHost::serve_http` (it binds a loopback socket), an approved
workspace server inside a `ConnectionPool` (the pool spawns without a launcher), `exact-json-ids`,
`with_cancelled_response`, or `AuthProvider` on a live HTTP connection.

State: one small trust file under a per-process directory in the OS temp directory, removed at the
end. A watchdog thread ends the
process with a failure if the run exceeds 50 seconds. No network, no process spawn, no real user
data. Run only by the CI consumer lane.
