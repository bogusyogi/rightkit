//! Bounded TypeScript execution. Nested capabilities always use the host's
//! normal dispatcher; this module owns no policy, approval, or receipt
//! authority. Stage 1 (`ProgramProfile::ReadOnly`) exposes read-only tools.
//! Stage 2 (`ProgramProfile::Workspace`) additionally exposes a fixed,
//! local-workspace mutation allowlist; every such call is one ordinary child
//! tool call that the host admits through the same permission engine and
//! approval path as a direct call. Nothing here can pre-approve a call: the
//! dispatcher trait carries no approval input. Stage 3 (model, classifier and
//! roster calls) is not enabled; its semantics are an open decision.

mod declarations;
mod ts_strip;

use crate::coding_tools::{
    context::ToolContext,
    registry::ToolRegistry,
    tool_trait::{Tool, ToolResult},
};
use serde_json::{json, Value};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

pub const RUN_PROGRAM_NAME: &str = "run_program";
pub const PROGRAM_SOURCE_LIMIT: usize = 64 * 1024;
pub const PROGRAM_OUTPUT_LIMIT: usize = 16 * 1024;
pub const PROGRAM_MAX_CALLS: usize = 32;
pub const PROGRAM_TIMEOUT: Duration = Duration::from_secs(10);
pub const PROGRAM_MEMORY_LIMIT: usize = 64 * 1024 * 1024;
const PROGRAM_STACK_LIMIT: usize = 512 * 1024;
/// Mutating child calls one program may attempt (inside the shared call cap).
pub const PROGRAM_MAX_MUTATIONS: usize = 8;
/// The only mutating tools stage 2 may ever expose. Closed set: external
/// effects (git push, cloud, MCP, messaging, subagents) are never composable.
pub const WORKSPACE_MUTATING_TOOLS: &[&str] = &[
    "write_file",
    "edit_file",
    "multi_edit",
    "apply_patch",
    "undo_edit",
    "shell",
    "powershell",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramProfile {
    /// Stage 1: read-only tools and Membrane retrieval.
    ReadOnly,
    /// Stage 2: read-only tools plus `WORKSPACE_MUTATING_TOOLS`, serialized.
    Workspace,
}

/// Closed, named stage-1 allowlist of local-only readers (spec section 4). It
/// is never derived from `is_read_only()`: network, browser and interactive
/// tools can be read-only yet exfiltrate or block. A new tool is excluded
/// until it is explicitly added here.
pub const PROGRAM_LOCAL_READ_TOOLS: &[&str] = &[
    "read_file",
    "list_files",
    "search",
    "blueprint_search",
    "blueprint_expand",
    "membrane_source_read",
];

/// Network, browser, interactive and recursive tools. Rejected at execution
/// even if a caller-supplied spec list names them.
const PROGRAM_EXCLUDED_TOOLS: &[&str] = &[
    "web_fetch",
    "web_search",
    "browser_fetch",
    "browser_open",
    "browser_read",
    "browser_act",
    "browser_close",
    "browser_console",
    "browser_screenshot",
    "browser_control_click",
    "browser_control_navigate",
    "browser_control_read",
    "browser_control_screenshot",
    "browser_control_type",
    "recursive_fetch",
    "context7",
    "ask_user",
    RUN_PROGRAM_NAME,
    "exec",
    "pull_program_result",
];

fn is_program_local_read(name: &str) -> bool {
    PROGRAM_LOCAL_READ_TOOLS.contains(&name)
}

fn is_program_excluded(name: &str) -> bool {
    PROGRAM_EXCLUDED_TOOLS.contains(&name) || name.starts_with("browser")
}

fn is_workspace_mutating(name: &str) -> bool {
    WORKSPACE_MUTATING_TOOLS.contains(&name)
}

/// Membrane operation -> the tool it dispatches to.
fn membrane_target(name: &str) -> Option<&'static str> {
    match name {
        "blueprint_search" => Some("blueprint_search"),
        "blueprint_expand" => Some("blueprint_expand"),
        "ledger" => Some("membrane_source_read"),
        "pull" => Some("mcp_call"),
        _ => None,
    }
}

