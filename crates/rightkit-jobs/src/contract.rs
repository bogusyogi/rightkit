//! Engine command contract: one dispatcher serves both transports.
//!
//! stdio (`serve`): JSON-lines frames (`rightkit-framed-sidecar`).
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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rightkit_framed_sidecar::{read_json_line, write_json_line, FrameError};
use serde_json::{json, Value};

use crate::config::Ext;
use crate::error::{JobError, JobResult};
use crate::runner::{EventSink, RunOutcome, Runner};
use crate::store::{JobStore, ListFilter, NewJob};

pub use crate::wire::{finite_frame_limit, CONTRACT_VERSION, DEFAULT_FRAME_LIMIT};

/// Receives event values emitted while a command runs.
pub type Sink = Arc<dyn Fn(Value) + Send + Sync>;

/// A group of commands. Apps implement this for product commands and add
/// [`JobCommands`] for the shared `job.*` surface.
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
}

pub struct Outcome {
    pub response: Value,
    pub shutdown: bool,
}

fn ok(id: u64, result: Value) -> Value {
    json!({"type": "response", "id": id, "ok": true, "result": result})
}
fn err(id: u64, code: &str, message: &str) -> Value {
    json!({"type": "response", "id": id, "ok": false, "error": {"code": code, "message": message, "retryable": false}})
}
fn err_job(id: u64, e: &JobError, retryable: bool) -> Value {
    json!({"type": "response", "id": id, "ok": false, "error": e.to_json_with(retryable)})
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
        }
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
    pub fn hello(&self) -> Value {
        json!({"type": "hello", "engine": self.engine, "version": self.version,
               "contract_version": CONTRACT_VERSION})
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
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.call_inner(conn, method, params, dry_run, emit)
        }))
        .unwrap_or_else(|p| Err(panic_error(method, p.as_ref())))
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
            Arc::new(move |ev| emit(json!({"type": "event", "id": id, "event": ev})))
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
    let mut buf = Vec::new();
    let mut threads = Vec::new();
    let running = Arc::new((Mutex::new(0usize), std::sync::Condvar::new()));
    loop {
        match read_json_line::<_, Value>(&mut reader, &mut buf, limit) {
            Ok(None) => break,
            Ok(Some(req)) => {
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
            Err(FrameError::Json(e)) => emit(err(0, "bad_request", &format!("bad frame: {e}"))),
            Err(FrameError::InvalidUtf8 { len }) => emit(err(
                0,
                "bad_request",
                &format!("bad frame: {len} bytes are not valid UTF-8"),
            )),
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
    if detached == 0 {
        for t in threads {
            let _ = t.join();
        }
    }
    // Otherwise the handles drop here: the running requests are detached, finished ones are gone.
    Ok(ServeReport { detached })
}

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
            0
        }
        Err(e) => {
            print(out, &envelope(&e.code, &e.message));
            1
        }
    }
}

// ------------------------------------------------------- job.* commands ----

/// Maps the shared `job.*` payloads to an app's own wire contract. `method`
/// is always the canonical name (`job.submit`, `artifact.resolve`, ...), even
/// when [`JobCommands::with_name`] serves it under another name.
pub trait PayloadMapper: Send + Sync {
    /// Rewrite request params before the canonical command reads them.
    fn input(&self, _method: &str, params: Value) -> JobResult<Value> {
        Ok(params)
    }
    /// Rewrite the canonical result before it is returned.
    fn output(&self, _method: &str, result: Value) -> JobResult<Value> {
        Ok(result)
    }
    /// Rewrite each streamed progress event.
    fn event(&self, event: Value) -> Value {
        event
    }
    /// Rewrite an error (e.g. a different `code`).
    fn error(&self, _method: &str, e: JobError) -> JobError {
        e
    }
}

/// Canonical `job.*` method names served by [`JobCommands`].
pub const JOB_METHODS: [&str; 12] = [
    "job.submit",
    "job.status",
    "job.result",
    "job.list",
    "job.events",
    "job.cancel",
    "job.ack",
    "job.run",
    "job.recover",
    "job.recoverable",
    "job.wait",
    "artifact.resolve",
];

