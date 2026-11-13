//! Engine command contract: one dispatcher serves both transports.
//!
//! stdio (`serve`): JSON-lines frames (`rightkit-sidecar`).
//!   out once: `{"type":"hello","engine","version","contract_version"}`
//!   in:  `{"type":"request","id":<u64>,"method":"..","params":{..}}`
//!   out: `{"type":"event","id":<request id>,"event":{..}}` zero or more, then exactly one
//!        `{"type":"response","id","ok":true,"result":{..}}` or
//!        `{"type":"response","id","ok":false,"error":{"code","message"}}`.
//!
//! Errors carry `retryable` (a retryable error means resubmitting can succeed,
//! e.g. `host_handoff`). A request may set `cancel_on_disconnect: true` on
//! `job.submit|run`: if the client connection closes while the job runs, the
//! job is cancelled (`CommandSet::connection_closed`).
//! CLI (`run_cli`): `<words> [--input <json>|-] [--dry-run] [--wait]
//!   [--wait-timeout <s>] [--request-id <id>]`; pretty JSON result on stdout
//!   (exit 0) or `{"error":{"code","message"}}` (exit 1; 2 for usage); progress
//!   events are NDJSON lines on stderr.

use std::io::{self, BufRead, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rightkit_sidecar::wire::{Frame, WireError};
use rightkit_sidecar::{
    encode_json_line, read_json_line_or_oversized, write_json_line, FrameError, Incoming,
    ReadLimits,
};
use serde_json::{json, Value};

use crate::error::{JobError, JobResult};

pub use crate::wire::{finite_frame_limit, CONTRACT_VERSION, DEFAULT_FRAME_LIMIT};

/// Receives event values emitted while a command runs.
pub type Sink = Arc<dyn Fn(Value) + Send + Sync>;

/// A group of commands. Apps implement this for product commands and add
/// [`JobCommands`](crate::JobCommands) for the shared `job.*` surface.
pub trait CommandSet: Send + Sync {
    fn methods(&self) -> Vec<String>;
    /// Long-running requests run on their own thread so others stay live.
    fn is_long(&self, _method: &str, _params: &Value) -> bool {
        false
    }
    fn call(&self, method: &str, params: &Value, dry_run: bool, emit: &Sink) -> JobResult<Value>;
    /// Like [`CommandSet::call`] but told which client connection asked (`0` for
    /// stdio and the CLI), so disconnect can be tied to the request.
    fn call_conn(
        &self,
        _conn: u64,
        method: &str,
        params: &Value,
        dry_run: bool,
        emit: &Sink,
    ) -> JobResult<Value> {
        self.call(method, params, dry_run, emit)
    }
    /// A client connection closed (possibly while its request was still running).
    fn connection_closed(&self, _conn: u64) {}
    /// Called once when the engine is shutting down or stdin closed.
    fn shutdown(&self) {}
}

/// Sends unsolicited events (`{"type":"event","event":{..}}`, no `id`) on the connection
/// [`serve`] / [`serve_report`] is serving, from any thread. Get one from
/// [`Dispatcher::event_emitter`] before the dispatcher is moved into `serve`; clones share the
/// binding.
///
/// The frame is the one a request's events use minus the `id`, which
/// `EngineClient` delivers as an unsolicited event (it never counts as
/// progress of a call). Frames and responses are written whole under one lock, so an emitted event
/// never interleaves with another frame. Nothing is queued: an event emitted while no connection
/// is being served is refused, not held. The first frame on a connection is still `hello`: the
/// emitter binds after it is written.
#[derive(Clone, Default)]
pub struct EventEmitter {
    inner: Arc<EmitterInner>,
}

#[derive(Default)]
struct EmitterInner {
    bound: Mutex<Option<Bound>>,
    next: AtomicU64,
}

struct Bound {
    token: u64,
    sink: Sink,
    limit: usize,
}

/// Unbinds its connection when `serve_report` returns, however it returns.
struct BoundGuard {
    inner: Arc<EmitterInner>,
    token: u64,
}

impl Drop for BoundGuard {
    fn drop(&mut self) {
        let mut bound = self.inner.bound.lock().unwrap_or_else(|p| p.into_inner());
        // A later serve may have bound since; leave that one.
        if bound.as_ref().is_some_and(|b| b.token == self.token) {
            *bound = None;
        }
    }
}

impl EventEmitter {
    /// True while a connection is being served.
    pub fn is_connected(&self) -> bool {
        self.inner
            .bound
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }

    /// Sends `event` as an unsolicited event frame. Refused with `not_serving` when no connection
    /// is being served, and with `frame_too_large` / `bad_input` when the frame exceeds the
    /// dispatcher's frame limit or cannot be written as one line; nothing is written then.
    /// Delivery to a peer that has already gone is not reported (the connection ends the serve).
    pub fn emit(&self, event: Value) -> JobResult<()> {
        self.send(Frame::event(None, event).to_value(true))
    }

    /// As [`Self::emit`], with the event frame carrying a top-level `session_id` so a client can
    /// route it to one session (subscribers receive the frame whole).
    pub fn emit_for_session(&self, session_id: &str, event: Value) -> JobResult<()> {
        let frame = Frame::Event {
            id: None,
            session_id: Some(session_id.to_string()),
            event,
        };
        self.send(frame.to_value(true))
    }

    fn send(&self, frame: Value) -> JobResult<()> {
        let (sink, limit) = {
            let bound = self.inner.bound.lock().unwrap_or_else(|p| p.into_inner());
            match bound.as_ref() {
                Some(b) => (Arc::clone(&b.sink), b.limit),
                None => {
                    return Err(JobError::new(
                        "not_serving",
                        "no connection is being served; the event was not sent",
                    ))
                }
            }
        };
        match encode_json_line(&frame, limit) {
            Ok(_) => {}
            Err(FrameError::TooLarge { limit }) => {
                return Err(JobError::new(
                    "frame_too_large",
                    format!("event frame exceeds {limit} bytes"),
                ))
            }
            Err(e) => return Err(JobError::new("bad_input", format!("event frame: {e}"))),
        }
        sink(frame);
        Ok(())
    }

    fn bind(&self, sink: Sink, limit: usize) -> BoundGuard {
        let token = self.inner.next.fetch_add(1, Ordering::Relaxed) + 1;
        *self.inner.bound.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(Bound { token, sink, limit });
        BoundGuard {
            inner: Arc::clone(&self.inner),
            token,
        }
    }
}

/// Replaces the built-in `doctor` payload. Gets `dry_run` and the method list.
pub type DoctorFn = Arc<dyn Fn(bool, &[String]) -> Value + Send + Sync>;

pub struct Dispatcher {
    engine: String,
    version: String,
    sets: Vec<Arc<dyn CommandSet>>,
    frame_limit: usize,
    builtins: Vec<&'static str>,
    doctor: Option<DoctorFn>,
    cli_targets: Vec<(String, String)>,
    error_retryable: bool,
    drain_timeout: Option<Duration>,
    aliases: Vec<(String, String)>,
    method_meta: Vec<(String, Value)>,
    request_dry_run: bool,
    doctor_readiness: Option<ReadinessFn>,
    error_redactor: Option<ErrorRedactor>,
    emitter: EventEmitter,
    hello_fields: Vec<(String, Value)>,
    /// Requests being run now, and the most that ran at once ([`Dispatcher::queue_depth`]).
    active: std::sync::atomic::AtomicUsize,
    active_peak: std::sync::atomic::AtomicUsize,
}

/// Decrements the dispatcher's active count when a request ends, however it ends.
struct ActiveGuard<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Judges a `doctor` report: `true` when the app is ready. See [`Dispatcher::with_doctor_readiness`].
pub type ReadinessFn = Arc<dyn Fn(&Value) -> bool + Send + Sync>;

/// Rewrites every error the dispatcher returns, given the method and its params. See
/// [`Dispatcher::with_error_redactor`].
pub type ErrorRedactor = Arc<dyn Fn(&str, &Value, JobError) -> JobError + Send + Sync>;

pub struct Outcome {
    pub response: Value,
    pub shutdown: bool,
}

fn ok(id: u64, result: Value) -> Value {
    Frame::ok(id, result).to_value(true)
}
fn err(id: u64, code: &str, message: &str) -> Value {
    Frame::err(id, WireError::new(code, message)).to_value(true)
}
fn err_job(id: u64, e: &JobError, retryable: bool) -> Value {
    let error = WireError {
        code: e.code.clone(),
        message: e.message.clone(),
        retryable: e.retryable,
        details: e.details().cloned(),
    };
    Frame::err(id, error).to_value(retryable)
}

/// `internal` error for a panic while serving `method`.
fn panic_error(method: &str, p: &(dyn std::any::Any + Send)) -> JobError {
    let msg = p
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into());
    JobError::new("internal", format!("{method} panicked: {msg}"))
}

