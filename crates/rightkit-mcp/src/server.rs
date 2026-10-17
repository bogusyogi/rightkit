//! Server host: serves a [`ToolProvider`] over stdio and loopback HTTP with per-tool admission.

use std::collections::{HashMap, HashSet};
use std::future::{poll_fn, Future};
use std::io::{BufRead, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::client_features::CancellationToken;
use crate::error::{McpError, Result};
use crate::events::{emit, next_id, EventSink, LifecycleKind, McpEvent};
use crate::types::*;

/// The typed tool surface a host app (e.g. rightkit-agent-tools) plugs in.
pub trait ToolProvider: Send + Sync {
    fn list(&self) -> Vec<ToolDescriptor>;
    /// Unknown tool => `Err(McpError::ToolNotFound)`. `Ok(ToolResult::error(..))` is a tool-level failure.
    fn call(&self, name: &str, args: Value) -> Result<ToolResult>;
    /// Like [`call`](Self::call) with the request-scoped [`CallHandle`] (progress, cancellation).
    /// The default ignores the handle. Used only when `call_tool*` returns `None`.
    fn call_with_handle(&self, name: &str, args: Value, _call: &CallHandle) -> Result<ToolResult> {
        self.call(name, args)
    }
    /// Complete tools/list payload; preserves custom metadata & pagination.
    fn list_tools(&self, _request: &Value, _context: &RequestContext) -> Result<Value> {
        Ok(json!({"tools": self.list()}))
    }
    /// Optional verbatim tools/call payload, invoked only after admission.
    /// Receives original params, including missing/null arguments. With an admission
    /// hook, calls must have a string name; without one, raw calls may omit the name.
    fn call_tool(&self, _request: &Value, _context: &RequestContext) -> Option<Result<Value>> {
        None
    }
    /// Like [`call_tool`](Self::call_tool) with the request-scoped [`CallHandle`]. The default
    /// delegates to `call_tool`, so existing providers are unchanged.
    fn call_tool_with_handle(
        &self,
        request: &Value,
        context: &RequestContext,
        _call: &CallHandle,
    ) -> Option<Result<Value>> {
        self.call_tool(request, context)
    }
    fn resources(&self) -> Vec<ResourceDescriptor> {
        vec![]
    }
    fn list_resources(&self, _request: &Value, _context: &RequestContext) -> Result<Value> {
        Ok(json!({"resources": self.resources()}))
    }
    /// None keeps the legacy method-not-found response; Some opts in.
    fn list_resource_templates(
        &self,
        _request: &Value,
        _context: &RequestContext,
    ) -> Option<Result<Value>> {
        None
    }
    fn read_resource(&self, uri: &str) -> Result<ResourceContents> {
        Err(McpError::ResourceNotFound(uri.into()))
    }
    /// Complete resources/read payload. Grants are caller claims, not authorization.
    fn read_resource_with_context(&self, uri: &str, _context: &ReadContext<'_>) -> Result<Value> {
        self.read_resource(uri).map(|c| json!({"contents": [c]}))
    }
    fn prompts(&self) -> Vec<PromptDescriptor> {
        vec![]
    }
    /// Complete prompts/list payload, including application-specific metadata.
    fn list_prompts(&self, _request: &Value, _context: &RequestContext) -> Result<Value> {
        Ok(json!({"prompts": self.prompts()}))
    }
    fn get_prompt(&self, name: &str, _args: Value) -> Result<Vec<PromptMessage>> {
        Err(McpError::ToolNotFound(name.into()))
    }
    /// Complete prompts/get payload, including metadata or application-level errors.
    fn get_prompt_payload(&self, request: &Value, _context: &RequestContext) -> Result<Value> {
        let name = request
            .pointer("/params/name")
            .and_then(Value::as_str)
            .unwrap_or("");
        let args = request
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or(Value::Null);
        self.get_prompt(name, args)
            .map(|m| json!({"messages": m.iter().map(PromptMessage::to_value).collect::<Vec<_>>()}))
    }
}

/// Optional asynchronous raw request handler. `None` delegates to legacy dispatch.
pub trait AsyncRequestHandler: Send + Sync {
    fn handle<'a>(
        &'a self,
        request: &'a Value,
        context: &'a RequestContext,
    ) -> Pin<Box<dyn Future<Output = Option<Result<Value>>> + Send + 'a>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerTransport {
    Stdio,
    Http { peer: SocketAddr },
}

/// Host-supplied context for raw dispatch & request-aware handlers.
/// Transport serving fills transport/session; metadata defaults to null.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestContext {
    pub transport: ServerTransport,
    pub session: Option<String>,
    pub metadata: Value,
}

impl Default for RequestContext {
    fn default() -> Self {
        Self {
            transport: ServerTransport::Stdio,
            session: None,
            metadata: Value::Null,
        }
    }
}

type Notify = Arc<dyn Fn(Value) -> bool + Send + Sync>;

/// Request-scoped handle passed to a tool handler (`ToolProvider::call_tool_with_handle` /
/// `call_with_handle`). It carries the request's `_meta.progressToken`, a cancellation signal
/// that fires on `notifications/cancelled` for this request, and a way to emit server
/// notifications such as `notifications/progress`.
///
/// Notifications and cancellation need a transport that can write outside a reply and read
/// while a call runs: [`ServerHost::serve_stdio_concurrent`]. Elsewhere (HTTP, `dispatch*`) the
/// handle is inert: never cancelled, and notifying returns `false`.
#[derive(Clone, Default)]
pub struct CallHandle {
    shared: Option<Arc<CallShared>>,
}

struct CallShared {
    token: CancellationToken,
    progress_token: Option<Value>,
    notify: Notify,
}

impl std::fmt::Debug for CallHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallHandle")
            .field("live", &self.shared.is_some())
            .field("progress_token", &self.progress_token())
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl CallHandle {
    /// The request's `params._meta.progressToken`, if the client sent one.
    pub fn progress_token(&self) -> Option<&Value> {
        self.shared.as_ref().and_then(|s| s.progress_token.as_ref())
    }

    /// True once the client sent `notifications/cancelled` for this request.
    pub fn is_cancelled(&self) -> bool {
        self.shared.as_ref().is_some_and(|s| s.token.is_cancelled())
    }

    /// A clone of this call's cancellation token (never cancelled when the handle is inert).
    pub fn cancellation(&self) -> CancellationToken {
        self.shared
            .as_ref()
            .map(|s| s.token.clone())
            .unwrap_or_default()
    }

    /// Emits `notifications/progress` with this request's token. Returns `false` (nothing sent)
    /// when the client sent no progress token or the transport cannot carry notifications.
    /// Progress must increase between calls; that is the handler's responsibility.
    pub fn notify_progress(
        &self,
        progress: f64,
        total: Option<f64>,
        message: Option<&str>,
    ) -> bool {
        let Some(shared) = &self.shared else {
            return false;
        };
        let Some(token) = &shared.progress_token else {
            return false;
        };
        let mut params = json!({"progressToken": token, "progress": progress});
        if let Some(total) = total {
            params["total"] = json!(total);
        }
        if let Some(message) = message {
            params["message"] = json!(message);
        }
        (shared.notify)(json!({"jsonrpc":"2.0","method":"notifications/progress","params":params}))
    }

    /// Emits any server-to-client notification. Returns `false` when it could not be sent.
    pub fn notify(&self, method: &str, params: Value) -> bool {
        match &self.shared {
            Some(shared) => {
                (shared.notify)(json!({"jsonrpc":"2.0","method":method,"params":params}))
            }
            None => false,
        }
    }
}

