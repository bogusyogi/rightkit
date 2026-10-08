# rightkit-mcp

Shared MCP client and server host for Right Suite apps. See API.md in the lane deliverables for the full contract.

## Trust approvals (0.3.2): re-approval required

Workspace-declared stdio servers are gated before spawn. The approval fingerprint now binds the full launch configuration by value: command, args, cwd, and every environment variable's name **and value**. Changing `PATH`, `NODE_OPTIONS`, or any other env value invalidates the approval.

- **Breaking for stored approvals:** fingerprints changed format (canonical version 2). Every approval recorded by 0.3.x is now `Pending`; re-approve each workspace server once after upgrading.
- Only the SHA-256 digest is persisted in the trust store. Environment values are never written to the approval file or surfaced in `PendingApproval` (which still lists env variable names only).
- Digests are unsalted. Low-entropy env values could be guessed offline from a copied trust file; keep the trust store private, as with other local credentials.

## Also (0.3.0)

All additions are opt-in. Existing `ClientOptions`, `ServerOptions`, `ServerSpec`, provider methods & transport defaults remain source-compatible. `connect_with_extensions` selects cancellable HTTP I/O; existing `connect` retains its original HTTP transport. Synchronous client calls can also run from async hosts. New typed errors are `McpError::Cancelled`, `ResponseTooLarge { limit }` & `CrossOriginEndpoint { configured_origin, endpoint_origin }`.

1. **Legacy HTTP+SSE:** one persistent GET stream announces its POST endpoint & carries replies. Relative endpoints resolve against configured URL. Scheme, normalized hostname & effective port must match; explicit origin allow-list entries permit selected alternatives. Redirects stay disabled.

```rust,no_run
use rightkit_mcp::*;
# fn example() -> Result<()> {
let client = McpClient::connect_with_extensions(
    "legacy", ServerSpec::Http { url: "https://mcp.example/sse".into() },
    ClientOptions::default(), ClientExtensions {
        legacy_sse: true,
        endpoint_origin_policy: EndpointOriginPolicy {
            allowed_origins: vec!["https://post.example".into()],
        },
        ..Default::default()
    },
)?;
client.ping()?;
# Ok(()) }
```

2. **Per-call session/headers & cancellation:** overrides never replace shared session, including case-insensitive `Mcp-Session-Id` header overrides. Cancellation returns `Cancelled` promptly (the HTTP connection may stay open while hyper drains the response) & sends a best-effort `notifications/cancelled` with original request ID, headers & session (except initialize). Cancellation notification has a 200ms cap. Stdio cancellation retains connection while waiting for replies; cancelling a blocked stdin write retires its child to interrupt shared pipe I/O.

```rust,no_run
use rightkit_mcp::*;
use serde_json::json;
# fn example(client: std::sync::Arc<McpClient>) -> Result<()> {
let cancellation = CancellationToken::new();
let options = CallOptions {
    session_id: Some("caller-session".into()),
    headers: [("X-Request-Tag".into(), "one-call".into())].into(),
    cancellation: Some(cancellation.clone()),
};
let call = std::thread::spawn(move ||
    client.call_tool_with_options("slow-tool", json!({}), &options));
cancellation.cancel();
let result = call.join().unwrap();
# let _ = result; Ok(()) }
```

3. **Confined process launch:** launcher gets exact program, args, environment delta & cwd, then returns an owned `ProcessChild` & readable/writable stdio. Closures implement `ProcessLauncher`; default launch behavior is unchanged. Sandbox adapters implement child ID/kill/try-wait/wait while retaining AppContainer/seatbelt supervision & return their own pipes through `LaunchedProcess`. Native `std::process::Child` already implements this trait; `LaunchedProcess::from_child` takes its piped stdin/stdout.

