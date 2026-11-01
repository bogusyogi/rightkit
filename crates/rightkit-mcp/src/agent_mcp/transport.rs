//! the host app policy/API adapters over rightkit-mcp; transport I/O belongs to RightKit.

use std::collections::HashMap;

use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type ElicitationFuture = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;
pub type ElicitationHandler = Arc<
    dyn Fn(String, rightkit_agent_engine_protocol::ElicitationRequestEvent) -> ElicitationFuture
        + Send
        + Sync,
>;

use crate::agent_mcp::error::McpError;
use crate::agent_mcp::types::{McpResourceContent, McpResourceDef, McpToolDef, McpToolResult};

use crate::{
    CallOptions, CancellationToken, ClientExtensions, ClientOptions, EndpointOriginPolicy,
    LaunchRequest, LaunchedProcess, McpClient, ProcessChild, ProcessLauncher, ResponseLimits,
    ServerSource, ServerSpec, TrustStore,
};
use std::io::{Read, Write};
use std::sync::{Condvar, Mutex};

/// Keep RightKit failures behind the host app's existing public error boundary.
pub fn map_rightkit_error(error: crate::McpError) -> McpError {
    match error {
        crate::McpError::Cancelled => McpError::Cancelled,
        crate::McpError::Closed => McpError::NotConnected,
        crate::McpError::ToolNotFound(name) => McpError::ToolNotFound(name),
        crate::McpError::Rpc { code, message } => {
            McpError::Transport(serde_json::json!({"code": code, "message": message}).to_string())
        }
        crate::McpError::RpcWithData {
            code,
            message,
            data,
        } => McpError::Transport(
            serde_json::json!({"code": code, "message": message, "data": data}).to_string(),
        ),
        crate::McpError::CrossOriginEndpoint {
            configured_origin,
            endpoint_origin,
        } => McpError::CrossOriginEndpoint {
            configured_origin,
            endpoint_origin,
        },
        crate::McpError::ResponseTooLarge { limit } => {
            McpError::Transport(format!("MCP response exceeded {limit} bytes"))
        }
        other => McpError::Transport(other.to_string()),
    }
}

struct BearerToken(crate::Secret);

impl crate::AuthProvider for BearerToken {
    fn bearer(&self, _: &str) -> crate::Result<Option<crate::Secret>> {
        Ok(Some(self.0.clone()))
    }

    fn on_unauthorized(&self, _: &str, _: Option<&str>) -> crate::Result<bool> {
        // Refresh & credential persistence remain in the host app's OAuth owner.
        Ok(false)
    }
}

type HandlerRegistry = Arc<Mutex<HashMap<String, (String, ElicitationHandler)>>>;

struct ClientAdapter {
    spec: ServerSpec,
    options: ClientOptions,
    extensions: ClientExtensions,
    client: Mutex<Option<Arc<McpClient>>>,
    handlers: HandlerRegistry,
    adapter: Option<Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>>,
    launch_context: Option<rightkit_sandbox::execution::ProcessLaunchContext>,
    configuration_error: Option<String>,
    /// Count of `notifications/tools/list_changed` seen from the server.
    tools_list_changed: Arc<std::sync::atomic::AtomicU64>,
}

