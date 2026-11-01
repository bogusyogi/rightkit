use rightkit_mcp::agent_mcp::{
    McpServerModePolicy, McpServerToolSummary, RequestContext, RightKitMcpBackend,
    RightKitMcpError, RightKitMcpServer, ToolDescriptor, MCP_SUPPORTED_VERSIONS,
};
use serde_json::{json, Value};
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const MAX_MCP_LINE_BYTES: usize = 1024 * 1024;

pub struct McpServerOptions {
    pub workspace: PathBuf,
    pub allow_mutate: bool,
    pub session_id: Option<String>,
}

#[derive(Clone)]
struct SessionGateway {
    daemon_url: String,
    capability_token: String,
    driver_instance_secret: String,
    session_id: String,
    client: rightkit_http::AsyncClient,
}

impl SessionGateway {
    fn from_options(options: &McpServerOptions) -> Result<Option<Self>, String> {
        let Some(session_id) = options.session_id.as_deref() else {
            return Ok(None);
        };
        if session_id.is_empty()
            || !session_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err("--session-id contains unsupported characters".to_string());
        }
        if options.allow_mutate {
            return Err("--allow-mutate cannot be combined with --session-id".to_string());
        }
        let required = |name: &str| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| format!("{name} is required for a session-bound MCP gateway"))
        };
        let daemon_url = std::env::var("RIGHTKIT_DAEMON_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:51123".to_string())
            .trim_end_matches('/')
            .to_string();
        // No proxy, no redirects: the capability token goes to the daemon host only.
        let client =
            rightkit_http::http_client::daemon_client_probe(&daemon_url, CONNECT_TIMEOUT, REQUEST_TIMEOUT)?;
        Ok(Some(Self {
            daemon_url,
            capability_token: required("RIGHTKIT_GATEWAY_TOKEN")?,
            driver_instance_secret: required("RIGHTKIT_DRIVER_INSTANCE_SECRET")?,
            session_id: session_id.to_string(),
            client,
        }))
    }

    fn endpoint(&self, suffix: &str) -> String {
        format!(
            "{}/sessions/{}/driver-capabilities/{}",
            self.daemon_url, self.session_id, suffix
        )
    }

    async fn tools(&self) -> Result<Vec<Value>, String> {
        let response = self
            .client
            .send_any(
                &rightkit_http::Request::get(self.endpoint("tools"))
                    .secret_header("x-coderight-capability-token", &self.capability_token)
                    .secret_header("x-coderight-driver-instance", &self.driver_instance_secret),
            )
            .await
            .map_err(|error| format!("gateway tools request failed: {error}"))?;
        let status = response.status;
        let body: Value = response
            .json()
            .map_err(|error| format!("gateway tools response was not JSON: {error}"))?;
        if !rightkit_http::http_client::is_success(status) {
            return Err(format!("gateway tools rejected ({status}): {body}"));
        }
        body.get("tools")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| "gateway tools response omitted tools".to_string())
    }

    async fn call(
        &self,
        request_id: &Value,
        name: &str,
        arguments: Value,
    ) -> Result<Value, String> {
        let request = rightkit_http::Request::post(self.endpoint("call"))
            .secret_header("x-coderight-capability-token", &self.capability_token)
            .secret_header("x-coderight-driver-instance", &self.driver_instance_secret)
            .json(&json!({
                "callId": request_id,
                "name": name,
                "arguments": arguments,
            }))
            .map_err(|error| format!("gateway tool call failed: {error}"))?;
        let response = self
            .client
            .send_any(&request)
            .await
            .map_err(|error| format!("gateway tool call failed: {error}"))?;
        let status = response.status;
        let body: Value = response
            .json()
            .map_err(|error| format!("gateway tool response was not JSON: {error}"))?;
        if !rightkit_http::http_client::is_success(status) {
            return Err(format!("gateway tool call rejected ({status}): {body}"));
        }
        Ok(body)
    }
}

pub async fn run_stdio(options: McpServerOptions) -> i32 {
    #[cfg(windows)]
    if std::env::var_os("RIGHTKIT_VENDOR_PIPE").is_some() {
        return rightkit_agents::vendor_mcp_pipe::proxy_stdio().await;
    }
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let reader = stdin.lock();
    run_reader(reader, &mut stdout, options).await
}

