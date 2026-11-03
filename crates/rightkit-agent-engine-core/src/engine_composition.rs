use super::*;
#[cfg(feature = "host")]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(feature = "host")]
use std::sync::LazyLock;
#[cfg(feature = "host")]
use std::time::{Duration, Instant};

#[cfg(feature = "host")]
const MAX_SOURCE_BYTES: usize = 64 * 1024;
#[cfg(feature = "host")]
const MAX_OUTPUT_BYTES: usize = 8 * 1024;
#[cfg(feature = "host")]
const MAX_NESTED_CALLS: usize = 100;
const MAX_NESTED_RESULT_BYTES: usize = 8 * 1024 * 1024;
const MAX_TIMEOUT_MS: u64 = 120_000;
#[cfg(feature = "host")]
const JS_MEMORY_LIMIT_BYTES: usize = 16 * 1024 * 1024;
#[cfg(feature = "host")]
const JS_STACK_LIMIT_BYTES: usize = 512 * 1024;

const COMPOSABLE_TOOL_NAMES: [&str; 2] = ["read_file", "search"];

#[cfg(feature = "host")]
fn nested_failure_projection(output: &str) -> String {
    let mut selected = output.to_string();
    loop {
        let projected = serde_json::json!({
            "ok": false,
            "status": "nested_call_failed",
            "result": selected,
        })
        .to_string();
        if projected.len() <= MAX_OUTPUT_BYTES {
            return projected;
        }

        let excess = projected.len() - MAX_OUTPUT_BYTES;
        let target = selected.len().saturating_sub(excess.max(1));
        selected = selected
            .char_indices()
            .take_while(|(index, _)| *index < target)
            .map(|(_, character)| character)
            .collect();
    }
}

#[cfg(feature = "host")]
static JS_RUNTIME_PERMITS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

enum ExecForm {
    Capability {
        capability: String,
        schema_digest: String,
        args: serde_json::Value,
    },
    JavaScript {
        source: String,
        timeout_ms: u64,
    },
}

#[cfg(feature = "host")]
struct BridgeRequest {
    name: String,
    args_json: String,
    reply: tokio::sync::oneshot::Sender<String>,
}

pub(in crate::engine) async fn execute_exec_call(
    engine: &mut SessionEngine,
    call: &ToolCall,
    args: &serde_json::Value,
    cancel_rx: tokio::sync::watch::Receiver<bool>,
) -> rightkit_agent_tools::coding_tools::ToolResult {
    match parse_exec_form(args) {
        Err(error) => rightkit_agent_tools::coding_tools::ToolResult::error(error),
        Ok(ExecForm::Capability {
            capability,
            schema_digest,
            args,
        }) => run_capability_call(engine, call, &capability, &schema_digest, args, cancel_rx).await,
        Ok(ExecForm::JavaScript { source, timeout_ms }) => {
            run_js_call(engine, call, &source, timeout_ms, cancel_rx).await
        }
    }
}

#[cfg(feature = "host")]
struct ProgramNestedDispatcher<'a> {
    engine: tokio::sync::Mutex<&'a mut SessionEngine>,
    parent_call_id: String,
    next_call: std::sync::atomic::AtomicUsize,
}

/// Owns the engine lock and the `internal_composition_call_ids` entry for one
/// nested call. The entry is removed in `Drop`, so it is cleared on every
/// path, including when the dispatching future is dropped mid-flight.
#[cfg(feature = "host")]
struct CompositionCallGuard<'g, 'a> {
    engine: tokio::sync::MutexGuard<'g, &'a mut SessionEngine>,
    id: String,
}

#[cfg(feature = "host")]
impl<'g, 'a> CompositionCallGuard<'g, 'a> {
    fn enter(mut engine: tokio::sync::MutexGuard<'g, &'a mut SessionEngine>, id: String) -> Self {
        engine.internal_composition_call_ids.insert(id.clone());
        Self { engine, id }
    }
}

#[cfg(feature = "host")]
impl Drop for CompositionCallGuard<'_, '_> {
    fn drop(&mut self) {
        self.engine.internal_composition_call_ids.remove(&self.id);
    }
}

