//! Opt-in client features with builder methods & defaulted options.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};

use serde_json::Value;

use crate::{McpError, Result};

/// Shared cancellation handle. Cancellation is permanent & wakes every waiter.
#[derive(Clone, Default, Debug)]
pub struct CancellationToken {
    inner: Arc<CancellationState>,
}

#[derive(Default, Debug)]
struct CancellationState {
    cancelled: AtomicBool,
    /// Wakers of pending `cancelled()` futures; runtime-agnostic (no tokio needed).
    wakers: Mutex<Vec<Waker>>,
}

impl PartialEq for CancellationToken {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for CancellationToken {}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        let wakers =
            std::mem::take(&mut *self.inner.wakers.lock().unwrap_or_else(|e| e.into_inner()));
        for waker in wakers {
            waker.wake();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    pub async fn cancelled(&self) {
        std::future::poll_fn(|cx| {
            if self.is_cancelled() {
                return Poll::Ready(());
            }
            {
                let mut wakers = self.inner.wakers.lock().unwrap_or_else(|e| e.into_inner());
                if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
                    wakers.push(cx.waker().clone());
                }
            }
            // Re-check after registering, so cancel cannot fall between check & wait.
            if self.is_cancelled() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await
    }
}

/// Limits count encoded bytes, including JSON-RPC envelopes & SSE framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseLimits {
    pub max_response_bytes: usize,
    pub max_frame_bytes: usize,
}

impl Default for ResponseLimits {
    fn default() -> Self {
        Self {
            max_response_bytes: 16 * 1024 * 1024,
            max_frame_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Only configured origin & explicit allow-list entries may receive SSE POSTs by default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndpointOriginPolicy {
    /// Explicit origin allow-list; "*" is reserved for allow_any_origin_insecure().
    pub allowed_origins: Vec<String>,
}

impl EndpointOriginPolicy {
    /// INSECURE: permits any HTTP(S) endpoint origin, potentially exposing bearer tokens; never enabled by default.
    pub fn allow_any_origin_insecure() -> Self {
        Self {
            allowed_origins: vec!["*".into()],
        }
    }

    pub fn resolve_endpoint(&self, configured: &str, announced: &str) -> Result<String> {
        fn parse(value: &str) -> Result<url::Url> {
            let url = url::Url::parse(value)
                .map_err(|e| McpError::Protocol(format!("invalid endpoint URL: {e}")))?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(McpError::Protocol(
                    "endpoint must be an HTTP(S) URL without credentials or fragment".into(),
                ));
            }
            Ok(url)
        }
        let base = parse(configured)?;
        let resolved = base
            .join(announced.trim())
            .map_err(|e| McpError::Protocol(format!("invalid SSE endpoint: {e}")))?;
        let endpoint = parse(resolved.as_str())?;
        let configured_origin = base.origin().ascii_serialization();
        let endpoint_origin = endpoint.origin().ascii_serialization();
        if configured_origin != endpoint_origin {
            let mut allowed = false;
            for entry in &self.allowed_origins {
                if entry == "*" {
                    allowed = true;
                    continue;
                }
                if parse(entry)?.origin() == endpoint.origin() {
                    allowed = true;
                }
            }
            if !allowed {
                return Err(McpError::CrossOriginEndpoint {
                    configured_origin,
                    endpoint_origin,
                });
            }
        }
        Ok(endpoint.to_string())
    }
}

/// Exact launch inputs, with caller's environment delta (no ambient secrets copied).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
}

/// Minimal process ownership surface for native or confined child handles.
pub trait ProcessChild: Send {
    fn id(&self) -> u32;
    fn kill(&mut self) -> std::io::Result<()>;
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>>;
    fn wait(&mut self) -> std::io::Result<ExitStatus>;
}

impl ProcessChild for Child {
    fn id(&self) -> u32 {
        Child::id(self)
    }
    fn kill(&mut self) -> std::io::Result<()> {
        Child::kill(self)
    }
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        Child::try_wait(self)
    }
    fn wait(&mut self) -> std::io::Result<ExitStatus> {
        Child::wait(self)
    }
}

/// Caller retains confinement/job ownership through its child implementation &
/// supplies wrapped pipes. `kill` must interrupt that child's pending pipe I/O.
pub struct LaunchedProcess {
    pub child: Box<dyn ProcessChild>,
    pub stdin: Box<dyn Write + Send>,
    pub stdout: Box<dyn Read + Send>,
}

impl LaunchedProcess {
    pub fn from_child(mut child: Child) -> Result<Self> {
        let pipes = child.stdin.take().zip(child.stdout.take());
        match pipes {
            Some((stdin, stdout)) => Ok(Self {
                child: Box::new(child),
                stdin: Box::new(stdin),
                stdout: Box::new(stdout),
            }),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                Err(McpError::Transport(
                    "launcher must return piped stdin & stdout".into(),
                ))
            }
        }
    }
}

pub trait ProcessLauncher: Send + Sync {
    fn launch(&self, request: &LaunchRequest) -> Result<LaunchedProcess>;
}