impl ClientAdapter {
    fn new(spec: ServerSpec, access_token: Option<String>, legacy_sse: bool) -> Self {
        let stdio = matches!(&spec, ServerSpec::Stdio { .. });
        let tools_list_changed = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let list_changed_counter = tools_list_changed.clone();
        Self {
            spec,
            options: ClientOptions {
                request_timeout: if stdio {
                    crate::agent_mcp::limits::MCP_STDIO_REQUEST_TIMEOUT
                } else {
                    crate::agent_mcp::limits::MCP_HTTP_REQUEST_TIMEOUT
                },
                connect_timeout: None,
                auth: access_token.map(|token| {
                    Arc::new(BearerToken(crate::Secret::new(token))) as Arc<dyn crate::AuthProvider>
                }),
                client_name: "rightkit-agent".into(),
                client_version: env!("CARGO_PKG_VERSION").into(),
                on_event: Some(Arc::new(
                    |event| tracing::debug!(event = ?event, "MCP lifecycle"),
                )),
                on_notification: Some(Arc::new(move |notification| {
                    tracing::debug!(server = %notification.server, method = %notification.method, "MCP server notification");
                    if notification.method == "notifications/tools/list_changed" {
                        // McpManager compares this count via `tools_list_changed_seq`
                        // and drops its listing and routing caches on a change.
                        list_changed_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                })),
            },
            extensions: ClientExtensions {
                legacy_sse,
                endpoint_origin_policy: EndpointOriginPolicy::default(),
                // rightkit-mcp 0.7.0 (rightkit#165): literal localhost/127.0.0.1/[::1] HTTP servers
                // are reachable; names resolving to loopback and private ranges stay refused.
                allow_loopback: !stdio,
                protocol_versions: Some(
                    MCP_SUPPORTED_VERSIONS.iter().map(|v| (*v).into()).collect(),
                ),
                response_limits: ResponseLimits {
                    max_response_bytes: 8 * 1024 * 1024,
                    max_frame_bytes: if stdio || legacy_sse {
                        1024 * 1024
                    } else {
                        8 * 1024 * 1024
                    },
                },
                #[cfg(feature = "agent_mcp-test-loopback")]
                address_policy: Some(rightkit_http::testing::loopback_policy()),
                ..Default::default()
            },
            client: Mutex::new(None),
            handlers: Arc::new(Mutex::new(HashMap::new())),
            adapter: None,
            launch_context: None,
            configuration_error: None,
            tools_list_changed,
        }
    }

    // Only called from spawn_blocking: RightKit's synchronous API & managed
    // pipe adapters must never block an async executor thread.
    fn connected(
        &self,
        runtime: tokio::runtime::Handle,
        cancellation: &CancellationToken,
    ) -> Result<Arc<McpClient>, McpError> {
        if let Some(message) = &self.configuration_error {
            return Err(McpError::Transport(message.clone()));
        }
        let mut slot = self
            .client
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(client) = slot.as_ref() {
            return Ok(client.clone());
        }
        let mut extensions = self.extensions.clone();
        if matches!(&self.spec, ServerSpec::Stdio { .. }) {
            let adapter = self.adapter.clone().ok_or_else(|| {
                McpError::Transport(
                    "MCP stdio launch requires an injected host process adapter".into(),
                )
            })?;
            let context = self.launch_context.clone().ok_or_else(|| {
                McpError::Transport("MCP stdio launch requires a host launch context".into())
            })?;
            extensions.launcher =
                Some(rightkit_process_launcher(adapter, context, runtime.clone()));
        }
        extensions.elicitation = Some(self.callback(runtime));
        // Trust is decided before a transport exists: the host app's own first-use gate
        // (`trust_gate`, applied in `config::load_with_trust_gate*`) refuses to register
        // an unapproved workspace stdio server, and user-level/default servers are
        // exempt by design. RightKit's `ServerSource::User` is the same exemption; a
        // `Workspace` source here would need records in RightKit's fingerprint format
        // and would deny every server (empty store), so it is not used.
        let client = Arc::new(
            McpClient::connect_with_extensions_cancellable(
                "rightkit-agent",
                &ServerSource::User,
                &TrustStore::new(),
                self.spec.clone(),
                self.options.clone(),
                extensions,
                cancellation,
            )
            .map_err(map_rightkit_error)?,
        );
        *slot = Some(client.clone());
        Ok(client)
    }

    fn callback(&self, runtime: tokio::runtime::Handle) -> crate::ElicitationHandler {
        let handlers = self.handlers.clone();
        Arc::new(move |context, params| {
            let registration = handlers
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&context.session_id)
                .cloned();
            let (server_name, handler) = registration.ok_or_else(|| {
                crate::McpError::Protocol("elicitation has no registered session".into())
            })?;
            // RightKit owns wire IDs. the host app uses its own UI correlation ID;
            // it never rewrites or replies to the JSON-RPC request itself.
            static NEXT_ELICITATION: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(1);
            let id = format!(
                "rightkit-elicitation-{}",
                NEXT_ELICITATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            );
            let event = parse_elicitation_request(&server_name, id, params)
                .map_err(crate::McpError::Protocol)?;
            runtime
                .block_on(handler(context.session_id.clone(), event))
                .map_err(crate::McpError::Protocol)
        })
    }

