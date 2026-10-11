//! Opt-in HTTP I/O. Persistent runtime owns legacy SSE & cancellable sockets.
use crate::auth::AuthProvider;
use crate::client::{rpc_outcome, ClientOptions};
use crate::client_features::*;
use crate::events::{emit, next_id, EventSink, LifecycleKind, McpEvent};
use crate::{McpError, Result};
use futures_util::StreamExt;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use rightkit_http::{
    AsyncClient, AsyncStreamResponse, BodyStream, ClientConfig, HttpError, Method, Request,
    RetryPolicy,
};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}
#[derive(Clone)]
struct WireContext {
    headers: HeaderMap,
    session: String,
    override_session: bool,
    override_auth: bool,
    cancellation: Option<CancellationToken>,
}
struct Pending {
    context: WireContext,
    sender: oneshot::Sender<Result<Value>>,
}
struct LegacySession {
    endpoint: String,
    context: WireContext,
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    reader: tokio::task::AbortHandle,
    alive: Arc<AtomicBool>,
}
impl Drop for LegacySession {
    fn drop(&mut self) {
        self.reader.abort();
        for (_, pending) in lock(&self.pending).drain() {
            if let Some(token) = &pending.context.cancellation {
                token.cancel();
            }
            let _ = pending.sender.send(Err(McpError::Closed));
        }
    }
}
struct Shared {
    server: String,
    url: String,
    client: AsyncClient,
    auth: Option<Arc<dyn AuthProvider>>,
    extensions: ClientExtensions,
    session: Mutex<Option<String>>,
    version: Mutex<Option<String>>,
    handlers: Mutex<HashMap<String, ElicitationHandler>>,
    events: Option<EventSink>,
    life_id: String,
    started: AtomicBool,
}
impl Shared {
    fn event(&self, kind: LifecycleKind, reason: &str) {
        emit(&self.events, || {
            let mut event = McpEvent::new(kind, &self.life_id, reason);
            event.server = Some(self.server.clone());
            event.transport = Some(if self.extensions.legacy_sse {
                "sse"
            } else {
                "http"
            });
            event
        });
    }
    fn context(&self, options: &CallOptions) -> Result<WireContext> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "accept",
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        let session = options
            .session_id
            .clone()
            .or_else(|| lock(&self.session).clone());
        let value = |text: &str| {
            HeaderValue::from_str(text)
                .map_err(|e| McpError::Config(format!("invalid HTTP header value: {e}")))
        };
        if let Some(session) = session {
            headers.insert("mcp-session-id", value(&session)?);
        }
        if let Some(version) = lock(&self.version).clone() {
            headers.insert("mcp-protocol-version", value(&version)?);
        }
        if let Some(auth) = &self.auth {
            if let Some(token) = auth.bearer(&self.server)? {
                headers.insert(
                    "authorization",
                    value(&format!("Bearer {}", token.expose()))?,
                );
            }
        }
        let override_session = options.session_id.is_some()
            || options
                .headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("mcp-session-id"));
        for (name, text) in &options.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| McpError::Config(format!("invalid HTTP header name: {e}")))?;
            headers.insert(name, value(text)?);
        }
        let session = headers
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(&self.life_id)
            .to_string();
        let override_auth = options
            .headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("authorization"));
        Ok(WireContext {
            headers,
            session,
            override_session,
            override_auth,
            cancellation: Some(CancellationToken::new()),
        })
    }
    fn adopt_session(&self, response: &AsyncStreamResponse, context: &mut WireContext) {
        if context.override_session {
            return;
        }
        if let Some(session) = response
            .header("mcp-session-id")
            .filter(|value| !value.is_empty())
        {
            let newly_bound = {
                let mut current = lock(&self.session);
                let changed = current.as_deref() != Some(session);
                *current = Some(session.into());
                changed
            };
            context.session = session.into();
            if let Ok(value) = HeaderValue::from_str(session) {
                context.headers.insert("mcp-session-id", value);
            }
            if newly_bound {
                if let Some(handler) = &self.extensions.elicitation {
                    lock(&self.handlers)
                        .entry(session.into())
                        .or_insert_with(|| handler.clone());
                }
            }
        }
    }
    async fn send(
        &self,
        endpoint: &str,
        message: &Value,
        context: &WireContext,
    ) -> Result<AsyncStreamResponse> {
        for attempt in 0..2 {
            let mut headers = context.headers.clone();
            if attempt != 0 && !context.override_auth {
                if let Some(auth) = &self.auth {
                    if let Some(token) = auth.bearer(&self.server)? {
                        headers.insert(
                            "authorization",
                            HeaderValue::from_str(&format!("Bearer {}", token.expose()))
                                .map_err(|e| McpError::Config(e.to_string()))?,
                        );
                    }
                }
            }
            let body =
                serde_json::to_vec(message).map_err(|e| McpError::Protocol(e.to_string()))?;
            let response = self
                .client
                .stream_any(&wire_request(Method::Post, endpoint, &headers, body))
                .await
                .map_err(transport_error)?;
            if response.status == 401 {
                let challenge = response.header("www-authenticate");
                if attempt == 0
                    && self
                        .auth
                        .as_ref()
                        .map(|auth| auth.on_unauthorized(&self.server, challenge))
                        .transpose()?
                        .unwrap_or(false)
                {
                    continue;
                }
                return Err(McpError::Unauthorized);
            }
            if response.status == 404
                && context.headers.contains_key("mcp-session-id")
                && !context.override_session
            {
                *lock(&self.session) = None;
                lock(&self.handlers).remove(&context.session);
                if self.started.swap(false, Ordering::SeqCst) {
                    self.event(LifecycleKind::Stop, "server ended the session");
                }
                return Err(McpError::Closed);
            }
            if !(200..300).contains(&response.status) {
                return Err(McpError::Transport(format!(
                    "HTTP {}",
                    status_text(response.status)
                )));
            }
            return Ok(response);
        }
        Err(McpError::Unauthorized)
    }
    async fn elicitation(
        &self,
        message: &Value,
        context: &WireContext,
        endpoint: &str,
    ) -> Result<()> {
        let Some(id) = message.get("id") else {
            return Ok(());
        };
        let result = match message.get("method").and_then(Value::as_str) {
            Some("ping") => Ok(json!({})),
            Some("elicitation/create") => {
                let handler = lock(&self.handlers).get(&context.session).cloned();
                if let Some(handler) = handler {
                    let callback_context = ElicitationContext {
                        server: self.server.clone(),
                        session_id: context.session.clone(),
                        wire_request_id: id.clone(),
                        call_cancellation: context.cancellation.clone(),
                    };
                    let params = message.get("params").cloned().unwrap_or(Value::Null);
                    tokio::task::spawn_blocking(move || {
                        catch_unwind(AssertUnwindSafe(|| handler(&callback_context, &params)))
                            .unwrap_or_else(|_| {
                                Err(McpError::Protocol("elicitation callback panicked".into()))
                            })
                    })
                    .await
                    .map_err(|e| McpError::Transport(e.to_string()))?
                } else {
                    Err(McpError::Rpc {
                        code: -32000,
                        message: "elicitation has no session-bound callback".into(),
                    })
                }
            }
            _ => Err(McpError::Rpc {
                code: -32601,
                message: "method not supported by client".into(),
            }),
        };
        let result = if context
            .cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            Err(McpError::Cancelled)
        } else {
            result
        };
        self.send(endpoint, &reply(id.clone(), result), context)
            .await
            .map(|_| ())
    }
}