impl<F> ProcessLauncher for F
where
    F: Fn(&LaunchRequest) -> Result<LaunchedProcess> + Send + Sync,
{
    fn launch(&self, request: &LaunchRequest) -> Result<LaunchedProcess> {
        self(request)
    }
}

/// Existing spawn behavior: inherited environment, piped stdio, null stderr,
/// CREATE_NO_WINDOW on Windows.
pub struct DefaultProcessLauncher;

impl ProcessLauncher for DefaultProcessLauncher {
    fn launch(&self, request: &LaunchRequest) -> Result<LaunchedProcess> {
        let mut command = Command::new(&request.program);
        command
            .args(&request.args)
            .envs(&request.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(cwd) = &request.cwd {
            command.current_dir(cwd);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        LaunchedProcess::from_child(
            command
                .spawn()
                .map_err(|e| McpError::Transport(format!("spawn '{}': {e}", request.program)))?,
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ElicitationContext {
    pub server: String,
    /// Protocol HTTP session or caller's stdio session; never another active call's session.
    pub session_id: String,
    /// Original JSON-RPC request id, retaining string or numeric wire representation.
    pub wire_request_id: Value,
    /// Originating call's cancellation signal, if that call can be identified unambiguously.
    pub call_cancellation: Option<CancellationToken>,
}

impl ElicitationContext {
    /// Returns elicitation's original wire request id.
    pub fn request_id(&self) -> &Value {
        &self.wire_request_id
    }

    /// Returns originating call's shared cancellation signal; None means no unambiguous originating call.
    pub fn cancellation(&self) -> Option<&CancellationToken> {
        self.call_cancellation.as_ref()
    }
}

/// Receives original elicitation params & returns complete MCP elicitation result.
pub type ElicitationHandler =
    Arc<dyn Fn(&ElicitationContext, &Value) -> Result<Value> + Send + Sync>;

/// A server-initiated JSON-RPC notification (a message with a `method` and no `id`), for example
/// `notifications/tools/list_changed`, `notifications/resources/list_changed`,
/// `notifications/prompts/list_changed`, `notifications/progress` or `notifications/message`.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerNotification {
    /// Name of the server the notification came from (the connect `server_name`).
    pub server: String,
    /// JSON-RPC method, e.g. `notifications/tools/list_changed`.
    pub method: String,
    /// `params` exactly as sent; `Value::Null` when absent.
    pub params: Value,
}

/// Receives each server notification. Runs on the connection's reader (stdio, legacy SSE) or on the
/// thread driving the request (streamable HTTP), so it must return quickly and must not call back
/// into the same client; hand the value to a channel instead. A panic is caught and ignored.
pub type NotificationHandler = Arc<dyn Fn(&ServerNotification) + Send + Sync>;

/// Hands `message` to `handler` when it is a notification (`method`, no `id`). Returns true when it
/// was one, whether or not a handler is set, so callers can skip it as a response candidate.
pub(crate) fn deliver_notification(
    handler: &Option<NotificationHandler>,
    server: &str,
    message: &Value,
) -> bool {
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return false;
    };
    if message.get("id").is_some() {
        return false;
    }
    if let Some(handler) = handler {
        let notification = ServerNotification {
            server: server.to_string(),
            method: method.to_string(),
            params: message.get("params").cloned().unwrap_or(Value::Null),
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(&notification)));
    }
    true
}

#[derive(Clone, Default)]
pub struct CallOptions {
    /// Overrides stored HTTP session for this call; a stdio logical routing session.
    pub session_id: Option<String>,
    /// Header names compare case-insensitively. Values override transport defaults.
    pub headers: BTreeMap<String, String>,
    pub cancellation: Option<CancellationToken>,
}

#[derive(Clone, Default)]
pub struct ClientExtensions {
    /// Opt into legacy HTTP+SSE for `ServerSpec::Http` (url is SSE GET URL).
    pub legacy_sse: bool,
    pub endpoint_origin_policy: EndpointOriginPolicy,
    pub launcher: Option<Arc<dyn ProcessLauncher>>,
    /// Default callback binds to each connection's own session.
    pub elicitation: Option<ElicitationHandler>,
    /// None retains existing version pin; Some advertises & negotiates highest common.
    pub protocol_versions: Option<Vec<String>>,
    pub response_limits: ResponseLimits,
    /// Let an HTTP connection reach a loopback MCP server (a user-configured local dev server).
    /// Off by default: HTTP MCP uses the public-web address policy, which refuses loopback.
    /// Applies only when the endpoint URL's host is literally `localhost`, `127.0.0.1` or
    /// `[::1]`, and then only loopback addresses are added for that host. A name that merely
    /// resolves to loopback, and every other private, link-local or metadata address, stays
    /// refused. Hosts should set it only for user-configured servers (rightkit#165).
    pub allow_loopback: bool,
    /// Test-only: replaces the HTTP client's address policy (default: public-web).
    #[cfg(feature = "testing")]
    pub address_policy: Option<rightkit_http::AddressPolicy>,
}
