//! Module fan-out under deadlines, with an exit-0 contract.
//!
//! Mirrors `@rightkit/hooks` `dispatchHookEvent` (serial modules in declared order, a typed
//! per-module result, a per-module deadline that defaults to 3000 ms) and adds one total deadline,
//! so two modules on one event share one budget. Nothing a module does can fail the host: an
//! error, a panic or a timeout becomes a typed result, and the reply falls back to nothing.
//!
//! A module that times out is not killed: Rust cannot stop a thread. Its thread keeps running
//! until the module returns (cooperative modules watch [`ModuleContext::is_cancelled`]) and any
//! side effect it has, such as writing a receipt, can still land after the hook has replied.
//!
//! The only non-zero exit is a module's explicit [`Decision::DenyTool`] on Codex, which is exit 2
//! with the reason on stderr (Legion's shipped shape).

use crate::event::{EventKind, HookEvent, ParseError, Wire};
use crate::output::Reply;
use serde_json::Value;
use std::io::{Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Per-module budget, the JS default.
pub const DEFAULT_MODULE_DEADLINE: Duration = Duration::from_millis(3_000);
/// Whole-dispatch budget, inside the 10 s timeout the Pulse plugins register.
pub const DEFAULT_TOTAL_DEADLINE: Duration = Duration::from_millis(8_000);
/// Merged context cap in bytes (Pulse's `TEXT_CAP`).
pub const DEFAULT_CONTEXT_CAP_BYTES: usize = 60_000;

/// A module's verdict on the event. Applied only where the event's schema takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// `Stop`: keep the run going with this reason.
    BlockStop(String),
    /// `PreToolUse`: refuse the call.
    DenyTool(String),
    /// `PreToolUse`: ask the person (Claude; Codex denies).
    AskTool(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModuleOutput {
    pub additional_context: Option<String>,
    pub system_message: Option<String>,
    pub decision: Option<Decision>,
    /// Free-form, content-free detail for the result record.
    pub detail: Value,
}

impl ModuleOutput {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn context(text: impl Into<String>) -> Self {
        Self {
            additional_context: Some(text.into()),
            ..Self::default()
        }
    }

    pub fn decide(decision: Decision) -> Self {
        Self {
            decision: Some(decision),
            ..Self::default()
        }
    }
}

/// Passed to a module so it can stop early: the deadline, and a flag set once it has passed.
#[derive(Clone, Debug)]
pub struct ModuleContext {
    pub deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

impl ModuleContext {
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

pub trait HookModule: Send + Sync {
    fn id(&self) -> &str;
    fn handle(&self, event: &HookEvent, ctx: &ModuleContext) -> Result<ModuleOutput, String>;
}

/// A module from a closure.
pub struct FnModule<F> {
    id: String,
    run: F,
}

impl<F> FnModule<F>
where
    F: Fn(&HookEvent, &ModuleContext) -> Result<ModuleOutput, String> + Send + Sync,
{
    pub fn new(id: impl Into<String>, run: F) -> Self {
        Self { id: id.into(), run }
    }
}

impl<F> HookModule for FnModule<F>
where
    F: Fn(&HookEvent, &ModuleContext) -> Result<ModuleOutput, String> + Send + Sync,
{
    fn id(&self) -> &str {
        &self.id
    }

    fn handle(&self, event: &HookEvent, ctx: &ModuleContext) -> Result<ModuleOutput, String> {
        (self.run)(event, ctx)
    }
}

#[derive(Clone, Debug)]
pub struct DispatchOptions {
    pub module_deadline: Duration,
    pub total_deadline: Duration,
    pub context_cap_bytes: usize,
    /// Printed when the payload cannot be parsed or no module produced anything while one failed.
    pub fallback: Reply,
}

impl Default for DispatchOptions {
    fn default() -> Self {
        Self {
            module_deadline: DEFAULT_MODULE_DEADLINE,
            total_deadline: DEFAULT_TOTAL_DEADLINE,
            context_cap_bytes: DEFAULT_CONTEXT_CAP_BYTES,
            fallback: Reply::none(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModuleResult {
    pub id: String,
    pub status: Status,
    pub output: Option<ModuleOutput>,
    pub error: Option<String>,
    pub elapsed: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DispatchResult {
    pub event: EventKind,
    /// `Error` when any module failed, as in the JS result.
    pub status: Status,
    pub results: Vec<ModuleResult>,
    pub reply: Reply,
}

fn run_module(
    module: &Arc<dyn HookModule>,
    event: &Arc<HookEvent>,
    budget: Duration,
) -> ModuleResult {
    let started = Instant::now();
    let id = module.id().to_owned();
    let cancelled = Arc::new(AtomicBool::new(false));
    let ctx = ModuleContext {
        deadline: started + budget,
        cancelled: cancelled.clone(),
    };
    let (tx, rx) = mpsc::channel();
    let (module, event) = (module.clone(), event.clone());
    let spawned = std::thread::Builder::new()
        .name(format!("hook-module-{id}"))
        .spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| module.handle(&event, &ctx)))
                .unwrap_or_else(|_| Err("module panicked".to_owned()));
            let _ = tx.send(result);
        });
    let outcome = match spawned {
        Err(e) => Err(format!("module thread could not start: {e}")),
        Ok(_) => match rx.recv_timeout(budget) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                cancelled.store(true, Ordering::Relaxed);
                Err(format!("HookHost module timed out: {id}"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("module ended without a result".into())
            }
        },
    };
    let elapsed = started.elapsed();
    match outcome {
        Ok(output) => ModuleResult {
            id,
            status: Status::Ok,
            output: Some(output),
            error: None,
            elapsed,
        },
        Err(error) => ModuleResult {
            id,
            status: Status::Error,
            output: None,
            error: Some(error),
            elapsed,
        },
    }
}

fn cap_text(mut text: String, cap: usize) -> String {
    if text.len() > cap {
        let mut cut = cap;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    text
}

fn join(parts: Vec<String>) -> Option<String> {
    let parts: Vec<String> = parts.into_iter().filter(|p| !p.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// Merge module outputs into the one reply the event's schema allows.
///
/// A decision wins over context. Contexts are joined with a blank line and capped. A decision or
/// context the event's schema does not take is dropped, never emitted.
pub fn compose_reply(event: &HookEvent, results: &[ModuleResult], context_cap: usize) -> Reply {
    let outputs = results.iter().filter_map(|r| r.output.as_ref());
    let decision = outputs.clone().find_map(|o| o.decision.clone());
    let system = join(
        outputs
            .clone()
            .filter_map(|o| o.system_message.clone())
            .collect(),
    );
    let context = join(
        outputs
            .filter_map(|o| o.additional_context.clone())
            .collect(),
    )
    .map(|c| cap_text(c, context_cap));

    let base = match (&event.kind, decision) {
        (EventKind::Stop, Some(Decision::BlockStop(reason))) => Some(Reply::block_stop(&reason)),
        (EventKind::PreToolUse, Some(Decision::DenyTool(reason))) => {
            Some(Reply::deny_tool(event.wire, &reason))
        }
        (EventKind::PreToolUse, Some(Decision::AskTool(reason))) => {
            Some(Reply::ask_tool(event.wire, &reason))
        }
        _ => None,
    };
    let base = base.or_else(|| {
        context
            .as_deref()
            .and_then(|c| Reply::additional_context(&event.kind, c).ok())
    });
    match (base, system) {
        (Some(reply), Some(message)) => reply.with_system_message(&message),
        (Some(reply), None) => reply,
        (None, _) => Reply::none(),
    }
}

/// Run `modules` in order. Each gets `min(module_deadline, what is left of total_deadline)`;
/// once the total is spent the rest are recorded as errors and not started.
pub fn dispatch(
    event: HookEvent,
    modules: &[Arc<dyn HookModule>],
    options: &DispatchOptions,
) -> DispatchResult {
    let started = Instant::now();
    let event = Arc::new(event);
    let mut results = Vec::with_capacity(modules.len());
    for module in modules {
        let left = options.total_deadline.saturating_sub(started.elapsed());
        if left.is_zero() {
            results.push(ModuleResult {
                id: module.id().to_owned(),
                status: Status::Error,
                output: None,
                error: Some("HookHost total deadline spent".into()),
                elapsed: Duration::ZERO,
            });
            continue;
        }
        results.push(run_module(
            module,
            &event,
            left.min(options.module_deadline),
        ));
    }
    let failed = results.iter().any(|r| r.status == Status::Error);
    let mut reply = compose_reply(&event, &results, options.context_cap_bytes);
    if failed && reply.is_none() {
        reply = options.fallback.clone();
    }
    DispatchResult {
        event: event.kind.clone(),
        status: if failed { Status::Error } else { Status::Ok },
        results,
        reply,
    }
}

/// The whole hook process over arbitrary streams. Returns the exit code: 0 on every failure.
pub fn run_with(
    wire: Wire,
    modules: &[Arc<dyn HookModule>],
    options: &DispatchOptions,
    input: impl Read,
    output: impl Write,
    errors: impl Write,
) -> i32 {
    let reply = match HookEvent::from_reader(wire, input) {
        Err(error) => fallback_for(&error, options),
        Ok(event) => catch_unwind(AssertUnwindSafe(|| dispatch(event, modules, options).reply))
            .unwrap_or_else(|_| options.fallback.clone()),
    };
    reply.write_to(output, errors)
}

fn fallback_for(_error: &ParseError, options: &DispatchOptions) -> Reply {
    options.fallback.clone()
}

/// [`run_with`] on the process's own stdin, stdout and stderr.
pub fn run(wire: Wire, modules: &[Arc<dyn HookModule>], options: &DispatchOptions) -> i32 {
    run_with(
        wire,
        modules,
        options,
        std::io::stdin().lock(),
        std::io::stdout().lock(),
        std::io::stderr().lock(),
    )
}