    async fn execute<T: Send + 'static>(
        self: &Arc<Self>,
        cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
        operation: impl FnOnce(&McpClient, &CallOptions) -> Result<T, McpError> + Send + 'static,
        session_id: Option<String>,
    ) -> Result<T, McpError> {
        if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
            return Err(McpError::Cancelled);
        }
        let cancellation = CancellationToken::new();
        let _cancel_on_drop = CancelOnDrop(cancellation.clone());
        let state = self.clone();
        let runtime = tokio::runtime::Handle::current();
        let options = CallOptions {
            session_id,
            headers: Default::default(),
            cancellation: Some(cancellation.clone()),
        };
        let cancellation_for_connect = cancellation.clone();
        let mut task = tokio::task::spawn_blocking(move || {
            if options
                .cancellation
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                return Err(McpError::Cancelled);
            }
            let client = state.connected(runtime.clone(), &cancellation_for_connect)?;
            if options
                .cancellation
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                return Err(McpError::Cancelled);
            }
            let sessions: Vec<_> = state
                .handlers
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .keys()
                .cloned()
                .collect();
            for session in sessions {
                client
                    .set_elicitation_handler(&session, state.callback(runtime.clone()))
                    .map_err(map_rightkit_error)?;
            }
            operation(&client, &options)
        });
        tokio::select! {
            result = &mut task => result.map_err(|error| McpError::Transport(error.to_string()))?,
            _ = watch_cancellation(cancel_rx) => {
                cancellation.cancel();
                // RightKit sends notifications/cancelled & interrupts request I/O.
                // Cold connect also observes this token (connect_with_extensions_cancellable).
                Err(McpError::Cancelled)
            }
        }
    }

    fn set_handler(&self, session: String, server: String, handler: ElicitationHandler) {
        self.handlers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(session, (server, handler));
    }

    fn clear_handler(&self, session: &str) {
        self.handlers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(session);
        // Callback resolves registration at invocation, so stale RightKit
        // registrations cannot borrow a handler from another session.
    }

    fn initialized(&self) -> bool {
        match self.client.try_lock() {
            Ok(slot) => slot.is_some(),
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner().is_some(),
            Err(std::sync::TryLockError::WouldBlock) => false,
        }
    }

    async fn version(self: &Arc<Self>) -> Option<String> {
        // An unused adapter's diagnostics never establish a connection.
        let client = match self.client.try_lock() {
            Ok(slot) => slot.clone(),
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner().clone(),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }?;
        tokio::task::spawn_blocking(move || client.protocol_version())
            .await
            .ok()
    }

    async fn session(self: &Arc<Self>) -> Option<String> {
        let client = match self.client.try_lock() {
            Ok(slot) => slot.clone(),
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner().clone(),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }?;
        tokio::task::spawn_blocking(move || client.session_id().ok().flatten())
            .await
            .ok()
            .flatten()
    }
}

