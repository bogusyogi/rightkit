//! The shared `job.*` command set: [`JobCommands`] serves the job store and runner through the
//! dispatcher contract (feature `contract`; the dispatcher and serve loop need only `serve`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::config::Ext;
use crate::contract::{CommandSet, Sink};
use crate::error::{JobError, JobResult};
use crate::runner::{EventSink, RunOutcome, Runner};
use crate::store::{JobStore, ListFilter, NewJob};

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
