//! Engine command contract: one dispatcher serves both transports.
//!
//! stdio (`serve`): JSON-lines frames (`rightkit-framed-sidecar`).
//!   out once: `{"type":"hello","engine","version","contract_version"}`
//!   in:  `{"type":"request","id":<u64>,"method":"..","params":{..}}`
//!   out: `{"type":"event","id":<request id>,"event":{..}}` zero or more, then exactly one
//!        `{"type":"response","id","ok":true,"result":{..}}` or
//!        `{"type":"response","id","ok":false,"error":{"code","message"}}`.
//! CLI (`run_cli`): `<words> [--input <json>|-] [--dry-run] [--wait]
//!   [--wait-timeout <s>] [--request-id <id>]`; pretty JSON result on stdout
//!   (exit 0) or `{"error":{"code","message"}}` (exit 1; 2 for usage); progress
//!   events are NDJSON lines on stderr.

use std::io::{self, BufRead, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rightkit_framed_sidecar::{read_json_line, write_json_line, FrameError};
use serde_json::{json, Value};

use crate::error::{JobError, JobResult};
use crate::runner::{EventSink, RunOutcome, Runner};
use crate::store::{ListFilter, NewJob};

/// Bumped on breaking changes to frames, built-ins, or `job.*` commands.
pub const CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_FRAME_LIMIT: usize = 16 * 1024 * 1024;

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
    /// Called once when the engine is shutting down or stdin closed.
    fn shutdown(&self) {}
}

pub struct Dispatcher {
    engine: String,
    version: String,
    sets: Vec<Arc<dyn CommandSet>>,
    frame_limit: usize,
}

pub struct Outcome {
    pub response: Value,
    pub shutdown: bool,
}

fn ok(id: u64, result: Value) -> Value {
    json!({"type": "response", "id": id, "ok": true, "result": result})
}
fn err(id: u64, code: &str, message: &str) -> Value {
    json!({"type": "response", "id": id, "ok": false, "error": {"code": code, "message": message}})
}

const BUILTINS: [&str; 3] = ["ping", "doctor", "shutdown"];

impl Dispatcher {
    pub fn new(engine: impl Into<String>, version: impl Into<String>) -> Self {
        Self { engine: engine.into(), version: version.into(), sets: vec![], frame_limit: DEFAULT_FRAME_LIMIT }
    }
    pub fn with(mut self, set: Arc<dyn CommandSet>) -> Self {
        self.sets.push(set);
        self
    }
    pub fn with_frame_limit(mut self, limit: usize) -> Self {
        self.frame_limit = limit;
        self
    }
    pub fn hello(&self) -> Value {
        json!({"type": "hello", "engine": self.engine, "version": self.version,
               "contract_version": CONTRACT_VERSION})
    }
    pub fn methods(&self) -> Vec<String> {
        let mut m: Vec<String> = BUILTINS.iter().map(|s| s.to_string()).collect();
        for s in &self.sets {
            m.extend(s.methods());
        }
        m
    }
    fn set_for(&self, method: &str) -> Option<&Arc<dyn CommandSet>> {
        self.sets.iter().find(|s| s.methods().iter().any(|m| m == method))
    }
    fn is_long(&self, method: &str, params: &Value) -> bool {
        self.set_for(method).is_some_and(|s| s.is_long(method, params))
    }
    fn shutdown_all(&self) {
        for s in &self.sets {
            s.shutdown();
        }
    }

    /// Run one method (shared by both transports).
    pub fn call(&self, method: &str, params: &Value, dry_run: bool, emit: &Sink) -> JobResult<Value> {
        match method {
            "ping" => Ok(json!({"pong": true})),
            "shutdown" => Ok(json!({})),
            "doctor" => Ok(json!({"engine": self.engine, "version": self.version,
                "contract_version": CONTRACT_VERSION, "methods": self.methods(), "dry_run": dry_run})),
            m => match self.set_for(m) {
                Some(s) => s.call(m, params, dry_run, emit),
                None => Err(JobError::new("unknown_method", format!("unknown method: {m}"))),
            },
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
                response: err(0, "bad_request", "expected {type:request,id:u64,method:string}"),
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
            Err(e) => err(id, &e.code, &e.message),
        };
        Outcome { response, shutdown: method == "shutdown" }
    }
}