#[cfg(feature = "host")]
impl rightkit_agent_tools::coding_tools::code_mode::NestedToolDispatcher
    for ProgramNestedDispatcher<'_>
{
    fn invoke<'a>(
        &'a self,
        tool: &'a str,
        args: serde_json::Value,
        _context: &'a rightkit_agent_tools::coding_tools::ToolContext,
    ) -> rightkit_agent_tools::coding_tools::code_mode::NestedToolFuture<'a> {
        Box::pin(async move {
            let sequence = self
                .next_call
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            let (dispatch_name, dispatch_args) = if tool == "pull_program_result" {
                (
                    "mcp_call",
                    serde_json::json!({
                        "name": "mcp__membrane__pull",
                        "arguments": args,
                    }),
                )
            } else {
                (tool, args)
            };
            let call = ToolCall {
                id: format!("{}.program-nested.{sequence}", self.parent_call_id),
                name: dispatch_name.to_string(),
                arguments_json: dispatch_args.to_string(),
            };
            let mut results = HashMap::new();
            let mut guard = CompositionCallGuard::enter(self.engine.lock().await, call.id.clone());
            Box::pin(guard.engine.dispatch_tool_call(&call, &mut results)).await;
            drop(guard);
            results.remove(&call.id).unwrap_or_else(|| {
                rightkit_agent_tools::coding_tools::ToolResult::error(
                    "governed run_program child ended without a typed result",
                )
            })
        })
    }
}

#[cfg(feature = "host")]
pub(in crate::engine) async fn execute_run_program_call(
    engine: &mut SessionEngine,
    call: &ToolCall,
    args: &serde_json::Value,
    context: &rightkit_agent_tools::coding_tools::ToolContext,
) -> rightkit_agent_tools::coding_tools::ToolResult {
    let Some(source) = args.get("code").and_then(serde_json::Value::as_str) else {
        return rightkit_agent_tools::coding_tools::ToolResult::error(
            "run_program.code must be a string",
        );
    };
    let specs = rightkit_agent_tools::coding_tools::code_mode::RunProgramTool::new(&engine.tools)
        .tool_specs()
        .to_vec();
    let context_dispatcher = engine.context_tool_dispatcher.clone();
    let dispatcher = ProgramNestedDispatcher {
        engine: tokio::sync::Mutex::new(engine),
        parent_call_id: call.id.clone(),
        next_call: std::sync::atomic::AtomicUsize::new(0),
    };
    rightkit_agent_tools::coding_tools::code_mode::execute_program(
        source,
        &specs,
        context,
        &dispatcher,
        context_dispatcher.as_deref(),
        &call.id,
    )
    .await
}

fn parse_exec_form(args: &serde_json::Value) -> Result<ExecForm, String> {
    let Some(object) = args.as_object() else {
        return Err("exec arguments must be a JSON object".to_string());
    };
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "source" | "timeout_ms" | "capability" | "schema_digest" | "args"
        ) {
            return Err(format!("exec does not accept argument `{key}`"));
        }
    }
    let has_source = object.contains_key("source");
    let has_capability = object.contains_key("capability");
    match (has_source, has_capability) {
        (true, false) => {
            if object.contains_key("schema_digest") || object.contains_key("args") {
                return Err(
                    "exec source form does not accept `capability`, `schema_digest`, or `args`"
                        .to_string(),
                );
            }
            let Some(source) = object.get("source").and_then(serde_json::Value::as_str) else {
                return Err("exec.source must be a string".to_string());
            };
            let timeout_ms = match object.get("timeout_ms") {
                None => MAX_TIMEOUT_MS,
                Some(value) => {
                    let Some(ms) = value.as_u64() else {
                        return Err("exec.timeout_ms must be an integer".to_string());
                    };
                    if !(1..=MAX_TIMEOUT_MS).contains(&ms) {
                        return Err(format!(
                            "exec.timeout_ms must be within 1..={MAX_TIMEOUT_MS}"
                        ));
                    }
                    ms
                }
            };
            Ok(ExecForm::JavaScript {
                source: source.to_string(),
                timeout_ms,
            })
        }
        (false, true) => {
            if object.contains_key("timeout_ms") {
                return Err("exec capability form does not accept `timeout_ms`".to_string());
            }
            let Some(capability) = object
                .get("capability")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                return Err("exec.capability must be a non-empty string".to_string());
            };
            let Some(schema_digest) = object
                .get("schema_digest")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                return Err("exec.schema_digest must be a non-empty string".to_string());
            };
            let nested_args = object
                .get("args")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            if !nested_args.is_object() {
                return Err("exec.args must be a JSON object".to_string());
            }
            Ok(ExecForm::Capability {
                capability: capability.to_string(),
                schema_digest: schema_digest.to_string(),
                args: nested_args,
            })
        }
        _ => Err(
            "exec requires exactly one of `source` or `capability` + `schema_digest`".to_string(),
        ),
    }
}

