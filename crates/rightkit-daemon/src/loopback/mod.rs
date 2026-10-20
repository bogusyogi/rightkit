//! A loopback-only HTTP host with a bearer token (`remote-loopback` feature, PK06-5, ruling D5).
//!
//! This is the transport CodeRight's daemon speaks today: HTTP/1.1 on `127.0.0.1`, one per-launch
//! bearer token, a discovery file with the port and the token, and the client-presence refcount.
//! The spec's decision (D5) is to preserve that wire for its current clients (the window, the CLI,
//! the SSH-tunnel and phone clients) and let a shared owner provide the listener, the parser and the
//! authentication, so an app stops hand-rolling them. It is **not** the daemon's `remote` host:
//!
//! * `remote` is a TLS listener for enrolled devices with signed requests (`RK-Device`), durable
//!   enrolment state and the `daemon-api` v1 contract. A desktop app never ships it, and
//!   `releaseSwitches.remoteHost` stays false for desktop apps.
//! * `remote-loopback` is a different feature. It binds loopback only, speaks plain HTTP with a
//!   bearer token or an owner-declared capability, and needs no tokio, axum, hyper, rustls or
//!   device state: it is std threads over `TcpListener`. It does not enable `remote` and is not
//!   enabled by it (the `loopback_features` test pins that). The release scanner (outside this
//!   crate, `release-switch-scan.mjs`, PK17-2) keeps refusing a desktop build that enables
//!   `remote`; it gets its own rule for this feature.
//!
//! What it reuses from the rest of the crate rather than re-implementing: the typed error and its
//! status table ([`ServiceError`](crate::ServiceError), `contract::status_for_error`), the
//! failed-authentication limiter (`FailLimiter`), the JSON depth and node guard (`chunk`), the
//! atomic owner-only record write (`record::write_atomic`), and the process-birth identity check
//! (`rightkit-child-process`). The `remote` router itself is axum and device-signature bound, so it
//! cannot be reused without enabling `remote`; the loopback host keeps its own small request
//! pipeline, written to the same rules (bounded body, refuse-before-handler, no env or request
//! field sets a role).
//!
//! # What a request goes through
//!
//! In [`LoopbackApp::handle`]'s caller, in order: the global token bucket (CodeRight's 200/s,
//! burst 400, `429 rate limited`); the bounded parser (head, header and body limits,
//! timeouts, no `Transfer-Encoding`, no `Expect`, no smuggling-prone duplicates); the Host and
//! Origin policy (DNS-rebinding and cross-site refusal); the CORS preflight; `GET
//! /health` (exempt, the owner supplies the body); credentials by route class (bearer, capability,
//! public); the JSON-body guard; the built-in presence routes (`/clients`) when enabled; then the
//! owner's handler, under `catch_unwind`. A refused request never reaches the handler.
//!
//! The verification of the token and of capabilities is done here, inside the owner's process,
//! before any handler runs: an app's handler receives an [`Authenticated`] value, not a header to
//! check.
//!
//! # CodeRight wire points preserved
//!
//! * `127.0.0.1:<port>` bind, port 0 for an ephemeral port (`main_runtime_server.rs:268`).
//! * `Authorization: Bearer <token>`, the owner's legacy token headers (`X-CodeRight-Token`),
//!   the `access_token` query only on declared stream routes; `GET /health` and CORS preflight
//!   exempt; exact-match public routes for signed deliveries (`auth.rs:37-135`).
//! * `401` with the plain text `unauthorized`, `429` with `rate limited` (`auth.rs`).
//! * The discovery record in PascalCase ([`LoopbackRecord`], `startup.rs:18-37`).
//! * The presence routes and the exit rule ([`Presence`], `api_clients.rs`).
//!
//! Not preserved, deliberately: CodeRight's `allow_headers(Any)` CORS (the owner lists the request
//! headers it needs) and its unbounded request line and head (the parser has bounds). A client that
//! relied on either needs the owner to widen the policy explicitly.

mod auth;
mod http;
mod presence;
mod record;

