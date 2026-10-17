//! Tool executor (Orthic-Labs/rightkit#297).
//!
//! [`ToolSpec`], [`ContentBlock::ToolUse`] and [`ContentBlock::ToolResult`] describe tool use on
//! the wire. This module runs the calls a model makes: [`ToolExecutor::run`] drives the loop
//! (model call, tool calls, results back to the model, repeat) under
//!
//! - a **gate** ([`ToolGate`]) that sees every call before it runs and can deny it;
//! - a **budget** ([`ToolBudget`]): model rounds, tool calls, result bytes, reported tokens
//!   and a wall-clock deadline;
//! - **result re-entry**: one user message holding a [`ContentBlock::ToolResult`] for every
//!   tool call of the round, in call order, sent with the next model call.
//!
//! Every tool call gets a result, so the transcript stays valid for the providers: a call
//! that is denied, unknown, failed, panicked or skipped because a bound was hit becomes an
//! `is_error` result whose text says why. The model can react to a denial; the host sees every
//! outcome in [`ToolCallRecord`].
//!
//! Bounds and what they cannot do:
//!
//! - Bounds are checked between calls and between model calls. A handler that runs long is not
//!   interrupted (a synchronous Rust call cannot be stopped safely), so a handler that can block
//!   needs its own timeout. The deadline and the cancel flag stop the loop at the next check.
//! - When a bound stops the loop after a model response that asked for tools, those tools are
//!   **not** run (nothing would read their results); the response is returned with the cause in
//!   [`ToolLoopOutcome::stop`] and its calls in `response.tool_uses()`.
//! - Tool input is not checked against the tool's JSON schema (no schema validator is a
//!   dependency of this crate); a handler validates what it needs. A panic in a handler is
//!   caught and becomes an error result; a panic in a gate is not.
//! - Calls of one round run in order, one at a time.
//!
//! A provider failure ends the loop with [`ToolLoopError`], which keeps the usage and the tool
//! records of the rounds that had completed, so their spend is not lost (the failed call's own
//! billing is on `error`; see [`LlmError::billing`] and `ChatRequest::report_failed_usage`).

use std::collections::BTreeSet;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::LlmError;
use crate::provider::Provider;
use crate::types::*;

/// Runs one tool. `Ok` text and `Err` text both go back to the model (the second as an
/// `is_error` result), so keep both free of secrets.
pub trait ToolHandler: Send + Sync {
    fn call(&self, input: &Value) -> Result<String, String>;
}

impl<F> ToolHandler for F
where
    F: Fn(&Value) -> Result<String, String> + Send + Sync,
{
    fn call(&self, input: &Value) -> Result<String, String> {
        self(input)
    }
}

/// One tool call as the gate sees it.
#[derive(Debug, Clone, Copy)]
pub struct ToolInvocation<'a> {
    /// Model round, starting at 1.
    pub round: u32,
    /// Position of the call within its round, starting at 0.
    pub index: usize,
    /// The model's tool-use id.
    pub id: &'a str,
    pub name: &'a str,
    pub input: &'a Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    Allow,
    /// Do not run the call. The reason goes back to the model as an error result and into the
    /// record.
    Deny(String),
}

/// Decides, for every call, whether it may run. There is no default: an executor is built
/// with a gate, so allowing everything is a visible choice ([`AllowAll`]).
pub trait ToolGate: Send + Sync {
    fn decide(&self, call: &ToolInvocation<'_>) -> GateDecision;
}

impl<F> ToolGate for F
where
    F: Fn(&ToolInvocation<'_>) -> GateDecision + Send + Sync,
{
    fn decide(&self, call: &ToolInvocation<'_>) -> GateDecision {
        self(call)
    }
}

/// Allows every registered tool.
#[derive(Debug, Clone, Copy, Default)]
pub struct AllowAll;

impl ToolGate for AllowAll {
    fn decide(&self, _call: &ToolInvocation<'_>) -> GateDecision {
        GateDecision::Allow
    }
}

/// Allows only the named tools.
#[derive(Debug, Clone, Default)]
pub struct AllowTools {
    names: BTreeSet<String>,
}

impl AllowTools {
    pub fn new<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            names: names.into_iter().map(Into::into).collect(),
        }
    }
}

impl ToolGate for AllowTools {
    fn decide(&self, call: &ToolInvocation<'_>) -> GateDecision {
        if self.names.contains(call.name) {
            GateDecision::Allow
        } else {
            GateDecision::Deny(format!("tool '{}' is not allowed", call.name))
        }
    }
}