/// The shared `job.*` / `artifact.resolve` command surface over a [`Runner`].
/// Optional: an app that keeps its own contract either omits it from the
/// [`Dispatcher`] or renames/disables methods and maps payloads.
///
/// Passive: `job.status|list|events|result|recoverable|wait` and
/// `artifact.resolve` never run work. Explicit: `job.run`, `job.recover`,
/// `job.submit` with `run`/`wait`, `job.wait` with `resume`.
pub struct JobCommands {
    runner: Arc<Runner>,
    /// Jobs whose request set `cancel_on_disconnect`, by client connection.
    watched: Mutex<std::collections::HashMap<u64, Vec<String>>>,
    /// canonical -> wire name.
    names: std::collections::HashMap<String, String>,
    disabled: std::collections::HashSet<String>,
    mapper: Option<Arc<dyn PayloadMapper>>,
}

impl JobCommands {
    pub fn new(runner: Arc<Runner>) -> Self {
        Self {
            runner,
            watched: Mutex::new(Default::default()),
            names: Default::default(),
            disabled: Default::default(),
            mapper: None,
        }
    }
    /// Serve canonical `method` (one of [`JOB_METHODS`]) as `wire`, e.g.
    /// `artifact.resolve` as `asset.resolve`.
    pub fn with_name(mut self, method: &str, wire: impl Into<String>) -> Self {
        self.names.insert(method.to_string(), wire.into());
        self
    }
    /// Do not serve canonical `method`.
    pub fn without(mut self, method: &str) -> Self {
        self.disabled.insert(method.to_string());
        self
    }
    pub fn with_mapper(mut self, m: Arc<dyn PayloadMapper>) -> Self {
        self.mapper = Some(m);
        self
    }
    fn wire<'a>(&'a self, canonical: &'a str) -> &'a str {
        self.names.get(canonical).map_or(canonical, String::as_str)
    }
    /// Canonical name for a served wire method.
    pub fn canonical(&self, wire: &str) -> Option<&'static str> {
        JOB_METHODS
            .iter()
            .copied()
            .find(|c| !self.disabled.contains(*c) && self.wire(c) == wire)
    }
    fn map_event(&self, ev: Value) -> Value {
        match &self.mapper {
            Some(m) => m.event(ev),
            None => ev,
        }
    }
    fn store(&self) -> &JobStore {
        self.runner.store()
    }
    fn record_value(&self, job_id: &str) -> JobResult<Value> {
        Ok(self.store().record(job_id)?.to_value())
    }
    fn outcome_value(&self, o: &RunOutcome) -> JobResult<Value> {
        let Some(j) = o.job() else {
            return Ok(o.to_value());
        };
        let mut v = self.record_value(&j.job_id)?;
        if matches!(o, RunOutcome::Paused(_)) {
            v["recoverable"] = json!(true);
        }
        // The outcome is final but its terminal write is held (the stored row is not terminal yet).
        if matches!(o, RunOutcome::Unsettled(_)) {
            v["unsettled"] = json!(true);
        }
        Ok(v)
    }

    fn target(&self, p: &Value) -> JobResult<crate::store::Job> {
        let store = self.runner.store();
        match (p["job_id"].as_str(), p["request_id"].as_str()) {
            (Some(id), _) => store.get(id),
            (None, Some(rid)) => store.get_by_request(rid),
            _ => Err(JobError::bad_input("job_id or request_id required")),
        }
    }

    fn run(
        &self,
        id: &str,
        emit: &Sink,
        conn: u64,
        cancel_on_disconnect: bool,
    ) -> JobResult<Value> {
        let emit = Arc::clone(emit);
        let mapper = self.mapper.clone();
        let sink: EventSink = Arc::new(move |ev| {
            let v = ev.to_value();
            emit(match &mapper {
                Some(m) => m.event(v),
                None => v,
            })
        });
        if cancel_on_disconnect {
            self.watched
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .entry(conn)
                .or_default()
                .push(id.to_string());
        }
        let out = self.runner.run_with(id, Some(sink));
        if cancel_on_disconnect {
            if let Some(v) = self
                .watched
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get_mut(&conn)
            {
                v.retain(|j| j != id);
            }
        }
        match out? {
            // A handoff is an error the client must see so it can resubmit to the new host.
            RunOutcome::Paused(j)
                if j.error
                    .as_ref()
                    .is_some_and(|e| e["code"] == crate::error::HOST_HANDOFF) =>
            {
                Err(JobError::handoff())
            }
            o => self.outcome_value(&o),
        }
    }
}