/// Whether a request that was cancelled via `notifications/cancelled` still gets a reply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CancelledResponse {
    /// MCP spec: do not reply to a cancelled request.
    #[default]
    Suppress,
    /// Reply with whatever the handler returned.
    Send,
}

/// Overrides for the generated `initialize` result. Ignored when
/// `ServerOptions::initialize_result` supplies a verbatim result. Protocol negotiation is kept.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InitializeOverrides {
    /// Sets `serverInfo.title`.
    pub server_title: Option<String>,
    /// Object merged into `serverInfo` (after `name`/`version`; before `server_title`).
    pub server_info: Option<Value>,
    /// Replaces the instructions text (also set by `with_instructions`).
    pub instructions: Option<String>,
    /// Object deep-merged into the generated `capabilities`; its values win.
    pub capabilities: Option<Value>,
    /// Version answered when the client's request is not negotiable (default `PROTOCOL_VERSION`).
    pub default_protocol_version: Option<String>,
}

/// One outgoing response, as seen by the hook set with [`ServerHost::with_response_hook`].
#[derive(Debug, Clone, Copy)]
pub struct ResponseInfo<'a> {
    /// Method of the request answered; `None` for parse errors and oversize frames.
    pub method: Option<&'a str>,
    /// The complete JSON-RPC response frame, read only.
    pub response: &'a Value,
    pub transport: ServerTransport,
}

type ResponseHook = Arc<dyn Fn(&ResponseInfo<'_>) + Send + Sync>;

/// Context for the transform set with [`ServerHost::with_response_transform`].
#[derive(Debug, Clone, Copy)]
pub struct TransformInfo<'a> {
    /// Method of the request answered (`None` if the request carried no method).
    pub method: Option<&'a str>,
    pub transport: ServerTransport,
}

type ResponseTransform = Arc<dyn Fn(&TransformInfo<'_>, &mut Value) + Send + Sync>;

fn merge_json(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(existing) => merge_json(existing, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, o) => *b = o.clone(),
    }
}

