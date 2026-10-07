use crate::discovery::record_process_matches;
use crate::error::{ServiceError, ERROR_NOT_ALLOWED};
use crate::record::*;
use crate::suite::{make_private_dir, valid_service_name, Suite, SuiteError};
use crate::transport::{self, endpoint_for, pid_alive, probe_endpoint, Conn, Listener};
use rightkit_framed_sidecar::{read_json_line, write_json_line, FrameError};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub const DEFAULT_FRAME_LIMIT: usize = 16 * 1024 * 1024;

/// Sends an `event` frame for the request being handled.
pub type Emit = Arc<dyn Fn(Value) + Send + Sync>;

/// What a handler knows about the request it is serving.
#[derive(Clone)]
pub struct CallContext {
    conn: u64,
    app: String,
    closed: Arc<AtomicBool>,
    handoff: Arc<AtomicBool>,
}

impl CallContext {
    pub fn conn_id(&self) -> u64 {
        self.conn
    }
    /// The app label the client declared in `hello` (already allowlisted).
    pub fn app(&self) -> &str {
        &self.app
    }
    /// The client went away while this request was running.
    pub fn peer_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
    /// A higher-ranked host asked for the service: return
    /// [`ServiceError::handoff`] at the next safe boundary.
    pub fn handoff_requested(&self) -> bool {
        self.handoff.load(Ordering::SeqCst)
    }
}

pub trait Handler: Send + Sync + 'static {
    /// Serve one allowlisted request. Events go to `emit`; a returned
    /// retryable error is sent to the client as-is.
    fn call(
        &self,
        cx: &CallContext,
        method: &str,
        params: &Value,
        emit: &Emit,
    ) -> Result<Value, ServiceError>;
    /// A takeover was accepted: stop starting new steps, finish at a boundary.
    fn on_handoff(&self) {}
    /// A connection closed (also fired mid-request when the peer drops).
    fn connection_closed(&self, _conn: u64) {}
}

/// Content-free lifecycle events for the host's journal.
#[derive(Debug, Clone, PartialEq)]
pub enum ServiceEvent {
    Started {
        service: String,
        endpoint: String,
        rank: u8,
    },
    ConnectionOpened {
        conn: u64,
        app: String,
    },
    ConnectionRejected {
        app: String,
        reason: String,
    },
    Denied {
        conn: u64,
        app: String,
        method: String,
    },
    HandoffRequested {
        by_role: String,
        by_rank: u8,
    },
    Released,
}

/// Callback for [`ServiceEvent`]s.
pub type EventFn = Arc<dyn Fn(&ServiceEvent) + Send + Sync>;

pub struct ServiceConfig {
    pub service: String,
    pub role: String,
    pub rank: u8,
    pub version: String,
    pub allowlist: Allowlist,
    pub frame_limit: usize,
    /// How long a higher-ranked host waits for the incumbent to release.
    pub takeover_timeout: Duration,
    /// How long the incumbent lets in-flight requests reach a boundary.
    pub handoff_grace: Duration,
    pub on_event: Option<EventFn>,
    /// Role -> rank for peers that publish a `role` but no `rank` (HeardRight's
    /// `service.json` and `takeover.json`). A peer whose rank cannot be
    /// resolved is never taken over from and never obliges a handoff.
    pub known_roles: std::collections::BTreeMap<String, u8>,
    /// Explicit endpoint (a compatibility convention such as HeardRight's
    /// `<service_dir>/speech.sock`); default is [`endpoint_for`].
    pub endpoint: Option<String>,
}

impl ServiceConfig {
    pub fn new(service: &str, role: &str, rank: u8, version: &str, allowlist: Allowlist) -> Self {
        Self {
            service: service.into(),
            role: role.into(),
            rank,
            version: version.into(),
            allowlist,
            frame_limit: DEFAULT_FRAME_LIMIT,
            takeover_timeout: Duration::from_secs(60),
            handoff_grace: Duration::from_secs(30),
            on_event: None,
            known_roles: Default::default(),
            endpoint: None,
        }
    }
}