impl CommandSet for JobCommands {
    fn methods(&self) -> Vec<String> {
        JOB_METHODS
            .iter()
            .filter(|c| !self.disabled.contains(**c))
            .map(|c| self.wire(c).to_string())
            .collect()
    }

    fn is_long(&self, method: &str, p: &Value) -> bool {
        let Some(method) = self.canonical(method) else {
            return false;
        };
        let mapped;
        let p = match &self.mapper {
            Some(m) => match m.input(method, p.clone()) {
                Ok(v) => {
                    mapped = v;
                    &mapped
                }
                Err(_) => p,
            },
            None => p,
        };
        match method {
            "job.run" | "job.recover" | "job.wait" => true,
            "job.submit" => p["run"] == true || p["wait"] == true,
            _ => false,
        }
    }

    fn call(&self, method: &str, p: &Value, dry_run: bool, emit: &Sink) -> JobResult<Value> {
        self.call_conn(0, method, p, dry_run, emit)
    }

    fn connection_closed(&self, conn: u64) {
        let ids = self
            .watched
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&conn)
            .unwrap_or_default();
        for id in ids {
            if let Ok(j) = self.runner.store().get(&id) {
                if !j.is_terminal() && self.runner.store().cancel(&id).is_ok() {
                    let _ = self.runner.store().add_event(
                        &id,
                        "cancelled",
                        None,
                        Some("client disconnected"),
                    );
                }
            }
        }
    }

    fn call_conn(
        &self,
        conn: u64,
        method: &str,
        p: &Value,
        dry_run: bool,
        emit: &Sink,
    ) -> JobResult<Value> {
        let Some(canonical) = self.canonical(method) else {
            return Err(JobError::new(
                "unknown_method",
                format!("unknown method: {method}"),
            ));
        };
        let Some(m) = self.mapper.clone() else {
            return self.call_canonical(conn, canonical, p, dry_run, emit);
        };
        let p = m
            .input(canonical, p.clone())
            .map_err(|e| m.error(canonical, e))?;
        self.call_canonical(conn, canonical, &p, dry_run, emit)
            .and_then(|v| m.output(canonical, v))
            .map_err(|e| m.error(canonical, e))
    }
}