#[derive(Debug)]
pub struct ReadContext<'a> {
    pub request: &'a Value,
    pub context: &'a RequestContext,
    /// String entries from params.accessGrants, in order; nonstrings are ignored.
    pub access_grants: Vec<&'a str>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IdPolicy {
    /// Missing/null ids are notifications & never invoke handlers.
    #[default]
    Notifications,
    /// Missing/null ids invoke handlers & receive a response with id:null (Membrane).
    NullResponse,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InitializedPolicy {
    /// Treat notifications/initialized like any unknown method.
    #[default]
    Dispatch,
    /// Always ignore notifications/initialized, even with an id (Membrane).
    Ignore,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BatchPolicy {
    #[default]
    Process,
    /// One -32600 Invalid Request response with id:null, without invoking handlers.
    Reject,
    /// No response & no handler calls (Membrane).
    Ignore,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StdioInputMode {
    #[default]
    ParseErrorReply,
    /// Malformed JSON ends serving with io::ErrorKind::InvalidData (Membrane).
    FailFast,
}

/// Default cap on one stdio input frame (one line), matching the HTTP body cap.
pub const DEFAULT_MAX_STDIO_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerOptions {
    pub id_policy: IdPolicy,
    pub initialized_policy: InitializedPolicy,
    pub batch_policy: BatchPolicy,
    /// None preserves "method not found: {method}"; Membrane uses "Method not found".
    pub method_not_found_message: Option<String>,
    /// Verbatim initialize result; None preserves negotiation & generated capabilities.
    pub initialize_result: Option<Value>,
    pub stdio_input_mode: StdioInputMode,
}

pub struct AdmissionRequest<'a> {
    pub tool: &'a str,
    pub args: &'a Value,
    pub descriptor: Option<&'a ToolDescriptor>,
    pub transport: ServerTransport,
    pub session: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    Allow,
    Deny(String),
}

/// Runs before `ToolProvider::call`. A `Deny` means the provider is never invoked.
pub trait AdmissionHook: Send + Sync {
    fn admit(&self, req: &AdmissionRequest) -> Admission;
    fn expose(&self, _tool: &ToolDescriptor) -> bool {
        true
    }
}

/// Blocks listed tool names and (unless `allow_mutate`) every tool not annotated read-only.
pub struct ReadOnlyPolicy {
    pub allow_mutate: bool,
    pub blocked: Vec<String>,
}

impl AdmissionHook for ReadOnlyPolicy {
    fn admit(&self, req: &AdmissionRequest) -> Admission {
        if self.blocked.iter().any(|b| b == req.tool) {
            return Admission::Deny(format!(
                "tool '{}' is not available through this server",
                req.tool
            ));
        }
        let read_only = req.descriptor.and_then(|d| d.annotations.read_only) == Some(true);
        if !read_only && !self.allow_mutate {
            return Admission::Deny(format!(
                "tool '{}' is mutating and mutation is not enabled",
                req.tool
            ));
        }
        Admission::Allow
    }
    fn expose(&self, tool: &ToolDescriptor) -> bool {
        !self.blocked.contains(&tool.name)
    }
}

pub struct HttpServeOptions {
    pub port: u16,
    /// `None` => a random 192-bit token is generated.
    pub token: Option<String>,
}

struct Inner {
    name: String,
    version: String,
    instructions: Option<String>,
    provider: Arc<dyn ToolProvider>,
    admission: Option<Arc<dyn AdmissionHook>>,
    events: Option<EventSink>,
    options: ServerOptions,
    async_handler: Option<Arc<dyn AsyncRequestHandler>>,
    protocol_versions: Option<Vec<String>>,
    response_limits: Option<crate::ResponseLimits>,
    max_stdio_frame_bytes: usize,
    initialize_overrides: Option<InitializeOverrides>,
    response_hook: Option<ResponseHook>,
    response_transform: Option<ResponseTransform>,
    cancelled_response: CancelledResponse,
}

#[derive(Clone)]
pub struct ServerHost {
    inner: Arc<Inner>,
}

#[derive(Clone)]
struct Ctx {
    request: RequestContext,
    /// Local id of this serving run (`mcps-N`), used for lifecycle events.
    run: String,
    /// Present only under concurrent stdio serving.
    live: Option<Arc<Live>>,
    call: CallHandle,
}

impl Ctx {
    fn new(request: RequestContext, run: String) -> Self {
        Ctx {
            request,
            run,
            live: None,
            call: CallHandle::default(),
        }
    }
}

/// Shared state of one concurrent stdio run.
struct Live {
    notify: Notify,
    /// In-flight request id (JSON text) -> its cancellation token.
    cancels: Mutex<HashMap<String, CancellationToken>>,
}

struct CancelGuard {
    live: Arc<Live>,
    key: String,
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.live
            .cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

enum Incoming {
    Msg(Value),
    Fail(std::io::Error),
}

/// Bytes in the generated loopback bearer token (192 bits, 48 hex characters).
const LOOPBACK_TOKEN_BYTES: usize = 24;
/// Bytes in a generated HTTP session id (128 bits, 32 hex characters).
const SESSION_ID_BYTES: usize = 16;

/// Fill `n` random bytes via `fill` and hex-encode them. Fails closed: a fill error or an
/// all-zero buffer is an error, never a usable secret.
fn random_hex_with<F>(n: usize, fill: F) -> Result<String>
where
    F: FnOnce(&mut [u8]) -> std::result::Result<(), getrandom::Error>,
{
    let mut b = vec![0u8; n];
    fill(&mut b).map_err(|e| McpError::RandomUnavailable(format!("{e:?}")))?;
    if b.iter().all(|&x| x == 0) {
        return Err(McpError::RandomUnavailable(
            "generator returned all-zero bytes".into(),
        ));
    }
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// MCP "Resource not found": code -32002 with the URI in `data`.
fn resource_not_found(id: &Value, uri: &str) -> Value {
    let mut reply = rpc_error(id, -32002, "Resource not found");
    reply["error"]["data"] = json!({"uri": uri});
    reply
}

fn rpc_error(id: &Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

enum LineRead {
    Eof,
    Line,
    /// The line exceeded the limit; its remaining bytes were consumed and discarded.
    Oversize,
}

/// Reads one `\n`-terminated line into `buf` (terminator and a trailing `\r` removed) without
/// ever holding more than `max` bytes. An oversize line is drained to its terminator so the
/// stream stays aligned on the next frame.
fn read_bounded_line(
    reader: &mut impl BufRead,
    buf: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<LineRead> {
    buf.clear();
    let mut oversize = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if buf.is_empty() && !oversize {
                return Ok(LineRead::Eof);
            }
            break;
        }
        let (done, used) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (true, i + 1),
            None => (false, available.len()),
        };
        if !oversize {
            let take = if done { used - 1 } else { used };
            // One extra byte of slack admits a CR that the terminator-strip below removes.
            if buf.len() + take > max.saturating_add(1) {
                oversize = true;
                buf.clear();
            } else {
                buf.extend_from_slice(&available[..take]);
            }
        }
        reader.consume(used);
        if done {
            break;
        }
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    if oversize || buf.len() > max {
        buf.clear();
        return Ok(LineRead::Oversize);
    }
    Ok(LineRead::Line)
}

async fn join_options<'a, I>(futures: I) -> Vec<Value>
where
    I: IntoIterator<Item = Pin<Box<dyn Future<Output = Option<Value>> + Send + 'a>>>,
{
    let mut futures: Vec<_> = futures.into_iter().map(Some).collect();
    let mut output = Vec::new();
    poll_fn(|cx| {
        let mut pending = false;
        for slot in futures.iter_mut() {
            let Some(future) = slot.as_mut() else {
                continue;
            };
            match future.as_mut().poll(cx) {
                std::task::Poll::Ready(value) => {
                    *slot = None;
                    if let Some(value) = value {
                        output.push(value);
                    }
                }
                std::task::Poll::Pending => pending = true,
            }
        }
        if pending {
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(std::mem::take(&mut output))
        }
    })
    .await
}

impl ServerHost {
    pub fn new(name: &str, version: &str, provider: Arc<dyn ToolProvider>) -> Self {
        ServerHost {
            inner: Arc::new(Inner {
                name: name.into(),
                version: version.into(),
                instructions: None,
                provider,
                admission: None,
                events: None,
                options: ServerOptions::default(),
                async_handler: None,
                protocol_versions: None,
                response_limits: None,
                max_stdio_frame_bytes: DEFAULT_MAX_STDIO_FRAME_BYTES,
                initialize_overrides: None,
                response_hook: None,
                response_transform: None,
                cancelled_response: CancelledResponse::default(),
            }),
        }
    }

    fn rebuild(self, f: impl FnOnce(&mut Inner)) -> Self {
        let i = &*self.inner;
        let mut n = Inner {
            name: i.name.clone(),
            version: i.version.clone(),
            instructions: i.instructions.clone(),
            provider: i.provider.clone(),
            admission: i.admission.clone(),
            events: i.events.clone(),
            options: i.options.clone(),
            async_handler: i.async_handler.clone(),
            protocol_versions: i.protocol_versions.clone(),
            response_limits: i.response_limits,
            max_stdio_frame_bytes: i.max_stdio_frame_bytes,
            initialize_overrides: i.initialize_overrides.clone(),
            response_hook: i.response_hook.clone(),
            response_transform: i.response_transform.clone(),
            cancelled_response: i.cancelled_response,
        };
        f(&mut n);
        ServerHost { inner: Arc::new(n) }
    }

    pub fn with_admission(self, hook: Arc<dyn AdmissionHook>) -> Self {
        self.rebuild(|i| i.admission = Some(hook))
    }

    pub fn with_options(self, options: ServerOptions) -> Self {
        self.rebuild(|i| i.options = options)
    }

    pub fn with_async_handler(self, handler: Arc<dyn AsyncRequestHandler>) -> Self {
        self.rebuild(|i| i.async_handler = Some(handler))
    }

    pub fn with_protocol_versions(self, versions: Vec<String>) -> Self {
        self.rebuild(|i| i.protocol_versions = Some(versions))
    }

    pub fn with_response_limits(self, limits: crate::ResponseLimits) -> Self {
        self.rebuild(|i| i.response_limits = Some(limits))
    }

    /// Maximum bytes in one stdio input line, excluding its terminator (default
    /// [`DEFAULT_MAX_STDIO_FRAME_BYTES`]). A longer line is drained, answered with a JSON-RPC
    /// Invalid Request error (id null), and serving continues.
    pub fn with_max_stdio_frame_bytes(self, max: usize) -> Self {
        self.rebuild(|i| i.max_stdio_frame_bytes = max)
    }

    /// Overrides parts of the generated `initialize` result (title, server info, instructions,
    /// capabilities, default protocol version) while keeping protocol negotiation.
    pub fn with_initialize_overrides(self, overrides: InitializeOverrides) -> Self {
        self.rebuild(|i| i.initialize_overrides = Some(overrides))
    }

    /// Observes every outgoing response (result or error, including parse errors) once, after it
    /// is built and before it is written. The hook gets read-only access; a panic in it is
    /// caught and ignored. Responses suppressed for a cancelled request are not observed.
    pub fn with_response_hook(
        self,
        hook: impl Fn(&ResponseInfo<'_>) + Send + Sync + 'static,
    ) -> Self {
        self.rebuild(|i| i.response_hook = Some(Arc::new(hook)))
    }

    /// Rewrites the `result` of every successful response before it is observed and written, for
    /// example to add fields to a tool or resource result. The closure receives only the result
    /// value (a working copy), never the frame: `id` and `jsonrpc` cannot be touched. Error
    /// responses, parse errors and notifications skip it. It runs before the hook set with
    /// [`ServerHost::with_response_hook`]. If it panics, the untransformed result is sent.
    /// Opt-in: without it replies are unchanged.
    pub fn with_response_transform(
        self,
        transform: impl Fn(&TransformInfo<'_>, &mut Value) + Send + Sync + 'static,
    ) -> Self {
        self.rebuild(|i| i.response_transform = Some(Arc::new(transform)))
    }

    fn transform(&self, method: Option<&str>, reply: &mut Value, transport: ServerTransport) {
        let Some(transform) = &self.inner.response_transform else {
            return;
        };
        let Some(original) = reply.get("result") else {
            return;
        };
        let mut working = original.clone();
        let info = TransformInfo { method, transport };
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            transform(&info, &mut working)
        }))
        .is_ok();
        if ok {
            reply["result"] = working;
        }
    }

    /// Chooses whether a request cancelled by the client still gets a reply (default: no, per
    /// the MCP spec). Only concurrent stdio serving delivers cancellation.
    pub fn with_cancelled_response(self, policy: CancelledResponse) -> Self {
        self.rebuild(|i| i.cancelled_response = policy)
    }

    fn observe(&self, method: Option<&str>, response: &Value, transport: ServerTransport) {
        if let Some(hook) = &self.inner.response_hook {
            let info = ResponseInfo {
                method,
                response,
                transport,
            };
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook(&info)));
        }
    }

    fn observed_error(
        &self,
        transport: ServerTransport,
        code: i64,
        message: impl Into<String>,
    ) -> Value {
        let reply = rpc_error(&Value::Null, code, message);
        self.observe(None, &reply, transport);
        reply
    }

    /// Dispatch a parsed frame without transport I/O or HTTP session issuance.
    pub fn dispatch(&self, request: &Value) -> Option<Value> {
        self.dispatch_with_context(request, &RequestContext::default())
    }

    pub fn dispatch_with_context(
        &self,
        request: &Value,
        context: &RequestContext,
    ) -> Option<Value> {
        self.dispatch_frame(request, &Ctx::new(context.clone(), next_id("mcps")))
    }

    pub fn dispatch_checked(
        &self,
        request: &Value,
        context: &RequestContext,
    ) -> Result<Option<Value>> {
        self.check_response(self.dispatch_with_context(request, context))
    }

    pub async fn dispatch_async(&self, request: &Value) -> Option<Value> {
        self.dispatch_async_with_context(request, &RequestContext::default())
            .await
    }

    pub async fn dispatch_async_with_context(
        &self,
        request: &Value,
        context: &RequestContext,
    ) -> Option<Value> {
        let ctx = Ctx::new(context.clone(), next_id("mcps"));
        if let Value::Array(items) = request {
            return match self.inner.options.batch_policy {
                BatchPolicy::Process => {
                    let futures = items.iter().map(|item| {
                        Box::pin(self.dispatch_async_one(item, &ctx))
                            as Pin<Box<dyn Future<Output = Option<Value>> + Send + '_>>
                    });
                    let replies = join_options(futures).await;
                    if replies.is_empty() {
                        None
                    } else {
                        Some(Value::Array(replies))
                    }
                }
                BatchPolicy::Reject => {
                    Some(self.observed_error(ctx.request.transport, -32600, "Invalid Request"))
                }
                BatchPolicy::Ignore => None,
            };
        }
        self.dispatch_async_one(request, &ctx).await
    }

    pub async fn dispatch_checked_async(
        &self,
        request: &Value,
        context: &RequestContext,
    ) -> Result<Option<Value>> {
        self.check_response(self.dispatch_async_with_context(request, context).await)
    }

    fn check_response(&self, response: Option<Value>) -> Result<Option<Value>> {
        let Some(response) = response else {
            return Ok(None);
        };
        if let Some(limits) = &self.inner.response_limits {
            let bytes = crate::raw::to_wire_string(&response).len();
            let limit = limits.max_response_bytes.min(limits.max_frame_bytes);
            if bytes > limit {
                return Err(McpError::ResponseTooLarge { limit });
            }
        }
        Ok(Some(response))
    }

    async fn dispatch_async_one(&self, msg: &Value, ctx: &Ctx) -> Option<Value> {
        let mut reply = self.dispatch_async_one_inner(msg, ctx).await;
        if let Some(r) = &mut reply {
            self.transform(
                msg.get("method").and_then(Value::as_str),
                r,
                ctx.request.transport,
            );
        }
        if let Some(r) = &reply {
            self.observe(
                msg.get("method").and_then(Value::as_str),
                r,
                ctx.request.transport,
            );
        }
        reply
    }

    async fn dispatch_async_one_inner(&self, msg: &Value, ctx: &Ctx) -> Option<Value> {
        let method = msg.get("method").and_then(Value::as_str)?;
        if method == "notifications/initialized"
            && self.inner.options.initialized_policy == InitializedPolicy::Ignore
        {
            return None;
        }
        let id = match msg.get("id") {
            Some(id) if !id.is_null() => id.clone(),
            _ if self.inner.options.id_policy == IdPolicy::NullResponse => Value::Null,
            _ => return None,
        };
        if let Some(handler) = &self.inner.async_handler {
            if method == "tools/call" {
                if let Some(denied) = self.admission_only(msg, ctx) {
                    return Some(self.wrap_result(&id, denied));
                }
            }
            if let Some(result) = handler.handle(msg, &ctx.request).await {
                let result = if method == "tools/list" {
                    result.map(|payload| self.filter_tools(payload))
                } else {
                    result
                };
                return Some(self.wrap_result(&id, result));
            }
            if method == "tools/call" {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                return Some(self.wrap_result(&id, self.tools_call_inner(msg, &params, ctx, true)));
            }
        }
        self.handle_one_inner(msg, ctx)
    }

    fn wrap_result(&self, id: &Value, out: Result<Value>) -> Value {
        match out {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(McpError::ToolNotFound(n)) => rpc_error(id, -32602, format!("unknown: {n}")),
            Err(McpError::ResourceNotFound(uri)) => resource_not_found(id, &uri),
            Err(McpError::Rpc { code, message }) => rpc_error(id, code, message),
            Err(McpError::RpcWithData {
                code,
                message,
                data,
            }) => {
                let mut r = rpc_error(id, code, message);
                r["error"]["data"] = data;
                r
            }
            Err(e) => rpc_error(id, -32603, e.to_string()),
        }
    }

    fn admission_only(&self, request: &Value, ctx: &Ctx) -> Option<Result<Value>> {
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let hook = self.inner.admission.as_ref()?;
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return Some(Err(McpError::Rpc {
                code: -32602,
                message: "missing tool name".into(),
            }));
        };
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let descriptor = self
            .inner
            .provider
            .list()
            .into_iter()
            .find(|t| t.name == name);
        let verdict = if descriptor.as_ref().is_some_and(|d| !hook.expose(d)) {
            Admission::Deny(format!("tool '{name}' is not exposed"))
        } else {
            hook.admit(&AdmissionRequest {
                tool: name,
                args: &args,
                descriptor: descriptor.as_ref(),
                transport: ctx.request.transport,
                session: ctx.request.session.as_deref(),
            })
        };
        match verdict {
            Admission::Allow => None,
            Admission::Deny(reason) => {
                let transport = match ctx.request.transport {
                    ServerTransport::Stdio => "stdio",
                    ServerTransport::Http { .. } => "http",
                };
                self.emit(
                    LifecycleKind::Denied,
                    &ctx.run,
                    transport,
                    reason.clone(),
                    Some(name),
                );
                let mut result = ToolResult::error(format!("denied: {reason}"));
                result.structured = Some(json!({"denied":true,"reason":reason}));
                Some(Ok(result.to_value()))
            }
        }
    }

    /// Receives start/stop/crash (serving runs) and denied (admission, HTTP gates) events.
    pub fn with_events(self, sink: EventSink) -> Self {
        self.rebuild(|i| i.events = Some(sink))
    }

    pub fn on_event(self, f: impl Fn(&McpEvent) + Send + Sync + 'static) -> Self {
        self.with_events(Arc::new(f))
    }

    fn emit(
        &self,
        kind: LifecycleKind,
        run: &str,
        transport: &'static str,
        reason: impl Into<String>,
        tool: Option<&str>,
    ) {
        emit(&self.inner.events, || {
            let mut e = McpEvent::new(kind, run, reason);
            e.server = Some(self.inner.name.clone());
            e.transport = Some(transport);
            e.tool = tool.map(str::to_string);
            e
        });
    }

    pub fn with_instructions(self, text: &str) -> Self {
        self.rebuild(|i| i.instructions = Some(text.into()))
    }

    /// Blocking loop on process stdin/stdout; returns at EOF.
    pub fn serve_stdio(&self) -> Result<()> {
        let stdin = std::io::stdin();
        self.serve_stdio_on(stdin.lock(), std::io::stdout())
    }

    pub fn serve_stdio_on(&self, reader: impl BufRead, writer: impl Write) -> Result<()> {
        self.serve_stdio_on_io(reader, writer).map_err(|e| {
            if self.inner.options.stdio_input_mode == StdioInputMode::FailFast {
                McpError::IoError(Arc::new(e))
            } else {
                e.into()
            }
        })
    }

    /// Like serve_stdio, returning original I/O errors without string conversion.
    pub fn serve_stdio_io(&self) -> std::io::Result<()> {
        let stdin = std::io::stdin();
        self.serve_stdio_on_io(stdin.lock(), std::io::stdout())
    }

    /// Honors stdio_input_mode; read/write/flush errors are returned unchanged.
    pub fn serve_stdio_on_io(
        &self,
        reader: impl BufRead,
        writer: impl Write,
    ) -> std::io::Result<()> {
        let ctx = Ctx::new(RequestContext::default(), next_id("mcps"));
        self.emit(
            LifecycleKind::Start,
            &ctx.run,
            "stdio",
            "serving stdio",
            None,
        );
        match self.stdio_loop(reader, writer, &ctx) {
            Ok(()) => {
                self.emit(LifecycleKind::Stop, &ctx.run, "stdio", "input closed", None);
                Ok(())
            }
            Err(e) => {
                self.emit(
                    LifecycleKind::Crash,
                    &ctx.run,
                    "stdio",
                    format!("serving failed: io error: {e}"),
                    None,
                );
                Err(e)
            }
        }
    }

    fn stdio_loop(
        &self,
        mut reader: impl BufRead,
        mut writer: impl Write,
        ctx: &Ctx,
    ) -> std::io::Result<()> {
        let max = self.inner.max_stdio_frame_bytes;
        let mut buf = Vec::new();
        loop {
            match read_bounded_line(&mut reader, &mut buf, max)? {
                LineRead::Eof => break,
                // JSON-lines framing also lives in `rightkit-sidecar` (owner); see client.rs.
                LineRead::Oversize => {
                    let reply = self.observed_error(
                        ServerTransport::Stdio,
                        -32600,
                        format!("request frame exceeds {max} byte limit"),
                    );
                    writeln!(writer, "{reply}")?;
                    writer.flush()?;
                    continue;
                }
                LineRead::Line => {}
            }
            let line = std::str::from_utf8(&buf)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            if line.trim().is_empty() {
                continue;
            }
            let reply = match serde_json::from_str::<Value>(line) {
                Ok(msg) => self.dispatch_frame(&msg, ctx),
                Err(e) if self.inner.options.stdio_input_mode == StdioInputMode::FailFast => {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e));
                }
                Err(e) => Some(self.observed_error(
                    ServerTransport::Stdio,
                    -32700,
                    format!("parse error: {e}"),
                )),
            };
            if let Some(r) = reply {
                writeln!(writer, "{}", crate::raw::to_wire_string(&r))?;
                writer.flush()?;
            }
        }
        Ok(())
    }

    /// Like [`serve_stdio`](Self::serve_stdio) on process stdin/stdout, but reading continues
    /// while a request runs; see [`serve_stdio_concurrent_on`](Self::serve_stdio_concurrent_on).
    pub fn serve_stdio_concurrent(&self) -> std::io::Result<()> {
        self.serve_stdio_concurrent_on(std::io::BufReader::new(std::io::stdin()), std::io::stdout())
    }

    /// Stdio serving with concurrent input. A reader thread keeps reading while a request runs:
    /// it answers `ping` at once, turns `notifications/cancelled` into a signal on the matching
    /// in-flight request's [`CallHandle`], and queues everything else for in-order handling on
    /// the calling thread. Handlers can emit `notifications/progress` through their
    /// [`CallHandle`]; notifications and replies are written whole, one line each.
    ///
    /// Returns when input closes (after queued requests are answered) or on an I/O error. The
    /// reader thread is detached, so a failed write returns without waiting for more input.
    pub fn serve_stdio_concurrent_on<R, W>(&self, reader: R, writer: W) -> std::io::Result<()>
    where
        R: BufRead + Send + 'static,
        W: Write + Send + 'static,
    {
        let run = next_id("mcps");
        self.emit(LifecycleKind::Start, &run, "stdio", "serving stdio", None);
        let writer = Arc::new(Mutex::new(writer));
        let write_line = move |value: &Value| -> std::io::Result<()> {
            let mut w = writer.lock().unwrap_or_else(|e| e.into_inner());
            writeln!(w, "{}", crate::raw::to_wire_string(value))?;
            w.flush()
        };
        let write_line = Arc::new(write_line);
        let notify: Notify = {
            let write_line = write_line.clone();
            Arc::new(move |value: Value| (*write_line)(&value).is_ok())
        };
        let live = Arc::new(Live {
            notify,
            cancels: Mutex::new(HashMap::new()),
        });
        let ctx = Ctx {
            live: Some(live),
            ..Ctx::new(RequestContext::default(), run.clone())
        };
        let (tx, rx) = mpsc::channel::<Incoming>();
        {
            let host = self.clone();
            let ctx = ctx.clone();
            let write_line = write_line.clone();
            std::thread::spawn(move || {
                host.concurrent_reader(reader, &tx, &ctx, &*write_line);
            });
        }
        let result = (|| {
            for incoming in rx {
                let msg = match incoming {
                    Incoming::Msg(msg) => msg,
                    Incoming::Fail(e) => return Err(e),
                };
                if let Some(reply) = self.dispatch_frame(&msg, &ctx) {
                    (*write_line)(&reply)?;
                }
            }
            Ok(())
        })();
        match &result {
            Ok(()) => self.emit(LifecycleKind::Stop, &run, "stdio", "input closed", None),
            Err(e) => self.emit(
                LifecycleKind::Crash,
                &run,
                "stdio",
                format!("serving failed: io error: {e}"),
                None,
            ),
        }
        result
    }

    fn concurrent_reader(
        &self,
        mut reader: impl BufRead,
        tx: &mpsc::Sender<Incoming>,
        ctx: &Ctx,
        write_line: &dyn Fn(&Value) -> std::io::Result<()>,
    ) {
        let fail = |e: std::io::Error| {
            let _ = tx.send(Incoming::Fail(e));
        };
        let max = self.inner.max_stdio_frame_bytes;
        let mut buf = Vec::new();
        loop {
            match read_bounded_line(&mut reader, &mut buf, max) {
                Err(e) => return fail(e),
                Ok(LineRead::Eof) => return,
                Ok(LineRead::Oversize) => {
                    let reply = self.observed_error(
                        ServerTransport::Stdio,
                        -32600,
                        format!("request frame exceeds {max} byte limit"),
                    );
                    if let Err(e) = write_line(&reply) {
                        return fail(e);
                    }
                    continue;
                }
                Ok(LineRead::Line) => {}
            }
            let line = match std::str::from_utf8(&buf) {
                Ok(l) => l,
                Err(e) => return fail(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            };
            if line.trim().is_empty() {
                continue;
            }
            let msg = match serde_json::from_str::<Value>(line) {
                Ok(msg) => msg,
                Err(e) if self.inner.options.stdio_input_mode == StdioInputMode::FailFast => {
                    return fail(std::io::Error::new(std::io::ErrorKind::InvalidData, e));
                }
                Err(e) => {
                    let reply = self.observed_error(
                        ServerTransport::Stdio,
                        -32700,
                        format!("parse error: {e}"),
                    );
                    if let Err(e) = write_line(&reply) {
                        return fail(e);
                    }
                    continue;
                }
            };
            let method = msg.get("method").and_then(Value::as_str);
            let has_id = msg.get("id").is_some_and(|id| !id.is_null());
            if method == Some("notifications/cancelled") && !has_id {
                if let (Some(live), Some(request_id)) =
                    (&ctx.live, msg.pointer("/params/requestId"))
                {
                    let token = live
                        .cancels
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&request_id.to_string())
                        .cloned();
                    if let Some(token) = token {
                        token.cancel();
                    }
                }
                continue;
            }
            if method == Some("ping") && has_id {
                if let Some(reply) = self.handle_one(&msg, ctx) {
                    if let Err(e) = write_line(&reply) {
                        return fail(e);
                    }
                }
                continue;
            }
            if tx.send(Incoming::Msg(msg)).is_err() {
                return;
            }
        }
    }

    /// Returns (reply, issued-session-id-if-initialize). Fails closed when no session id can be
    /// generated, so a successful initialize never returns an unusable or guessable session.
    fn handle_any(&self, msg: Value, ctx: &Ctx) -> Result<(Option<Value>, Option<String>)> {
        let is_init = msg.get("method").and_then(Value::as_str) == Some("initialize");
        let reply = self.dispatch_frame(&msg, ctx);
        let sid = if is_init && reply.as_ref().is_some_and(|r| r.get("result").is_some()) {
            Some(random_hex_with(SESSION_ID_BYTES, getrandom::fill)?)
        } else {
            None
        };
        Ok((reply, sid))
    }

    fn dispatch_frame(&self, msg: &Value, ctx: &Ctx) -> Option<Value> {
        if let Value::Array(items) = msg {
            return match self.inner.options.batch_policy {
                BatchPolicy::Process => {
                    let replies: Vec<Value> = items
                        .iter()
                        .filter_map(|m| self.handle_one(m, ctx))
                        .collect();
                    if replies.is_empty() {
                        None
                    } else {
                        Some(Value::Array(replies))
                    }
                }
                BatchPolicy::Reject => {
                    Some(self.observed_error(ctx.request.transport, -32600, "Invalid Request"))
                }
                BatchPolicy::Ignore => None,
            };
        }
        self.handle_one(msg, ctx)
    }

    fn method_not_found(&self, id: &Value, method: &str) -> Value {
        rpc_error(
            id,
            -32601,
            self.inner
                .options
                .method_not_found_message
                .clone()
                .unwrap_or_else(|| format!("method not found: {method}")),
        )
    }

    /// Builds the request-scoped context (cancellation, progress) for concurrent serving.
    fn scope_call(&self, msg: &Value, ctx: &Ctx) -> Option<(Ctx, CancelGuard)> {
        let live = ctx.live.as_ref()?;
        let id = msg.get("id").filter(|id| !id.is_null())?;
        msg.get("method").and_then(Value::as_str)?;
        let token = CancellationToken::new();
        let key = id.to_string();
        live.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone(), token.clone());
        let mut scoped = ctx.clone();
        scoped.call = CallHandle {
            shared: Some(Arc::new(CallShared {
                token,
                progress_token: msg
                    .pointer("/params/_meta/progressToken")
                    .filter(|t| !t.is_null())
                    .cloned(),
                notify: live.notify.clone(),
            })),
        };
        Some((
            scoped,
            CancelGuard {
                live: live.clone(),
                key,
            },
        ))
    }

    fn handle_one(&self, msg: &Value, ctx: &Ctx) -> Option<Value> {
        let scoped = self.scope_call(msg, ctx);
        let ctx = scoped.as_ref().map_or(ctx, |(c, _)| c);
        let mut reply = self.handle_one_inner(msg, ctx)?;
        if scoped.is_some()
            && ctx.call.is_cancelled()
            && self.inner.cancelled_response == CancelledResponse::Suppress
        {
            return None;
        }
        self.transform(
            msg.get("method").and_then(Value::as_str),
            &mut reply,
            ctx.request.transport,
        );
        self.observe(
            msg.get("method").and_then(Value::as_str),
            &reply,
            ctx.request.transport,
        );
        Some(reply)
    }

    fn handle_one_inner(&self, msg: &Value, ctx: &Ctx) -> Option<Value> {
        let method = msg.get("method").and_then(Value::as_str)?;
        if method == "notifications/initialized"
            && self.inner.options.initialized_policy == InitializedPolicy::Ignore
        {
            return None;
        }
        let id = match msg.get("id") {
            Some(id) if !id.is_null() => id.clone(),
            _ if self.inner.options.id_policy == IdPolicy::NullResponse => Value::Null,
            _ => return None, // notification
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let out = match method {
            "initialize" => self.initialize(&params),
            "ping" => Ok(json!({})),
            "tools/list" => self.tools_list(msg, ctx),
            "tools/call" => self.tools_call(msg, &params, ctx),
            "resources/list" => self.inner.provider.list_resources(msg, &ctx.request),
            "resources/templates/list" => {
                match self
                    .inner
                    .provider
                    .list_resource_templates(msg, &ctx.request)
                {
                    Some(result) => result,
                    None => return Some(self.method_not_found(&id, method)),
                }
            }
            "resources/read" => {
                let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
                let access_grants = params
                    .get("accessGrants")
                    .and_then(Value::as_array)
                    .map(|entries| entries.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                self.inner.provider.read_resource_with_context(
                    uri,
                    &ReadContext {
                        request: msg,
                        context: &ctx.request,
                        access_grants,
                    },
                )
            }
            "prompts/list" => self.inner.provider.list_prompts(msg, &ctx.request),
            "prompts/get" => self.inner.provider.get_prompt_payload(msg, &ctx.request),
            _ => return Some(self.method_not_found(&id, method)),
        };
        Some(match out {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(McpError::ResourceNotFound(uri)) => resource_not_found(&id, &uri),
            Err(McpError::ToolNotFound(n)) if method == "resources/read" => {
                resource_not_found(&id, &n)
            }
            Err(McpError::ToolNotFound(n)) => rpc_error(&id, -32602, format!("unknown: {n}")),
            Err(McpError::Rpc { code, message }) => rpc_error(&id, code, message),
            Err(McpError::RpcWithData {
                code,
                message,
                data,
            }) => {
                let mut reply = rpc_error(&id, code, message);
                reply["error"]["data"] = data;
                reply
            }
            Err(e) => rpc_error(&id, -32603, e.to_string()),
        })
    }

    fn initialize(&self, params: &Value) -> Result<Value> {
        if let Some(result) = &self.inner.options.initialize_result {
            return Ok(result.clone());
        }
        let requested = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("");
        let fallback = self
            .inner
            .initialize_overrides
            .as_ref()
            .and_then(|o| o.default_protocol_version.as_deref())
            .unwrap_or(PROTOCOL_VERSION);
        let configured = self.inner.protocol_versions.as_deref().unwrap_or(&[]);
        if self.inner.protocol_versions.is_some()
            && (configured.is_empty() || configured.iter().any(String::is_empty))
        {
            return Err(McpError::Rpc {
                code: -32602,
                message: "supported protocol versions must be nonempty".into(),
            });
        }
        let version = if let Some(client_versions) = params
            .get("supportedProtocolVersions")
            .and_then(Value::as_array)
        {
            let candidates: Vec<&str> = if self.inner.protocol_versions.is_some() {
                configured.iter().map(String::as_str).collect()
            } else {
                SUPPORTED_VERSIONS.to_vec()
            };
            let best = candidates
                .into_iter()
                .filter(|v| client_versions.iter().any(|c| c.as_str() == Some(*v)))
                .max();
            if let Some(best) = best {
                best
            } else if self.inner.protocol_versions.is_some() {
                return Err(McpError::Rpc {
                    code: -32602,
                    message: "no common protocol version".into(),
                });
            } else if SUPPORTED_VERSIONS.contains(&requested) {
                requested
            } else {
                fallback
            }
        } else if self.inner.protocol_versions.is_some() {
            if configured.iter().any(|v| v == requested) {
                requested
            } else if configured.iter().any(|v| v == fallback) {
                fallback
            } else {
                configured
                    .iter()
                    .max()
                    .map(String::as_str)
                    .unwrap_or(fallback)
            }
        } else if SUPPORTED_VERSIONS.contains(&requested) {
            requested
        } else {
            fallback
        };
        let p = &self.inner.provider;
        let mut caps = json!({"tools": {}});
        if !p.resources().is_empty() {
            caps["resources"] = json!({});
        }
        if !p.prompts().is_empty() {
            caps["prompts"] = json!({});
        }
        let mut r = json!({
            "protocolVersion": version,
            "capabilities": caps,
            "serverInfo": {"name": self.inner.name, "version": self.inner.version},
        });
        if let Some(i) = &self.inner.instructions {
            r["instructions"] = json!(i);
        }
        if let Some(o) = &self.inner.initialize_overrides {
            if let Some(i) = &o.instructions {
                r["instructions"] = json!(i);
            }
            if let Some(info) = &o.server_info {
                merge_json(&mut r["serverInfo"], info);
            }
            if let Some(title) = &o.server_title {
                r["serverInfo"]["title"] = json!(title);
            }
            if let Some(caps) = &o.capabilities {
                merge_json(&mut r["capabilities"], caps);
            }
        }
        Ok(r)
    }

    fn tools_list(&self, request: &Value, ctx: &Ctx) -> Result<Value> {
        self.inner
            .provider
            .list_tools(request, &ctx.request)
            .map(|payload| self.filter_tools(payload))
    }

    fn filter_tools(&self, mut payload: Value) -> Value {
        if let Some(hook) = &self.inner.admission {
            if let Some(tools) = payload.get_mut("tools").and_then(Value::as_array_mut) {
                // Deserialize only for policy inspection; retain every raw field on output.
                tools.retain(|tool| {
                    serde_json::from_value::<ToolDescriptor>(tool.clone())
                        .is_ok_and(|descriptor| hook.expose(&descriptor))
                });
            }
        }
        payload
    }

    fn tools_call(&self, request: &Value, params: &Value, ctx: &Ctx) -> Result<Value> {
        self.tools_call_inner(request, params, ctx, false)
    }

    fn tools_call_inner(
        &self,
        request: &Value,
        params: &Value,
        ctx: &Ctx,
        admission_checked: bool,
    ) -> Result<Value> {
        if params.get("name").and_then(Value::as_str).is_none() {
            if self.inner.admission.is_none() {
                if let Some(result) =
                    self.inner
                        .provider
                        .call_tool_with_handle(request, &ctx.request, &ctx.call)
                {
                    return result;
                }
            }
            return Err(McpError::Rpc {
                code: -32602,
                message: "missing tool name".into(),
            });
        }
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let descriptor = self
            .inner
            .provider
            .list()
            .into_iter()
            .find(|t| t.name == name);
        if let Some(hook) = self.inner.admission.as_ref().filter(|_| !admission_checked) {
            let hidden = descriptor.as_ref().is_some_and(|d| !hook.expose(d));
            let verdict = if hidden {
                Admission::Deny(format!("tool '{name}' is not exposed"))
            } else {
                hook.admit(&AdmissionRequest {
                    tool: name,
                    args: &args,
                    descriptor: descriptor.as_ref(),
                    transport: ctx.request.transport,
                    session: ctx.request.session.as_deref(),
                })
            };
            if let Admission::Deny(reason) = verdict {
                let transport = match ctx.request.transport {
                    ServerTransport::Stdio => "stdio",
                    ServerTransport::Http { .. } => "http",
                };
                self.emit(
                    LifecycleKind::Denied,
                    &ctx.run,
                    transport,
                    reason.clone(),
                    Some(name),
                );
                let mut r = ToolResult::error(format!("denied: {reason}"));
                r.structured = Some(json!({"denied": true, "reason": reason}));
                return Ok(r.to_value());
            }
        }
        if let Some(result) =
            self.inner
                .provider
                .call_tool_with_handle(request, &ctx.request, &ctx.call)
        {
            return result;
        }
        if descriptor.is_none() {
            return Err(McpError::ToolNotFound(name.into()));
        }
        self.inner
            .provider
            .call_with_handle(name, args, &ctx.call)
            .map(|r| r.to_value())
    }

    /// Loopback-only HTTP server on `POST /mcp`, bearer-token and Origin gated.
    pub fn serve_http(&self, opts: HttpServeOptions) -> Result<HttpServer> {
        self.serve_http_with_fill(opts, getrandom::fill)
    }

    /// `serve_http` with an injectable RNG. The token is resolved before the socket is bound,
    /// so a failed generator refuses to serve and leaves no listener behind.
    fn serve_http_with_fill<F>(&self, opts: HttpServeOptions, fill: F) -> Result<HttpServer>
    where
        F: FnOnce(&mut [u8]) -> std::result::Result<(), getrandom::Error>,
    {
        let token = match opts.token {
            // An empty caller-supplied bearer would accept any request with an empty header.
            Some(token) if token.trim().is_empty() => {
                return Err(McpError::RandomUnavailable(
                    "caller-supplied loopback token is empty".into(),
                ))
            }
            Some(token) => token,
            None => random_hex_with(LOOPBACK_TOKEN_BYTES, fill)?,
        };
        let listener =
            TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), opts.port))?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let sessions: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let run = next_id("mcps");
        self.emit(
            LifecycleKind::Start,
            &run,
            "http",
            format!("serving http on {addr}"),
            None,
        );
        let host = self.clone();
        let (stop2, token2, run2) = (stop.clone(), token.clone(), run.clone());
        let join = std::thread::spawn(move || {
            let mut failures = 0u32;
            for conn in listener.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    return;
                }
                let stream = match conn {
                    Ok(s) => {
                        failures = 0;
                        s
                    }
                    Err(e) => {
                        failures += 1;
                        if failures > 100 {
                            host.emit(
                                LifecycleKind::Crash,
                                &run2,
                                "http",
                                format!("listener failed: {e}"),
                                None,
                            );
                            return;
                        }
                        continue;
                    }
                };
                let (h, t, s, r) = (host.clone(), token2.clone(), sessions.clone(), run2.clone());
                std::thread::spawn(move || {
                    let _ = handle_http(&h, stream, &t, &s, &r);
                });
            }
        });
        Ok(HttpServer {
            addr,
            token,
            stop,
            join: Some(join),
            host: self.clone(),
            run,
            stopped: false,
        })
    }
}