const BUILTINS: [&str; 3] = ["ping", "doctor", "shutdown"];

impl Dispatcher {
    pub fn new(engine: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            engine: engine.into(),
            version: version.into(),
            sets: vec![],
            frame_limit: DEFAULT_FRAME_LIMIT,
            builtins: BUILTINS.to_vec(),
            doctor: None,
            cli_targets: vec![],
            error_retryable: true,
            drain_timeout: None,
            aliases: vec![],
            method_meta: vec![],
            request_dry_run: false,
            doctor_readiness: None,
            error_redactor: None,
            emitter: EventEmitter::default(),
            hello_fields: vec![],
            active: std::sync::atomic::AtomicUsize::new(0),
            active_peak: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Requests the dispatcher is running right now: the depth of the work it holds. The
    /// dispatcher queues nothing of its own (each request runs on its connection's thread), so
    /// this is the queue a host sees when requests arrive faster than commands finish. Read as a
    /// number; no lock is taken (rightkit#1280).
    pub fn queue_depth(&self) -> usize {
        self.active.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The most requests that were running at once since the dispatcher was built.
    pub fn queue_depth_peak(&self) -> usize {
        self.active_peak.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// A handle for events this engine sends on its own initiative, with no request behind them
    /// (see [`EventEmitter`]). Take it before the dispatcher moves into `serve`; clones share the
    /// connection `serve` binds.
    pub fn event_emitter(&self) -> EventEmitter {
        self.emitter.clone()
    }
    /// Drop a built-in (`ping`, `doctor`, `shutdown`) so an app command set
    /// can own that method name, or so it is not served at all.
    pub fn without_builtin(mut self, name: &str) -> Self {
        self.builtins.retain(|b| *b != name);
        self
    }
    /// Serve the app's own `doctor` payload from the built-in.
    pub fn with_doctor(mut self, f: DoctorFn) -> Self {
        self.doctor = Some(f);
        self
    }
    /// Opt-in (default off): a request whose top-level `params.dry_run` is the JSON boolean
    /// `true` runs as a dry run, in addition to the process-wide flag (`--dry-run`, `serve`'s
    /// argument). Other values are ignored, so one live `serve` session can preview single
    /// requests. Off keeps the 0.5.x behaviour byte-identical.
    pub fn with_request_dry_run(mut self) -> Self {
        self.request_dry_run = true;
        self
    }
    /// Opt-in: `run_cli` runs the canonical `doctor` method, prints its report, and exits 1 when
    /// `f(&report)` is false (0 otherwise). Stdio `serve` and the window `doctor` frame are
    /// unchanged. Without it `run_cli` always exits 0 for an `Ok` report.
    pub fn with_doctor_readiness(mut self, f: ReadinessFn) -> Self {
        self.doctor_readiness = Some(f);
        self
    }
    /// Applies `f` to every error the dispatcher returns (window frames and `run_cli`), so an
    /// app that must not echo secret input values passes its redaction once, here.
    pub fn with_error_redactor(mut self, f: ErrorRedactor) -> Self {
        self.error_redactor = Some(f);
        self
    }
    /// The dry-run flag one request runs under: the process flag, or the request's own
    /// `params.dry_run` when [`Self::with_request_dry_run`] is on.
    fn effective_dry_run(&self, dry_run: bool, params: &Value) -> bool {
        dry_run
            || (self.request_dry_run
                && params.get("dry_run").and_then(Value::as_bool) == Some(true))
    }
    /// CLI: the positional word after a method whose first segment is
    /// `prefix` fills `key` (defaults: `job` -> `job_id`, `artifact` -> `artifact_id`).
    pub fn with_cli_target(mut self, prefix: impl Into<String>, key: impl Into<String>) -> Self {
        self.cli_targets.push((prefix.into(), key.into()));
        self
    }
    /// Whether response `error` objects carry `retryable` (default `true`).
    /// `false` serves `{code, message}` for an app with that wire shape.
    pub fn with_error_retryable(mut self, yes: bool) -> Self {
        self.error_retryable = yes;
        self
    }
    /// Bound how long [`serve`] waits for running long requests after the connection ended (EOF
    /// or `shutdown`): the connection-closed and shutdown hooks run first, then `serve` waits at
    /// most `timeout` for the request threads and returns, leaving any still running detached
    /// ([`serve_report`] says how many). Default: wait for all of them, however long. A detached
    /// request keeps running; cancellation stays cooperative and is not proof it stopped.
    pub fn with_drain_timeout(mut self, timeout: Duration) -> Self {
        self.drain_timeout = Some(timeout);
        self
    }
    /// Serve `alias` as another name of `method`, on the window frames and on the CLI alike (the
    /// CLI joins words with `.`, so `alias` is the dotted form). The command table stays one:
    /// the alias is not a second method, `methods()` does not list it, and a name that a command
    /// set or built-in already serves is never shadowed by an alias. CodeRight's registry aliases map here.
    pub fn with_alias(mut self, alias: impl Into<String>, method: impl Into<String>) -> Self {
        self.aliases.push((alias.into(), method.into()));
        self
    }
    /// Attach app-owned metadata (help text, categories, flags) to a method. It is served in the
    /// built-in `doctor` payload under `method_meta` and read with [`Self::method_meta`]; the
    /// dispatcher does not interpret it. Ember's command metadata maps here.
    pub fn with_method_meta(mut self, method: impl Into<String>, meta: Value) -> Self {
        self.method_meta.push((method.into(), meta));
        self
    }
    /// Metadata attached to `method` by [`Self::with_method_meta`] (the latest, when set twice).
    pub fn method_meta(&self, method: &str) -> Option<&Value> {
        self.method_meta
            .iter()
            .rev()
            .find(|(m, _)| m == method)
            .map(|(_, v)| v)
    }
    /// `(alias, method)` pairs, in registration order.
    pub fn aliases(&self) -> &[(String, String)] {
        &self.aliases
    }
    /// The served method `name` stands for: itself when it is served, else the method an alias
    /// names when that is served.
    fn canonical(&self, name: &str) -> Option<String> {
        if self.methods().iter().any(|m| m == name) {
            return Some(name.to_string());
        }
        let (_, target) = self.aliases.iter().find(|(a, _)| a == name)?;
        self.methods()
            .iter()
            .any(|m| m == target)
            .then(|| target.clone())
    }
    fn has_builtin(&self, m: &str) -> bool {
        self.builtins.contains(&m)
    }
    pub fn with(mut self, set: Arc<dyn CommandSet>) -> Self {
        self.sets.push(set);
        self
    }
    /// Largest frame read or written; bounded to `MAX_FRAME_LIMIT` (`usize::MAX` gives the ceiling).
    pub fn with_frame_limit(mut self, limit: usize) -> Self {
        self.frame_limit = finite_frame_limit(limit);
        self
    }
    /// The frame limit this dispatcher serves with.
    pub fn frame_limit(&self) -> usize {
        self.frame_limit
    }
    /// Extra top-level fields for the hello frame (an engine instance id, build info). `type`,
    /// `engine`, `version` and `contract_version` are the dispatcher's own and are not
    /// overridden; a later call with the same key replaces the earlier value.
    pub fn with_hello_fields(
        mut self,
        fields: impl IntoIterator<Item = (impl Into<String>, Value)>,
    ) -> Self {
        for (k, v) in fields {
            let k = k.into();
            if matches!(
                k.as_str(),
                "type" | "engine" | "version" | "contract_version"
            ) {
                continue;
            }
            match self.hello_fields.iter_mut().find(|(have, _)| *have == k) {
                Some(slot) => slot.1 = v,
                None => self.hello_fields.push((k, v)),
            }
        }
        self
    }
    pub fn hello(&self) -> Value {
        let mut hello = Frame::Hello {
            engine: self.engine.clone(),
            version: self.version.clone(),
            contract_version: CONTRACT_VERSION,
        }
        .to_value(true);
        for (k, v) in &self.hello_fields {
            hello[k.as_str()] = v.clone();
        }
        hello
    }
    /// Served method names. A built-in that a command set also registers is
    /// listed once and served by the command set.
    pub fn methods(&self) -> Vec<String> {
        let mut m: Vec<String> = Vec::new();
        for b in &self.builtins {
            if self.set_for(b).is_none() {
                m.push(b.to_string());
            }
        }
        for s in &self.sets {
            m.extend(s.methods());
        }
        m
    }
    fn set_for(&self, method: &str) -> Option<&Arc<dyn CommandSet>> {
        self.sets
            .iter()
            .find(|s| s.methods().iter().any(|m| m == method))
    }
    fn is_long(&self, method: &str, params: &Value) -> bool {
        let canonical = self.canonical(method);
        let method = canonical.as_deref().unwrap_or(method);
        self.set_for(method)
            .is_some_and(|s| s.is_long(method, params))
    }
    fn shutdown_all(&self) {
        for s in &self.sets {
            s.shutdown();
        }
    }

    /// Run one method (shared by both transports).
    pub fn call(
        &self,
        method: &str,
        params: &Value,
        dry_run: bool,
        emit: &Sink,
    ) -> JobResult<Value> {
        self.call_conn(0, method, params, dry_run, emit)
    }

    /// A client connection closed: let command sets cancel what was tied to it.
    pub fn connection_closed(&self, conn: u64) {
        for s in &self.sets {
            s.connection_closed(conn);
        }
    }

    /// Run one method on behalf of connection `conn`.
    ///
    /// A command set that registers a built-in name (`doctor`, `ping`,
    /// `shutdown`) takes precedence over the built-in; the built-in answers
    /// only when no set provides the method. A panic in a command set (or a
    /// `with_doctor` callback) is caught and returned as an `internal` error,
    /// so every request still gets exactly one response.
    pub fn call_conn(
        &self,
        conn: u64,
        method: &str,
        params: &Value,
        dry_run: bool,
        emit: &Sink,
    ) -> JobResult<Value> {
        let dry_run = self.effective_dry_run(dry_run, params);
        let depth = self
            .active
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        self.active_peak
            .fetch_max(depth, std::sync::atomic::Ordering::SeqCst);
        let _active = ActiveGuard(&self.active);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.call_inner(conn, method, params, dry_run, emit)
        }))
        .unwrap_or_else(|p| Err(panic_error(method, p.as_ref())));
        match (&self.error_redactor, result) {
            (Some(redact), Err(e)) => Err(redact(method, params, e)),
            (_, result) => result,
        }
    }