impl Drop for ClientAdapter {
    fn drop(&mut self) {
        let client = self
            .client
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(client) = client {
            // RightKit closes & reaps synchronously. Keep disposal off Tokio,
            // including current-thread runtimes used by callers & tests.
            std::thread::spawn(move || client.close());
        }
    }
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn watch_cancellation(mut receiver: Option<tokio::sync::watch::Receiver<bool>>) {
    if let Some(receiver) = receiver.as_mut() {
        loop {
            if *receiver.borrow() {
                return;
            }
            if receiver.changed().await.is_err() {
                break;
            }
        }
    }
    std::future::pending::<()>().await;
}

pub struct HttpTransport(Arc<ClientAdapter>);
pub struct SseTransport(Arc<ClientAdapter>);
pub struct StdioTransport(Arc<ClientAdapter>);

impl HttpTransport {
    pub fn new(url: impl Into<String>) -> Self {
        Self::with_access_token(url, None)
    }
    pub fn with_access_token(url: impl Into<String>, token: Option<String>) -> Self {
        Self(Arc::new(ClientAdapter::new(
            ServerSpec::Http { url: url.into() },
            token,
            false,
        )))
    }
    pub async fn negotiated_version(&self) -> Option<String> {
        self.0.version().await
    }
    pub async fn session_id(&self) -> Option<String> {
        self.0.session().await
    }
    pub fn is_initialized(&self) -> bool {
        self.0.initialized()
    }
}

impl SseTransport {
    pub fn new(url: impl Into<String>) -> Self {
        Self::with_access_token(url, None)
    }
    pub fn with_access_token(url: impl Into<String>, token: Option<String>) -> Self {
        Self::with_access_token_and_origin_policy(url, token, false)
    }
    pub fn with_access_token_and_origin_policy(
        url: impl Into<String>,
        token: Option<String>,
        allow_cross_origin_endpoint: bool,
    ) -> Self {
        let mut adapter = ClientAdapter::new(ServerSpec::Http { url: url.into() }, token, true);
        if allow_cross_origin_endpoint {
            adapter.configuration_error = Some(
                "rightkit-mcp requires explicit allowed origins; blanket allow_cross_origin_endpoint is unsupported".into(),
            );
        }
        Self(Arc::new(adapter))
    }
    /// Explicit origins are enforced by RightKit before credentials are sent.
    pub fn with_endpoint_origin_policy(mut self, policy: EndpointOriginPolicy) -> Self {
        let adapter = Arc::get_mut(&mut self.0).expect("unshared transport builder");
        adapter.extensions.endpoint_origin_policy = policy;
        adapter.configuration_error = None;
        self
    }
    pub async fn negotiated_version(&self) -> Option<String> {
        self.0.version().await
    }
    pub fn is_initialized(&self) -> bool {
        self.0.initialized()
    }
}

impl StdioTransport {
    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> Self {
        Self(Arc::new(ClientAdapter::new(
            ServerSpec::Stdio {
                command: command.into(),
                args,
                env: env.into_iter().collect(),
                cwd: None,
            },
            None,
            false,
        )))
    }
    pub fn with_launch_context(
        mut self,
        context: rightkit_sandbox::execution::ProcessLaunchContext,
    ) -> Self {
        let adapter = Arc::get_mut(&mut self.0).expect("unshared transport builder");
        if let ServerSpec::Stdio { cwd, .. } = &mut adapter.spec {
            *cwd = Some(context.workspace_root.clone().into());
        }
        adapter.launch_context = Some(context);
        self
    }
    pub fn with_process_launch_adapter(
        mut self,
        launcher: Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>,
    ) -> Self {
        Arc::get_mut(&mut self.0)
            .expect("unshared transport builder")
            .adapter = Some(launcher);
        self
    }
    pub async fn negotiated_version(&self) -> Option<String> {
        self.0.version().await
    }
}

fn tool_definition(tool: crate::ToolDescriptor) -> Result<McpToolDef, McpError> {
    if tool.name.trim().is_empty() {
        return Err(McpError::Serde("MCP tool omitted a non-empty name".into()));
    }
    Ok(McpToolDef {
        name: tool.name,
        description: tool.description,
        input_schema: tool.input_schema,
        annotations: crate::agent_mcp::types::McpToolAnnotations {
            title: tool.annotations.title,
            read_only_hint: tool.annotations.read_only,
            destructive_hint: tool.annotations.destructive,
            idempotent_hint: tool.annotations.idempotent,
            open_world_hint: tool.annotations.open_world,
        },
    })
}

macro_rules! rightkit_transport {
    ($transport:ty) => {
        #[async_trait::async_trait]
        impl McpTransport for $transport {
            fn set_elicitation_handler(&self, session: String, server: String, handler: ElicitationHandler) {
                self.0.set_handler(session, server, handler);
            }
            fn clear_elicitation_handler(&self, session: &str) { self.0.clear_handler(session); }
            fn tools_list_changed_seq(&self) -> u64 {
                self.0.tools_list_changed.load(std::sync::atomic::Ordering::Relaxed)
            }
            fn endpoint_identity(&self) -> Option<String> {
                Some(match &self.0.spec {
                    ServerSpec::Stdio { command, args, .. } => format!("stdio://{command} {args:?}"),
                    ServerSpec::Http { url } => url.clone(),
                })
            }
            async fn list_tools(&self) -> Result<Vec<McpToolDef>, McpError> {
                self.0.execute(None, |client, options| {
                    // Use the cancellable raw API so future disposal owns every
                    // page; typed legacy list methods have no CallOptions.
                    let mut tools = Vec::new();
                    let mut params = serde_json::json!({});
                    loop {
                        let result = client.request_with_options("tools/list", params, options).map_err(map_rightkit_error)?;
                        let page: Vec<crate::ToolDescriptor> = serde_json::from_value(
                            result.get("tools").cloned().ok_or_else(|| McpError::Serde("expected 'tools' array".into()))?,
                        ).map_err(|error| McpError::Serde(error.to_string()))?;
                        tools.extend(page.into_iter().map(tool_definition).collect::<Result<Vec<_>, _>>()?);
                        match result.get("nextCursor").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                            Some(cursor) => params = serde_json::json!({"cursor": cursor}),
                            None => return Ok(tools),
                        }
                    }
                }, None).await
            }
            async fn call_tool(&self, name: &str, args: Value, session: Option<&str>) -> Result<McpToolResult, McpError> {
                self.call_tool_with_cancel(name, args, session, None).await
            }
            async fn call_tool_with_cancel(&self, name: &str, args: Value, session: Option<&str>, cancel: Option<tokio::sync::watch::Receiver<bool>>) -> Result<McpToolResult, McpError> {
                let name = name.to_string();
                self.0.execute(cancel, move |client, options| {
                    let result = client.request_with_options("tools/call", serde_json::json!({"name": name, "arguments": args}), options).map_err(map_rightkit_error)?;
                    parse_tool_result(result)
                }, session.map(str::to_string)).await
            }
            async fn list_resources(&self) -> Result<Vec<McpResourceDef>, McpError> {
                self.0.execute(None, |client, options| {
                    let result = client.request_with_options("resources/list", serde_json::json!({}), options).map_err(map_rightkit_error)?;
                    let resources: Vec<crate::ResourceDescriptor> = serde_json::from_value(
                        result.get("resources").cloned().ok_or_else(|| McpError::Serde("expected 'resources' array".into()))?,
                    ).map_err(|error| McpError::Serde(error.to_string()))?;
                    Ok(resources.into_iter().map(|r| McpResourceDef {
                        uri: r.uri, name: r.name, description: r.description, mime_type: r.mime_type,
                    }).collect())
                }, None).await
            }
            async fn read_resource(&self, uri: &str) -> Result<McpResourceContent, McpError> {
                let uri = uri.to_string();
                self.0.execute(None, move |client, options| {
                    let result = client.request_with_options("resources/read", serde_json::json!({"uri": uri}), options).map_err(map_rightkit_error)?;
                    let mut item = result.pointer("/contents/0").cloned().ok_or_else(|| McpError::Serde("empty contents array".into()))?;
                    if item.get("uri").and_then(Value::as_str).is_none() {
                        item.as_object_mut().ok_or_else(|| McpError::Serde("expected resource content object".into()))?
                            .insert("uri".into(), Value::String(uri.clone()));
                    }
                    let content: crate::ResourceContents = serde_json::from_value(item).map_err(|error| McpError::Serde(error.to_string()))?;
                    Ok(McpResourceContent {
                        uri: content.uri,
                        text: content.text, blob: content.blob, mime_type: content.mime_type,
                    })
                }, None).await
            }
        }
    };
}

