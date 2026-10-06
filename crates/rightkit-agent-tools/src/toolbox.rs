//! [`Toolbox`]: the registry and the one gate every call passes.
//!
//! Order: lookup, hard exposure rules, per-caller allowlist, argument schema,
//! the caller's admission hook (typed, content-free, may deny before any side
//! effect), execute, settle, journal.

use crate::exposure;
use crate::spec::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Content-free description of one intended call (no argument values).
#[derive(Debug, Clone)]
pub struct ToolRequest {
    pub caller: String,
    pub tool: String,
    pub subject: Option<String>,
    pub effect: Effect,
    /// Top-level argument names, never values.
    pub arg_keys: Vec<String>,
    pub arg_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny(String),
}

/// The caller-supplied pre-effect hook (CodeRight EFF-001: validate, approve,
/// execute, settle). `approve` runs after the toolbox's own validation and
/// before the tool does anything; a denial executes nothing.
pub trait ToolAdmission: Send + Sync {
    fn approve(&self, req: &ToolRequest) -> Verdict;
    fn settle(&self, _req: &ToolRequest, _ok: bool, _error_code: Option<&str>) {}
}

/// Lifecycle events for the caller's journal. Content-free.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolEvent {
    Started { caller: String, tool: String },
    Denied { caller: String, tool: String, by: DeniedBy, reason: String },
    Settled { caller: String, tool: String, ok: bool, error_code: Option<String>, millis: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeniedBy {
    Exposure,
    Allowlist,
    Arguments,
    Admission,
}

impl ToolEvent {
    pub fn to_value(&self) -> Value {
        match self {
            ToolEvent::Started { caller, tool } => json!({"event": "started", "caller": caller, "tool": tool}),
            ToolEvent::Denied { caller, tool, by, reason } => json!({"event": "denied", "caller": caller, "tool": tool, "by": format!("{by:?}").to_lowercase(), "reason": reason}),
            ToolEvent::Settled { caller, tool, ok, error_code, millis } => json!({"event": "settled", "caller": caller, "tool": tool, "ok": ok, "error": error_code, "ms": millis}),
        }
    }
}

pub trait Journal: Send + Sync {
    fn record(&self, event: &ToolEvent);
}

#[derive(Default)]
pub struct MemoryJournal(Mutex<Vec<ToolEvent>>);
impl MemoryJournal {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn events(&self) -> Vec<ToolEvent> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}
impl Journal for MemoryJournal {
    fn record(&self, event: &ToolEvent) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(event.clone());
    }
}

/// Content-free counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Metrics {
    pub calls: u64,
    pub ok: u64,
    pub failed: u64,
    pub denied: u64,
}

#[derive(Default)]
struct Counters {
    calls: AtomicU64,
    ok: AtomicU64,
    failed: AtomicU64,
    denied: AtomicU64,
}

/// `*` matches everything, `prefix*` a prefix, anything else exactly. Patterns
/// are matched against `<tool>` and, for tools that carry a subject, against
/// `<tool>:<subject>`; a tool with a subject needs a pattern that matches the
/// full `<tool>:<subject>` form.
fn pattern_matches(pattern: &str, candidate: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => candidate.starts_with(prefix),
        None => pattern == candidate,
    }
}

pub struct Toolbox {
    tools: BTreeMap<String, Arc<dyn Tool>>,
    allow: HashMap<String, Vec<String>>,
    admission: Option<Arc<dyn ToolAdmission>>,
    journals: Vec<Arc<dyn Journal>>,
    counters: Counters,
}

impl Default for Toolbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Toolbox {
    pub fn new() -> Self {
        Self { tools: BTreeMap::new(), allow: HashMap::new(), admission: None, journals: vec![], counters: Counters::default() }
    }