pub async fn run_reader<R: BufRead, W: std::io::Write>(
    reader: R,
    writer: &mut W,
    options: McpServerOptions,
) -> i32 {
    let gateway = match SessionGateway::from_options(&options) {
        Ok(gateway) => gateway,
        Err(error) => {
            let _ = writeln!(writer, "{}", error_response(None, -32000, error));
            return 1;
        }
    };
    let inline = if gateway.is_none() {
        match crate::main_runtime::InlineMcpProjection::build(
            &options.workspace,
            options.allow_mutate,
        )
        .await
        {
            Ok(inline) => Some(inline),
            Err(error) => {
                let _ = writeln!(writer, "{}", error_response(None, -32000, error));
                return 1;
            }
        }
    } else {
        None
    };
    run_reader_with_gateway_and_inline(reader, writer, options, gateway, inline).await
}

/// Test-only entry point: same request loop as `run_reader`, but takes a
/// caller-supplied inline MCP projection (built from a hand-constructed
/// `DaemonState` via `InlineMcpProjection::build_with_state`) instead of
/// resolving `InlineMcpProjection::build()` -> `InlineHost::shared()`.
/// Production traffic always goes through `run_reader` above, which still
/// requires a canonical Membrane binding; this path exists only so
/// stdio-protocol tests don't depend on a live Membrane installation.
#[cfg(test)]
async fn run_reader_with_inline<R: BufRead, W: std::io::Write>(
    reader: R,
    writer: &mut W,
    options: McpServerOptions,
    inline: Option<crate::main_runtime::InlineMcpProjection>,
) -> i32 {
    let gateway = match SessionGateway::from_options(&options) {
        Ok(gateway) => gateway,
        Err(error) => {
            let _ = writeln!(writer, "{}", error_response(None, -32000, error));
            return 1;
        }
    };
    run_reader_with_gateway_and_inline(reader, writer, options, gateway, inline).await
}

async fn run_reader_with_gateway_and_inline<R: BufRead, W: std::io::Write>(
    reader: R,
    writer: &mut W,
    options: McpServerOptions,
    gateway: Option<SessionGateway>,
    inline: Option<crate::main_runtime::InlineMcpProjection>,
) -> i32 {
    let inline_tools = if let Some(host) = inline.as_ref() {
        match host.tools().await {
            Ok(tools) => tools,
            Err(error) => {
                let _ = writeln!(writer, "{}", error_response(None, -32000, error));
                return 1;
            }
        }
    } else {
        Vec::new()
    };
    let allow_mutate = options.allow_mutate;
    let trust_tier = std::env::var("RIGHTKIT_MCP_TRUST_TIER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            if allow_mutate {
                "trusted".to_string()
            } else {
                "untrusted".to_string()
            }
        });
    let mut server = McpStdioServer::new(
        options,
        gateway,
        inline,
        inline_tools,
        McpServerModePolicy::new(&trust_tier, allow_mutate),
        false,
    );
    let mut line = String::new();
    let mut reader = reader;
    loop {
        line.clear();
        let read = match reader.read_line(&mut line) {
            Ok(read) => read,
            Err(_) => {
                return 1;
            }
        };
        if read == 0 {
            break;
        }
        if line.len() > MAX_MCP_LINE_BYTES {
            let _ = writeln!(
                writer,
                "{}",
                error_response(None, -32600, "MCP request exceeds size limit")
            );
            let _ = writer.flush();
            return 1;
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => server.handle_request(request).await,
            Err(e) => Some(error_response(None, -32700, format!("parse error: {e}"))),
        };
        if let Some(response) = response {
            if writeln!(writer, "{response}").is_err() {
                return 1;
            }
            let _ = writer.flush();
        }
    }
    0
}

/// Application backend: RightKit retains execution, permissions & error codes.
struct RightKitMcpBackend {
    gateway: Option<SessionGateway>,
    inline: Option<Arc<crate::main_runtime::InlineMcpProjection>>,
    inline_tools: Vec<Value>,
    policy: McpServerModePolicy,
    vendor_permissions: bool,
}

impl RightKitMcpBackend {
    fn raw_tools(&self) -> Vec<Value> {
        let mut tools = self.inline_tools.clone();
        if self.vendor_permissions {
            tools.push(json!({
                "name": "permission_prompt",
                "description": "Request RightKit policy approval for a RightKit tool call",
                "inputSchema": {
                    "type": "object",
                    "properties": {"tool_name": {"type": "string"}, "input": {"type": "object"}},
                    "required": ["tool_name", "input"], "additionalProperties": false
                }
            }));
        }
        sort_tools(&mut tools);
        tools
    }
}