fn nested_call_id(parent_id: &str, seq: usize) -> String {
    format!("{parent_id}.exec-nested.{seq}")
}

async fn run_capability_call(
    engine: &mut SessionEngine,
    call: &ToolCall,
    capability: &str,
    schema_digest: &str,
    nested_args: serde_json::Value,
    cancel_rx: tokio::sync::watch::Receiver<bool>,
) -> rightkit_agent_tools::coding_tools::ToolResult {
    if *cancel_rx.borrow() {
        return rightkit_agent_tools::coding_tools::ToolResult::error("exec cancelled");
    }
    let tool = match engine.tools.resolve_capability(capability, schema_digest) {
        Ok(tool) => tool,
        Err(error) => return rightkit_agent_tools::coding_tools::ToolResult::error(error),
    };
    if !COMPOSABLE_TOOL_NAMES.contains(&tool.name()) {
        return rightkit_agent_tools::coding_tools::ToolResult::error(format!(
            "exec refuses capability `{capability}`: composition admits only {}",
            COMPOSABLE_TOOL_NAMES.join(", ")
        ));
    }
    if !tool.is_read_only() {
        return rightkit_agent_tools::coding_tools::ToolResult::error(format!(
            "exec refuses mutating capability `{capability}`: composition is read-only"
        ));
    }
    let nested_call = ToolCall {
        id: nested_call_id(&call.id, 1),
        name: tool.name().to_string(),
        arguments_json: nested_args.to_string(),
    };
    let mut results = HashMap::new();
    let result = dispatch_nested(engine, &nested_call, &mut results).await;
    if result.content.len() > MAX_NESTED_RESULT_BYTES {
        return rightkit_agent_tools::coding_tools::ToolResult::error(format!(
            "exec nested result exceeds the {MAX_NESTED_RESULT_BYTES}-byte aggregate limit"
        ));
    }
    let payload = serde_json::json!({
        "capability": capability,
        "schema_digest": schema_digest,
        "ok": !result.is_error,
        "content": result.content,
    })
    .to_string();
    if result.is_error {
        rightkit_agent_tools::coding_tools::ToolResult::error(payload)
    } else {
        rightkit_agent_tools::coding_tools::ToolResult::ok(payload)
    }
}

fn nested_call_recorded(engine: &SessionEngine, call_id: &str) -> bool {
    engine.transcript.messages().iter().any(|message| {
        matches!(
            message,
            TranscriptMessage::Tool { tool_call_id, .. } if tool_call_id == call_id
        )
    })
}

/// Owns the engine borrow and the `internal_composition_call_ids` entry for
/// one admitted nested call. If the dispatching future is dropped before the
/// call completes (deadline, cancel), `Drop` records the call as cancelled
/// unless a result row already exists, then clears the entry.
struct NestedDispatchGuard<'e> {
    engine: &'e mut SessionEngine,
    call: ToolCall,
    completed: bool,
}

impl Drop for NestedDispatchGuard<'_> {
    fn drop(&mut self) {
        if !self.completed && !nested_call_recorded(self.engine, &self.call.id) {
            let sid = self.engine.session_id.clone();
            let mut scratch = HashMap::new();
            self.engine.record_tool_error(
                &sid,
                &self.call,
                "nested call cancelled before completion".to_string(),
                &mut scratch,
            );
        }
        self.engine
            .internal_composition_call_ids
            .remove(&self.call.id);
    }
}

async fn dispatch_nested(
    engine: &mut SessionEngine,
    nested_call: &ToolCall,
    results: &mut HashMap<String, rightkit_agent_tools::coding_tools::ToolResult>,
) -> rightkit_agent_tools::coding_tools::ToolResult {
    engine
        .internal_composition_call_ids
        .insert(nested_call.id.clone());
    let mut guard = NestedDispatchGuard {
        engine,
        call: nested_call.clone(),
        completed: false,
    };
    Box::pin(guard.engine.dispatch_tool_call(nested_call, results)).await;
    guard.completed = true;
    drop(guard);
    results.remove(&nested_call.id).unwrap_or_else(|| {
        rightkit_agent_tools::coding_tools::ToolResult::error(
            "governed nested dispatch ended without a typed result",
        )
    })
}