/// Bounds on one [`ToolExecutor::run`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolBudget {
    /// Model calls. Default 8.
    pub max_rounds: u32,
    /// Tool calls across all rounds, counting only calls that reach a handler. Default 32.
    pub max_tool_calls: u32,
    /// Bytes of one tool result sent back; longer text is cut on a character boundary and
    /// marked. Default 64 KiB.
    pub max_result_bytes: usize,
    /// Bytes of tool results across the run. Default 512 KiB.
    pub max_total_result_bytes: usize,
    /// Input plus output tokens the model reported, summed over rounds. `None` (default) sets no
    /// token bound. A round whose usage is unknown adds nothing.
    pub max_total_tokens: Option<u64>,
    /// Wall-clock limit for the whole run, measured from its start. `None` (default) sets none.
    pub deadline: Option<Duration>,
}

impl Default for ToolBudget {
    fn default() -> Self {
        Self {
            max_rounds: 8,
            max_tool_calls: 32,
            max_result_bytes: 64 * 1024,
            max_total_result_bytes: 512 * 1024,
            max_total_tokens: None,
            deadline: None,
        }
    }
}

/// Why a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCause {
    /// The model answered without asking for a tool.
    Completed,
    /// `max_rounds` model calls were made.
    RoundLimit,
    /// `max_tool_calls` was reached.
    CallLimit,
    /// `max_total_result_bytes` was reached.
    ResultBytesLimit,
    /// `max_total_tokens` was reached.
    TokenLimit,
    /// The deadline passed.
    Deadline,
    /// The cancel flag was set.
    Cancelled,
    /// A provider call failed. Appears only in [`ToolLoopError::partial`].
    Failed,
}

impl StopCause {
    fn describe(self) -> &'static str {
        match self {
            StopCause::Completed => "completed",
            StopCause::RoundLimit => "model round limit reached",
            StopCause::CallLimit => "tool call limit reached",
            StopCause::ResultBytesLimit => "tool result size limit reached",
            StopCause::TokenLimit => "token limit reached",
            StopCause::Deadline => "deadline passed",
            StopCause::Cancelled => "cancelled",
            StopCause::Failed => "model call failed",
        }
    }
}

/// What happened to one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutcome {
    /// The handler returned text.
    Ok,
    /// The handler returned an error; the text is what the model saw.
    Failed(String),
    /// The handler panicked.
    Panicked,
    /// The gate denied the call.
    Denied(String),
    /// No tool of that name is registered.
    Unknown,
    /// Not run because a bound was hit or the run was cancelled.
    Skipped(StopCause),
}

/// One tool call and its outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRecord {
    pub round: u32,
    pub id: String,
    pub name: String,
    pub input: Value,
    pub outcome: ToolOutcome,
    /// Bytes of the result sent back, after the per-result cap.
    pub result_bytes: usize,
    /// True when the result was cut to `max_result_bytes`.
    pub truncated: bool,
    pub elapsed: Duration,
}

/// Why a tool could not be registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSetupError {
    EmptyName,
    Duplicate(String),
}

impl fmt::Display for ToolSetupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolSetupError::EmptyName => write!(f, "tool name is empty"),
            ToolSetupError::Duplicate(n) => write!(f, "tool '{n}' is registered twice"),
        }
    }
}

impl std::error::Error for ToolSetupError {}

/// The tools an executor can run, each with the spec the model sees.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<(ToolSpec, Arc<dyn ToolHandler>)>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        spec: ToolSpec,
        handler: impl ToolHandler + 'static,
    ) -> Result<(), ToolSetupError> {
        if spec.name.trim().is_empty() {
            return Err(ToolSetupError::EmptyName);
        }
        if self.tools.iter().any(|(s, _)| s.name == spec.name) {
            return Err(ToolSetupError::Duplicate(spec.name));
        }
        self.tools.push((spec, Arc::new(handler)));
        Ok(())
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|(s, _)| s.clone()).collect()
    }

    fn handler(&self, name: &str) -> Option<&Arc<dyn ToolHandler>> {
        self.tools
            .iter()
            .find(|(s, _)| s.name == name)
            .map(|(_, h)| h)
    }
}

impl fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.tools.iter().map(|(s, _)| &s.name))
            .finish()
    }
}

/// Running totals of one run, shared by [`ToolExecutor::execute_round`] calls.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolTally {
    /// Calls that reached a handler.
    pub calls: u32,
    /// Result bytes sent back.
    pub result_bytes: usize,
    pub records: Vec<ToolCallRecord>,
}

/// The result of running one model response's tool calls.
#[derive(Debug, Clone, PartialEq)]
pub struct RoundResult {
    /// The user message to send with the next model call: one `ToolResult` per call.
    pub message: Message,
    /// Set when a bound or the cancel flag stopped the round; the calls from that point on
    /// were answered with an error result and not run.
    pub stop: Option<StopCause>,
}