    fn call_inner(
        &self,
        conn: u64,
        method: &str,
        params: &Value,
        dry_run: bool,
        emit: &Sink,
    ) -> JobResult<Value> {
        let canonical = self.canonical(method);
        let method = canonical.as_deref().unwrap_or(method);
        if let Some(s) = self.set_for(method) {
            return s.call_conn(conn, method, params, dry_run, emit);
        }
        match method {
            "ping" if self.has_builtin("ping") => Ok(json!({"pong": true})),
            "shutdown" if self.has_builtin("shutdown") => Ok(json!({})),
            "doctor" if self.has_builtin("doctor") => Ok(match &self.doctor {
                Some(f) => f(dry_run, &self.methods()),
                None => {
                    let mut doctor = json!({"engine": self.engine, "version": self.version,
                    "contract_version": CONTRACT_VERSION, "methods": self.methods(), "dry_run": dry_run});
                    if !self.method_meta.is_empty() {
                        let meta: serde_json::Map<String, Value> = self
                            .method_meta
                            .iter()
                            .map(|(m, v)| (m.clone(), v.clone()))
                            .collect();
                        doctor["method_meta"] = Value::Object(meta);
                    }
                    doctor
                }
            }),
            m => Err(JobError::new(
                "unknown_method",
                format!("unknown method: {m}"),
            )),
        }
    }