impl RightKitMcpBackend for RightKitMcpBackend {
    fn should_audit(&self, name: &str) -> bool {
        !(self.vendor_permissions && name == "permission_prompt")
    }

    fn tools(&self) -> Vec<ToolDescriptor> {
        self.raw_tools()
            .into_iter()
            .filter_map(|tool| serde_json::from_value(tool).ok())
            .collect()
    }

    fn list_tools<'a>(
        &'a self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Value, RightKitMcpError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut tools = match self.gateway.as_ref() {
                Some(gateway) => {
                    gateway
                        .tools()
                        .await
                        .map_err(|message| RightKitMcpError::Rpc {
                            code: -32001,
                            message,
                        })?
                }
                None => self.raw_tools(),
            };
            tools.retain(|tool| {
                tool.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| !name.trim().is_empty())
            });
            sort_tools(&mut tools);
            Ok(json!({"tools": tools}))
        })
    }

    fn call<'a>(
        &'a self,
        request_id: &'a Value,
        name: &'a str,
        arguments: Value,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Value, RightKitMcpError>> + Send + 'a>,
    > {
        Box::pin(async move {
            if self.vendor_permissions && name == "permission_prompt" {
                let inline = self.inline.as_ref().ok_or_else(|| RightKitMcpError::Rpc {
                    code: -32002,
                    message: "vendor inline host is unavailable".into(),
                })?;
                let result = inline
                    .permission_prompt(arguments)
                    .await
                    .unwrap_or_else(|_| {
                        json!({
                            "behavior": "deny", "message": "RightKit approval channel unavailable"
                        })
                    });
                return Ok(json!({"content": [{"type": "text", "text": result.to_string()}]}));
            }
            if let Some(gateway) = self.gateway.as_ref() {
                return gateway
                    .call(request_id, name, arguments)
                    .await
                    .map_err(|message| RightKitMcpError::Rpc {
                        code: -32002,
                        message,
                    });
            }
            let inline = self.inline.as_ref().ok_or_else(|| RightKitMcpError::Rpc {
                code: -32002,
                message: "inline MCP execution host is unavailable".into(),
            })?;
            inline
                .call(request_id, name, arguments)
                .await
                .map_err(|message| RightKitMcpError::Rpc {
                    code: -32002,
                    message,
                })
        })
    }
}

fn sort_tools(tools: &mut [Value]) {
    tools.sort_by(|left, right| {
        left.get("name")
            .and_then(Value::as_str)
            .cmp(&right.get("name").and_then(Value::as_str))
    });
}

/// Thin ingress policy adapter; RightKit owns parsed protocol dispatch.
pub(crate) struct McpStdioServer {
    host: RightKitMcpServer,
    backend: Arc<RightKitMcpBackend>,
    initialized: bool,
    session_id: Option<String>,
}

impl McpStdioServer {
    fn new(
        options: McpServerOptions,
        gateway: Option<SessionGateway>,
        inline: Option<crate::main_runtime::InlineMcpProjection>,
        inline_tools: Vec<Value>,
        policy: McpServerModePolicy,
        vendor_permissions: bool,
    ) -> Self {
        let backend = Arc::new(RightKitMcpBackend {
            gateway,
            inline: inline.map(Arc::new),
            inline_tools,
            policy,
            vendor_permissions,
        });
        let host = RightKitMcpServer::new(
            "coderight",
            env!("CARGO_PKG_VERSION"),
            backend.clone(),
            policy,
        );
        Self {
            host,
            backend,
            initialized: false,
            session_id: options.session_id,
        }
    }

    pub(crate) async fn for_vendor(
        workspace: PathBuf,
        inline: crate::main_runtime::InlineMcpProjection,
    ) -> Result<Self, String> {
        let inline_tools = inline.tools().await?;
        Ok(Self::new(
            McpServerOptions {
                workspace,
                allow_mutate: true,
                session_id: None,
            },
            None,
            Some(inline),
            inline_tools,
            McpServerModePolicy::new("trusted", true),
            true,
        ))
    }