pub use auth::{
    Authenticated, CapabilityGrant, CapabilitySource, CapabilityVerifier, LoopbackToken,
    OriginPolicy, PatternError, RouteClass, RoutePattern, RouteSpec, TokenError, MAX_TOKEN_LEN,
    MIN_TOKEN_LEN,
};
pub use http::{
    percent_decode, write_response, Body, Limits, Method, ParseError, Request, RequestReader,
    Response, StreamFn, StreamSink, TimedRead,
};
pub use presence::{
    iso8601_utc, AttachError, AttachGrant, ClientRecord, Presence, PresenceConfig, MAX_CLIENTS,
};
pub use record::{LoopbackRecord, RecordLabels};

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::ServiceError;
use crate::fail_limit::FailLimiter;
use auth::Policy;

/// Requests served on one connection before it is closed.
pub const MAX_REQUESTS_PER_CONNECTION: usize = 1000;
/// Largest body limit the host accepts from its owner.
pub const MAX_BODY_CEILING: usize = 64 * 1024 * 1024;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The owner's side: what to do with a request the host admitted.
pub trait LoopbackApp: Send + Sync + 'static {
    /// Serve one request that passed the host's checks. `auth` says how it was admitted. For a
    /// [`RouteClass::Public`] route nothing was verified here and the handler must verify its own
    /// proof and fail closed. A panic is a `500`.
    fn handle(&self, request: &Request, auth: &Authenticated) -> Response;

    /// The body of `GET /health` (answered without credentials, after the Host and Origin
    /// checks). Default `{"ok":true}`. Keep it free of secrets: any local process can read it.
    fn health(&self) -> Value {
        json!({"ok": true})
    }
}

/// How the host is configured. Every default is the stricter choice.
pub struct LoopbackConfig {
    port: u16,
    token: Option<LoopbackToken>,
    limits: Limits,
    origin: OriginPolicy,
    routes: Vec<RouteSpec>,
    legacy_headers: Vec<String>,
    capability: Option<Arc<dyn CapabilityVerifier>>,
    presence: Option<PresenceConfig>,
    busy: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    on_idle: Option<Arc<dyn Fn() + Send + Sync>>,
    record_path: Option<PathBuf>,
    labels: RecordLabels,
    max_connections: usize,
    rate: Option<(f64, f64)>,
}

impl Default for LoopbackConfig {
    fn default() -> Self {
        Self {
            port: 0,
            token: None,
            limits: Limits::default(),
            origin: OriginPolicy::default(),
            routes: Vec::new(),
            legacy_headers: Vec::new(),
            capability: None,
            presence: None,
            busy: None,
            on_idle: None,
            record_path: None,
            labels: RecordLabels::default(),
            max_connections: 128,
            rate: Some((200.0, 400.0)),
        }
    }
}