/// A Membrane operation is allowed only when its target is in the program
/// allowlist (the same list `tools` uses). `pull` targets the generic MCP
/// bridge, which is never read-only-allowlisted, so it is admitted only when
/// the host supplied its authenticated context dispatcher.
fn membrane_permitted(name: &str, allowed: &[String], pull_enabled: bool) -> bool {
    match name {
        "pull" => pull_enabled,
        other => {
            membrane_target(other).is_some_and(|target| allowed.iter().any(|item| item == target))
        }
    }
}

pub type NestedToolFuture<'a> = Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>>;

struct BridgeRequest {
    namespace: String,
    name: String,
    args: Value,
    response: oneshot::Sender<String>,
}

struct JsCancellation(Arc<std::sync::atomic::AtomicBool>);

impl Drop for JsCancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// The host implements this with its normal EFF-001 dispatch path.
pub trait NestedToolDispatcher: Send + Sync {
    fn invoke<'a>(
        &'a self,
        tool: &'a str,
        args: Value,
        context: &'a ToolContext,
    ) -> NestedToolFuture<'a>;
}

/// Host adapter for authenticated Blueprint, Ledger, and Pull operations.
pub trait ContextToolDispatcher: Send + Sync {
    /// Pull's canonical result reduction before any program result returns to
    /// the model. Hosts must bind this to the authenticated context worker.
    fn compress<'a>(
        &'a self,
        output: String,
        context: &'a ToolContext,
        dispatcher: &'a dyn NestedToolDispatcher,
        parent_task_id: &'a str,
    ) -> NestedToolFuture<'a>;
}

#[derive(Clone)]
pub struct ProgramToolSpec {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

pub struct RunProgramTool {
    tools: Vec<ProgramToolSpec>,
    description: String,
}

impl RunProgramTool {
    pub fn new(registry: &ToolRegistry) -> Self {
        Self::with_profile(registry, ProgramProfile::ReadOnly)
    }

    /// Stage 2 surface. Constructing this grants no authority; the host must
    /// dispatch it with `ProgramProfile::Workspace`.
    pub fn new_workspace(registry: &ToolRegistry) -> Self {
        Self::with_profile(registry, ProgramProfile::Workspace)
    }

    pub fn with_profile(registry: &ToolRegistry, profile: ProgramProfile) -> Self {
        let tools = registry
            .all()
            .iter()
            .filter(|tool| {
                (is_program_local_read(tool.name()) && tool.is_read_only())
                    || (profile == ProgramProfile::Workspace && is_workspace_mutating(tool.name()))
            })
            .filter(|tool| {
                !matches!(
                    tool.name(),
                    RUN_PROGRAM_NAME | "exec" | "pull_program_result"
                )
            })
            .map(|tool| ProgramToolSpec {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                schema: tool.input_schema(),
            })
            .collect();
        let mut this = Self {
            tools,
            description: String::new(),
        };
        let stage = match profile {
            ProgramProfile::ReadOnly => "Stage 1 only: read-only tools and context retrieval. Writes/shell and model, classifier, and roster calls are not enabled.",
            ProgramProfile::Workspace => "Stage 2: read-only tools plus workspace edits and shell. Every mutating call goes through the normal permission and approval path and may be denied or wait for approval; mutations run one at a time. Model, classifier, and roster calls are not enabled.",
        };
        this.description = format!(
            "Run bounded TypeScript/JavaScript using generated API:\n{}\n{stage}",
            this.generated_api()
        );
        this
    }