    pub(crate) async fn handle_request(&mut self, request: Value) -> Option<Value> {
        let Some(object) = request.as_object() else {
            return Some(error_response(
                None,
                -32600,
                "request must be a JSON object",
            ));
        };
        let id = object.get("id").cloned();
        let is_notification = id.is_none();
        let method = object.get("method").and_then(Value::as_str);
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || method.is_none() {
            return if is_notification {
                None
            } else {
                Some(error_response(
                    id,
                    -32600,
                    "invalid JSON-RPC request; expected jsonrpc=2.0 and method",
                ))
            };
        }
        let method = method.unwrap_or_default();
        if method == "initialize" {
            if let Some(version) = request
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
            {
                if !MCP_SUPPORTED_VERSIONS.contains(&version) {
                    return if is_notification {
                        None
                    } else {
                        Some(error_response(
                            id,
                            -32602,
                            format!("unsupported MCP protocolVersion: {version}"),
                        ))
                    };
                }
            }
        }
        if matches!(method, "tools/list" | "tools/call") && !self.initialized {
            return if is_notification {
                None
            } else {
                Some(error_response(id, -32002, "MCP server is not initialized"))
            };
        }
        if method == "notifications/initialized" {
            self.initialized = true;
        }
        let context = RequestContext {
            session: self.session_id.clone(),
            ..RequestContext::default()
        };
        let response = match self.host.dispatch_with_context(&request, &context).await {
            Ok(response) => response,
            Err(error) => Some(error_response(
                id,
                -32603,
                rightkit_mcp::agent_mcp::map_rightkit_error(error).to_string(),
            )),
        };
        if method == "initialize"
            && response
                .as_ref()
                .is_some_and(|reply| reply.get("result").is_some())
        {
            self.initialized = true;
        }
        if is_notification {
            None
        } else {
            response
        }
    }

    #[cfg(test)]
    fn tools_json(&self) -> Vec<Value> {
        self.backend
            .raw_tools()
            .into_iter()
            .filter(|tool| {
                self.backend.policy.exposes_tool(&McpServerToolSummary::new(
                    tool.get("name").and_then(Value::as_str).unwrap_or_default(),
                    gateway_tool_is_read_only(tool),
                ))
            })
            .collect()
    }
}