pub struct HttpServer {
    addr: SocketAddr,
    token: String,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    host: ServerHost,
    run: String,
    stopped: bool,
}

impl HttpServer {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.addr.port())
    }
    pub fn token(&self) -> &str {
        &self.token
    }
    /// Local id this serving run reports lifecycle events under.
    pub fn session_id(&self) -> &str {
        &self.run
    }
    pub fn shutdown(mut self) {
        self.stop_now();
    }
    fn stop_now(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(500));
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        if !std::mem::replace(&mut self.stopped, true) {
            self.host
                .emit(LifecycleKind::Stop, &self.run, "http", "shutdown", None);
        }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stop_now();
    }
}

/// Origin/Host loopback check: exact host match (a substring check would accept `localhost.evil.com`).
pub(crate) fn host_is_loopback(value: &str, has_scheme: bool) -> bool {
    let rest = if has_scheme {
        match value.split_once("://") {
            Some((_, r)) => r,
            None => return false,
        }
    } else {
        value
    };
    let hp = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if hp.contains('@') {
        return false;
    }
    let host = if let Some(r) = hp.strip_prefix('[') {
        r.split(']').next().unwrap_or(r)
    } else {
        hp.rsplit_once(':').map(|(h, _)| h).unwrap_or(hp)
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().min(b.len()) {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn respond(
    s: &mut TcpStream,
    status: &str,
    extra: &[(&str, String)],
    body: &str,
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes())?;
    s.write_all(body.as_bytes())?;
    s.flush()?;
    graceful_close(s);
    Ok(())
}

/// Early gate responses (401/403/404/405) leave the request body unread; closing a socket
/// with unread input sends RST on Windows, which can discard the response before the client
/// reads it. Half-close, then drain bounded input so the peer sees FIN after the response.
fn graceful_close(s: &mut TcpStream) {
    let _ = s.shutdown(std::net::Shutdown::Write);
    let _ = s.set_read_timeout(Some(Duration::from_millis(250)));
    let mut buf = [0u8; 8192];
    let mut drained = 0usize;
    while drained < MAX_BODY {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained += n,
        }
    }
}

const MAX_HEAD: usize = 64 * 1024;
const MAX_BODY: usize = 8 * 1024 * 1024;

fn handle_http(
    host: &ServerHost,
    mut s: TcpStream,
    token: &str,
    sessions: &Mutex<HashSet<String>>,
    run: &str,
) -> std::io::Result<()> {
    let deny = |why: &str| host.emit(LifecycleKind::Denied, run, "http", why, None);
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    let peer = s.peer_addr()?;
    // Read head.
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        if buf.len() > MAX_HEAD {
            return respond(&mut s, "431 Request Header Fields Too Large", &[], "{}");
        }
        let n = s.read(&mut tmp)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let hdr = |name: &str| -> Option<String> {
        head.split("\r\n").skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_string())
        })
    };
    // Gate order: path, Host (rebinding), Origin, bearer.
    if path.split('?').next() != Some("/mcp") {
        return respond(&mut s, "404 Not Found", &[], "{}");
    }
    if hdr("host").is_some_and(|h| !host_is_loopback(&h, false)) {
        deny("non-loopback Host header");
        return respond(&mut s, "403 Forbidden", &[], r#"{"error":"bad host"}"#);
    }
    if hdr("origin").is_some_and(|o| !host_is_loopback(&o, true)) {
        deny("non-loopback Origin header");
        return respond(&mut s, "403 Forbidden", &[], r#"{"error":"bad origin"}"#);
    }
    let authed = hdr("authorization")
        .and_then(|a| a.strip_prefix("Bearer ").map(str::to_string))
        .is_some_and(|t| ct_eq(&t, token));
    if !authed {
        deny("missing or wrong bearer token");
        return respond(
            &mut s,
            "401 Unauthorized",
            &[("WWW-Authenticate", "Bearer".into())],
            r#"{"error":"unauthorized"}"#,
        );
    }
    match method {
        "POST" => {}
        "DELETE" => {
            if let Some(sid) = hdr("mcp-session-id") {
                sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&sid);
            }
            return respond(&mut s, "200 OK", &[], "{}");
        }
        _ => {
            return respond(
                &mut s,
                "405 Method Not Allowed",
                &[("Allow", "POST, DELETE".into())],
                "{}",
            )
        }
    }
    let len: usize = hdr("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if len > MAX_BODY {
        return respond(&mut s, "413 Payload Too Large", &[], "{}");
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = s.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    let session = hdr("mcp-session-id");
    if let Some(sid) = &session {
        if !sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(sid)
        {
            return respond(
                &mut s,
                "404 Not Found",
                &[],
                r#"{"error":"unknown session"}"#,
            );
        }
    }
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            let r = host.observed_error(
                ServerTransport::Http { peer },
                -32700,
                format!("parse error: {e}"),
            );
            return respond(&mut s, "400 Bad Request", &[], &r.to_string());
        }
    };
    let ctx = Ctx::new(
        RequestContext {
            transport: ServerTransport::Http { peer },
            session,
            metadata: Value::Null,
        },
        run.to_string(),
    );
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let (reply, new_sid) = match host.handle_any(msg, &ctx) {
        Ok(pair) => pair,
        Err(e) => {
            let r = rpc_error(&id, -32603, format!("cannot issue session: {e}"));
            return respond(&mut s, "500 Internal Server Error", &[], &r.to_string());
        }
    };
    match reply {
        None => respond(&mut s, "202 Accepted", &[], ""),
        Some(r) => {
            let mut extra = vec![];
            if let Some(sid) = new_sid {
                sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(sid.clone());
                extra.push(("Mcp-Session-Id", sid));
            }
            respond(&mut s, "200 OK", &extra, &crate::raw::to_wire_string(&r))
        }
    }
}