impl LoopbackConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// The port on `127.0.0.1`; 0 (the default) takes an ephemeral one. There is no way to ask for
    /// another address.
    #[must_use]
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Use this token instead of generating one.
    #[must_use]
    pub fn token(mut self, token: LoopbackToken) -> Self {
        self.token = Some(token);
        self
    }

    /// Request and connection bounds (see [`Limits`]). The body limit is capped at
    /// [`MAX_BODY_CEILING`].
    #[must_use]
    pub fn limits(mut self, mut limits: Limits) -> Self {
        limits.max_body_bytes = limits.max_body_bytes.min(MAX_BODY_CEILING);
        self.limits = limits;
        self
    }

    /// The browser-facing rules (see [`OriginPolicy`]).
    #[must_use]
    pub fn origin_policy(mut self, policy: OriginPolicy) -> Self {
        self.origin = policy;
        self
    }

    /// Declare a route class (public, capability, query-token stream). Undeclared routes are
    /// bearer routes.
    #[must_use]
    pub fn route(mut self, spec: RouteSpec) -> Self {
        self.routes.push(spec);
        self
    }

    /// A legacy header that may carry the bearer token (CodeRight: `X-CodeRight-Token`). Read
    /// before `Authorization`, as CodeRight reads it.
    #[must_use]
    pub fn legacy_token_header(mut self, name: &str) -> Self {
        self.legacy_headers.push(name.to_ascii_lowercase());
        self
    }

    /// The owner's check for [`RouteClass::Capability`] routes.
    #[must_use]
    pub fn capability_verifier(mut self, verifier: Arc<dyn CapabilityVerifier>) -> Self {
        self.capability = Some(verifier);
        self
    }

    /// Serve CodeRight's client-presence routes (`/clients`) behind the bearer token.
    #[must_use]
    pub fn presence(mut self, config: PresenceConfig) -> Self {
        self.presence = Some(config);
        self
    }

    /// Whether something is in flight (a running session), for the presence exit rule.
    #[must_use]
    pub fn busy_probe<F>(mut self, busy: F) -> Self
    where
        F: Fn() -> bool + Send + Sync + 'static,
    {
        self.busy = Some(Arc::new(busy));
        self
    }

    /// Called (once per idle spell, on the host's reaper thread) when the presence exit rule says
    /// the host may exit. It decides nothing by itself: the owner shuts down or ignores it.
    #[must_use]
    pub fn on_idle<F>(mut self, on_idle: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.on_idle = Some(Arc::new(on_idle));
        self
    }

    /// Publish the discovery record ([`LoopbackRecord`]) here once listening, and remove it on
    /// shutdown if it still holds this host's record. The directory must exist and should be
    /// private (the owner hardens it).
    #[must_use]
    pub fn record_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.record_path = Some(path.into());
        self
    }

    /// The owner's labels for the record (`DatabasePath`, `SchemaId`, ...).
    #[must_use]
    pub fn record_labels(mut self, labels: RecordLabels) -> Self {
        self.labels = labels;
        self
    }

    /// Most connections at once (default 128, at least 1). One over the cap gets a `503` and is
    /// closed.
    #[must_use]
    pub fn max_connections(mut self, n: usize) -> Self {
        self.max_connections = n.max(1);
        self
    }

    /// The global token bucket: `per_second` refill, `burst` capacity. Default 200 and 400
    /// (CodeRight's). `None` turns it off.
    #[must_use]
    pub fn rate_limit(mut self, limit: Option<(f64, f64)>) -> Self {
        self.rate = limit;
        self
    }
}

struct Bucket {
    tokens: f64,
    last: Instant,
    rate: f64,
    burst: f64,
}

impl Bucket {
    fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.burst);
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

struct Shared {
    port: u16,
    policy: Policy,
    limits: Limits,
    app: Arc<dyn LoopbackApp>,
    presence: Option<Presence>,
    busy: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    on_idle: Option<Arc<dyn Fn() + Send + Sync>>,
    idle_fired: AtomicBool,
    bucket: Mutex<Option<Bucket>>,
    fails: Mutex<FailLimiter>,
    conns: Mutex<HashMap<u64, TcpStream>>,
    live: AtomicUsize,
    next_conn: AtomicU64,
    max_connections: usize,
    stop: AtomicBool,
    /// The accept loop was woken by a connection to the listener at shutdown.
    woke: AtomicBool,
    stopped: Mutex<bool>,
    stopped_cv: Condvar,
    record: LoopbackRecord,
    record_path: Option<PathBuf>,
}

/// A running loopback host. Dropping it shuts it down.
pub struct Loopback {
    shared: Arc<Shared>,
    accept: Mutex<Option<JoinHandle<()>>>,
    reaper: Mutex<Option<JoinHandle<()>>>,
}