impl JobCommands {
    fn call_canonical(
        &self,
        conn: u64,
        method: &str,
        p: &Value,
        dry_run: bool,
        emit: &Sink,
    ) -> JobResult<Value> {
        let store = self.runner.store();
        let cod = p["cancel_on_disconnect"] == true;
        match method {
            "job.submit" => {
                let kind = p["kind"]
                    .as_str()
                    .filter(|k| !k.is_empty())
                    .ok_or_else(|| JobError::bad_input("kind required"))?;
                let new = NewJob {
                    request_id: p["request_id"].as_str().map(Into::into),
                    kind: kind.into(),
                    request: p.get("request").cloned().unwrap_or_else(|| json!({})),
                };
                let ext: Ext = match p.get("ext") {
                    None | Some(Value::Null) => Ext::new(),
                    Some(Value::Object(o)) => o.clone(),
                    Some(_) => return Err(JobError::bad_input("ext must be an object")),
                };
                if dry_run {
                    return Ok(
                        json!({"dry_run": true, "kind": new.kind, "request_id": new.request_id}),
                    );
                }
                let (rec, dedup) = store.submit_ext(&new, &ext)?;
                let mut v = if p["run"] == true || p["wait"] == true {
                    self.run(&rec.job.job_id, emit, conn, cod)?
                } else {
                    rec.to_value()
                };
                v["deduplicated"] = json!(dedup);
                Ok(v)
            }
            "job.status" => self.record_value(&self.target(p)?.job_id),
            "job.result" => {
                let j = self.target(p)?;
                if j.status != crate::store::COMPLETE {
                    return Err(JobError::new(
                        "not_complete",
                        format!("job {} is {}", j.job_id, j.status),
                    ));
                }
                let arts = store
                    .artifacts(&j.job_id)?
                    .iter()
                    .map(|a| -> JobResult<Value> {
                        let mut v = store.artifact_value(a);
                        v["verified"] = json!(store.verify_artifact(a)?);
                        Ok(v)
                    })
                    .collect::<JobResult<Vec<_>>>()?;
                Ok(json!({"job_id": j.job_id, "result": j.result, "artifacts": arts}))
            }
            "job.list" => {
                let (jobs, truncated) = store.list_records(&ListFilter {
                    limit: p["limit"].as_i64(),
                    status: p["status"].as_str().map(Into::into),
                    request_id: p["request_id"].as_str().map(Into::into),
                })?;
                Ok(
                    json!({"jobs": jobs.iter().map(|j| j.to_value()).collect::<Vec<_>>(), "truncated": truncated}),
                )
            }
            "job.events" => {
                let j = self.target(p)?;
                let evs = store.events_after(&j.job_id, p["after"].as_i64().unwrap_or(0))?;
                Ok(
                    json!({"job_id": j.job_id, "events": evs.iter().map(|e| self.map_event(e.to_value())).collect::<Vec<_>>()}),
                )
            }
            "job.cancel" => {
                let id = self.target(p)?.job_id;
                store.cancel(&id)?;
                self.record_value(&id)
            }
            "job.ack" => {
                let who = p["consumer"].as_str().unwrap_or("cli");
                let id = self.target(p)?.job_id;
                store.ack(&id, who)?;
                self.record_value(&id)
            }
            "job.run" => self.run(&self.target(p)?.job_id, emit, conn, cod),
            "job.recoverable" => Ok(json!({"job_ids": self.runner.recoverable_ids()?})),
            "job.recover" => {
                let emit2 = Arc::clone(emit);
                let mapper = self.mapper.clone();
                let sink: EventSink = Arc::new(move |ev| {
                    let v = ev.to_value();
                    emit2(match &mapper {
                        Some(m) => m.event(v),
                        None => v,
                    })
                });
                let mut out = Vec::new();
                for id in self.runner.recoverable_ids()? {
                    let o = self.runner.run_with(&id, Some(sink.clone()))?;
                    out.push(self.outcome_value(&o)?);
                }
                Ok(json!({"jobs": out}))
            }
            "job.wait" => {
                let mut j = self.target(p)?;
                if p["resume"] == true && !j.is_terminal() {
                    if let RunOutcome::Finished(done) = self.runner.run_with(&j.job_id, None)? {
                        j = done;
                    }
                }
                let deadline = Instant::now()
                    + Duration::from_secs_f64(p["wait_timeout_s"].as_f64().unwrap_or(30.0));
                while !j.is_terminal() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(50));
                    j = store.get(&j.job_id)?;
                }
                let mut v = self.record_value(&j.job_id)?;
                v["timed_out"] = json!(!j.is_terminal());
                Ok(v)
            }
            "artifact.resolve" => {
                let wire = store.config().artifact_names().wire_id.clone();
                let id = p[wire.as_str()]
                    .as_str()
                    .or_else(|| p["artifact_id"].as_str())
                    .ok_or_else(|| JobError::bad_input(format!("{wire} required")))?;
                let a = store.get_artifact(id)?;
                let mut v = store.artifact_value(&a);
                v["verified"] = json!(store.verify_artifact(&a)?);
                Ok(v)
            }
            other => Err(JobError::new(
                "unknown_method",
                format!("unknown method: {other}"),
            )),
        }
    }
}