```rust,no_run
use rightkit_mcp::*;
use std::sync::Arc;
# fn example(spec: ServerSpec) -> Result<()> {
let launcher = |request: &LaunchRequest| -> Result<LaunchedProcess> {
    // Replace with AppContainer/seatbelt launch; return child & wrapped pipes.
    DefaultProcessLauncher.launch(request)
};
let client = McpClient::connect_with_extensions("confined", spec,
    ClientOptions::default(), ClientExtensions {
        launcher: Some(Arc::new(launcher)), ..Default::default()
    })?;
# client.close(); Ok(()) }
```

4. **Session-bound elicitation:** callback receives original params & session context, returning MCP elicitation result. Default callback supplied at connect binds to that connection's own session. Additional session handlers must be registered explicitly; per-call override never borrows another session's handler. On shared stdio/legacy SSE, `params._meta.relatedRequestId` (or `params.relatedRequestId`) selects originating call; unassociated requests require an unambiguous session.

```rust,no_run
use rightkit_mcp::*;
use serde_json::json;
use std::sync::Arc;
# fn example(client: &McpClient) -> Result<()> {
let session = client.session_id()?.unwrap();
client.set_elicitation_handler(&session, Arc::new(|context, params| {
    // Host UI decides accept/decline/cancel within context.session_id.
    let _ = (context, params);
    Ok(json!({"action":"decline"}))
}))?;
# Ok(()) }
```

5. **Async parsed dispatch:** handler returns complete result payload or `None` for legacy fallback. Each independent dispatch future retains original JSON-RPC ID; async batches poll handlers concurrently & return replies in completion order. Missing/null IDs, initialized/batch policy & tool admission retain configured semantics. Transport adapters drive these futures & write returned envelopes.

```rust,no_run
use rightkit_mcp::*;
use serde_json::{json, Value};
use std::{future::Future, pin::Pin, sync::Arc};
struct AsyncEcho;
impl AsyncRequestHandler for AsyncEcho {
    fn handle<'a>(&'a self, request: &'a Value, _: &'a RequestContext)
        -> Pin<Box<dyn Future<Output = Option<Result<Value>>> + Send + 'a>> {
        Box::pin(async move {
            (request["method"] == "echo").then(|| Ok(request["params"].clone()))
        })
    }
}
# async fn example(host: ServerHost) {
let host = host.with_async_handler(Arc::new(AsyncEcho));
let reply = host.dispatch_async(&json!({"id":"original-id","method":"echo","params":{}})).await;
assert_eq!(reply.unwrap()["id"], "original-id");
# }
```

6. **Protocol versions & bounded responses:** configured lists negotiate lexicographically highest common date-version via `supportedProtocolVersions` initialize extension. Standard peers still receive `protocolVersion` & their selected version must belong to client list. Unconfigured defaults retain `2025-03-26` pin & existing supported list. Client response/frame limits default to 16MiB on extended connections, reject before truncation & preserve UTF-8 across stream chunks. Server checked dispatch validates encoded envelopes; existing unchecked dispatch remains unchanged. IDs clone original parsed JSON values, including arbitrary-precision numbers.

```rust,no_run
use rightkit_mcp::*;
use serde_json::json;
# fn example(host: ServerHost, spec: ServerSpec) -> Result<()> {
let versions = vec!["2025-03-26".into(), "2025-11-25".into()];
let limits = ResponseLimits { max_response_bytes: 1024 * 1024, max_frame_bytes: 64 * 1024 };
let host = host.with_protocol_versions(versions.clone()).with_response_limits(limits);
let client = McpClient::connect_with_extensions("versioned", spec,
    ClientOptions::default(), ClientExtensions {
        protocol_versions: Some(versions), response_limits: limits, ..Default::default()
    })?;
let reply = host.dispatch_checked(&json!({"id":1,"method":"ping"}), &RequestContext::default())?;
# let _ = (client, reply); Ok(()) }
```

## Also (0.2.0)