#[cfg(test)]
mod token_tests {
    use super::*;

    struct NoTools;

    impl ToolProvider for NoTools {
        fn list(&self) -> Vec<ToolDescriptor> {
            vec![]
        }
        fn call(&self, name: &str, _args: Value) -> Result<ToolResult> {
            Err(McpError::ToolNotFound(name.into()))
        }
    }

    fn host() -> ServerHost {
        ServerHost::new("test", "0.0.0", Arc::new(NoTools))
    }

    fn assert_hex_token(token: &str) {
        assert_eq!(token.len(), LOOPBACK_TOKEN_BYTES * 2);
        assert!(token.bytes().all(|c| c.is_ascii_hexdigit()));
        assert!(
            token.bytes().any(|c| c != b'0'),
            "token must not be all zeros"
        );
    }

    #[test]
    fn rng_failure_refuses_to_serve_and_binds_nothing() {
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let err = host()
            .serve_http_with_fill(HttpServeOptions { port, token: None }, |_| {
                Err(getrandom::Error::UNSUPPORTED)
            })
            .err()
            .expect("server must refuse to start without a generated token");
        assert!(matches!(err, McpError::RandomUnavailable(_)), "got {err:?}");
        assert!(
            TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok(),
            "a failed generation must not leave a listener bound"
        );
    }

    #[test]
    fn failed_or_zero_fill_is_never_accepted() {
        assert!(matches!(
            random_hex_with(LOOPBACK_TOKEN_BYTES, |_| Err(getrandom::Error::UNSUPPORTED)),
            Err(McpError::RandomUnavailable(_))
        ));
        assert!(matches!(
            random_hex_with(LOOPBACK_TOKEN_BYTES, |b| {
                b.fill(0);
                Ok(())
            }),
            Err(McpError::RandomUnavailable(_))
        ));
    }

    #[test]
    fn generated_value_has_expected_length_and_is_not_zero() {
        for _ in 0..8 {
            let generated = random_hex_with(LOOPBACK_TOKEN_BYTES, getrandom::fill).unwrap();
            assert_hex_token(&generated);
        }
    }

    #[test]
    fn served_default_is_generated() {
        let srv = host()
            .serve_http(HttpServeOptions {
                port: 0,
                token: None,
            })
            .unwrap();
        assert_hex_token(srv.token());
        srv.shutdown();
    }
}