#[cfg(feature = "host")]
fn bridge_error_json(message: &str) -> String {
    serde_json::json!({ "ok": false, "content": message }).to_string()
}

#[cfg(feature = "host")]
async fn wait_for_cancel(
    mut cancel_rx: tokio::sync::watch::Receiver<bool>,
    interrupt_rx: Option<tokio::sync::watch::Receiver<bool>>,
) {
    let mut interrupt_rx = interrupt_rx;
    let mut cancel_open = true;
    let mut interrupt_open = interrupt_rx.is_some();
    loop {
        if *cancel_rx.borrow() || interrupt_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
            return;
        }
        if !cancel_open && !interrupt_open {
            std::future::pending::<()>().await;
        }
        tokio::select! {
            result = cancel_rx.changed(), if cancel_open => {
                cancel_open = result.is_ok();
            }
            result = async {
                match interrupt_rx.as_mut() {
                    Some(rx) => rx.changed().await.is_ok(),
                    None => false,
                }
            }, if interrupt_open => {
                interrupt_open = result;
            }
        }
    }
}

#[cfg(not(feature = "host"))]
async fn run_js_call(
    _engine: &mut SessionEngine,
    _call: &ToolCall,
    source: &str,
    _timeout_ms: u64,
    _cancel_rx: tokio::sync::watch::Receiver<bool>,
) -> rightkit_agent_tools::coding_tools::ToolResult {
    let _ = source;
    rightkit_agent_tools::coding_tools::ToolResult::error(
        "exec JavaScript composition is unavailable on this build (no host QuickJS runtime)",
    )
}