    /// Register a tool. Names that the hard exposure rules forbid are refused
    /// here, so they cannot even be listed.
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), ToolError> {
        let name = tool.spec().name.clone();
        if let Some(why) = exposure::forbidden(&name) {
            return Err(ToolError::new(ERR_NOT_EXPOSED, why));
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    pub fn register_all(&mut self, tools: Vec<Arc<dyn Tool>>) -> Result<(), ToolError> {
        tools.into_iter().try_for_each(|t| self.register(t))
    }

    /// Let `caller` use tools matching `patterns` (`*`, `prefix*`, exact; see [`pattern_matches`]).
    pub fn allow<I: IntoIterator<Item = S>, S: Into<String>>(&mut self, caller: &str, patterns: I) -> &mut Self {
        self.allow.entry(caller.to_string()).or_default().extend(patterns.into_iter().map(Into::into));
        self
    }

    pub fn set_admission(&mut self, hook: Arc<dyn ToolAdmission>) -> &mut Self {
        self.admission = Some(hook);
        self
    }

    pub fn add_journal(&mut self, j: Arc<dyn Journal>) -> &mut Self {
        self.journals.push(j);
        self
    }

    pub fn metrics(&self) -> Metrics {
        Metrics {
            calls: self.counters.calls.load(Ordering::SeqCst),
            ok: self.counters.ok.load(Ordering::SeqCst),
            failed: self.counters.failed.load(Ordering::SeqCst),
            denied: self.counters.denied.load(Ordering::SeqCst),
        }
    }

    fn permitted(&self, caller: &str, tool: &str, subject: Option<&str>) -> bool {
        let Some(patterns) = self.allow.get(caller) else { return false };
        match subject {
            Some(s) => {
                let full = format!("{tool}:{s}");
                patterns.iter().any(|p| pattern_matches(p, &full))
            }
            None => patterns.iter().any(|p| pattern_matches(p, tool)),
        }
    }

    /// Specs visible to `caller`: only tools it may call at all (subject-carrying
    /// tools are listed when any pattern could match them).
    pub fn list_for(&self, caller: &str) -> Vec<ToolSpec> {
        let Some(patterns) = self.allow.get(caller) else { return vec![] };
        self.tools
            .values()
            .filter(|t| {
                let n = &t.spec().name;
                patterns.iter().any(|p| pattern_matches(p, n) || p.starts_with(&format!("{n}:")) || (p.ends_with('*') && n.starts_with(p.trim_end_matches('*'))))
            })
            .map(|t| t.spec().clone())
            .collect()
    }

    pub fn list_all(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec().clone()).collect()
    }

    fn emit(&self, ev: ToolEvent) {
        for j in &self.journals {
            j.record(&ev);
        }
    }

    fn deny(&self, caller: &str, tool: &str, by: DeniedBy, code: &str, reason: String) -> ToolError {
        self.counters.denied.fetch_add(1, Ordering::SeqCst);
        self.emit(ToolEvent::Denied { caller: caller.into(), tool: tool.into(), by, reason: reason.clone() });
        ToolError::new(code, reason)
    }

    pub fn call(&self, caller: &Caller, name: &str, args: Value) -> Result<Value, ToolError> {
        self.call_cancellable(caller, name, args, &CancelToken::new())
    }

    pub fn call_cancellable(&self, caller: &Caller, name: &str, args: Value, cancel: &CancelToken) -> Result<Value, ToolError> {
        self.counters.calls.fetch_add(1, Ordering::SeqCst);
        let tool = self.tools.get(name).cloned().ok_or_else(|| ToolError::new(ERR_UNKNOWN_TOOL, format!("no tool `{name}`")))?;
        let subject = tool.subject(&args);

        if let Some(why) = exposure::forbidden(name).or_else(|| subject.as_deref().and_then(exposure::forbidden)) {
            return Err(self.deny(&caller.app, name, DeniedBy::Exposure, ERR_NOT_EXPOSED, why));
        }
        if !self.permitted(&caller.app, name, subject.as_deref()) {
            let what = subject.as_deref().map(|s| format!("{name}:{s}")).unwrap_or_else(|| name.to_string());
            return Err(self.deny(&caller.app, name, DeniedBy::Allowlist, ERR_NOT_ALLOWED, format!("`{}` may not call `{what}`", caller.app)));
        }
        if let Err(e) = validate_args(&tool.spec().input_schema, &args) {
            return Err(self.deny(&caller.app, name, DeniedBy::Arguments, ERR_INVALID_ARGS, e));
        }

        let req = ToolRequest {
            caller: caller.app.clone(),
            tool: name.to_string(),
            subject,
            effect: tool.spec().effect,
            arg_keys: args.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default(),
            arg_bytes: args.to_string().len(),
        };
        if let Some(h) = &self.admission {
            if let Verdict::Deny(reason) = h.approve(&req) {
                return Err(self.deny(&caller.app, name, DeniedBy::Admission, ERR_DENIED, reason));
            }
        }

        self.emit(ToolEvent::Started { caller: caller.app.clone(), tool: name.into() });
        let started = Instant::now();
        let ctx = CallCtx { caller, cancel };
        let res = if cancel.is_cancelled() { Err(ToolError::new(ERR_CANCELLED, "cancelled before start")) } else { tool.call(&ctx, args) };
        let (ok, code) = match &res {
            Ok(_) => (true, None),
            Err(e) => (false, Some(e.code.clone())),
        };
        if ok {
            self.counters.ok.fetch_add(1, Ordering::SeqCst);
        } else {
            self.counters.failed.fetch_add(1, Ordering::SeqCst);
        }
        if let Some(h) = &self.admission {
            h.settle(&req, ok, code.as_deref());
        }
        self.emit(ToolEvent::Settled { caller: caller.app.clone(), tool: name.into(), ok, error_code: code, millis: started.elapsed().as_millis() as u64 });
        res
    }
}