    /// Handle one request line; events go to `emit` as event frames.
    pub fn handle_line(&self, line: &str, dry_run: bool, emit: &Sink) -> Outcome {
        let parsed: Result<Value, _> = serde_json::from_str(line);
        match parsed {
            Ok(v) if v["type"] == "request" && v["id"].is_u64() && v["method"].is_string() => {
                self.handle_request(&v, dry_run, emit)
            }
            _ => Outcome {
                response: err(
                    0,
                    "bad_request",
                    "expected {type:request,id:u64,method:string}",
                ),
                shutdown: false,
            },
        }
    }

    fn handle_request(&self, req: &Value, dry_run: bool, emit: &Sink) -> Outcome {
        let id = req["id"].as_u64().unwrap_or(0);
        let method = req["method"].as_str().unwrap_or("");
        let frames: Sink = {
            let emit = Arc::clone(emit);
            Arc::new(move |ev| emit(Frame::event(Some(id), ev).to_value(true)))
        };
        let params = req.get("params").cloned().unwrap_or_else(|| json!({}));
        let response = match self.call(method, &params, dry_run, &frames) {
            Ok(v) => ok(id, v),
            Err(e) => err_job(id, &e, self.error_retryable),
        };
        Outcome {
            response,
            shutdown: self.canonical(method).as_deref() == Some("shutdown")
                && self.has_builtin("shutdown"),
        }
    }
}