#[cfg(feature = "host")]
async fn run_js_call(
    engine: &mut SessionEngine,
    call: &ToolCall,
    source: &str,
    timeout_ms: u64,
    cancel_rx: tokio::sync::watch::Receiver<bool>,
) -> rightkit_agent_tools::coding_tools::ToolResult {
    if source.len() > MAX_SOURCE_BYTES {
        return rightkit_agent_tools::coding_tools::ToolResult::error(format!(
            "exec source exceeds the {MAX_SOURCE_BYTES}-byte limit ({} bytes)",
            source.len()
        ));
    }
    let timeout_ms = timeout_ms.clamp(1, MAX_TIMEOUT_MS);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let permit = match tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        JS_RUNTIME_PERMITS.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        Ok(Err(_)) => {
            return rightkit_agent_tools::coding_tools::ToolResult::error(
                "exec JavaScript runtime pool is closed",
            )
        }
        Err(_) => {
            return rightkit_agent_tools::coding_tools::ToolResult::error(format!(
                "exec timed out after {timeout_ms}ms waiting for a JavaScript runtime slot"
            ))
        }
    };

    let sid = engine.session_id.clone();
    let cancel_tokens = engine.cancel_tokens.clone();
    let interrupt_rx = engine.interrupt_rx.clone();
    let abort = Arc::new(AtomicBool::new(false));
    let (req_tx, mut req_rx) = tokio::sync::mpsc::unbounded_channel::<BridgeRequest>();
    let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<Result<String, String>>();
    let source_owned = source.to_string();
    let abort_for_thread = abort.clone();
    let spawned = std::thread::Builder::new()
        .name("exec-quickjs".to_string())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let _permit = permit;
            let result = run_js_runtime(source_owned, req_tx, abort_for_thread, deadline);
            let _ = done_tx.send(result);
        });
    if let Err(error) = spawned {
        return rightkit_agent_tools::coding_tools::ToolResult::error(format!(
            "exec failed to start the JavaScript runtime thread: {error}"
        ));
    }

    let tokio_deadline = tokio::time::Instant::from_std(deadline);
    let cancel_check_rx = cancel_rx.clone();
    let interrupt_check_rx = interrupt_rx.clone();
    let mut nested_results = HashMap::new();
    let mut nested_count = 0usize;
    let mut nested_bytes = 0usize;
    let mut limit_exceeded: Option<String> = None;
    let mut aborted: Option<String> = None;
    // A composed call may catch a failed nested call and return a value. Keep
    // the provider-facing disposition truthful without copying nested payloads
    // into the outer result.
    let mut nested_failure = false;
    let mut req_closed = false;
    let mut cancel_wait = std::pin::pin!(wait_for_cancel(cancel_rx, interrupt_rx));

    let done: Result<String, String> = loop {
        tokio::select! {
            result = &mut done_rx => {
                let mut unfinished = 0usize;
                while let Ok(request) = req_rx.try_recv() {
                    let _ = request.reply.send(bridge_error_json(
                        "exec returned while a nested capability call was still in flight",
                    ));
                    unfinished += 1;
                }
                let outcome = result
                    .map_err(|_| {
                        "exec JavaScript runtime terminated without a result".to_string()
                    })
                    .and_then(|outcome| outcome);
                break if let Some(reason) = &aborted {
                    Err(reason.clone())
                } else if let Some(reason) = &limit_exceeded {
                    Err(reason.clone())
                } else {
                    outcome.and_then(|output| {
                        if unfinished > 0 {
                            return Err(format!(
                                "exec returned with {unfinished} unfinished nested capability call(s)"
                            ));
                        }
                        if output.len() > MAX_OUTPUT_BYTES {
                            return Err(format!(
                                "exec output exceeds the {MAX_OUTPUT_BYTES}-byte limit ({} bytes)",
                                output.len()
                            ));
                        }
                        Ok(output)
                    })
                };
            }
            request = req_rx.recv(), if !req_closed => {
                match request {
                    None => req_closed = true,
                    Some(request) => {
                        let BridgeRequest {
                            name,
                            args_json,
                            reply,
                        } = request;
                        if aborted.is_some() {
                            let _ = reply.send(bridge_error_json("exec aborted"));
                            continue;
                        }
                        if Instant::now() >= deadline {
                            aborted = Some(format!("exec timed out after {timeout_ms}ms"));
                            abort.store(true, Ordering::SeqCst);
                            let _ = reply.send(bridge_error_json("exec timed out"));
                            continue;
                        }
                        if *cancel_check_rx.borrow()
                            || interrupt_check_rx
                                .as_ref()
                                .is_some_and(|rx| *rx.borrow())
                        {
                            aborted = Some("exec cancelled".to_string());
                            abort.store(true, Ordering::SeqCst);
                            let _ = reply.send(bridge_error_json("exec cancelled"));
                            continue;
                        }
                        nested_count += 1;
                        if nested_count > MAX_NESTED_CALLS {
                            let reason = format!(
                                "exec exceeded the {MAX_NESTED_CALLS} nested-call limit"
                            );
                            limit_exceeded = Some(reason.clone());
                            abort.store(true, Ordering::SeqCst);
                            let _ = reply.send(bridge_error_json(&reason));
                            continue;
                        }
                        if nested_bytes > MAX_NESTED_RESULT_BYTES {
                            let reason = format!(
                                "exec exceeded the {MAX_NESTED_RESULT_BYTES}-byte aggregate nested-result limit"
                            );
                            limit_exceeded = Some(reason.clone());
                            abort.store(true, Ordering::SeqCst);
                            let _ = reply.send(bridge_error_json(&reason));
                            continue;
                        }
                        if let Some(error) = validate_bridge_target(engine, &name, &args_json) {
                            nested_failure = true;
                            let _ = reply.send(bridge_error_json(&error));
                            continue;
                        }
                        let nested_call = ToolCall {
                            id: nested_call_id(&call.id, nested_count),
                            name,
                            arguments_json: args_json,
                        };
                        let outcome = {
                            let service =
                                dispatch_nested(engine, &nested_call, &mut nested_results);
                            tokio::pin!(service);
                            loop {
                                tokio::select! {
                                    biased;
                                    result = &mut service => break Ok(result),
                                    _ = &mut cancel_wait, if aborted.is_none() => {
                                        aborted = Some("exec cancelled".to_string());
                                    }
                                    _ = tokio::time::sleep_until(tokio_deadline), if aborted.is_none() => {
                                        aborted = Some(format!("exec timed out after {timeout_ms}ms"));
                                    }
                                }
                                if aborted.is_some() {
                                    if let Some(sender) = cancel_tokens
                                        .lock()
                                        .unwrap()
                                        .get(&nested_call.id)
                                        .cloned()
                                    {
                                        let _ = sender.send(true);
                                    }
                                    break Err(());
                                }
                            }
                        };
                        let reply_json = match outcome {
                            Ok(result) => {
                                nested_failure |= result.is_error;
                                nested_bytes = nested_bytes.saturating_add(result.content.len());
                                if nested_bytes > MAX_NESTED_RESULT_BYTES {
                                    let reason = format!(
                                        "exec exceeded the {MAX_NESTED_RESULT_BYTES}-byte aggregate nested-result limit"
                                    );
                                    limit_exceeded = Some(reason);
                                    abort.store(true, Ordering::SeqCst);
                                }
                                serde_json::json!({
                                    "ok": !result.is_error,
                                    "content": result.content,
                                })
                                .to_string()
                            }
                            Err(()) => {
                                nested_failure = true;
                                abort.store(true, Ordering::SeqCst);
                                engine
                                    .internal_composition_call_ids
                                    .insert(nested_call.id.clone());
                                // The dispatch guard already recorded a cancelled
                                // row when the pinned future was dropped; only
                                // record here if nothing was written.
                                if nested_results.remove(&nested_call.id).is_none()
                                    && !nested_call_recorded(engine, &nested_call.id)
                                {
                                    engine.record_tool_error(
                                        &sid,
                                        &nested_call,
                                        aborted.clone().unwrap_or_else(|| {
                                            "exec aborted while a nested call was in flight"
                                                .to_string()
                                        }),
                                        &mut nested_results,
                                    );
                                }
                                engine
                                    .internal_composition_call_ids
                                    .remove(&nested_call.id);
                                let settled = nested_results
                                    .remove(&nested_call.id)
                                    .unwrap_or_else(|| {
                                        rightkit_agent_tools::coding_tools::ToolResult::error(
                                            aborted
                                                .clone()
                                                .unwrap_or_else(|| "exec aborted".to_string()),
                                        )
                                    });
                                serde_json::json!({
                                    "ok": !settled.is_error,
                                    "content": settled.content,
                                })
                                .to_string()
                            }
                        };
                        let _ = reply.send(reply_json);
                    }
                }
            }
            _ = &mut cancel_wait, if aborted.is_none() => {
                aborted = Some("exec cancelled".to_string());
                abort.store(true, Ordering::SeqCst);
            }
            _ = tokio::time::sleep_until(tokio_deadline), if aborted.is_none() => {
                aborted = Some(format!("exec timed out after {timeout_ms}ms"));
                abort.store(true, Ordering::SeqCst);
            }
        }
    };
    match done {
        Ok(output) if nested_failure => rightkit_agent_tools::coding_tools::ToolResult::error(
            nested_failure_projection(&output),
        ),
        Ok(output) => rightkit_agent_tools::coding_tools::ToolResult::ok(output),
        Err(error) => rightkit_agent_tools::coding_tools::ToolResult::error(error),
    }
}