impl Loopback {
    /// Bind `127.0.0.1`, publish the record if one was asked for, and serve `app`.
    ///
    /// Errors: the port is taken, a route pattern does not parse, a capability route has no
    /// verifier, the token is unusable, the record cannot be written.
    pub fn start(config: LoopbackConfig, app: Arc<dyn LoopbackApp>) -> io::Result<Loopback> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port))?;
        let port = listener.local_addr()?.port();
        let token = match config.token {
            Some(token) => token,
            None => LoopbackToken::generate().map_err(io::Error::other)?,
        };
        let mut limits = config.limits;
        limits
            .singleton_headers
            .extend(config.legacy_headers.iter().cloned());
        let policy = Policy::new(
            token,
            config.legacy_headers,
            config.origin,
            port,
            config.routes,
            config.capability,
        )
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let record =
            LoopbackRecord::for_this_process(port, policy.token().expose(), &config.labels);
        let presence = config.presence.map(Presence::new);
        let has_presence = presence.is_some();
        let shared = Arc::new(Shared {
            port,
            policy,
            limits,
            app,
            presence,
            busy: config.busy,
            on_idle: config.on_idle,
            idle_fired: AtomicBool::new(false),
            bucket: Mutex::new(config.rate.map(|(rate, burst)| Bucket {
                tokens: burst,
                last: Instant::now(),
                rate,
                burst,
            })),
            fails: Mutex::new(FailLimiter::new()),
            conns: Mutex::new(HashMap::new()),
            live: AtomicUsize::new(0),
            next_conn: AtomicU64::new(1),
            max_connections: config.max_connections,
            stop: AtomicBool::new(false),
            woke: AtomicBool::new(false),
            stopped: Mutex::new(false),
            stopped_cv: Condvar::new(),
            record,
            record_path: config.record_path,
        });
        let accept_shared = shared.clone();
        let accept = std::thread::Builder::new()
            .name("rk-loopback-accept".into())
            .spawn(move || accept_loop(accept_shared, listener))?;
        let host = Loopback {
            shared: shared.clone(),
            accept: Mutex::new(Some(accept)),
            reaper: Mutex::new(None),
        };
        // The record goes out only after the accept thread runs: a client that reads it connects.
        if let Some(path) = &shared.record_path {
            if let Err(e) = shared.record.publish(path) {
                host.shutdown();
                return Err(e);
            }
        }
        if has_presence {
            let reaper_shared = shared.clone();
            let reaper = std::thread::Builder::new()
                .name("rk-loopback-presence".into())
                .spawn(move || reaper_loop(reaper_shared))?;
            *lock(&host.reaper) = Some(reaper);
        }
        Ok(host)
    }

    pub fn port(&self) -> u16 {
        self.shared.port
    }

    /// `http://127.0.0.1:<port>`.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.shared.port)
    }

    pub fn token(&self) -> &LoopbackToken {
        self.shared.policy.token()
    }

    /// The discovery record of this host (also what the file holds, when one was asked for).
    pub fn record(&self) -> &LoopbackRecord {
        &self.shared.record
    }

    pub fn presence(&self) -> Option<&Presence> {
        self.shared.presence.as_ref()
    }

    /// Connections open now.
    pub fn connections(&self) -> usize {
        self.shared.live.load(Ordering::SeqCst)
    }

    /// Stop accepting, close every connection, remove the record file if it is still this
    /// host's. Idempotent; safe to call from a handler or from `on_idle`.
    pub fn shutdown(&self) {
        self.shared.shutdown();
        for (slot, wakeable) in [
            (&self.accept, self.shared.woke.load(Ordering::SeqCst)),
            (&self.reaper, true),
        ] {
            if let Some(handle) = lock(slot).take() {
                // An accept loop that could not be woken is left to end on its own rather than
                // blocking the caller forever.
                if wakeable && handle.thread().id() != std::thread::current().id() {
                    let _ = handle.join();
                }
            }
        }
    }

    /// Block until the host has shut down. True if it did within `timeout`.
    pub fn wait_stopped(&self, timeout: Duration) -> bool {
        let guard = lock(&self.shared.stopped);
        let (guard, _) = self
            .shared
            .stopped_cv
            .wait_timeout_while(guard, timeout, |stopped| !*stopped)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept_loop(shared: Arc<Shared>, listener: TcpListener) {
    for incoming in listener.incoming() {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let stream = match incoming {
            Ok(stream) => stream,
            Err(_) => {
                // A transient accept failure (descriptor exhaustion): do not spin on it.
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        let Ok(peer) = stream.peer_addr() else {
            continue;
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        if shared.live.load(Ordering::SeqCst) >= shared.max_connections {
            refuse_busy(stream);
            continue;
        }
        let id = shared.next_conn.fetch_add(1, Ordering::SeqCst);
        let Ok(registered) = stream.try_clone() else {
            continue;
        };
        lock(&shared.conns).insert(id, registered);
        shared.live.fetch_add(1, Ordering::SeqCst);
        let conn_shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("rk-loopback-conn".into())
            .spawn(move || {
                serve_connection(&conn_shared, stream, peer, id);
                lock(&conn_shared.conns).remove(&id);
                lock(&conn_shared.fails).forget(id, Instant::now());
                conn_shared.live.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            lock(&shared.conns).remove(&id);
            shared.live.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn refuse_busy(mut stream: TcpStream) {
    use std::io::Write;
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let _ = stream.write_all(
        b"HTTP/1.1 503 Service Unavailable\r\nconnection: close\r\ncontent-length: 0\r\n\r\n",
    );
    let _ = stream.shutdown(Shutdown::Both);
}

fn serve_connection(shared: &Arc<Shared>, stream: TcpStream, peer: SocketAddr, conn: u64) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_write_timeout(Some(shared.limits.write_timeout));
    let mut writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(_) => return,
    };
    let mut reader = RequestReader::new(stream);
    let mut served = 0usize;
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let mut request = match reader.read_request(&shared.limits) {
            Ok(request) => request,
            Err(error) => {
                // Answer what is worth answering, then close: the stream may be mid-frame.
                if let Some(status) = error.status() {
                    let body = Response::json(
                        status,
                        &json!({"error": {
                            "code": "bad_request",
                            "message": error.message(),
                            "retryable": false
                        }}),
                    );
                    let _ = write_response(&mut writer, body, false, &|| true);
                }
                break;
            }
        };
        request.peer = Some(peer);
        served += 1;
        let keep = request.keep_alive && served < MAX_REQUESTS_PER_CONNECTION;
        let response = shared.dispatch(&request, conn);
        let stopping = || shared.stop.load(Ordering::SeqCst);
        match write_response(&mut writer, response, keep, &stopping) {
            Ok(true) => {}
            _ => break,
        }
    }
    let _ = writer.shutdown(Shutdown::Both);
}

fn reaper_loop(shared: Arc<Shared>) {
    loop {
        {
            let guard = lock(&shared.stopped);
            let (guard, _) = shared
                .stopped_cv
                .wait_timeout_while(guard, Duration::from_secs(1), |stopped| !*stopped)
                .unwrap_or_else(|e| e.into_inner());
            if *guard {
                return;
            }
        }
        let Some(presence) = shared.presence.as_ref() else {
            return;
        };
        let now = Instant::now();
        presence.reap(now);
        let busy = shared.busy.as_ref().is_some_and(|probe| probe());
        if presence.should_shut_down(now, busy) {
            if !shared.idle_fired.swap(true, Ordering::SeqCst) {
                if let Some(on_idle) = &shared.on_idle {
                    on_idle();
                }
            }
        } else {
            shared.idle_fired.store(false, Ordering::SeqCst);
        }
    }
}

#[derive(Deserialize)]
struct AttachBody {
    kind: String,
    pid: i64,
    #[serde(default)]
    label: Option<String>,
}

fn valid_client_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Shared {
    fn shutdown(&self) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        // Wake the accept loop, which is blocked in accept.
        let woke = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)),
            Duration::from_secs(1),
        )
        .is_ok();
        self.woke.store(woke, Ordering::SeqCst);
        let open: Vec<TcpStream> = lock(&self.conns).drain().map(|(_, s)| s).collect();
        for stream in open {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if let Some(path) = &self.record_path {
            self.record.remove_if_ours(path);
        }
        *lock(&self.stopped) = true;
        self.stopped_cv.notify_all();
    }

    fn take_token(&self, now: Instant) -> bool {
        match lock(&self.bucket).as_mut() {
            Some(bucket) => bucket.take(now),
            None => true,
        }
    }

    fn dispatch(&self, request: &Request, conn: u64) -> Response {
        let now = Instant::now();
        if !self.take_token(now) {
            return Response::text(429, "rate limited");
        }
        let origin = match self.policy.check_context(request) {
            Ok(origin) => origin,
            Err(refusal) => return refusal.into_response(),
        };
        if request.method == Method::Options {
            return self.policy.preflight(request, origin.as_deref());
        }
        let response = self.route(request, conn, now);
        match origin {
            // The browser may read the answer (a 401 included) only with these.
            Some(origin) => response
                .header("access-control-allow-origin", &origin)
                .header("vary", "origin"),
            None => response,
        }
    }

    fn route(&self, request: &Request, conn: u64, now: Instant) -> Response {
        if request.method == Method::Get && request.path == "/health" {
            return match catch_unwind(AssertUnwindSafe(|| self.app.health())) {
                Ok(body) => Response::json(200, &body),
                Err(_) => internal_error(),
            };
        }
        let auth = match self.policy.authenticate(request) {
            Ok(auth) => auth,
            Err(refusal) => {
                if refusal.auth_failure {
                    // Consulted only for a credential that failed: a correct one is never
                    // refused because other callers failed.
                    let mut fails = lock(&self.fails);
                    let limited = fails.refuses_unverified(conn, now);
                    fails.record_failure(conn, now);
                    if limited {
                        return Response::text(429, "rate limited");
                    }
                }
                return refusal.into_response();
            }
        };
        if let Some(answer) = self.presence_route(request, now) {
            return answer;
        }
        if let Err(answer) = guard_json_body(request) {
            return answer;
        }
        match catch_unwind(AssertUnwindSafe(|| self.app.handle(request, &auth))) {
            Ok(response) => response,
            Err(_) => internal_error(),
        }
    }

    /// CodeRight's `/clients` routes, with the bare statuses CodeRight answers.
    fn presence_route(&self, request: &Request, now: Instant) -> Option<Response> {
        let presence = self.presence.as_ref()?;
        let path = request.path.as_str();
        if path == "/clients" {
            return Some(match request.method {
                Method::Get => {
                    let clients: Vec<Value> = presence
                        .list(now)
                        .iter()
                        .map(ClientRecord::to_json)
                        .collect();
                    Response::json(200, &json!({ "clients": clients }))
                }
                Method::Post => attach_client(presence, request, now),
                _ => Response::empty(405),
            });
        }
        let rest = path.strip_prefix("/clients/")?;
        let (id, action) = match rest.split_once('/') {
            Some((id, action)) => (id, Some(action)),
            None => (rest, None),
        };
        if !valid_client_id(id) {
            return Some(Response::empty(404));
        }
        match (request.method, action) {
            (Method::Post, Some("heartbeat")) => Some(if presence.heartbeat(id, now) {
                Response::empty(204)
            } else {
                Response::empty(404)
            }),
            (Method::Delete, None) => Some(if presence.detach(id, now) {
                Response::empty(204)
            } else {
                Response::empty(404)
            }),
            _ => None,
        }
    }
}

fn attach_client(presence: &Presence, request: &Request, now: Instant) -> Response {
    let Ok(body) = serde_json::from_slice::<AttachBody>(&request.body) else {
        return Response::empty(400);
    };
    match presence.attach(&body.kind, body.pid, body.label, now) {
        Ok(grant) => Response::json(200, &grant.to_json()),
        Err(AttachError::BadKind) => Response::empty(400),
        Err(AttachError::Conflict) => Response::empty(409),
        Err(AttachError::Full) => Response::empty(503),
    }
}

fn internal_error() -> Response {
    Response::from_error(&ServiceError::new("panic", "the handler panicked"))
}

/// A JSON body must be UTF-8 and within the depth and size the crate's other transports allow,
/// checked before any handler parses it.
fn guard_json_body(request: &Request) -> Result<(), Response> {
    if request.body.is_empty() {
        return Ok(());
    }
    let is_json = request.header("content-type").is_some_and(|value| {
        value
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("application/json")
    });
    if !is_json {
        return Ok(());
    }
    let text = std::str::from_utf8(&request.body).map_err(|_| {
        Response::from_error(&ServiceError::new(
            "bad_request",
            "the JSON body is not UTF-8",
        ))
    })?;
    crate::chunk::check_shape(text).map_err(|e| Response::from_error(&e))
}