/// What [`serve_report`] saw when the connection ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ServeReport {
    /// Long requests still running when the bounded drain ([`Dispatcher::with_drain_timeout`])
    /// passed; they were detached, not stopped. Always 0 without a drain timeout.
    pub detached: usize,
}

/// Serve frames until `shutdown` or EOF. Long requests run on threads; all are
/// joined before returning, unless the dispatcher has a drain timeout.
pub fn serve<R, W>(d: Arc<Dispatcher>, reader: R, writer: W, dry_run: bool) -> io::Result<()>
where
    R: BufRead,
    W: Write + Send + 'static,
{
    serve_report(d, reader, writer, dry_run).map(|_| ())
}

/// As [`serve`], reporting how many long requests were left running by a bounded drain.
///
/// Finished long-request threads are joined as the loop admits frames: after each frame is read,
/// every handle whose thread [`JoinHandle::is_finished`] is joined and the rest are kept, without
/// ever waiting on a running one. A resident connection therefore does not accumulate one handle
/// per completed request. The bound is not zero: the loop reaps only when a frame arrives, so
/// while it is blocked on the reader the handles retained are at most the long requests still
/// running plus those admitted since the last reap (a finished one stays until the next frame is
/// read, or until the connection ends).
pub fn serve_report<R, W>(
    d: Arc<Dispatcher>,
    mut reader: R,
    writer: W,
    dry_run: bool,
) -> io::Result<ServeReport>
where
    R: BufRead,
    W: Write + Send + 'static,
{
    let out = Arc::new(Mutex::new(writer));
    let limit = d.frame_limit;
    let emit: Sink = {
        let out = Arc::clone(&out);
        Arc::new(move |v: Value| {
            let mut w = out.lock().unwrap_or_else(|p| p.into_inner());
            let r = write_json_line(&mut *w, &v, limit).map_err(|e| e.to_string());
            if let Err(msg) = r {
                // An oversized response must still answer its request.
                if let Some(id) = v["id"].as_u64().filter(|_| v["type"] == "response") {
                    let _ = write_json_line(&mut *w, &err(id, "response_too_large", &msg), limit);
                }
            }
            let _ = w.flush();
        })
    };
    emit(d.hello());
    // After hello, so an unsolicited event never precedes it; released when this returns.
    let _emitter = d.emitter.bind(Arc::clone(&emit), limit);
    let mut buf = Vec::new();
    let mut threads: Vec<JoinHandle<()>> = Vec::new();
    let running = Arc::new((Mutex::new(0usize), std::sync::Condvar::new()));
    loop {
        let frame = read_json_line_or_oversized::<_, Value>(
            &mut reader,
            &mut buf,
            limit,
            ReadLimits::default(),
        );
        // Bound retained handles by active work: join what already finished, never a running one.
        reap_finished(&mut threads);
        note_retained(threads.len());
        match frame {
            Ok(None) => break,
            // An oversized request is answered under its own id (read from the frame head) so the
            // caller fails fast instead of waiting out its timeout; the reader is back on a line
            // boundary and the loop carries on.
            Ok(Some(Incoming::Oversized(over))) => emit(err(
                over.id().unwrap_or(0),
                "frame_too_large",
                &format!(
                    "frame of {} bytes exceeded {} bytes",
                    over.bytes, over.limit
                ),
            )),
            Ok(Some(Incoming::Frame(req))) => {
                let long = req["type"] == "request"
                    && req["method"]
                        .as_str()
                        .is_some_and(|m| d.is_long(m, &req["params"]));
                if long {
                    let (d, emit) = (Arc::clone(&d), Arc::clone(&emit));
                    *running.0.lock().unwrap_or_else(|p| p.into_inner()) += 1;
                    let done = RunningGuard(Arc::clone(&running));
                    threads.push(std::thread::spawn(move || {
                        let _done = done;
                        // call_conn already converts handler panics; this also
                        // covers anything else on the thread so the request is answered.
                        let id = req["id"].as_u64().unwrap_or(0);
                        let method = req["method"].as_str().unwrap_or("").to_string();
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            d.handle_request_value(&req, dry_run, &emit).response
                        }))
                        .unwrap_or_else(|p| {
                            err_job(id, &panic_error(&method, p.as_ref()), d.error_retryable)
                        });
                        emit(r)
                    }));
                    note_retained(threads.len());
                    continue;
                }
                let o = d.handle_request_value(&req, dry_run, &emit);
                emit(o.response);
                if o.shutdown {
                    break;
                }
            }
            Err(FrameError::Io(_)) => break,
            Err(FrameError::TooLarge { limit }) => emit(err(
                0,
                "frame_too_large",
                &format!("frame exceeded {limit} bytes"),
            )),
            Err(FrameError::Json(e)) => {
                // The rejected line may hold secrets; wipe it before the buffer is reused.
                buf.iter_mut().for_each(|b| *b = 0);
                emit(err(0, "bad_request", &format!("bad frame: {e}")))
            }
            Err(FrameError::InvalidUtf8 { len }) => {
                buf.iter_mut().for_each(|b| *b = 0);
                emit(err(
                    0,
                    "bad_request",
                    &format!("bad frame: {len} bytes are not valid UTF-8"),
                ))
            }
            // The blocking reader never times out; a deadline reader ends the session.
            Err(FrameError::TimedOut { .. }) => break,
            // Only produced when a reader opts out of skipping empty lines; this loop does not.
            Err(FrameError::EmptyLine) => emit(err(0, "bad_request", "bad frame: empty line")),
            // Write-side errors; the reader never produces them, but a frame error is a bad frame.
            Err(e @ (FrameError::EmbeddedNewline | FrameError::MissingTerminator)) => {
                emit(err(0, "bad_request", &format!("bad frame: {e}")))
            }
        }
    }
    // The client is gone (EOF) or asked to shut down: cancel jobs tied to this connection first.
    d.connection_closed(0);
    d.shutdown_all();
    let Some(limit) = d.drain_timeout else {
        for t in threads {
            let _ = t.join();
        }
        return Ok(ServeReport::default());
    };
    let deadline = Instant::now() + limit;
    let (count, cv) = &*running;
    let mut n = count.lock().unwrap_or_else(|p| p.into_inner());
    while *n > 0 {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        n = cv
            .wait_timeout(n, left)
            .unwrap_or_else(|p| p.into_inner())
            .0;
    }
    let detached = *n;
    drop(n);
    // A count of 0 means every `RunningGuard` dropped, not that every thread has exited, and a
    // running request never finishes by the deadline. Join only handles that report finished
    // (re-polled with a short sleep, never past the deadline); the rest drop here, detached.
    loop {
        reap_finished(&mut threads);
        let left = deadline.saturating_duration_since(Instant::now());
        if threads.is_empty() || left.is_zero() {
            break;
        }
        std::thread::sleep(left.min(Duration::from_millis(2)));
    }
    Ok(ServeReport { detached })
}