    pub fn generated_api(&self) -> String {
        let tools = declarations::render_tools_namespace(
            &self
                .tools
                .iter()
                .map(|tool| declarations::ToolDecl {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    schema: tool.schema.clone(),
                })
                .collect::<Vec<_>>(),
        );
        let args_type = |name: &str| {
            self.tools
                .iter()
                .find(|tool| tool.name == name)
                .map(|tool| declarations::schema_to_ts(&tool.schema, 0))
                .unwrap_or_else(|| "Record<string, unknown>".to_string())
        };
        format!("{tools}declare const membrane: {{ blueprint_search(args: {}): Promise<string>; blueprint_expand(args: {}): Promise<string>; ledger(args: {}): Promise<string>; pull(args: Record<string, unknown>): Promise<string>; }};\n",
            args_type("blueprint_search"), args_type("blueprint_expand"), args_type("membrane_source_read"))
    }

    pub fn tool_specs(&self) -> &[ProgramToolSpec] {
        &self.tools
    }
}

#[async_trait::async_trait]
impl Tool for RunProgramTool {
    fn name(&self) -> &str {
        RUN_PROGRAM_NAME
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"code":{"type":"string","description":"TypeScript program to execute."}},"required":["code"],"additionalProperties":false})
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn execute(&self, _args: Value, _context: &ToolContext) -> ToolResult {
        ToolResult::error(
            "run_program must be dispatched through the engine EFF-001 composition bridge",
        )
    }
}

/// Engine-owned execution entry point. Every callback is one child tool call
/// that must be admitted and receipted independently by the host dispatcher.
pub async fn execute_program(
    source: &str,
    tools: &[ProgramToolSpec],
    context: &ToolContext,
    dispatcher: &dyn NestedToolDispatcher,
    context_dispatcher: Option<&dyn ContextToolDispatcher>,
    parent_task_id: &str,
) -> ToolResult {
    execute_program_with_profile(
        ProgramProfile::ReadOnly,
        source,
        tools,
        context,
        dispatcher,
        context_dispatcher,
        parent_task_id,
    )
    .await
}

/// Profile-aware entry point. The profile, not the caller-supplied spec list,
/// decides mutation reach: under `ReadOnly` every `WORKSPACE_MUTATING_TOOLS`
/// name is dropped from the allowlist even if the caller passed it.
pub async fn execute_program_with_profile(
    profile: ProgramProfile,
    source: &str,
    tools: &[ProgramToolSpec],
    context: &ToolContext,
    dispatcher: &dyn NestedToolDispatcher,
    context_dispatcher: Option<&dyn ContextToolDispatcher>,
    parent_task_id: &str,
) -> ToolResult {
    if source.len() > PROGRAM_SOURCE_LIMIT {
        return ToolResult::error(format!(
            "program exceeds {} byte limit",
            PROGRAM_SOURCE_LIMIT
        ));
    }
    let source = match ts_strip::strip_typescript(source) {
        Ok(source) => source,
        Err(error) => return ToolResult::error(error),
    };
    let allowed: Vec<String> = tools
        .iter()
        .filter(|tool| !is_program_excluded(&tool.name))
        .filter(|tool| profile == ProgramProfile::Workspace || !is_workspace_mutating(&tool.name))
        .map(|tool| tool.name.clone())
        .collect();
    let pull_enabled = context_dispatcher.is_some();
    let deadline = Instant::now() + PROGRAM_TIMEOUT;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut result = match run_js(
        source,
        allowed,
        pull_enabled,
        context,
        dispatcher,
        deadline,
        calls,
    )
    .await
    {
        Ok(output) => ToolResult::ok(output),
        Err(error) => return ToolResult::error(error),
    };
    let Some(pull) = context_dispatcher else {
        return ToolResult::error("Pull result compression unavailable");
    };
    result = match tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        pull.compress(result.content, context, dispatcher, parent_task_id),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => return ToolResult::error("run_program timed out during Pull compression"),
    };
    if result.is_error {
        return result;
    }
    if result.content.len() <= PROGRAM_OUTPUT_LIMIT {
        result
    } else {
        ToolResult::error("Pull compressed program result exceeds output byte limit")
    }
}