- Public `ServerHost::dispatch(&Value) -> Option<Value>` & `dispatch_with_context(&Value, &RequestContext) -> Option<Value>` route parsed frames without transport I/O.
- Opt-in `IdPolicy::NullResponse` replies to missing/null IDs; independent `InitializedPolicy::Ignore` suppresses initialized notifications even with IDs.
- `BatchPolicy::{Process, Reject, Ignore}` controls arrays; default `Process` retains existing batch behavior, `Reject` emits `-32600 / "Invalid Request" / id:null`, & `Ignore` invokes no handlers.
- `ServerOptions::method_not_found_message` selects exact error text (Membrane: `"Method not found"`); `McpError::RpcWithData { code, message, data }` preserves error data, including null.
- Request-aware `ToolProvider::list_tools` & raw payload hooks preserve custom tool/resource/prompt fields; `list_resource_templates` opts into `resources/templates/list`; prompt list/get hooks retain metadata.
- `ToolProvider::read_resource_with_context` receives original frame, host context & string entries from `params.accessGrants`; providers decide authorization.
- `ServerOptions::initialize_result` returns fixed initialize payload verbatim; default retains protocol negotiation & generated capabilities/instructions.
- Opt-in `StdioInputMode::FailFast` stops malformed JSON with `InvalidData`; `serve_stdio_io`/`serve_stdio_on_io` return original read/write/flush errors, while existing serving methods retain originals in `McpError::IoError` under this mode.

All new provider methods have legacy defaults. Default initialized handling, missing/null-ID suppression, method-not-found text, typed payloads, admission, initialize negotiation & stdio parse-error replies stay unchanged. Resource template listing stays unsupported unless its provider hook returns `Some`.

Membrane adapters can select:

```rust
use rightkit_mcp::{BatchPolicy, IdPolicy, InitializedPolicy, ServerOptions, StdioInputMode};
use serde_json::Value;

fn membrane_options(initialize_response: Value) -> ServerOptions {
    ServerOptions {
        id_policy: IdPolicy::NullResponse,
        initialized_policy: InitializedPolicy::Ignore,
        batch_policy: BatchPolicy::Ignore,
        method_not_found_message: Some("Method not found".into()),
        initialize_result: Some(initialize_response),
        stdio_input_mode: StdioInputMode::FailFast,
    }
}

// Override list_resource_templates in ToolProvider:
// Some(Ok(json!({"resourceTemplates": []})))
```

Provider payload hooks (all return complete MCP result payloads, without JSON-RPC envelopes):

```rust,ignore
fn list_tools(&self, request: &Value, context: &RequestContext) -> Result<Value>;
fn call_tool(&self, request: &Value, context: &RequestContext) -> Option<Result<Value>>;
fn list_resources(&self, request: &Value, context: &RequestContext) -> Result<Value>;
fn list_resource_templates(&self, request: &Value, context: &RequestContext) -> Option<Result<Value>>;
fn read_resource_with_context(&self, uri: &str, context: &ReadContext<'_>) -> Result<Value>;
fn list_prompts(&self, request: &Value, context: &RequestContext) -> Result<Value>;
fn get_prompt_payload(&self, request: &Value, context: &RequestContext) -> Result<Value>;
```

`RequestContext` carries `transport: ServerTransport`, `session: Option<String>` & `metadata: Value`; default is stdio, no session & null metadata. `ReadContext<'a>` carries `request: &'a Value`, `context: &'a RequestContext` & `access_grants: Vec<&'a str>`; grants preserve string order/duplicates & ignore nonstrings, matching Membrane.

Apply configuration with `ServerHost::with_options(self, ServerOptions) -> Self`. Raw `call_tool` runs after admission & can handle unknown names without typed descriptors. Missing/nonstring names reach raw handlers only without admission; with admission, existing `-32602 / "missing tool name"` response stays intact. Raw tool listing applies configured exposure policy while retaining payload metadata. `serve_stdio_on_io(&self, impl BufRead, impl Write) -> std::io::Result<()>` & `serve_stdio_io(&self) -> std::io::Result<()>` honor selected input mode & retain original I/O errors; `McpError::IoError` also exposes its original error via `std::error::Error::source`.