rightkit_transport!(HttpTransport);
rightkit_transport!(SseTransport);
rightkit_transport!(StdioTransport);

/// Adapt the host app's sole process policy owner to RightKit's blocking pipes.
/// Call the returned launcher from a blocking thread with a live Tokio host.
pub fn rightkit_process_launcher(
    adapter: Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>,
    context: rightkit_sandbox::execution::ProcessLaunchContext,
    runtime: tokio::runtime::Handle,
) -> Arc<dyn ProcessLauncher> {
    Arc::new(
        move |request: &LaunchRequest| -> crate::Result<LaunchedProcess> {
            let mut child = runtime
                .block_on(
                    adapter.spawn(rightkit_sandbox::execution::CommandLaunchSpec {
                        program: request.program.clone(),
                        argv: request.args.clone(),
                        cwd: request
                            .cwd
                            .as_ref()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_else(|| context.workspace_root.clone()),
                        context: context.clone(),
                        env: request
                            .env
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                    }),
                )
                .map_err(|error| {
                    crate::McpError::Transport(format!("managed MCP launch: {error:?}"))
                })?;
            let pipes = child.take_stdin().zip(child.take_stdout());
            let (stdin, stdout) = match pipes {
                Some(pipes) => pipes,
                None => {
                    let _ = runtime.block_on(child.cancel());
                    let _ = runtime.block_on(child.wait());
                    return Err(crate::McpError::Transport(
                        "managed MCP launch omitted piped stdio".into(),
                    ));
                }
            };
            let pid = child.pid().unwrap_or(0);
            let state = Arc::new(ManagedExit {
                outcome: Mutex::new(None),
                changed: Condvar::new(),
            });
            let (kill_tx, mut kill_rx) = tokio::sync::mpsc::unbounded_channel();
            let completion = state.clone();
            runtime.spawn(async move {
                let mut monitor = ManagedMonitor { child, completion };
                let result = tokio::select! {
                    result = monitor.child.wait() => result,
                    _ = kill_rx.recv() => {
                        if let Err(error) = monitor.child.cancel().await {
                            tracing::warn!(pid, error = ?error, "managed MCP cancellation failed");
                            // Releasing process ownership while it may still be alive
                            // is unsafe; continue waiting for the actual exit below.
                        }
                        monitor.child.wait().await
                    }
                }
                .map_err(|error| format!("managed MCP wait: {error:?}"));
                *monitor
                    .completion
                    .outcome
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(result);
                monitor.completion.changed.notify_all();
            });
            Ok(LaunchedProcess {
                child: Box::new(ManagedProcess {
                    pid,
                    kill: kill_tx,
                    state,
                }),
                stdin: Box::new(ManagedInput {
                    pipe: stdin,
                    runtime: runtime.clone(),
                }),
                stdout: Box::new(ManagedOutput {
                    pipe: stdout,
                    runtime: runtime.clone(),
                }),
            })
        },
    )
}