async fn run_js(
    source: String,
    allowed: Vec<String>,
    pull_enabled: bool,
    context: &ToolContext,
    dispatcher: &dyn NestedToolDispatcher,
    deadline: Instant,
    calls: Arc<AtomicUsize>,
) -> Result<String, String> {
    let mut mutations = 0usize;
    let (request_tx, mut request_rx) = mpsc::channel::<BridgeRequest>(8);
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _cancel_on_drop = JsCancellation(cancelled.clone());
    let js_cancelled = cancelled.clone();
    let js_allowed = allowed.clone();
    let (completed_tx, mut completed_rx) = oneshot::channel();
    std::thread::Builder::new()
        .name("rightkit-agent-run-program-js".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(runtime) => runtime.block_on(run_js_local(
                    source,
                    js_allowed,
                    calls,
                    deadline,
                    request_tx,
                    js_cancelled,
                )),
                Err(error) => Err(format!("run_program runtime init failed: {error}")),
            };
            let _ = completed_tx.send(result);
        })
        .map_err(|error| format!("run_program worker start failed: {error}"))?;
    let mut bridge_open = true;
    loop {
        tokio::select! {
            completed = &mut completed_rx => {
                return completed.map_err(|_| "run_program worker ended without a result".to_string())?;
            }
            request = request_rx.recv(), if bridge_open => {
                let Some(request) = request else {
                    // The JS thread drops its bridge sender while tearing down its runtime,
                    // which can race ahead of the completion message. Stop polling the closed
                    // bridge and wait for the program's real result (or the deadline).
                    bridge_open = false;
                    continue;
                };
                let result = if request.namespace == "tools" {
                    if !allowed.iter().any(|item| item == &request.name) {
                        ToolResult::error("tool is outside the program allowlist")
                    } else if is_workspace_mutating(&request.name) && mutations >= PROGRAM_MAX_MUTATIONS {
                        ToolResult::error("program mutation limit exceeded")
                    } else {
                        if is_workspace_mutating(&request.name) {
                            mutations += 1;
                        }
                        // Awaited inline: mutations are serialized, and the host
                        // dispatcher applies permission and approval per call.
                        invoke_before_deadline(dispatcher, &request.name, request.args, context, deadline).await
                    }
                } else if request.namespace == "membrane" {
                    if !membrane_permitted(&request.name, &allowed, pull_enabled) {
                        let _ = request.response.send(json!({"ok":false,"content":"Membrane operation is outside the program allowlist"}).to_string());
                        continue;
                    }
                    let (tool, args) = match request.name.as_str() {
                        "blueprint_search" => ("blueprint_search", request.args),
                        "blueprint_expand" => ("blueprint_expand", request.args),
                        "ledger" => ("membrane_source_read", request.args),
                        "pull" => ("mcp_call", json!({"name":"mcp__membrane__pull","arguments":request.args})),
                        _ => {
                            let _ = request.response.send(json!({"ok":false,"content":"unknown Membrane program operation"}).to_string());
                            continue;
                        }
                    };
                    invoke_before_deadline(dispatcher, tool, args, context, deadline).await
                } else {
                    ToolResult::error("unknown program API namespace")
                };
                let _ = request.response.send(json!({"ok":!result.is_error,"content":result.content}).to_string());
            }
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                return Err("run_program timed out".to_string());
            }
        }
    }
}