#[cfg(feature = "host")]
fn validate_bridge_target(engine: &SessionEngine, name: &str, args_json: &str) -> Option<String> {
    if !COMPOSABLE_TOOL_NAMES.contains(&name) {
        return Some(format!("exec bridge does not expose capability `{name}`"));
    }
    match serde_json::from_str::<serde_json::Value>(args_json) {
        Ok(value) if value.is_object() => {}
        Ok(_) => return Some("exec bridge arguments must be a JSON object".to_string()),
        Err(error) => {
            return Some(format!("exec bridge arguments are not valid JSON: {error}"));
        }
    }
    match engine.tools.get(name) {
        Some(tool) if tool.is_read_only() => None,
        Some(_) => Some(format!(
            "exec bridge refuses mutating capability `{name}`: composition is read-only"
        )),
        None => Some(format!("exec bridge capability `{name}` is unavailable")),
    }
}

#[cfg(feature = "host")]
fn run_js_runtime(
    source: String,
    req_tx: tokio::sync::mpsc::UnboundedSender<BridgeRequest>,
    abort: Arc<AtomicBool>,
    deadline: Instant,
) -> Result<String, String> {
    futures::executor::block_on(async move {
        let runtime = rquickjs::AsyncRuntime::new()
            .map_err(|error| format!("exec JavaScript runtime init failed: {error}"))?;
        runtime.set_memory_limit(JS_MEMORY_LIMIT_BYTES).await;
        runtime.set_max_stack_size(JS_STACK_LIMIT_BYTES).await;
        let abort_flag = abort.clone();
        runtime
            .set_interrupt_handler(Some(Box::new(move || {
                abort_flag.load(Ordering::SeqCst) || Instant::now() >= deadline
            })))
            .await;
        let context = rquickjs::AsyncContext::full(&runtime)
            .await
            .map_err(|error| format!("exec JavaScript context init failed: {error}"))?;
        context
            .async_with(async move |ctx| {
                let outstanding = Arc::new(AtomicUsize::new(0));
                let bridge_outstanding = outstanding.clone();
                let bridge = rquickjs::Function::new(
                    ctx.clone(),
                    rquickjs::function::Async(move |name: String, args_json: String| {
                        let req_tx = req_tx.clone();
                        let outstanding = bridge_outstanding.clone();
                        outstanding.fetch_add(1, Ordering::SeqCst);
                        async move {
                            let (reply_tx, reply_rx) =
                                tokio::sync::oneshot::channel::<String>();
                            let sent = req_tx.send(BridgeRequest {
                                name,
                                args_json,
                                reply: reply_tx,
                            });
                            let reply = if sent.is_err() {
                                bridge_error_json("exec bridge channel closed")
                            } else {
                                reply_rx.await.unwrap_or_else(|_| {
                                    bridge_error_json("exec bridge reply dropped")
                                })
                            };
                            outstanding.fetch_sub(1, Ordering::SeqCst);
                            Ok::<String, rquickjs::Error>(reply)
                        }
                    }),
                )
                .map_err(|error| format!("exec bridge init failed: {error}"))?;
                ctx.globals()
                    .set("__cr_bridge", bridge)
                    .map_err(|error| format!("exec bridge install failed: {error}"))?;
                let program = format!(
                    r#"const __cr_tools = Object.freeze({{
    read: async (args) => {{
        const r = JSON.parse(await globalThis.__cr_bridge("read_file", JSON.stringify(args ?? {{}})));
        if (!r.ok) throw new Error(String(r.content));
        return r.content;
    }},
    search: async (args) => {{
        const r = JSON.parse(await globalThis.__cr_bridge("search", JSON.stringify(args ?? {{}})));
        if (!r.ok) throw new Error(String(r.content));
        return r.content;
    }},
}});
(async () => {{
    const __cr_fn = async function (tools) {{
{source}
    }};
    try {{
        const __cr_value = await __cr_fn(__cr_tools);
        return JSON.stringify({{ ok: true, value: __cr_value === undefined ? null : __cr_value }});
    }} catch (__cr_error) {{
        return JSON.stringify({{
            ok: false,
            error: String(__cr_error && (__cr_error.message || __cr_error.stack) || __cr_error),
        }});
    }}
}})()"#
                );
                let promise: rquickjs::Promise<'_> = ctx
                    .eval(program.as_bytes())
                    .map_err(|error| describe_js_error(&ctx, error))?;
                let settled = promise
                    .into_future::<String>()
                    .await
                    .map_err(|error| describe_js_error(&ctx, error))?;
                if outstanding.load(Ordering::SeqCst) > 0 {
                    return Err(
                        "exec returned while nested capability calls were still unfinished"
                            .to_string(),
                    );
                }
                let payload = serde_json::from_str::<serde_json::Value>(&settled)
                    .map_err(|error| {
                        format!("exec script returned malformed result payload: {error}")
                    })?;
                if payload.get("ok").and_then(|ok| ok.as_bool()) == Some(true) {
                    serde_json::to_string(&payload["value"]).map_err(|error| {
                        format!("exec result serialization failed: {error}")
                    })
                } else {
                    let message = payload
                        .get("error")
                        .and_then(|error| error.as_str())
                        .unwrap_or("exec script failed");
                    Err(format!("exec script failed: {message}"))
                }
            })
            .await
    })
}

#[cfg(feature = "host")]
fn describe_js_error(ctx: &rquickjs::Ctx<'_>, error: rquickjs::Error) -> String {
    let caught = ctx.catch();
    if caught.is_uncatchable_error() {
        return "exec aborted: interrupted by timeout or cancellation".to_string();
    }
    if let Some(exception) = caught.as_exception() {
        let message = exception
            .message()
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| error.to_string());
        return format!("exec script failed: {message}");
    }
    format!("exec script failed: {error}")
}