struct ManagedInput {
    pipe: Pin<Box<dyn tokio::io::AsyncWrite + Send>>,
    runtime: tokio::runtime::Handle,
}

impl Write for ManagedInput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        use tokio::io::AsyncWriteExt;
        self.runtime.block_on(self.pipe.as_mut().write(bytes))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        self.runtime.block_on(self.pipe.as_mut().flush())
    }
}

struct ManagedOutput {
    pipe: Pin<Box<dyn tokio::io::AsyncRead + Send>>,
    runtime: tokio::runtime::Handle,
}

impl Read for ManagedOutput {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        use tokio::io::AsyncReadExt;
        self.runtime.block_on(self.pipe.as_mut().read(bytes))
    }
}

struct ManagedExit {
    outcome: Mutex<Option<Result<i32, String>>>,
    changed: Condvar,
}

struct ManagedMonitor {
    child: Box<dyn rightkit_sandbox::execution::ManagedChild>,
    completion: Arc<ManagedExit>,
}

impl Drop for ManagedMonitor {
    fn drop(&mut self) {
        let mut outcome = self
            .completion
            .outcome
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if outcome.is_none() {
            let _ = self.child.start_kill();
            *outcome = Some(Err(
                "managed MCP supervisor stopped before process exit".into()
            ));
            self.completion.changed.notify_all();
        }
    }
}

struct ManagedProcess {
    pid: u32,
    kill: tokio::sync::mpsc::UnboundedSender<()>,
    state: Arc<ManagedExit>,
}

fn exit_status(result: &Result<i32, String>) -> std::io::Result<std::process::ExitStatus> {
    let code = *result
        .as_ref()
        .map_err(|message| std::io::Error::other(message.clone()))?;
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        Ok(std::process::ExitStatus::from_raw(code as u32))
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        Ok(std::process::ExitStatus::from_raw(code << 8))
    }
}

impl ProcessChild for ManagedProcess {
    fn id(&self) -> u32 {
        self.pid
    }
    fn kill(&mut self) -> std::io::Result<()> {
        if self
            .state
            .outcome
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
        {
            return Ok(());
        }
        self.kill
            .send(())
            .map_err(|_| std::io::Error::other("managed MCP supervisor stopped"))
    }
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.state
            .outcome
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(exit_status)
            .transpose()
    }
    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let mut outcome = self
            .state
            .outcome
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while outcome.is_none() {
            outcome = self
                .state
                .changed
                .wait(outcome)
                .unwrap_or_else(|error| error.into_inner());
        }
        exit_status(outcome.as_ref().expect("observed process exit"))
    }
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        // Dropping RightKit's child ownership also retires host supervision.
        let _ = self.kill.send(());
    }
}