/// QuickJS state stays entirely inside its blocking thread. Its only bridge
/// back to engine dispatch is the owned channel, so no JS pointer or context
/// crosses an async task boundary.
async fn run_js_local(
    source: String,
    allowed: Vec<String>,
    calls: Arc<AtomicUsize>,
    deadline: Instant,
    request_tx: mpsc::Sender<BridgeRequest>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
) -> Result<String, String> {
    let runtime = rquickjs::AsyncRuntime::new()
        .map_err(|e| format!("JavaScript runtime init failed: {e}"))?;
    runtime.set_memory_limit(PROGRAM_MEMORY_LIMIT).await;
    runtime.set_max_stack_size(PROGRAM_STACK_LIMIT).await;
    runtime
        .set_interrupt_handler(Some(Box::new(move || {
            cancelled.load(Ordering::SeqCst) || Instant::now() >= deadline
        })))
        .await;
    let js_context = rquickjs::AsyncContext::full(&runtime)
        .await
        .map_err(|e| e.to_string())?;
    let callback_tx = request_tx;
    let js_allowed = allowed;
    js_context.async_with(async move |ctx| {
        let names = serde_json::to_string(&js_allowed).map_err(|e| e.to_string())?;
        let bridge = rquickjs::Function::new(ctx.clone(), rquickjs::function::Async(move |namespace: String, name: String, args_json: String| {
            let sender = callback_tx.clone();
            let calls = calls.clone();
            async move {
                if calls.fetch_add(1, Ordering::SeqCst) >= PROGRAM_MAX_CALLS {
                    return Ok::<String, rquickjs::Error>(json!({"ok":false,"content":"program call limit exceeded"}).to_string());
                }
                let args = serde_json::from_str::<Value>(&args_json).unwrap_or(Value::Null);
                let (response, receive) = oneshot::channel();
                if sender.send(BridgeRequest { namespace, name, args, response }).await.is_err() {
                    return Ok(json!({"ok":false,"content":"program host bridge closed"}).to_string());
                }
                Ok(receive.await.unwrap_or_else(|_| json!({"ok":false,"content":"program host bridge dropped response"}).to_string()))
            }
        })).map_err(|e| e.to_string())?;
        ctx.globals().set("__cr_bridge", bridge).map_err(|e| e.to_string())?;
        let program = format!(r#"const membrane = Object.freeze({{
  blueprint_search: async (args) => __cr_call("membrane", "blueprint_search", args),
  blueprint_expand: async (args) => __cr_call("membrane", "blueprint_expand", args),
  ledger: async (args) => __cr_call("membrane", "ledger", args),
  pull: async (args) => __cr_call("membrane", "pull", args),
}});
async function __cr_call(ns, name, args) {{ const raw = JSON.parse(await __cr_bridge(ns, name, JSON.stringify(args ?? {{}}))); if (!raw.ok) throw new Error(String(raw.content)); return raw.content; }}
const tools = Object.freeze(Object.fromEntries({names}.map(name => [name, async args => __cr_call("tools", name, args)])));
(async()=>{{ try {{ const value = await (async()=>{{
{source}
}})(); return JSON.stringify({{ok:true,value:value===undefined?null:value}}); }} catch(e) {{ return JSON.stringify({{ok:false,error:String(e?.message||e)}}); }} }})()"#);
        let promise: rquickjs::Promise<'_> = ctx.eval(program.as_bytes()).map_err(|e| e.to_string())?;
        let settled = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), promise.into_future::<String>()).await.map_err(|_| "run_program timed out".to_string())?.map_err(|e| e.to_string())?;
        let payload: Value = serde_json::from_str(&settled).map_err(|e| format!("invalid program result: {e}"))?;
        if payload["ok"] == true { serde_json::to_string(&payload["value"]).map_err(|e| e.to_string()) }
        else { Err(format!("program failed: {}", payload["error"].as_str().unwrap_or("unknown error"))) }
    }).await
}

/// The deadline gates admission only. Once a nested call is admitted it runs
/// to completion on the engine's own cancel path (the dispatcher observes
/// `context.cancel_rx`); the future is never dropped by a timeout, so an
/// in-flight effect is never abandoned. Always returns a typed outcome.
async fn invoke_before_deadline(
    dispatcher: &dyn NestedToolDispatcher,
    tool: &str,
    args: Value,
    context: &ToolContext,
    deadline: Instant,
) -> ToolResult {
    if Instant::now() >= deadline {
        return ToolResult::error("run_program deadline reached; nested call not attempted");
    }
    if context.cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        return ToolResult::error("run_program cancelled; nested call not attempted");
    }
    dispatcher.invoke(tool, args, context).await
}

#[cfg(test)]
mod tests;