#[derive(Debug)]
pub enum HostError {
    /// A live host of equal or better rank already serves this service.
    Held(ServiceRecord),
    /// The incumbent did not release within `takeover_timeout`.
    TakeoverTimeout,
    InvalidName(String),
    Suite(SuiteError),
    Io(std::io::Error),
}
impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::Held(r) => write!(
                f,
                "{} is already hosted by {} (rank {}, pid {})",
                r.service, r.role, r.rank, r.pid
            ),
            HostError::TakeoverTimeout => write!(f, "incumbent host did not release in time"),
            HostError::InvalidName(s) => write!(f, "invalid service name {s:?}"),
            HostError::Suite(e) => write!(f, "{e}"),
            HostError::Io(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for HostError {}
impl From<std::io::Error> for HostError {
    fn from(e: std::io::Error) -> Self {
        HostError::Io(e)
    }
}
impl From<SuiteError> for HostError {
    fn from(e: SuiteError) -> Self {
        HostError::Suite(e)
    }
}

fn resolve_rank(cfg: &ServiceConfig, role: &str, rank: u8) -> u8 {
    if rank != RANK_UNKNOWN {
        rank
    } else {
        cfg.known_roles.get(role).copied().unwrap_or(RANK_UNKNOWN)
    }
}

struct Shared {
    suite: Suite,
    cfg: ServiceConfig,
    handler: Arc<dyn Handler>,
    listener: Listener,
    record: ServiceRecord,
    stop: AtomicBool,
    handoff: Arc<AtomicBool>,
    released: Mutex<bool>,
    released_cv: Condvar,
    inflight: AtomicUsize,
    next_conn: AtomicU64,
    conns: Mutex<HashMap<u64, Arc<dyn Fn() + Send + Sync>>>,
}

impl Shared {
    fn event(&self, e: ServiceEvent) {
        if let Some(f) = &self.cfg.on_event {
            f(&e);
        }
    }

    fn release(&self) {
        {
            let mut r = self.released.lock().unwrap_or_else(|e| e.into_inner());
            if *r {
                return;
            }
            *r = true;
        }
        self.stop.store(true, Ordering::SeqCst);
        // Remove the record first so clients stop discovering this host, but only if it is ours.
        if read_record(&self.suite, &self.cfg.service).is_some_and(|r| r.pid == self.record.pid) {
            let _ = std::fs::remove_file(record_path(&self.suite, &self.cfg.service));
        }
        self.listener.wake();
        self.listener.cleanup();
        for (_, close) in self.conns.lock().unwrap_or_else(|e| e.into_inner()).drain() {
            close();
        }
        self.event(ServiceEvent::Released);
        self.released_cv.notify_all();
    }
}

/// A running host. Dropping it releases the service.
pub struct Hosted {
    shared: Arc<Shared>,
}

impl Hosted {
    pub fn record(&self) -> &ServiceRecord {
        &self.shared.record
    }
    pub fn handoff_requested(&self) -> bool {
        self.shared.handoff.load(Ordering::SeqCst)
    }
    pub fn inflight(&self) -> usize {
        self.shared.inflight.load(Ordering::SeqCst)
    }
    /// Block until the host has released (clean shutdown or takeover).
    pub fn wait_released(&self, timeout: Duration) -> bool {
        let g = self
            .shared
            .released
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (g, _) = self
            .shared
            .released_cv
            .wait_timeout_while(g, timeout, |r| !*r)
            .unwrap_or_else(|e| e.into_inner());
        *g
    }
    pub fn shutdown(&self) {
        self.shared.release();
    }
}

impl Drop for Hosted {
    fn drop(&mut self) {
        self.shared.release();
    }
}

/// Host `cfg.service` on `suite`: discover, rank, take over if outranking,
/// bind the current-user-only endpoint, publish `service.json`.
// `HostError::Held` carries the incumbent's record by value so callers can match on it directly;
// this runs once per host start, so the larger Err is not a hot-path cost.
#[allow(clippy::result_large_err)]
pub fn host(
    suite: &Suite,
    cfg: ServiceConfig,
    handler: Arc<dyn Handler>,
) -> Result<Hosted, HostError> {
    if !valid_service_name(&cfg.service) {
        return Err(HostError::InvalidName(cfg.service.clone()));
    }
    let dir = suite.service_dir(&cfg.service);
    make_private_dir(&dir)?;
    let me = std::process::id();
    let deadline = Instant::now() + cfg.takeover_timeout;
    let mut requested = false;
    loop {
        match read_record(suite, &cfg.service) {
            Some(rec)
                if rec.pid != me
                    && record_process_matches(&rec)
                    && probe_endpoint(&rec.endpoint) =>
            {
                let their = resolve_rank(&cfg, &rec.role, rec.rank);
                if their == RANK_UNKNOWN || cfg.rank >= their {
                    return Err(HostError::Held(rec));
                }
                if !requested {
                    let t = TakeoverRequest {
                        role: cfg.role.clone(),
                        rank: cfg.rank,
                        pid: me,
                        requested_at_ms: now_ms(),
                    };
                    write_atomic(
                        &takeover_path(suite, &cfg.service),
                        &serde_json::to_vec(&t).unwrap_or_default(),
                    )?;
                    requested = true;
                }
                if Instant::now() >= deadline {
                    let _ = std::fs::remove_file(takeover_path(suite, &cfg.service));
                    return Err(HostError::TakeoverTimeout);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Some(_) => {
                // Stale (dead endpoint) or our own leftover: discard.
                let _ = std::fs::remove_file(record_path(suite, &cfg.service));
            }
            None => break,
        }
    }
    let endpoint = cfg
        .endpoint
        .clone()
        .unwrap_or_else(|| endpoint_for(suite, &cfg.service));
    let listener = loop {
        match Listener::bind(&endpoint) {
            Ok(listener) => break listener,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                if let Some(rec) = read_record(suite, &cfg.service) {
                    return Err(HostError::Held(rec));
                }
                // A handing-off incumbent removes its record before its pipe instances finish
                // closing; on Windows the name stays taken for that moment. Wait it out.
                if !cfg!(windows) || Instant::now() >= deadline {
                    return Err(HostError::Io(e));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(HostError::Io(e)),
        }
    };
    let record = ServiceRecord {
        protocol_major: PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        service: cfg.service.clone(),
        role: cfg.role.clone(),
        rank: cfg.rank,
        pid: me,
        endpoint: endpoint.clone(),
        version: cfg.version.clone(),
        engine_version: cfg.version.clone(),
        started_ms: now_ms(),
        process_started_ms: crate::discovery::own_start_ms().unwrap_or(0),
    };
    write_atomic(
        &record_path(suite, &cfg.service),
        &serde_json::to_vec_pretty(&record).unwrap_or_default(),
    )?;
    if read_takeover(suite, &cfg.service).is_some_and(|t| t.pid == me) {
        let _ = std::fs::remove_file(takeover_path(suite, &cfg.service));
    }
    let shared = Arc::new(Shared {
        suite: suite.clone(),
        cfg,
        handler,
        listener,
        record,
        stop: AtomicBool::new(false),
        handoff: Arc::new(AtomicBool::new(false)),
        released: Mutex::new(false),
        released_cv: Condvar::new(),
        inflight: AtomicUsize::new(0),
        next_conn: AtomicU64::new(1),
        conns: Mutex::new(HashMap::new()),
    });
    shared.event(ServiceEvent::Started {
        service: shared.cfg.service.clone(),
        endpoint,
        rank: shared.cfg.rank,
    });
    let s = shared.clone();
    std::thread::Builder::new()
        .name("rk-svc-accept".into())
        .spawn(move || accept_loop(s))?;
    let s = shared.clone();
    std::thread::Builder::new()
        .name("rk-svc-takeover".into())
        .spawn(move || takeover_monitor(s))?;
    Ok(Hosted { shared })
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn accept_loop(s: Arc<Shared>) {
    while !s.stop.load(Ordering::SeqCst) {
        match s.listener.accept() {
            Ok(Some(conn)) => {
                if s.stop.load(Ordering::SeqCst) {
                    break;
                }
                let s2 = s.clone();
                let _ = std::thread::Builder::new()
                    .name("rk-svc-conn".into())
                    .spawn(move || serve_conn(s2, conn));
            }
            Ok(None) => {} // wrong user: dropped
            Err(_) => {
                if s.stop.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Watch for a takeover request from a higher-ranked live host.
fn takeover_monitor(s: Arc<Shared>) {
    let max_age = s.cfg.takeover_timeout.as_millis() as u64;
    while !s.stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(25));
        let Some(t) = read_takeover(&s.suite, &s.cfg.service) else {
            continue;
        };
        let fresh = now_ms().saturating_sub(t.requested_at_ms) <= max_age + 5_000;
        let their = resolve_rank(&s.cfg, &t.role, t.rank);
        if their == RANK_UNKNOWN
            || their >= s.cfg.rank
            || t.pid == s.record.pid
            || !pid_alive(t.pid)
            || !fresh
        {
            continue;
        }
        s.handoff.store(true, Ordering::SeqCst);
        s.event(ServiceEvent::HandoffRequested {
            by_role: t.role,
            by_rank: their,
        });
        s.handler.on_handoff();
        let until = Instant::now() + s.cfg.handoff_grace;
        while s.inflight.load(Ordering::SeqCst) > 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        s.release();
        return;
    }
}

fn frame_err(e: &FrameError) -> ServiceError {
    ServiceError::new("bad_frame", e.to_string())
}

fn serve_conn(s: Arc<Shared>, conn: Conn) {
    let Conn {
        reader,
        writer,
        peer_closed,
        shutdown,
    } = conn;
    let mut reader = BufReader::new(reader);
    let writer = Arc::new(Mutex::new(writer));
    let limit = s.cfg.frame_limit;
    let send = {
        let w = writer.clone();
        move |v: &Value| {
            let mut w = w.lock().unwrap_or_else(|e| e.into_inner());
            if write_json_line(&mut *w, v, limit).is_ok() {
                let _ = w.flush();
            }
        }
    };
    let mut buf = Vec::new();
    // ---- hello
    let hello: Value = match read_json_line(&mut reader, &mut buf, 64 * 1024) {
        Ok(Some(v)) => v,
        _ => return, // probe or garbage
    };
    let app = hello["app"].as_str().unwrap_or("").to_string();
    let reject = |reason: &str, code: &str, retryable: bool| {
        let e = ServiceError {
            code: code.into(),
            message: reason.into(),
            retryable,
        };
        send(&json!({"type": "hello", "ok": false, "error": e.to_value()}));
        s.event(ServiceEvent::ConnectionRejected {
            app: app.clone(),
            reason: reason.into(),
        });
        shutdown();
    };
    if hello["type"] != "hello" || app.is_empty() {
        return reject(
            "first frame must be hello with an app label",
            "bad_hello",
            false,
        );
    }
    if hello["protocol_major"].as_u64() != Some(u64::from(PROTOCOL_MAJOR)) {
        return reject("protocol major mismatch", "protocol_mismatch", false);
    }
    if s.handoff.load(Ordering::SeqCst) {
        return reject(
            "host is handing off",
            crate::error::ERROR_HOST_HANDOFF,
            true,
        );
    }
    if !s.cfg.allowlist.app_known(&app) {
        return reject(
            "app is not on the service allowlist",
            ERROR_NOT_ALLOWED,
            false,
        );
    }
    let id = s.next_conn.fetch_add(1, Ordering::SeqCst);
    s.conns
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, shutdown.clone());
    s.event(ServiceEvent::ConnectionOpened {
        conn: id,
        app: app.clone(),
    });
    send(&json!({
        "type": "hello", "ok": true, "service": s.cfg.service, "role": s.cfg.role, "rank": s.cfg.rank,
        "version": s.cfg.version, "protocol_major": PROTOCOL_MAJOR, "protocol_minor": PROTOCOL_MINOR,
        "methods": s.cfg.allowlist.methods_for(&app),
    }));

    let closed_notified = Arc::new(AtomicBool::new(false));
    let notify_closed = {
        let (s, flag) = (s.clone(), closed_notified.clone());
        move || {
            if !flag.swap(true, Ordering::SeqCst) {
                s.handler.connection_closed(id);
            }
        }
    };
    // ---- requests, lock-step: one in flight per connection
    loop {
        let req: Value = match read_json_line(&mut reader, &mut buf, limit) {
            Ok(Some(v)) => v,
            Ok(None) | Err(FrameError::Io(_)) => break,
            Err(e) => {
                send(
                    &json!({"type": "response", "id": 0, "ok": false, "error": frame_err(&e).to_value()}),
                );
                continue;
            }
        };
        let Some(rid) = req["id"].as_u64().filter(|_| req["type"] == "request") else {
            send(
                &json!({"type": "response", "id": 0, "ok": false, "error": ServiceError::new("bad_request", "expected {type:request,id,method}").to_value()}),
            );
            continue;
        };
        let respond_err = |e: ServiceError| {
            send(&json!({"type": "response", "id": rid, "ok": false, "error": e.to_value()}))
        };
        let method = req["method"].as_str().unwrap_or("").to_string();
        if s.handoff.load(Ordering::SeqCst) {
            respond_err(ServiceError::handoff());
            continue;
        }
        if !s.cfg.allowlist.permits(&app, &method) {
            s.event(ServiceEvent::Denied {
                conn: id,
                app: app.clone(),
                method: method.clone(),
            });
            respond_err(ServiceError::new(
                ERROR_NOT_ALLOWED,
                format!("{app} may not call {method}"),
            ));
            continue;
        }
        if method == "ping" {
            send(
                &json!({"type": "response", "id": rid, "ok": true, "result": {"pong": true, "rank": s.cfg.rank}}),
            );
            continue;
        }
        let params = req.get("params").cloned().unwrap_or_else(|| json!({}));
        let cx = CallContext {
            conn: id,
            app: app.clone(),
            closed: Arc::new(AtomicBool::new(false)),
            handoff: s.handoff.clone(),
        };
        let emit: Emit = {
            let send = send.clone();
            Arc::new(move |ev| send(&json!({"type": "event", "id": rid, "event": ev})))
        };
        s.inflight.fetch_add(1, Ordering::SeqCst);
        let done = AtomicBool::new(false);
        let result = std::thread::scope(|sc| {
            sc.spawn(|| {
                while !done.load(Ordering::SeqCst) {
                    if peer_closed() {
                        cx.closed.store(true, Ordering::SeqCst);
                        notify_closed();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(40));
                }
            });
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                s.handler.call(&cx, &method, &params, &emit)
            }))
            .unwrap_or_else(|_| Err(ServiceError::new("panic", "handler panicked")));
            done.store(true, Ordering::SeqCst);
            r
        });
        s.inflight.fetch_sub(1, Ordering::SeqCst);
        if cx.peer_closed() {
            break;
        }
        match result {
            Ok(v) => send(&json!({"type": "response", "id": rid, "ok": true, "result": v})),
            Err(e) => respond_err(e),
        }
    }
    notify_closed();
    s.conns
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
    shutdown();
}

#[allow(dead_code)]
fn _assert_transport_used() {
    let _ = transport::current_uid;
}