// ── Trait ──────────────────────────────────────────────────────────────────

#[async_trait::async_trait]
pub trait McpTransport: Send + Sync {
    fn set_elicitation_handler(
        &self,
        _session_id: String,
        _server_name: String,
        _handler: ElicitationHandler,
    ) {
    }
    fn clear_elicitation_handler(&self, _session_id: &str) {}
    /// Stable configured endpoint identity, when transport has one. Canonical
    /// startup binding compares this before accepting authority.
    fn endpoint_identity(&self) -> Option<String> {
        None
    }

    /// Monotonic count of `notifications/tools/list_changed` received from the
    /// server. The manager compares it to the last value it saw and, on a
    /// change, drops any in-flight listing and the routing registry so the
    /// next discovery or call lists live. Transports that cannot observe the
    /// notification keep the default `0`; the manager lists live on every
    /// discovery and call regardless, so the default is never stale.
    fn tools_list_changed_seq(&self) -> u64 {
        0
    }

    async fn list_tools(&self) -> Result<Vec<McpToolDef>, McpError>;
    /// HTTP session override or stdio logical session for elicitation routing.
    async fn call_tool(
        &self,
        name: &str,
        args: Value,
        session_id: Option<&str>,
    ) -> Result<McpToolResult, McpError>;

    /// Call a tool while retaining cancellation ownership at transport
    /// boundary. Implementations may override this to send protocol-level
    /// cancellation; default drops the in-flight request future, which
    /// cancels transports whose request lifetime is tied to that future.
    async fn call_tool_with_cancel(
        &self,
        name: &str,
        args: Value,
        session_id: Option<&str>,
        mut cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Result<McpToolResult, McpError> {
        if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
            return Err(McpError::Cancelled);
        }
        let call = self.call_tool(name, args, session_id);
        match cancel_rx.as_mut() {
            Some(rx) => {
                tokio::select! {
                    result = call => result,
                    changed = rx.changed() => {
                        if changed.is_ok() && *rx.borrow() { Err(McpError::Cancelled) }
                        else { Err(McpError::Transport("cancellation watch closed".into())) }
                    }
                }
            }
            None => call.await,
        }
    }

    async fn list_resources(&self) -> Result<Vec<McpResourceDef>, McpError>;
    async fn read_resource(&self, uri: &str) -> Result<McpResourceContent, McpError>;
}

// ── Stub (tests / offline) ─────────────────────────────────────────────────

pub struct StubTransport {
    tools: Vec<McpToolDef>,
    responses: HashMap<String, McpToolResult>,
    resources: Vec<McpResourceDef>,
    resource_contents: HashMap<String, McpResourceContent>,
    endpoint_identity: Option<String>,
}

impl StubTransport {
    pub fn new(tools: Vec<McpToolDef>) -> Self {
        Self {
            tools,
            responses: HashMap::new(),
            resources: Vec::new(),
            resource_contents: HashMap::new(),
            endpoint_identity: None,
        }
    }

    pub fn with_endpoint_identity(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint_identity = Some(endpoint.into());
        self
    }

    pub fn with_response(mut self, tool_name: &str, result: McpToolResult) -> Self {
        self.responses.insert(tool_name.to_string(), result);
        self
    }

    pub fn with_resources(mut self, resources: Vec<McpResourceDef>) -> Self {
        self.resources = resources;
        self
    }

    pub fn with_resource_content(mut self, uri: &str, content: McpResourceContent) -> Self {
        self.resource_contents.insert(uri.to_string(), content);
        self
    }
}

#[async_trait::async_trait]
impl McpTransport for StubTransport {
    fn endpoint_identity(&self) -> Option<String> {
        self.endpoint_identity.clone()
    }

    async fn list_tools(&self) -> Result<Vec<McpToolDef>, McpError> {
        Ok(self.tools.clone())
    }