pub(crate) struct AdvancedHttp {
    shared: Arc<Shared>,
    runtime: Option<tokio::runtime::Runtime>,
    legacy: tokio::sync::Mutex<Option<Arc<LegacySession>>>,
    next_id: AtomicU64,
}
impl AdvancedHttp {
    pub(crate) fn new(
        server: &str,
        url: &str,
        options: &ClientOptions,
        extensions: &ClientExtensions,
    ) -> Result<Self> {
        extensions
            .endpoint_origin_policy
            .resolve_endpoint(url, url)?;
        // One attempt, no redirects, no phase deadlines other than connect: the
        // MCP layer owns retry (auth refresh), timeout and cancellation itself.
        #[cfg(feature = "testing")]
        let address_policy = extensions.address_policy.clone().unwrap_or_default();
        #[cfg(not(feature = "testing"))]
        let address_policy = rightkit_http::AddressPolicy::default();
        let client = AsyncClient::new(ClientConfig {
            user_agent: concat!("rightkit-mcp/", env!("CARGO_PKG_VERSION")).into(),
            connect_timeout: Some(options.connect_timeout.unwrap_or(options.request_timeout)),
            response_timeout: None,
            max_redirects: 0,
            retry: RetryPolicy::none(),
            address_policy,
            ..ClientConfig::default()
        })
        .map_err(transport_error)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;
        let life_id = next_id("mcpc");
        let mut handlers = HashMap::new();
        if let Some(handler) = &extensions.elicitation {
            handlers.insert(life_id.clone(), handler.clone());
        }
        Ok(Self {
            shared: Arc::new(Shared {
                server: server.into(),
                url: url.into(),
                client,
                auth: options.auth.clone(),
                extensions: extensions.clone(),
                session: Mutex::new(None),
                version: Mutex::new(None),
                handlers: Mutex::new(handlers),
                events: options.on_event.clone(),
                life_id,
                started: AtomicBool::new(false),
            }),
            runtime: Some(runtime),
            legacy: tokio::sync::Mutex::new(None),
            next_id: AtomicU64::new(1),
        })
    }
    fn block_on<T: Send>(&self, future: impl std::future::Future<Output = T> + Send) -> Result<T> {
        // Scoped worker avoids nested-runtime panics in synchronous calls from async hosts.
        std::thread::scope(|scope| {
            scope
                .spawn(|| self.runtime.as_ref().unwrap().block_on(future))
                .join()
                .map_err(|_| McpError::Transport("HTTP worker panicked".into()))
        })
    }
    async fn legacy_session(&self, context: &WireContext) -> Result<Arc<LegacySession>> {
        let mut current = self.legacy.lock().await;
        if let Some(session) = current.as_ref().filter(|s| s.alive.load(Ordering::SeqCst)) {
            return Ok(session.clone());
        }
        let mut sse_headers = context.headers.clone();
        sse_headers.insert("accept", HeaderValue::from_static("text/event-stream"));
        let response = self
            .shared
            .client
            .stream_any(&wire_request(
                Method::Get,
                &self.shared.url,
                &sse_headers,
                Vec::new(),
            ))
            .await
            .map_err(transport_error)?;
        if response.status == 401 {
            return Err(McpError::Unauthorized);
        }
        if !(200..300).contains(&response.status) {
            return Err(McpError::Transport(format!(
                "SSE GET HTTP {}",
                status_text(response.status)
            )));
        }
        let mut context = context.clone();
        self.shared.adopt_session(&response, &mut context);
        if context.session.is_empty() {
            context.session = self.shared.life_id.clone();
        }
        if let Some(handler) = &self.shared.extensions.elicitation {
            lock(&self.shared.handlers)
                .entry(context.session.clone())
                .or_insert_with(|| handler.clone());
        }
        let limits = self.shared.extensions.response_limits;
        let mut stream = response.into_stream();
        let mut parser = SseParser::new(limits.max_frame_bytes.min(limits.max_response_bytes));
        let endpoint = loop {
            let chunk = stream
                .next()
                .await
                .ok_or_else(|| McpError::Protocol("SSE ended before endpoint announcement".into()))?
                .map_err(transport_error)?;
            parser.push(&chunk)?;
            let mut endpoint = None;
            while let Some(event) = parser.pop() {
                if (event.name.as_deref() == Some("endpoint") || event.name.is_none())
                    && !event.data.trim().is_empty()
                {
                    endpoint = Some(
                        self.shared
                            .extensions
                            .endpoint_origin_policy
                            .resolve_endpoint(&self.shared.url, event.data.trim())?,
                    );
                    break;
                }
            }
            if let Some(endpoint) = endpoint {
                break endpoint;
            }
        };
        let pending = Arc::new(Mutex::new(HashMap::<u64, Pending>::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let (shared, endpoint2, context2, pending2, alive2) = (
            self.shared.clone(),
            endpoint.clone(),
            context.clone(),
            pending.clone(),
            alive.clone(),
        );
        let reader = tokio::spawn(async move {
            let outcome = read_legacy(
                shared.clone(),
                &endpoint2,
                &context2,
                pending2.clone(),
                &mut stream,
                &mut parser,
            )
            .await;
            alive2.store(false, Ordering::SeqCst);
            let error = outcome.err().unwrap_or(McpError::Closed);
            let _ = catch_unwind(AssertUnwindSafe(|| {
                shared.event(LifecycleKind::Crash, "legacy SSE stream ended unexpectedly")
            }));
            for (_, pending) in lock(&pending2).drain() {
                if let Some(token) = &pending.context.cancellation {
                    token.cancel();
                }
                let _ = pending.sender.send(Err(error.clone()));
            }
        })
        .abort_handle();
        let session = Arc::new(LegacySession {
            endpoint,
            context,
            pending,
            reader,
            alive,
        });
        *current = Some(session.clone());
        Ok(session)
    }
    async fn perform(
        &self,
        message: &Value,
        id: Option<u64>,
        context: &WireContext,
        route: &Mutex<Option<String>>,
    ) -> Result<Value> {
        if self.shared.extensions.legacy_sse {
            let session = self.legacy_session(context).await?;
            *lock(route) = Some(session.endpoint.clone());
            let mut context = context.clone();
            if context.session.is_empty() {
                context.session = session.context.session.clone();
            }
            if let Some(id) = id {
                let (sender, receiver) = oneshot::channel();
                lock(&session.pending).insert(
                    id,
                    Pending {
                        context: context.clone(),
                        sender,
                    },
                );
                let result = async {
                    self.shared
                        .send(&session.endpoint, message, &context)
                        .await?;
                    receiver.await.map_err(|_| McpError::Closed)?
                }
                .await;
                lock(&session.pending).remove(&id);
                result
            } else {
                self.shared
                    .send(&session.endpoint, message, &context)
                    .await
                    .map(|_| Value::Null)
            }
        } else {
            *lock(route) = Some(self.shared.url.clone());
            let mut context = context.clone();
            let response = self
                .shared
                .send(&self.shared.url, message, &context)
                .await?;
            self.shared.adopt_session(&response, &mut context);
            if id.is_none() {
                return Ok(Value::Null);
            }
            let id = id.unwrap();
            if response
                .header("content-type")
                .is_some_and(|v| v.contains("text/event-stream"))
            {
                read_http_sse(self.shared.clone(), response, id, &context).await
            } else {
                let bytes = bounded_body(
                    response,
                    self.shared
                        .extensions
                        .response_limits
                        .max_response_bytes
                        .min(self.shared.extensions.response_limits.max_frame_bytes),
                )
                .await?;
                let message: Value = serde_json::from_slice(&bytes)
                    .map_err(|e| McpError::Protocol(e.to_string()))?;
                if message.get("id").and_then(Value::as_u64) != Some(id)
                    || message.get("method").is_some()
                {
                    return Err(McpError::Protocol(
                        "response id does not match request".into(),
                    ));
                }
                rpc_outcome(&message)
            }
        }
    }
    async fn call(
        &self,
        method: &str,
        params: Value,
        options: &CallOptions,
        notification: bool,
        timeout: Duration,
    ) -> Result<Value> {
        if options
            .cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(McpError::Cancelled);
        }
        let context = self.shared.context(options)?;
        let id = (!notification).then(|| self.next_id.fetch_add(1, Ordering::SeqCst));
        let mut message = json!({"jsonrpc":"2.0","method":method,"params":params});
        if let Some(id) = id {
            message["id"] = json!(id);
        }
        let route = Mutex::new(None);
        let result = {
            let operation =
                tokio::time::timeout(timeout, self.perform(&message, id, &context, &route));
            tokio::pin!(operation);
            if let Some(token) = &options.cancellation {
                tokio::select! { biased; _ = token.cancelled() => Err(McpError::Cancelled), result = &mut operation => result.unwrap_or(Err(McpError::Timeout)) }
            } else {
                operation.await.unwrap_or(Err(McpError::Timeout))
            }
        }; // Drop in-flight send/read before notification.
        if matches!(
            result,
            Err(McpError::Cancelled | McpError::Timeout | McpError::Closed)
        ) {
            if let Some(token) = &context.cancellation {
                token.cancel();
            }
        }
        if let Some(id) = id {
            if matches!(result, Err(McpError::Cancelled | McpError::Timeout)) {
                if let Some(session) = self.legacy.lock().await.as_ref() {
                    lock(&session.pending).remove(&id);
                }
                if method != "initialize" {
                    let endpoint = lock(&route).clone();
                    if let Some(endpoint) = endpoint {
                        let cancelled = json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id}});
                        let _ = tokio::time::timeout(
                            Duration::from_millis(200),
                            self.shared.send(&endpoint, &cancelled, &context),
                        )
                        .await;
                    }
                }
            }
        }
        result
    }
    pub(crate) fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        options: &CallOptions,
    ) -> Result<Value> {
        self.block_on(self.call(method, params, options, false, timeout))?
    }
    pub(crate) fn notify_with_options(
        &self,
        method: &str,
        timeout: Duration,
        options: &CallOptions,
    ) -> Result<()> {
        self.block_on(self.call(method, json!({}), options, true, timeout))?
            .map(|_| ())
    }
    pub(crate) fn set_version(&self, version: String) {
        *lock(&self.shared.version) = Some(version);
    }
    pub(crate) fn mark_started(&self) {
        if !self.shared.started.swap(true, Ordering::SeqCst) {
            self.shared
                .event(LifecycleKind::Start, "initialized session");
        }
    }
    pub(crate) fn alive(&self) -> bool {
        if !self.shared.extensions.legacy_sse {
            return true;
        }
        self.legacy
            .try_lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|s| s.alive.load(Ordering::SeqCst)))
            .unwrap_or(true)
    }
    pub(crate) fn session_id(&self) -> Option<String> {
        Some(
            lock(&self.shared.session)
                .clone()
                .unwrap_or_else(|| self.shared.life_id.clone()),
        )
    }
    pub(crate) fn set_elicitation_handler(&self, session: &str, handler: ElicitationHandler) {
        lock(&self.shared.handlers).insert(session.into(), handler);
    }
    pub(crate) fn clear_elicitation_handler(&self, session: &str) {
        lock(&self.shared.handlers).remove(session);
    }
}
impl Drop for AdvancedHttp {
    fn drop(&mut self) {
        self.legacy.get_mut().take();
        if self.shared.started.swap(false, Ordering::SeqCst) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                self.shared.event(LifecycleKind::Stop, "closed")
            }));
        }
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}
fn transport_error(error: HttpError) -> McpError {
    if matches!(error, HttpError::Timeout { .. }) {
        McpError::Timeout
    } else {
        McpError::Transport(error.to_string())
    }
}
fn status_text(status: u16) -> String {
    http::StatusCode::from_u16(status)
        .map(|s| s.to_string())
        .unwrap_or_else(|_| status.to_string())
}
fn wire_request(method: Method, url: &str, headers: &HeaderMap, body: Vec<u8>) -> Request {
    let mut request = Request::new(method, url);
    for (name, value) in headers {
        request.headers.push((
            name.as_str().into(),
            String::from_utf8_lossy(value.as_bytes()).into_owned(),
        ));
    }
    request.body = body;
    request
}
async fn bounded_body(response: AsyncStreamResponse, limit: usize) -> Result<Vec<u8>> {
    if response
        .header("content-length")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .is_some_and(|size| size > limit as u64)
    {
        return Err(McpError::ResponseTooLarge { limit });
    }
    let mut bytes = Vec::new();
    let mut stream = response.into_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(transport_error)?;
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(McpError::ResponseTooLarge { limit });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn reply(id: Value, result: Result<Value>) -> Value {
    match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(McpError::Rpc { code, message }) => {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
        }
        Err(McpError::RpcWithData {
            code,
            message,
            data,
        }) => json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message,"data":data}}),
        Err(error) => {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":error.to_string()}})
        }
    }
}
struct SseEvent {
    name: Option<String>,
    data: String,
}
struct SseParser {
    line: Vec<u8>,
    name: Option<String>,
    data: Vec<String>,
    frame_bytes: usize,
    limit: usize,
    after_cr: bool,
    cr_frame_end: Option<usize>,
    events: VecDeque<SseEvent>,
}
impl SseParser {
    fn new(limit: usize) -> Self {
        Self {
            line: Vec::new(),
            name: None,
            data: Vec::new(),
            frame_bytes: 0,
            limit,
            after_cr: false,
            cr_frame_end: None,
            events: VecDeque::new(),
        }
    }
    fn push(&mut self, bytes: &[u8]) -> Result<()> {
        for &byte in bytes {
            if byte == b'\n' && self.after_cr {
                self.after_cr = false;
                if let Some(ended) = self.cr_frame_end.take() {
                    if ended.saturating_add(1) > self.limit {
                        return Err(McpError::ResponseTooLarge { limit: self.limit });
                    }
                } else {
                    self.frame_bytes = self.frame_bytes.saturating_add(1);
                    if self.frame_bytes > self.limit {
                        return Err(McpError::ResponseTooLarge { limit: self.limit });
                    }
                }
                continue;
            }
            self.cr_frame_end = None;
            self.after_cr = byte == b'\r';
            self.frame_bytes = self.frame_bytes.saturating_add(1);
            if self.frame_bytes > self.limit {
                return Err(McpError::ResponseTooLarge { limit: self.limit });
            }
            if matches!(byte, b'\r' | b'\n') {
                let ended = (byte == b'\r' && self.line.is_empty()).then_some(self.frame_bytes);
                self.end_line()?;
                self.cr_frame_end = ended;
            } else {
                self.line.push(byte);
            }
        }
        Ok(())
    }
    fn end_line(&mut self) -> Result<()> {
        if self.line.is_empty() {
            if !self.data.is_empty() {
                self.events.push_back(SseEvent {
                    name: self.name.take(),
                    data: self.data.join("\n"),
                });
            }
            self.name = None;
            self.data.clear();
            self.frame_bytes = 0;
        } else {
            let bytes = std::mem::take(&mut self.line);
            let line =
                std::str::from_utf8(&bytes).map_err(|e| McpError::Protocol(e.to_string()))?;
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => self.name = Some(value.into()),
                "data" => self.data.push(value.into()),
                _ => {}
            }
        }
        Ok(())
    }
    fn pop(&mut self) -> Option<SseEvent> {
        self.events.pop_front()
    }
}
async fn read_http_sse(
    shared: Arc<Shared>,
    response: AsyncStreamResponse,
    id: u64,
    context: &WireContext,
) -> Result<Value> {
    let limits = shared.extensions.response_limits;
    let mut parser = SseParser::new(limits.max_frame_bytes);
    let mut received = 0usize;
    let mut stream = response.into_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(transport_error)?;
        received = received.saturating_add(chunk.len());
        if received > limits.max_response_bytes {
            return Err(McpError::ResponseTooLarge {
                limit: limits.max_response_bytes,
            });
        }
        parser.push(&chunk)?;
        while let Some(event) = parser.pop() {
            let message: Value =
                serde_json::from_str(&event.data).map_err(|e| McpError::Protocol(e.to_string()))?;
            if message.get("method").is_some() {
                shared.elicitation(&message, context, &shared.url).await?;
            } else if message.get("id").and_then(Value::as_u64) == Some(id) {
                return rpc_outcome(&message);
            }
        }
    }
    Err(McpError::Protocol("no response in event stream".into()))
}
async fn read_legacy(
    shared: Arc<Shared>,
    endpoint: &str,
    stream_context: &WireContext,
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    stream: &mut BodyStream,
    parser: &mut SseParser,
) -> Result<()> {
    loop {
        while let Some(event) = parser.pop() {
            let message: Value =
                serde_json::from_str(&event.data).map_err(|e| McpError::Protocol(e.to_string()))?;
            if message.get("method").is_none() {
                if let Some(id) = message.get("id").and_then(Value::as_u64) {
                    if let Some(pending) = lock(&pending).remove(&id) {
                        let _ = pending.sender.send(rpc_outcome(&message));
                    }
                }
            } else {
                let related = message
                    .pointer("/params/_meta/relatedRequestId")
                    .or_else(|| message.pointer("/params/relatedRequestId"));
                let context = {
                    let calls = lock(&pending);
                    if let Some(related) = related {
                        related
                            .as_u64()
                            .or_else(|| related.as_str().and_then(|s| s.parse().ok()))
                            .and_then(|id| calls.get(&id))
                            .map(|call| call.context.clone())
                    } else {
                        let mut contexts = calls.values().map(|call| &call.context);
                        match contexts.next() {
                            Some(first) if contexts.all(|other| other.session == first.session) => {
                                let mut context = first.clone();
                                if calls.len() != 1 {
                                    context.cancellation = None;
                                }
                                Some(context)
                            }
                            None => {
                                let mut context = stream_context.clone();
                                context.cancellation = None;
                                Some(context)
                            }
                            _ => None,
                        }
                    }
                };
                let (shared, endpoint, fallback) =
                    (shared.clone(), endpoint.to_string(), stream_context.clone());
                tokio::spawn(async move {
                    if let Some(context) = context {
                        let _ = shared.elicitation(&message, &context, &endpoint).await;
                    } else if let Some(id) = message.get("id") {
                        let response = reply(
                            id.clone(),
                            Err(McpError::Rpc {
                                code: -32000,
                                message: "elicitation has no unambiguous active session".into(),
                            }),
                        );
                        let _ = shared.send(&endpoint, &response, &fallback).await;
                    }
                });
            }
        }
        let chunk = stream
            .next()
            .await
            .ok_or(McpError::Closed)?
            .map_err(transport_error)?;
        parser.push(chunk.as_ref())?;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sse_crlf_multiline_and_split_utf8() {
        let mut parser = SseParser::new(1024);
        for byte in "event: endpoint\r\ndata: /méssages\r\ndata: ?sid=1\r\n\r\n".as_bytes() {
            parser.push(&[*byte]).unwrap();
        }
        let event = parser.pop().unwrap();
        assert_eq!(event.name.as_deref(), Some("endpoint"));
        assert_eq!(event.data, "/méssages\n?sid=1");
        assert!(parser.pop().is_none());
    }
    #[test]
    fn sse_unterminated_frame_is_bounded() {
        let mut parser = SseParser::new(8);
        assert!(matches!(
            parser.push(b"data: 123456"),
            Err(McpError::ResponseTooLarge { limit: 8 })
        ));
    }
    #[test]
    fn sse_limit_counts_complete_crlf_delimiters() {
        let bytes = b"data: x\r\n\r\n";
        let mut exact = SseParser::new(bytes.len());
        exact.push(bytes).unwrap();
        assert_eq!(exact.pop().unwrap().data, "x");
        let mut too_small = SseParser::new(bytes.len() - 1);
        assert!(matches!(
            too_small.push(bytes),
            Err(McpError::ResponseTooLarge { .. })
        ));
    }
}