/// Join every long-request thread that has finished and keep the rest. Never waits on a thread
/// that is still running, so it is safe to call while admitting frames.
fn reap_finished(threads: &mut Vec<JoinHandle<()>>) {
    let mut i = 0;
    while i < threads.len() {
        if threads[i].is_finished() {
            let _ = threads.swap_remove(i).join();
        } else {
            i += 1;
        }
    }
}

#[inline(always)]
fn note_retained(_: usize) {}

/// Counts a running long request; decrements (and wakes the drain) when the thread ends, however
/// it ends.
struct RunningGuard(Arc<(Mutex<usize>, std::sync::Condvar)>);

impl Drop for RunningGuard {
    fn drop(&mut self) {
        let mut n = self.0 .0.lock().unwrap_or_else(|p| p.into_inner());
        *n = n.saturating_sub(1);
        drop(n);
        self.0 .1.notify_all();
    }
}

impl Dispatcher {
    fn handle_request_value(&self, v: &Value, dry_run: bool, emit: &Sink) -> Outcome {
        if v["type"] == "request" && v["id"].is_u64() && v["method"].is_string() {
            self.handle_request(v, dry_run, emit)
        } else {
            Outcome {
                response: err(
                    0,
                    "bad_request",
                    "expected {type:request,id:u64,method:string}",
                ),
                shutdown: false,
            }
        }
    }
}

