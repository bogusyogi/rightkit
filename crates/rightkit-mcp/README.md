# rightkit-mcp

Shared MCP client and server host for Right Suite apps. See API.md in the lane deliverables for the full contract.

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