    async fn call_tool(
        &self,
        name: &str,
        _args: Value,
        _session_id: Option<&str>,
    ) -> Result<McpToolResult, McpError> {
        self.responses
            .get(name)
            .cloned()
            .ok_or_else(|| McpError::ToolNotFound(name.to_string()))
    }

    async fn list_resources(&self) -> Result<Vec<McpResourceDef>, McpError> {
        Ok(self.resources.clone())
    }

    async fn read_resource(&self, uri: &str) -> Result<McpResourceContent, McpError> {
        self.resource_contents
            .get(uri)
            .cloned()
            .ok_or_else(|| McpError::ResourceNotFound(uri.to_string()))
    }
}

// ── Protocol version negotiation (MCP 2025-03-26) ──────────────────────────

/// Current MCP protocol version pinned for the host app (2025-03-26).
pub use crate::PROTOCOL_VERSION as MCP_PROTOCOL_VERSION;

/// Supported protocol versions for negotiation (newest first).
pub const MCP_SUPPORTED_VERSIONS: &[&str] = &["2025-03-26", "2024-11-05", "2024-10-07"];

/// Build the `initialize` params object per MCP spec.
pub fn initialize_params() -> serde_json::Value {
    serde_json::json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": {"name": "rightkit-agent", "version": "0.1.0"}
    })
}

/// Validate the server's `initialize` result. Returns the negotiated version
/// or an error if the server's version is unsupported.
pub fn negotiate_version(
    server_result: &serde_json::Value,
) -> Result<String, crate::agent_mcp::error::McpError> {
    let version = server_result
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if MCP_SUPPORTED_VERSIONS.contains(&version) {
        Ok(version.to_string())
    } else if version.is_empty() {
        // Server omitted version — assume it accepted our pin.
        Ok(MCP_PROTOCOL_VERSION.to_string())
    } else {
        Err(crate::agent_mcp::error::McpError::Transport(format!(
            "unsupported MCP protocolVersion from server: {version} (expected one of {MCP_SUPPORTED_VERSIONS:?})"
        )))
    }
}

// ── Shared helpers ─────────────────────────────────────────────────────────

pub(super) fn parse_tool_result(val: Value) -> Result<McpToolResult, McpError> {
    // MCP tools/call result: content carries text; structuredContent carries
    // the complete canonical operation result when supplied.
    let result = crate::ToolResult::from_value(&val);
    let is_error = result.is_error;
    let content = if let Some(structured) = result.structured.as_ref() {
        if !structured.is_object() {
            return Err(McpError::Serde(
                "expected 'structuredContent' object".into(),
            ));
        }
        serde_json::to_string(structured).map_err(|error| McpError::Serde(error.to_string()))?
    } else {
        // the host app also accepts legacy blocks with a text field but no type.
        // Keep that application projection instead of RightKit's typed-only text_content().
        val.get("content")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| val.to_string())
    };
    Ok(McpToolResult { content, is_error })
}

fn parse_elicitation_request(
    server_name: &str,
    request_id: String,
    params: &Value,
) -> Result<rightkit_agent_engine_protocol::ElicitationRequestEvent, String> {
    use rightkit_agent_engine_protocol::ElicitationRequest;
    let mode = params
        .get("mode")
        .and_then(Value::as_str)
        .ok_or_else(|| "elicitation mode is missing".to_string())?;
    let message = params
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| "elicitation message is missing".to_string())?
        .to_string();
    let request = match mode {
        "form" => ElicitationRequest::Form {
            message,
            requested_schema: params
                .get("requestedSchema")
                .cloned()
                .ok_or_else(|| "elicitation requestedSchema is missing".to_string())?,
        },
        "url" => ElicitationRequest::Url {
            message,
            url: params
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| "elicitation URL is missing".to_string())?
                .to_string(),
            elicitation_id: params
                .get("elicitationId")
                .and_then(Value::as_str)
                .ok_or_else(|| "elicitation id is missing".to_string())?
                .to_string(),
        },
        _ => return Err(format!("unsupported elicitation mode: {mode}")),
    };
    Ok(rightkit_agent_engine_protocol::ElicitationRequestEvent {
        turn_id: None,
        server_name: server_name.to_string(),
        id: request_id,
        request,
    })
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod transport_tests;
