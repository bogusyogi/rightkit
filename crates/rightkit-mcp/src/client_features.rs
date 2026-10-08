//! Opt-in client features; existing `ClientOptions` & `ServerSpec` remain unchanged.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value;

use crate::{McpError, Result};

/// Shared cancellation handle. Cancellation is permanent & wakes every waiter.
#[derive(Clone, Default)]
pub struct CancellationToken {
    inner: Arc<CancellationState>,
}

#[derive(Default)]
struct CancellationState {
    cancelled: AtomicBool,
    notify: tokio::sync::Notify,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            // Register before checking, so cancel cannot fall between check & wait.
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
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

/// Only configured origin & explicit allow-list entries may receive SSE POSTs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndpointOriginPolicy {
    pub allowed_origins: Vec<String>,
}

impl EndpointOriginPolicy {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElicitationContext {
    pub server: String,
    /// Protocol HTTP session or caller's stdio session; never another active call's session.
    pub session_id: String,
}

/// Receives original elicitation params & returns complete MCP elicitation result.
pub type ElicitationHandler =
    Arc<dyn Fn(&ElicitationContext, &Value) -> Result<Value> + Send + Sync>;

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
}