/// The end of a run that reached a stop condition.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolLoopOutcome {
    /// The last model response, if a model call was made. Its text is the answer when `stop` is
    /// [`StopCause::Completed`]; otherwise it may still hold tool calls that were not run.
    pub response: Option<ChatResponse>,
    pub stop: StopCause,
    /// Model calls made.
    pub rounds: u32,
    /// Messages added after the request's own: assistant turns and tool-result turns, in order.
    pub transcript: Vec<Message>,
    pub calls: Vec<ToolCallRecord>,
    /// Sum of the usage the model reported across rounds.
    pub usage: Usage,
    /// False when any round reported no usage; then `usage` is a lower bound.
    pub usage_known: bool,
}

/// A provider failure inside a run, with what the earlier rounds cost.
#[derive(Debug)]
pub struct ToolLoopError {
    pub error: LlmError,
    /// Rounds, transcript, tool records and usage up to the failed call
    /// (`stop` is [`StopCause::Failed`], `response` is the last good response if any).
    pub partial: Box<ToolLoopOutcome>,
}

impl fmt::Display for ToolLoopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "tool loop failed in round {}: {}",
            self.partial.rounds + 1,
            self.error
        )
    }
}

impl std::error::Error for ToolLoopError {}

/// Runs tool calls under a gate and a budget.
pub struct ToolExecutor {
    registry: ToolRegistry,
    gate: Arc<dyn ToolGate>,
    budget: ToolBudget,
    cancel: Option<Arc<AtomicBool>>,
}

impl ToolExecutor {
    pub fn new(registry: ToolRegistry, gate: impl ToolGate + 'static, budget: ToolBudget) -> Self {
        Self {
            registry,
            gate: Arc::new(gate),
            budget,
            cancel: None,
        }
    }

    /// Stop the run when `flag` is set. Checked before every model call and every tool call.
    pub fn with_cancel(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancel = Some(flag);
        self
    }

    pub fn budget(&self) -> &ToolBudget {
        &self.budget
    }

