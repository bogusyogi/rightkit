use rightkit_mcp::agent_mcp::{
    McpServerModePolicy, RequestContext, RightKitMcpBackend, RightKitMcpError, RightKitMcpServer,
    ToolDescriptor, MCP_SUPPORTED_VERSIONS,
};
use serde_json::{json, Value};
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const MAX_MCP_LINE_BYTES: usize = 1024 * 1024;

/// Read one request line into `line`, but never more than one byte past the limit, so a peer
/// that sends gigabytes without a newline is refused after 1 MiB instead of buffered whole
/// (audit 2026-11-05 L5). Returns the byte count; 0 is EOF.
fn read_bounded_line<R: BufRead>(reader: &mut R, line: &mut Vec<u8>) -> std::io::Result<usize> {
    let mut limited = std::io::Read::take(reader, MAX_MCP_LINE_BYTES as u64 + 1);
    limited.read_until(b'\n', line)
}

/// Text returned when `mcp-server` starts without `--session-id` on a host that runs no public
/// MCP server (contract R32).
fn no_session_refusal() -> String {
    format!(
        "mcp-server is an internal driver helper and requires --session-id; {} does not run a public MCP server",
        crate::host_identity::app()
    )
}

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
    /// Changes whenever the vendor starts a fresh gateway process, because
    /// JSON-RPC request ids restart from a small integer at that point.
    generation: String,
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
        let daemon_url = std::env::var(rightkit_agent_config::env_name("DAEMON_URL"))
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:51123".to_string())
            .trim_end_matches('/')
            .to_string();
        // No proxy, no redirects: the capability token goes to the daemon host only.
        let client = rightkit_agent_daemon::http_client::daemon_client_probe(
            &daemon_url,
            CONNECT_TIMEOUT,
            REQUEST_TIMEOUT,
        )?;
        Ok(Some(Self {
            daemon_url,
            capability_token: required(&rightkit_agent_config::env_name("GATEWAY_TOKEN"))?,
            driver_instance_secret: required(&rightkit_agent_config::env_name(
                "DRIVER_INSTANCE_SECRET",
            ))?,
            session_id: session_id.to_string(),
            generation: format!(
                "{}-{:x}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_nanos())
                    .unwrap_or_default()
            ),
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
                    .secret_header(
                        crate::host_identity::header("capability-token"),
                        &self.capability_token,
                    )
                    .secret_header(
                        crate::host_identity::header("driver-instance"),
                        &self.driver_instance_secret,
                    ),
            )
            .await
            .map_err(|error| format!("gateway tools request failed: {error}"))?;
        let status = response.status;
        let body: Value = response
            .json()
            .map_err(|error| format!("gateway tools response was not JSON: {error}"))?;
        if !rightkit_agent_daemon::http_client::is_success(status) {
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
            .secret_header(
                crate::host_identity::header("capability-token"),
                &self.capability_token,
            )
            .secret_header(
                crate::host_identity::header("driver-instance"),
                &self.driver_instance_secret,
            )
            .json(&json!({
                "callId": format!(
                    "{}-{}",
                    self.generation,
                    match request_id {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    }
                ),
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
        if !rightkit_agent_daemon::http_client::is_success(status) {
            return Err(format!("gateway tool call rejected ({status}): {body}"));
        }
        Ok(body)
    }
}

pub async fn run_stdio(options: McpServerOptions) -> i32 {
    if std::env::var_os(rightkit_agent_config::env_name("VENDOR_PIPE")).is_some() {
        return crate::vendor_mcp_pipe::proxy_stdio().await;
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
    if gateway.is_none() && !crate::host_options::mcp_public_server() {
        // Contract R32: on this host `mcp-server` is the session-bound driver helper only.
        // The public no-session server is deleted.
        let _ = writeln!(
            writer,
            "{}",
            error_response(None, -32000, no_session_refusal())
        );
        return 1;
    }
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
    // Bound to a session's gateway, this process is a proxy: the engine decides which tools the
    // lease allows and puts every call through its own policy and approvals. Filtering here as
    // well left a vendor program with the read-only tools alone (memory recall), whatever the
    // lease allowed. The read-only default stays for the stand-alone server, where no engine
    // session governs the calls.
    let allow_mutate = options.allow_mutate || gateway.is_some();
    let trust_tier = std::env::var(rightkit_agent_config::env_name("MCP_TRUST_TIER"))
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
    let mut bytes = Vec::new();
    let mut reader = reader;
    loop {
        bytes.clear();
        let read = match read_bounded_line(&mut reader, &mut bytes) {
            Ok(read) => read,
            Err(_) => {
                return 1;
            }
        };
        if read == 0 {
            break;
        }
        if bytes.len() > MAX_MCP_LINE_BYTES {
            let _ = writeln!(
                writer,
                "{}",
                error_response(None, -32600, "MCP request exceeds size limit")
            );
            let _ = writer.flush();
            return 1;
        }
        let Ok(line) = std::str::from_utf8(&bytes) else {
            return 1;
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(line) {
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

/// Application backend: the host retains execution, permissions & error codes.
struct HostMcpBackend {
    gateway: Option<SessionGateway>,
    inline: Option<Arc<crate::main_runtime::InlineMcpProjection>>,
    inline_tools: Vec<Value>,
    vendor_permissions: bool,
}

impl HostMcpBackend {
    fn raw_tools(&self) -> Vec<Value> {
        let mut tools = self.inline_tools.clone();
        if self.vendor_permissions {
            tools.push(json!({
                "name": "permission_prompt",
                "description": format!(
                    "Request {app} policy approval for a {app} tool call",
                    app = crate::host_identity::app()
                ),
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

impl RightKitMcpBackend for HostMcpBackend {
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
                            "behavior": "deny",
                            "message": format!("{} approval channel unavailable", crate::host_identity::app())
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

/// Thin ingress policy adapter; the shared MCP crate owns parsed protocol dispatch.
pub(crate) struct McpStdioServer {
    host: RightKitMcpServer,
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
        let backend = Arc::new(HostMcpBackend {
            gateway,
            inline: inline.map(Arc::new),
            inline_tools,
            vendor_permissions,
        });
        let host = RightKitMcpServer::new(
            crate::host_identity::app(),
            env!("CARGO_PKG_VERSION"),
            backend,
            policy,
        );
        Self {
            host,
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

// The vendor MCP pipe (Windows) answers each request through this server (rightkit-agents `VendorMcpHandler`).
#[cfg(windows)]
impl rightkit_agents::vendor_mcp_pipe::VendorMcpHandler for McpStdioServer {
    fn handle_request(
        &mut self,
        request: Value,
    ) -> impl std::future::Future<Output = Option<Value>> + Send {
        McpStdioServer::handle_request(self, request)
    }
}

#[cfg(test)]
mod bounded_line_tests {
    use super::*;

    #[test]
    fn oversized_line_is_cut_at_the_limit() {
        let mut input = std::io::Cursor::new(vec![b'a'; 3 * MAX_MCP_LINE_BYTES]);
        let mut line = Vec::new();
        let read = read_bounded_line(&mut input, &mut line).unwrap();
        assert_eq!(read, MAX_MCP_LINE_BYTES + 1);
        assert!(line.len() > MAX_MCP_LINE_BYTES);
        assert_eq!(input.position(), MAX_MCP_LINE_BYTES as u64 + 1);
    }

    #[test]
    fn normal_lines_read_one_at_a_time() {
        let mut input = std::io::Cursor::new(b"{\"a\":1}\n{\"b\":2}\n".to_vec());
        let mut line = Vec::new();
        assert_eq!(read_bounded_line(&mut input, &mut line).unwrap(), 8);
        assert_eq!(line, b"{\"a\":1}\n");
        line.clear();
        assert_eq!(read_bounded_line(&mut input, &mut line).unwrap(), 8);
        line.clear();
        assert_eq!(read_bounded_line(&mut input, &mut line).unwrap(), 0);
    }
}