/// Serve frames until `shutdown` or EOF. Long requests run on threads; all are
/// joined before returning.
pub fn serve<R, W>(d: Arc<Dispatcher>, mut reader: R, writer: W, dry_run: bool) -> io::Result<()>
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
    loop {
        match read_json_line::<_, Value>(&mut reader, &mut buf, limit) {
            Ok(None) => break,
            Ok(Some(req)) => {
                let long = req["type"] == "request"
                    && req["method"].as_str().is_some_and(|m| d.is_long(m, &req["params"]));
                if long {
                    let (d, emit) = (Arc::clone(&d), Arc::clone(&emit));
                    threads.push(std::thread::spawn(move || {
                        emit(d.handle_request_value(&req, dry_run, &emit).response)
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
            Err(FrameError::TooLarge { limit }) => {
                emit(err(0, "frame_too_large", &format!("frame exceeded {limit} bytes")))
            }
            Err(FrameError::Json(e)) => emit(err(0, "bad_request", &format!("bad frame: {e}"))),
        }
    }
    d.shutdown_all();
    for t in threads {
        let _ = t.join();
    }
    Ok(())
}

impl Dispatcher {
    fn handle_request_value(&self, v: &Value, dry_run: bool, emit: &Sink) -> Outcome {
        if v["type"] == "request" && v["id"].is_u64() && v["method"].is_string() {
            self.handle_request(v, dry_run, emit)
        } else {
            Outcome { response: err(0, "bad_request", "expected {type:request,id:u64,method:string}"), shutdown: false }
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
    if matches!(f.words.first().map(String::as_str), Some("--version" | "-V")) {
        print(out, &json!({"version": d.version}));
        return 0;
    }
    let methods = d.methods();
    let Some((method, rest)) = (1..=f.words.len()).rev().find_map(|k| {
        let m = f.words[..k].join(".");
        methods.contains(&m).then(|| (m, f.words[k..].to_vec()))
    }) else {
        print(out, &envelope("unknown_command", &format!("unknown command: {}", f.words.join(" "))));
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
                print(out, &envelope("bad_input", &format!("input is not JSON: {e}")));
                return 2;
            }
        },
    };
    if !input.is_object() {
        print(out, &envelope("bad_input", "input must be a JSON object"));
        return 2;
    }
    if let Some(target) = rest.first() {
        let key = match method.split('.').next() {
            Some("job") => "job_id",
            Some("artifact") => "artifact_id",
            _ => "target",
        };
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
                print(out, &envelope("bad_input", "--wait-timeout takes a non-negative number of seconds"));
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

/// The shared `job.*` / `artifact.resolve` command surface over a [`Runner`].
///
/// Passive: `job.status|list|events|result|recoverable|wait` and
/// `artifact.resolve` never run work. Explicit: `job.run`, `job.recover`,
/// `job.submit` with `run`/`wait`, `job.wait` with `resume`.
pub struct JobCommands {
    runner: Arc<Runner>,
}

impl JobCommands {
    pub fn new(runner: Arc<Runner>) -> Self {
        Self { runner }
    }

    fn target(&self, p: &Value) -> JobResult<crate::store::Job> {
        let store = self.runner.store();
        match (p["job_id"].as_str(), p["request_id"].as_str()) {
            (Some(id), _) => store.get(id),
            (None, Some(rid)) => store.get_by_request(rid),
            _ => Err(JobError::bad_input("job_id or request_id required")),
        }
    }

    fn run(&self, id: &str, emit: &Sink) -> JobResult<Value> {
        let emit = Arc::clone(emit);
        let sink: EventSink = Arc::new(move |ev| emit(ev.to_value()));
        Ok(self.runner.run_with(id, Some(sink))?.to_value())
    }
}

impl CommandSet for JobCommands {
    fn methods(&self) -> Vec<String> {
        ["job.submit", "job.status", "job.result", "job.list", "job.events", "job.cancel",
         "job.ack", "job.run", "job.recover", "job.recoverable", "job.wait", "artifact.resolve"]
            .iter().map(|s| s.to_string()).collect()
    }

    fn is_long(&self, method: &str, p: &Value) -> bool {
        match method {
            "job.run" | "job.recover" | "job.wait" => true,
            "job.submit" => p["run"] == true || p["wait"] == true,
            _ => false,
        }
    }

    fn call(&self, method: &str, p: &Value, dry_run: bool, emit: &Sink) -> JobResult<Value> {
        let store = self.runner.store();
        match method {
            "job.submit" => {
                let kind = p["kind"].as_str().filter(|k| !k.is_empty())
                    .ok_or_else(|| JobError::bad_input("kind required"))?;
                let new = NewJob {
                    request_id: p["request_id"].as_str().map(Into::into),
                    kind: kind.into(),
                    request: p.get("request").cloned().unwrap_or_else(|| json!({})),
                };
                if dry_run {
                    return Ok(json!({"dry_run": true, "kind": new.kind, "request_id": new.request_id}));
                }
                let (job, dedup) = store.submit(&new)?;
                let mut v = if p["run"] == true || p["wait"] == true {
                    self.run(&job.job_id, emit)?
                } else {
                    job.to_value()
                };
                v["deduplicated"] = json!(dedup);
                Ok(v)
            }
            "job.status" => Ok(self.target(p)?.to_value()),
            "job.result" => {
                let j = self.target(p)?;
                if j.status != crate::store::COMPLETE {
                    return Err(JobError::new("not_complete", format!("job {} is {}", j.job_id, j.status)));
                }
                let arts = store.artifacts(&j.job_id)?
                    .iter()
                    .map(|a| -> JobResult<Value> {
                        let mut v = a.to_value();
                        v["verified"] = json!(store.verify_artifact(a)?);
                        Ok(v)
                    })
                    .collect::<JobResult<Vec<_>>>()?;
                Ok(json!({"job_id": j.job_id, "result": j.result, "artifacts": arts}))
            }
            "job.list" => {
                let (jobs, truncated) = store.list(&ListFilter {
                    limit: p["limit"].as_i64(),
                    status: p["status"].as_str().map(Into::into),
                    request_id: p["request_id"].as_str().map(Into::into),
                })?;
                Ok(json!({"jobs": jobs.iter().map(|j| j.to_value()).collect::<Vec<_>>(), "truncated": truncated}))
            }
            "job.events" => {
                let j = self.target(p)?;
                let evs = store.events_after(&j.job_id, p["after"].as_i64().unwrap_or(0))?;
                Ok(json!({"job_id": j.job_id, "events": evs.iter().map(|e| e.to_value()).collect::<Vec<_>>()}))
            }
            "job.cancel" => Ok(store.cancel(self.target(p)?.job_id.as_str())?.to_value()),
            "job.ack" => {
                let who = p["consumer"].as_str().unwrap_or("cli");
                Ok(store.ack(&self.target(p)?.job_id, who)?.to_value())
            }
            "job.run" => self.run(&self.target(p)?.job_id, emit),
            "job.recoverable" => Ok(json!({"job_ids": self.runner.recoverable_ids()?})),
            "job.recover" => {
                let emit2 = Arc::clone(emit);
                let sink: EventSink = Arc::new(move |ev| emit2(ev.to_value()));
                let mut out = Vec::new();
                for id in self.runner.recoverable_ids()? {
                    out.push(self.runner.run_with(&id, Some(sink.clone()))?.to_value());
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
                let deadline = Instant::now() + Duration::from_secs_f64(p["wait_timeout_s"].as_f64().unwrap_or(30.0));
                while !j.is_terminal() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(50));
                    j = store.get(&j.job_id)?;
                }
                let mut v = j.to_value();
                v["timed_out"] = json!(!j.is_terminal());
                Ok(v)
            }
            "artifact.resolve" => {
                let id = p["artifact_id"].as_str().ok_or_else(|| JobError::bad_input("artifact_id required"))?;
                let a = store.get_artifact(id)?;
                let mut v = a.to_value();
                v["verified"] = json!(store.verify_artifact(&a)?);
                Ok(v)
            }
            other => Err(JobError::new("unknown_method", format!("unknown method: {other}"))),
        }
    }
}