#[cfg(test)]
fn gateway_tool_is_read_only(tool: &Value) -> bool {
    tool.get("annotations")
        .and_then(|annotations| annotations.get("readOnlyHint"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn error_response(id: Option<Value>, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": {
            "code": code,
            "message": message.into(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("coderight-{name}-{nanos}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// Hand-built `DaemonState` for stdio-protocol tests, mirroring the
    /// precedent in `main_runtime_cancel_tests.rs` (`InlineHost { state }`
    /// constructed directly from a test-local `DaemonState`). This avoids
    /// depending on a live Membrane installation for tests that only
    /// exercise MCP request/response plumbing, not memory-backend behavior.
    fn test_daemon_state(name: &str) -> crate::api::DaemonState {
        let root = temp_dir(name);
        let storage = crate::api::SharedStorage::new(
            rightkit_agent_engine_storage::Storage::open(root.join("mcp-test.db")).unwrap(),
        );
        crate::api::DaemonState::new("test-token").with_storage(storage)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn vendor_approval_round_trip_allow_deny_and_payload_fence() {
        use axum::{
            extract::{Path, State},
            http::HeaderMap,
            response::Json,
        };
        use rightkit_agent_policy::PermissionMode;
        async fn resolve(
            state: &crate::api::DaemonState,
            live: &std::sync::Arc<crate::api::LiveSession>,
            approved: bool,
        ) {
            let call_id = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let id = live
                        .pending_approvals
                        .lock()
                        .unwrap()
                        .keys()
                        .next()
                        .cloned();
                    if let Some(id) = id {
                        break id;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("normal approval event must arrive");
            let body =
                serde_json::from_value(json!({"toolCallId":call_id,"approved":approved})).unwrap();
            let _ = crate::api_sessions::approvals::resolve_tool_approval(
                State(state.clone()),
                Path(live.id.clone()),
                HeaderMap::new(),
                Json(body),
            )
            .await
            .unwrap();
        }
        let dir = temp_dir("vendor-approval");
        let state = test_daemon_state("vendor-approval-state");
        let inline =
            crate::main_runtime::InlineMcpProjection::build_vendor_with_state(state.clone(), &dir)
                .await
                .unwrap();
        let live = state.list_sessions().into_iter().next().unwrap();
        crate::api_permissions::apply_live_permission_mode(&state, &live, PermissionMode::Ask);
        let mut server = McpStdioServer::for_vendor(dir.clone(), inline)
            .await
            .unwrap();
        server
            .handle_request(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
            .await;
        for mode in [
            PermissionMode::Ask,
            PermissionMode::AcceptEdits,
            PermissionMode::AutoSafe,
            PermissionMode::Bypass,
        ] {
            crate::api_permissions::apply_live_permission_mode(&state, &live, mode);
            let tools = server.tools_json();
            assert!(tools.iter().any(|tool| tool["name"] == "write_file"));
            assert!(tools.iter().any(|tool| tool["name"] == "permission_prompt"));
        }
        crate::api_permissions::apply_live_permission_mode(&state, &live, PermissionMode::Ask);

        for approved in [true, false] {
            let args = json!({"path":dir.join(if approved {"allowed.txt"} else {"denied.txt"}),"content":"fixture"});
            let request = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                "name":"permission_prompt","arguments":{"tool_name":"mcp__coderight__write_file","input":args}
            }});
            let (response, _) = tokio::join!(
                server.handle_request(request),
                resolve(&state, &live, approved)
            );
            let response = response.unwrap();
            let decision: Value =
                serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(
                decision["behavior"],
                if approved { "allow" } else { "deny" }
            );
            assert!(
                !dir.join(if approved {
                    "allowed.txt"
                } else {
                    "denied.txt"
                })
                .exists(),
                "callback must not execute"
            );
            if approved {
                assert_eq!(decision["updatedInput"], args);
                let duplicate = server.handle_request(json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{
                    "name":"permission_prompt","arguments":{"tool_name":"mcp__coderight__write_file","input":args}
                }})).await.unwrap();
                assert!(duplicate.to_string().contains("already awaiting execution"));
                let changed = json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{
                    "name":"write_file","arguments":{"path":dir.join("changed.txt"),"content":"different"}
                }});
                let (changed, _) = tokio::join!(
                    server.handle_request(changed),
                    resolve(&state, &live, false)
                );
                assert_eq!(changed.unwrap()["result"]["isError"], true);
                assert!(!dir.join("changed.txt").exists());
                let result = tokio::time::timeout(std::time::Duration::from_secs(10), server.handle_request(json!({
                    "jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"write_file","arguments":args}
                }))).await.expect("approved payload must execute without another prompt").unwrap();
                assert_eq!(result["result"]["isError"], false, "{result}");
                assert_eq!(
                    std::fs::read_to_string(dir.join("allowed.txt")).unwrap(),
                    "fixture"
                );
                let replay = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                    "name":"write_file","arguments":args
                }});
                let (replay, _) =
                    tokio::join!(server.handle_request(replay), resolve(&state, &live, false));
                assert_eq!(replay.unwrap()["result"]["isError"], true);
            }
        }
        let denied = server.handle_request(json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
            "name":"permission_prompt","arguments":{"tool_name":"Bash","input":{"command":"whoami"}}
        }})).await.unwrap();
        assert!(denied.to_string().contains("Only RightKit tools"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn stdio_server_lists_read_only_tools_by_default() {
        let dir = temp_dir("mcp-list");
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
"#;
        let mut output = Vec::new();

        let options = McpServerOptions {
            workspace: dir.clone(),
            allow_mutate: false,
            session_id: None,
        };
        let state = test_daemon_state("mcp-list-state");
        let inline = crate::main_runtime::InlineMcpProjection::build_with_state(
            state,
            &options.workspace,
            options.allow_mutate,
        )
        .await
        .unwrap();

        let code = run_reader_with_inline(&input[..], &mut output, options, Some(inline)).await;

        assert_eq!(
            code,
            0,
            "stdio output: {}",
            String::from_utf8_lossy(&output)
        );
        let lines = String::from_utf8(output).unwrap();
        assert!(!lines.contains("\"method\""));
        assert!(lines.contains("\"name\":\"read_file\""));
        assert!(!lines.contains("\"name\":\"write_file\""));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn stdio_server_calls_read_only_tool() {
        let dir = temp_dir("mcp-call");
        std::fs::write(dir.join("note.txt"), "hello").unwrap();
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"note.txt"}}}
"#;
        let mut output = Vec::new();

        let options = McpServerOptions {
            workspace: dir.clone(),
            allow_mutate: false,
            session_id: None,
        };
        let state = test_daemon_state("mcp-call-state");
        let inline = crate::main_runtime::InlineMcpProjection::build_with_state(
            state,
            &options.workspace,
            options.allow_mutate,
        )
        .await
        .unwrap();

        let code = run_reader_with_inline(&input[..], &mut output, options, Some(inline)).await;

        assert_eq!(
            code,
            0,
            "stdio output: {}",
            String::from_utf8_lossy(&output)
        );
        let response: Value =
            serde_json::from_str(String::from_utf8(output).unwrap().lines().nth(1).unwrap())
                .unwrap();
        assert_eq!(
            response["result"]["isError"], false,
            "stdio call response: {response}"
        );
        assert!(response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("hello"));
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(all(test, feature = "qualification"))]
mod eff001_mcp_ingress_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("coderight-eff001-mcp-{label}-{nanos}"));
        std::fs::create_dir_all(&path).expect("MCP fixture directory must be creatable");
        path
    }

    #[tokio::test]
    async fn tools_call_traverses_governed_mcp_ingress() {
        rightkit_agent_tools::coding_tools::process_resource_authority::install_process_resource_profile(
            rightkit_agent_tools::coding_tools::process_resource_authority::ResourceProfile::Expanded,
        )
        .expect("qualification fixture must install process authority");

        let workspace = temp_dir("workspace");
        let storage_root = temp_dir("state");
        std::fs::write(workspace.join("eff001-marker.txt"), "mcp ingress")
            .expect("MCP fixture file must be written");
        let storage = crate::api::SharedStorage::new(
            rightkit_agent_engine_storage::Storage::open(&storage_root.join("state.db"))
                .expect("isolated MCP SQLite state must open"),
        );
        let state = crate::api::DaemonState::new("test-token").with_storage(storage);
        let options = McpServerOptions {
            workspace: workspace.clone(),
            allow_mutate: false,
            session_id: None,
        };
        let inline = crate::main_runtime::InlineMcpProjection::build_with_state(
            state.clone(),
            &options.workspace,
            options.allow_mutate,
        )
        .await
        .expect("MCP qualification projection must build");
        let call_id = format!("eff001-mcp-{}", rightkit_agent_engine_protocol::uuid_v7());
        let sessions = state.list_sessions();
        assert_eq!(
            sessions.len(),
            1,
            "fixture must create exactly one MCP session"
        );
        let trace_call_id = format!("mcp:{}:{call_id}", sessions[0].id);
        let input = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{}}}}\n{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{{}}}}\n{{\"jsonrpc\":\"2.0\",\"id\":\"{call_id}\",\"method\":\"tools/call\",\"params\":{{\"name\":\"read_file\",\"arguments\":{{\"path\":\"eff001-marker.txt\"}}}}}}\n"
        );
        let mut output = Vec::new();

        let code = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run_reader_with_inline(input.as_bytes(), &mut output, options, Some(inline)),
        )
        .await
        .expect("MCP ingress fixture must settle within 10 seconds");

        assert_eq!(
            code,
            0,
            "stdio output: {}",
            String::from_utf8_lossy(&output)
        );
        let response: Value = serde_json::from_str(
            String::from_utf8(output)
                .expect("MCP output must be UTF-8")
                .lines()
                .nth(2)
                .expect("tools/call must produce a response"),
        )
        .expect("tools/call response must be JSON");
        assert_eq!(response["id"].as_str(), Some(call_id.as_str()));
        assert_eq!(
            response["result"]["isError"], false,
            "MCP call response: {response}"
        );
        assert!(response["result"]["content"][0]["text"]
            .as_str()
            .expect("read_file result must contain text")
            .contains("mcp ingress"));

        let trace = rightkit_agent_engine_core::qualification_trace::snapshot();
        let stages = trace.stages_for(&trace_call_id);
        assert_eq!(
            stages,
            rightkit_agent_engine_core::qualification_trace::EffectAdmissionStage::canonical_order()
        );
        assert!(trace.is_canonically_ordered(&trace_call_id));
        assert!(trace.reached_execute(&trace_call_id));

        let _ = std::fs::remove_dir_all(workspace);
        let _ = std::fs::remove_dir_all(storage_root);
    }
}