    /// The specs of the registered tools, as [`ToolExecutor::run`] sends them.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.registry.specs()
    }

    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|f| f.load(Ordering::SeqCst))
    }

    fn deadline_passed(&self, started: Instant) -> bool {
        self.budget
            .deadline
            .is_some_and(|d| started.elapsed() >= d)
    }

    /// Bounds that apply before a further model call.
    fn before_model_call(&self, rounds: u32, usage: &Usage, started: Instant) -> Option<StopCause> {
        if self.cancelled() {
            return Some(StopCause::Cancelled);
        }
        if self.deadline_passed(started) {
            return Some(StopCause::Deadline);
        }
        if rounds >= self.budget.max_rounds {
            return Some(StopCause::RoundLimit);
        }
        if let Some(max) = self.budget.max_total_tokens {
            if usage.input_tokens + usage.output_tokens >= max {
                return Some(StopCause::TokenLimit);
            }
        }
        None
    }

    /// Bounds that apply before a further tool call.
    fn before_tool_call(&self, tally: &ToolTally, started: Instant) -> Option<StopCause> {
        if self.cancelled() {
            return Some(StopCause::Cancelled);
        }
        if self.deadline_passed(started) {
            return Some(StopCause::Deadline);
        }
        if tally.calls >= self.budget.max_tool_calls {
            return Some(StopCause::CallLimit);
        }
        if tally.result_bytes >= self.budget.max_total_result_bytes {
            return Some(StopCause::ResultBytesLimit);
        }
        None
    }

    /// Run every tool call in `response` in order and build the message that carries the
    /// results. Hosts with their own model loop (a streaming one, for example) use this
    /// directly; [`ToolExecutor::run`] calls it. `started` is when the run began, for the
    /// deadline. Every call gets a `ToolResult`, in call order.
    pub fn execute_round(
        &self,
        round: u32,
        response: &ChatResponse,
        tally: &mut ToolTally,
        started: Instant,
    ) -> RoundResult {
        let mut blocks = Vec::new();
        let mut stop: Option<StopCause> = None;
        for (index, (id, name, input)) in response.tool_uses().into_iter().enumerate() {
            let at = Instant::now();
            if stop.is_none() {
                stop = self.before_tool_call(tally, started);
            }
            let (text, is_error, outcome) = match stop {
                Some(cause) => (
                    format!("not run: {}", cause.describe()),
                    true,
                    ToolOutcome::Skipped(cause),
                ),
                None => {
                    tally.calls += 1;
                    self.invoke(&ToolInvocation {
                        round,
                        index,
                        id,
                        name,
                        input,
                    })
                }
            };
            let (text, truncated) = cap_result(text, self.budget.max_result_bytes);
            tally.result_bytes += text.len();
            tally.records.push(ToolCallRecord {
                round,
                id: id.to_owned(),
                name: name.to_owned(),
                input: input.clone(),
                outcome,
                result_bytes: text.len(),
                truncated,
                elapsed: at.elapsed(),
            });
            blocks.push(ContentBlock::ToolResult {
                tool_use_id: id.to_owned(),
                content: text,
                is_error,
            });
        }
        RoundResult {
            message: Message {
                role: Role::User,
                content: blocks,
            },
            stop,
        }
    }

    fn invoke(&self, call: &ToolInvocation<'_>) -> (String, bool, ToolOutcome) {
        if let GateDecision::Deny(reason) = self.gate.decide(call) {
            return (format!("denied: {reason}"), true, ToolOutcome::Denied(reason));
        }
        let Some(handler) = self.registry.handler(call.name) else {
            return (
                format!("unknown tool '{}'", call.name),
                true,
                ToolOutcome::Unknown,
            );
        };
        match catch_unwind(AssertUnwindSafe(|| handler.call(call.input))) {
            Ok(Ok(text)) => (text, false, ToolOutcome::Ok),
            Ok(Err(e)) => (e.clone(), true, ToolOutcome::Failed(e)),
            Err(_) => (
                format!("tool '{}' failed unexpectedly", call.name),
                true,
                ToolOutcome::Panicked,
            ),
        }
    }

    /// Drive the loop: call the model, run the tools it asks for, send the results back, and
    /// repeat until it answers without a tool or a bound stops the run.
    ///
    /// The executor's tools replace `req.tools`. A request with `force_tool` set is rejected
    /// (a forced tool call would never end the loop) with [`LlmError::InvalidRequest`] inside a
    /// [`ToolLoopError`]. A bound that stops the run is a normal [`ToolLoopOutcome`] with the
    /// cause in `stop`, not an error.
    pub fn run(
        &self,
        provider: &dyn Provider,
        mut req: ChatRequest,
    ) -> Result<ToolLoopOutcome, ToolLoopError> {
        let mut state = ToolLoopOutcome {
            response: None,
            stop: StopCause::Completed,
            rounds: 0,
            transcript: Vec::new(),
            calls: Vec::new(),
            usage: Usage::default(),
            usage_known: true,
        };
        if req.force_tool.is_some() {
            return Err(fail(
                state,
                LlmError::InvalidRequest(
                    "force_tool cannot be combined with the tool executor".into(),
                ),
            ));
        }
        req.tools = self.registry.specs();
        let started = Instant::now();
        let mut tally = ToolTally::default();
        loop {
            if let Some(cause) = self.before_model_call(state.rounds, &state.usage, started) {
                state.stop = cause;
                break;
            }
            let response = match provider.complete(&req) {
                Ok(r) => r,
                Err(e) => {
                    state.calls = std::mem::take(&mut tally.records);
                    return Err(fail(state, e));
                }
            };
            state.rounds += 1;
            if response.usage_known {
                state.usage.add(&response.usage);
            } else {
                state.usage_known = false;
            }
            if response.tool_uses().is_empty() {
                state.response = Some(response);
                state.stop = StopCause::Completed;
                break;
            }
            // Tools run only when a further model call could read their results.
            if let Some(cause) = self.before_model_call(state.rounds, &state.usage, started) {
                state.response = Some(response);
                state.stop = cause;
                break;
            }
            let assistant = Message {
                role: Role::Assistant,
                content: response.content.clone(),
            };
            let round = self.execute_round(state.rounds, &response, &mut tally, started);
            req.messages.push(assistant.clone());
            req.messages.push(round.message.clone());
            state.transcript.push(assistant);
            state.transcript.push(round.message);
            state.response = Some(response);
            if let Some(cause) = round.stop {
                state.stop = cause;
                break;
            }
        }
        state.calls = tally.records;
        Ok(state)
    }
}

fn fail(mut partial: ToolLoopOutcome, error: LlmError) -> ToolLoopError {
    partial.stop = StopCause::Failed;
    ToolLoopError {
        error,
        partial: Box::new(partial),
    }
}

/// Cut `text` to at most `max` bytes on a character boundary, marking the cut.
fn cap_result(text: String, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let omitted = text.len() - end;
    let mut cut = text[..end].to_owned();
    cut.push_str(&format!("\n[truncated: {omitted} bytes omitted]"));
    (cut, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_result_cuts_on_a_character_boundary_and_says_so() {
        assert_eq!(cap_result("abc".into(), 3), ("abc".to_string(), false));
        let (cut, truncated) = cap_result("aéb".into(), 2);
        assert!(truncated);
        assert!(cut.starts_with('a'));
        assert!(cut.contains("[truncated: 3 bytes omitted]"));
    }
}