// ---------------------------------------------------------------- CLI ----

#[derive(Debug, Default, PartialEq)]
pub struct Flags {
    pub dry_run: bool,
    pub input: Option<String>,
    pub wait: bool,
    pub request_id: Option<String>,
    pub wait_timeout: Option<String>,
    pub words: Vec<String>,
}

pub fn parse_flags(args: impl Iterator<Item = String>) -> Flags {
    let mut f = Flags::default();
    let mut args = args;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dry-run" => f.dry_run = true,
            "--input" => f.input = args.next(),
            "--wait" => f.wait = true,
            "--request-id" => f.request_id = args.next(),
            "--wait-timeout" => f.wait_timeout = args.next(),
            _ => f.words.push(a),
        }
    }
    f
}

fn envelope(code: &str, message: &str) -> Value {
    json!({"error": {"code": code, "message": message}})
}

fn print(out: &mut dyn Write, v: &Value) {
    let text = serde_json::to_string_pretty(v)
        .unwrap_or_else(|_| r#"{"error":{"code":"encode","message":"json encode failed"}}"#.into());
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

/// Run one CLI invocation. `stdin` is read only for `--input -` (an unclosed
/// pipe attached by a caller must never block a plain command). Returns the exit code.
pub fn run_cli(
    d: &Dispatcher,
    args: impl Iterator<Item = String>,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
    err_out: &mut (dyn Write + Send),
) -> i32 {
    let f = parse_flags(args);
    if matches!(
        f.words.first().map(String::as_str),
        Some("--version" | "-V")
    ) {
        print(out, &json!({"version": d.version}));
        return 0;
    }
    if matches!(
        f.words.first().map(String::as_str),
        Some("--help" | "-h" | "help")
    ) {
        write_usage(d, out);
        return 0;
    }
    if let (true, Some(last)) = (f.words.len() > 1, f.words.last()) {
        if matches!(last.as_str(), "--help" | "-h") {
            let words = &f.words[..f.words.len() - 1];
            let found = (1..=words.len())
                .rev()
                .find_map(|k| d.canonical(&words[..k].join(".")));
            if let Some(method) = found {
                write_method_help(d, &method, out);
                return 0;
            }
        }
    }
    let Some((method, rest)) = (1..=f.words.len()).rev().find_map(|k| {
        let m = f.words[..k].join(".");
        d.canonical(&m).map(|m| (m, f.words[k..].to_vec()))
    }) else {
        print(
            out,
            &envelope(
                "unknown_command",
                &format!("unknown command: {}", f.words.join(" ")),
            ),
        );
        return 2;
    };
    let raw = match f.input.as_deref() {
        Some("-") => {
            let mut s = String::new();
            if let Err(e) = stdin.read_to_string(&mut s) {
                print(out, &envelope("bad_input", &format!("read stdin: {e}")));
                return 2;
            }
            Some(s)
        }
        _ => f.input.clone(),
    };
    let mut input: Value = match raw.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => json!({}),
        Some(t) => match serde_json::from_str(t) {
            Ok(v) => v,
            Err(e) => {
                print(
                    out,
                    &envelope("bad_input", &format!("input is not JSON: {e}")),
                );
                return 2;
            }
        },
    };
    if !input.is_object() {
        print(out, &envelope("bad_input", "input must be a JSON object"));
        return 2;
    }
    if let Some(target) = rest.first() {
        let prefix = method.split('.').next().unwrap_or("");
        let key = d
            .cli_targets
            .iter()
            .find(|(p, _)| p == prefix)
            .map(|(_, k)| k.as_str())
            .unwrap_or(match prefix {
                "job" => "job_id",
                "artifact" => "artifact_id",
                _ => "target",
            });
        input[key] = json!(target);
    }
    if f.wait {
        input["wait"] = json!(true);
    }
    if let Some(id) = &f.request_id {
        input["request_id"] = json!(id);
    }
    if let Some(raw) = &f.wait_timeout {
        match raw.trim().parse::<f64>() {
            Ok(s) if s.is_finite() && s >= 0.0 => input["wait_timeout_s"] = json!(s),
            _ => {
                print(
                    out,
                    &envelope(
                        "bad_input",
                        "--wait-timeout takes a non-negative number of seconds",
                    ),
                );
                return 2;
            }
        }
    }
    // Events print to stderr as NDJSON (live, via a scoped drain thread), never
    // into the stdout result.
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    let tx = Mutex::new(tx);
    let emit: Sink = Arc::new(move |v| {
        let _ = tx.lock().unwrap_or_else(|p| p.into_inner()).send(v);
    });
    let result = std::thread::scope(|s| {
        s.spawn(move || {
            for v in rx {
                let _ = writeln!(err_out, "{v}");
            }
        });
        let r = d.call(&method, &input, f.dry_run, &emit);
        drop(emit); // closes the channel so the drain thread ends
        r
    });
    d.shutdown_all();
    match result {
        Ok(v) => {
            print(out, &v);
            let not_ready =
                method == "doctor" && d.doctor_readiness.as_ref().is_some_and(|ready| !ready(&v));
            i32::from(not_ready)
        }
        Err(e) => {
            print(out, &envelope(&e.code, &e.message));
            1
        }
    }
}

/// Plain-text usage for `run_cli --help`: the engine, its version, and the canonical methods.
fn write_usage(d: &Dispatcher, out: &mut dyn Write) {
    let _ = writeln!(out, "{} {}", d.engine, d.version);
    let _ = writeln!(out, "usage: {} <method words> [--input <json>|-] [--dry-run] [--wait] [--wait-timeout <s>] [--request-id <id>]", d.engine);
    let _ = writeln!(
        out,
        "       {} <method words> --help   (method help)",
        d.engine
    );
    let _ = writeln!(out, "methods:");
    for m in d.methods() {
        match d.method_meta(&m).and_then(method_help_line) {
            Some(line) => {
                let _ = writeln!(out, "  {m}  {line}");
            }
            None => {
                let _ = writeln!(out, "  {m}");
            }
        }
    }
}

/// `run_cli <method> --help`: the method's meta, or its name when none is set.
fn write_method_help(d: &Dispatcher, method: &str, out: &mut dyn Write) {
    let _ = writeln!(out, "{method}");
    if let Some(meta) = d.method_meta(method) {
        match method_help_line(meta) {
            Some(line) => {
                let _ = writeln!(out, "{line}");
            }
            None => print(out, meta),
        }
    }
}

/// The one-line help of a method's meta: a JSON string, or its `help` string field.
fn method_help_line(meta: &Value) -> Option<String> {
    meta.as_str()
        .or_else(|| meta.get("help").and_then(Value::as_str))
        .map(str::to_string)
}
