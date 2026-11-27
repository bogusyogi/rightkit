//! `spawn_subagent` tool — spawns a specialist sub-agent for a given role and task.
//!
//! S:F10 — typed subagent wait/interrupt tools (Codex `multi_agents_v2`
//! children). Codex's donor mechanism
//! (`core/src/tools/handlers/multi_agents_v2.rs`) addresses every spawned
//! child by an `AgentPath` and offers `spawn` / **`wait`** /
//! `send_message` / **`interrupt`** / `followup_task` over
//! `InterAgentMessage` items. Per the Port-first rule (priority 2: port the
//! donor's contract & test shapes faithfully into Rust), [`AgentPath`],
//! [`WaitForSubagentTool`] and [`InterruptSubagentTool`] below carry that
//! same typed addressing and aggregation contract. `followup_task` is
//! implemented in `send_message.rs` (steering an existing child fits that
//! file's message-delivery domain).
//!
//! The live wait/interrupt boundary is
//! [`SubagentSupervisor`](crate::context::SubagentSupervisor), carried on
//! [`ToolContext`]. These tools parse, validate, & invoke that typed contract
//! while failing closed when a host has not configured a supervisor. Host
//! ownership of child registration, lifecycle persistence & tool registry
//! exposure remains outside this module-local seam.

use crate::context::{
    SpawnRuntimeKind, SubagentContinuity, SubagentSpawnOptions, SubagentTerminalStatus,
    SubagentWaitOutcome, SubagentWorktreeOptions, ToolContext,
};
use crate::tool_trait::{Tool, ToolResult};
use rightkit_agent_telemetry::agent_observability::{record_agent_observation, AgentObservationV1};
use serde_json::{json, Value};

pub struct SpawnSubagentTool;

/// Wall-clock milliseconds for [`AgentObservationV1`] timestamps.
///
/// `unwrap_or(0)` is deliberate: a clock before the Unix epoch is not a
/// plausible reading here, and a spawn must not fail because the system clock
/// is nonsense. A zero timestamp is visibly wrong in the observation log,
/// which is the honest outcome.
fn observation_now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[async_trait::async_trait]
impl Tool for SpawnSubagentTool {
    fn name(&self) -> &str {
        "spawn_subagent"
    }

    fn description(&self) -> &str {
        if crate::tool_options::spawn_subagent_extended() {
            "Spawn a sub-agent for a focused task that is independent of your next step and big enough \
             to pay for its own context. Do not spawn for a small task, a single lookup or work that \
             needs what you already hold: use none for a simple task and a few at most for a broad one. \
             Brief it completely, because it sees only `task`: the objective, the output you want back \
             and what it must not touch. By default the sub-agent runs to \
             completion and returns its final response. Pass `background: true` to \
             return `{agentId, status: \"running\" | \"queued\"}` immediately instead and keep working \
             (`queued`: the session's concurrent sub-agent cap is full, so the child starts when a slot \
             frees); then call `wait_for_subagents` with that agentId, which returns the child's final \
             response. Pass `bare: true` for a zero-context child (a reviewer): it loads no instruction \
             files, skills, plugin prompts, persona, MCP tools or checkpoints and sees only the task \
             you give it. Pass `fork: true` for a child that starts from this conversation so far (it sees \
             everything said and done before this call) and gets the task as its next message, instead \
             of starting empty. Roles include 'challenger', 'designer' and 'lens'."
        } else {
            "Spawn a specialist sub-agent to handle a specific task. The sub-agent runs to completion \
             and returns its final response. Use when delegating a focused subtask to a role like \
             'challenger', 'designer', or 'lens'."
        }
    }

    fn input_schema(&self) -> Value {
        let extended = crate::tool_options::spawn_subagent_extended();
        let app = rightkit_agent_config::app_name();
        let host = rightkit_agent_config::host_slug();
        let (runtime_description, runtime_kind_description, visible_description) = if extended {
            (
                format!("Optional. Run the sub-agent in a signed-in vendor CLI instead of {app}'s own session. Omit to use {app}'s own session."),
                "Which runtime hosts the sub-agent. The only value is the signed-in vendor CLI.".to_string(),
                format!("Opt-in only: open a real, visible `{host} attach` window (the actual interactive TUI) so an operator can watch this subagent work. Defaults to false \u{2014} spawns are never visible unless explicitly requested. On non-Windows platforms an explicit `true` fails the spawn with a typed unsupported-platform error rather than silently spawning invisibly."),
            )
        } else {
            (
                "Optional PR-5 runtime selector. When omitted the spawner falls back to the legacy Agent-native session. When set to `{ kind: \"authenticated_cli\", ... }` the daemon routes the spawn through the CLI child executor.".to_string(),
                "Selector for the runtime that should host the sub-agent. PR 5 only ships `authenticated_cli`; `rightkit_native` falls through to the legacy path.".to_string(),
                "Opt-in only: open a real, visible `rightkit-agent attach` window (the actual interactive TUI) so an operator can watch this subagent work. Defaults to false \u{2014} spawns are never visible unless explicitly requested. On non-Windows platforms an explicit `true` fails the spawn with a typed unsupported-platform error rather than silently spawning invisibly.".to_string(),
            )
        };
        let mut schema = json!({
            "type": "object",
            "properties": {
                "role": {
                    "type": "string",
                    "description": "The agent role to spawn (e.g. 'coder', 'designer', 'researcher')"
                },
                "task": {
                    "type": "string",
                    "description": "The full task description for the sub-agent"
                },
                "worktree": {
                    "type": "object",
                    "description": "Optional isolated git worktree for the sub-agent.",
                    "properties": {
                        "branch": {
                            "type": "string",
                            "description": "Branch name to check out or create for the sub-agent."
                        },
                        "path": {
                            "type": "string",
                            "description": format!("Optional worktree directory. Defaults to a temporary {app} worktree path.")
                        },
                        "cleanup": {
                            "type": "boolean",
                            "description": "Remove the worktree after the sub-agent finishes. Defaults to false."
                        }
                    }
                },
                "maxTurns": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional maximum LLM/tool iterations for the sub-agent."
                },
                "runtime": {
                    "type": "object",
                    "description": runtime_description,
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": ["authenticated_cli"],
                            "description": runtime_kind_description
                        },
                        "providerId": {
                            "type": "string",
                            "description": "Provider id declared by the built-in registry (claude, codex, grok, cursor, cline, commandcode)."
                        },
                        "modelId": {
                            "type": "string",
                            "description": "Optional model id override; the adapter queries the live catalogue if omitted."
                        },
                        "accountProfileId": {
                            "type": "string",
                            "description": "Account profile id; resolves to `<provider>/<label>`."
                        }
                    }
                },
                "continuity": {
                    "type": "object",
                    "description": "Optional continuity preference. Default (`new`) creates a fresh child; `continue_agent` re-attaches to a previously-issued child.",
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": ["new", "continue_agent"]
                        },
                        "continueAgentId": {
                            "type": "string",
                            "description": "Child agent id to re-attach to when `kind == continue_agent`."
                        }
                    }
                },
                "visible": {
                    "type": "boolean",
                    "description": visible_description
                }
            },
            "required": ["role", "task"]
        });
        if extended {
            if let Some(properties) = schema["properties"].as_object_mut() {
                properties.insert("background".into(), json!({
                    "type": "boolean",
                    "description": "Optional, default false (blocking). When true the sub-agent runs in the background: this call returns `{\"agentId\": ..., \"status\": \"running\"}` immediately (status is `\"queued\"` when this session's concurrent sub-agent cap is already full: the child then starts by itself when a slot frees), and `wait_for_subagents` with that agentId later returns the child's final `response` (or `error`). Cannot be combined with `visible: true`. A background child keeps running after this turn ends; stop it with `interrupt_subagent`."
                }));
                properties.insert("fork".into(), json!({
                    "type": "boolean",
                    "description": "Optional, default false. Context fork: the child starts from this session's conversation so far (completed messages only) and receives `task` as its next user message, instead of an empty context. Runs under the same permission rules as any spawn. Cannot be combined with `bare: true`, a `continue_agent` continuity or an `authenticated_cli` runtime."
                }));
                properties.insert("handoff_summary".into(), json!({
                    "type": "boolean",
                    "description": "Optional, default false. Summary start: the child starts from a short summary of this session so far (each earlier user message, cut to 500 characters, as one message) and receives `task` as its next user message, instead of an empty context or the whole transcript. No model call is made for the summary. Cannot be combined with `fork: true`, `bare: true`, a `continue_agent` continuity or an `authenticated_cli` runtime."
                }));
                properties.insert("bare".into(), json!({
                    "type": "boolean",
                    "description": "Optional, default false. Zero-context child for reviewers: no AGENTS.md/CLAUDE.md or other instruction files, skills, plugin prompts, persona or instincts, and it also runs without MCP tools and without checkpoints; it sees only the task payload and its built-in tools. Not supported with an `authenticated_cli` runtime."
                }));
            }
        }
        schema
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let role = match args.get("role").and_then(|v| v.as_str()) {
            Some(r) => r,
            None => return ToolResult::error("spawn_subagent: 'role' is required"),
        };
        let task = match args.get("task").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return ToolResult::error("spawn_subagent: 'task' is required"),
        };
        let options = match parse_spawn_options(&args) {
            Ok(options) => options,
            Err(e) => return ToolResult::error(e),
        };
        // FM-086 — `SubagentSpawner::spawn` is an awaited operation whose
        // contract returns the child's final response, so both terminal
        // outcomes can be recorded here. The separate wait/interrupt tools
        // remain supervisor-backed for callers that need to control a child
        // through another lifecycle surface.
        // Visible-window opt-in (S:F-visible-subagent): resolved *before* the
        // spawner is invoked, and any failure here returns an error without
        // ever calling `spawner.spawn` — an explicit request for a visible
        // window must never silently degrade into an invisible spawn.
        //
        // The window attaches to the CHILD's session, not the parent's. The
        // host normally mints the child id inside the spawn future, where no
        // caller could observe it in time, so a visible spawn allocates the
        // id here and hands it to the host through
        // `SubagentSpawnOptions::child_session_id`, which the host uses
        // verbatim instead of minting its own.
        let mut options = options;
        let extended = crate::tool_options::spawn_subagent_extended();
        if extended {
            options.tool_call_id = Some(ctx.tool_call_id.clone()).filter(|id| !id.is_empty());
            // F-11: opt-in non-blocking spawn. Anything but a literal boolean is
            // rejected (never coerced); `visible` is incompatible because the
            // visible window's lifetime is tied to an awaited spawn.
            let background = match args.get("background") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => {
                    return ToolResult::error("spawn_subagent: 'background' must be a boolean")
                }
            };
            if background {
                if options.visible {
                    return ToolResult::error(
                        "spawn_subagent: 'background: true' cannot be combined with 'visible: true'",
                    );
                }
                return match &ctx.spawner {
                    Some(spawner) => spawn_in_background(spawner, role, task, ctx, options),
                    None => ToolResult::error(
                        "spawn_subagent: orchestration not available (no spawner configured)",
                    ),
                };
            }
        }
        let visible_window = if options.visible {
            let child_session_id = options
                .child_session_id
                .clone()
                .unwrap_or_else(child_session_id_for_visible_spawn);
            options.child_session_id = Some(child_session_id.clone());
            match crate::visible_subagent_window::spawn_visible_window(&child_session_id) {
                Ok(handle) => Some(handle),
                Err(error) => {
                    return ToolResult::error(format!(
                        "spawn_subagent: failed to open visible window: {error}"
                    ))
                }
            }
        } else {
            None
        };

        let spawned_at_unix_ms = observation_now_unix_ms();
        let result = match &ctx.spawner {
            Some(spawner) => match spawner.spawn(role, task, &ctx.session_id, options).await {
                Ok(result) => {
                    record_agent_observation(AgentObservationV1::ended(
                        role,
                        ctx.session_id.clone(),
                        spawned_at_unix_ms,
                        observation_now_unix_ms(),
                        rightkit_agent_telemetry::agent_observability::AgentOutcome::Completed,
                    ));
                    ToolResult::ok(result)
                }
                Err(e) => {
                    // A failed spawn IS a genuine terminal outcome — the child
                    // never came up. No child id was ever assigned, so the
                    // requested role is the only stable identity available.
                    record_agent_observation(AgentObservationV1::failed(
                        role,
                        ctx.session_id.clone(),
                        spawned_at_unix_ms,
                        observation_now_unix_ms(),
                        e.clone(),
                    ));
                    ToolResult::error(format!("sub-agent failed: {e}"))
                }
            },
            None => ToolResult::error(
                "spawn_subagent: orchestration not available (no spawner configured)",
            ),
        };
        // Close the visible window once the awaited spawn reaches a
        // terminal state (success or failure) — the enforced half of "no
        // orphan consoles": the window's process lifetime is tied to this
        // await, not left to `host app attach` noticing the daemon side
        // has gone away on its own.
        if let Some(handle) = visible_window {
            handle.close();
        }
        result
    }
}

fn parse_spawn_options(args: &Value) -> Result<SubagentSpawnOptions, String> {
    let max_iterations = match args.get("maxTurns") {
        Some(value) => {
            let raw = value.as_u64().ok_or_else(|| {
                "spawn_subagent: 'maxTurns' must be a positive integer".to_string()
            })?;
            if raw == 0 || raw > u32::MAX as u64 {
                return Err("spawn_subagent: 'maxTurns' must be between 1 and 4294967295".into());
            }
            Some(raw as u32)
        }
        None => None,
    };

    let runtime = SpawnRuntimeKind::from_tool_args(args.get("runtime").unwrap_or(&Value::Null))?;
    let continuity =
        SubagentContinuity::from_tool_args(args.get("continuity").unwrap_or(&Value::Null))?;
    // Explicit opt-in only: anything other than a literal JSON `true` (a
    // missing field, `false`, `null`, or a malformed non-boolean value)
    // stays invisible. A malformed value must not be silently coerced to
    // `true`, so it is rejected outright rather than guessed at.
    let visible = match args.get("visible") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("spawn_subagent: 'visible' must be a boolean".to_string()),
    };

    // `bare`, `fork` and `handoff_summary` exist only with `set_spawn_subagent_extended`; without
    // it the arguments are ignored.
    let (bare, fork, handoff_summary) = if crate::tool_options::spawn_subagent_extended() {
        parse_start_modes(args, &runtime, &continuity)?
    } else {
        (false, false, false)
    };

    let Some(worktree_value) = args.get("worktree") else {
        return Ok(SubagentSpawnOptions {
            worktree: None,
            max_iterations,
            runtime,
            continuity,
            visible,
            bare,
            fork,
            handoff_summary,
            ..SubagentSpawnOptions::default()
        });
    };
    if !worktree_value.is_object() {
        return Err("spawn_subagent: 'worktree' must be an object".into());
    }
    let branch = worktree_value
        .get("branch")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let path = worktree_value
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let cleanup = worktree_value
        .get("cleanup")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if branch.is_none() && path.is_none() {
        return Err("spawn_subagent: 'worktree.branch' or 'worktree.path' is required".into());
    }
    Ok(SubagentSpawnOptions {
        worktree: Some(SubagentWorktreeOptions {
            branch,
            path,
            cleanup,
        }),
        max_iterations,
        runtime,
        continuity,
        visible,
        bare,
        fork,
        handoff_summary,
        ..SubagentSpawnOptions::default()
    })
}

/// The `(bare, fork, handoff_summary)` start modes of a spawn. Each is a strict boolean, never
/// coerced. A forked child carries the parent's context, so it cannot also be zero-context,
/// re-attach to another child, or run in a vendor CLI; a summary start is a third way to start a
/// child (empty, whole transcript, summary), so it excludes the other two and the same
/// combinations a fork excludes.
fn parse_start_modes(
    args: &Value,
    runtime: &Option<SpawnRuntimeKind>,
    continuity: &Option<SubagentContinuity>,
) -> Result<(bool, bool, bool), String> {
    // Zero-context child (for reviewers). Strict boolean, never coerced.
    let bare = match args.get("bare") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("spawn_subagent: 'bare' must be a boolean".to_string()),
    };

    // Context fork: strict boolean, never coerced.
    let fork = match args.get("fork") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("spawn_subagent: 'fork' must be a boolean".to_string()),
    };
    if fork {
        if bare {
            return Err("spawn_subagent: 'fork: true' cannot be combined with 'bare: true'".into());
        }
        if matches!(continuity, Some(SubagentContinuity::ContinueAgent { .. })) {
            return Err(
                "spawn_subagent: 'fork: true' cannot be combined with continuity 'continue_agent'"
                    .into(),
            );
        }
        if matches!(runtime, Some(SpawnRuntimeKind::AuthenticatedCli { .. })) {
            return Err(
                "spawn_subagent: 'fork: true' is not supported with runtime 'authenticated_cli' (the vendor CLI builds its own context)"
                    .into(),
            );
        }
    }

    // Summary start: strict boolean.
    let handoff_summary = match args.get("handoff_summary") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("spawn_subagent: 'handoff_summary' must be a boolean".to_string()),
    };
    if handoff_summary {
        if fork {
            return Err(
                "spawn_subagent: 'handoff_summary: true' cannot be combined with 'fork: true'"
                    .into(),
            );
        }
        if bare {
            return Err(
                "spawn_subagent: 'handoff_summary: true' cannot be combined with 'bare: true'"
                    .into(),
            );
        }
        if matches!(continuity, Some(SubagentContinuity::ContinueAgent { .. })) {
            return Err(
                "spawn_subagent: 'handoff_summary: true' cannot be combined with continuity 'continue_agent'"
                    .into(),
            );
        }
        if matches!(runtime, Some(SpawnRuntimeKind::AuthenticatedCli { .. })) {
            return Err(
                "spawn_subagent: 'handoff_summary: true' is not supported with runtime 'authenticated_cli' (the vendor CLI builds its own context)"
                    .into(),
            );
        }
    }
    Ok((bare, fork, handoff_summary))
}

/// Addresses one spawned subagent, matching Codex's `AgentPath` shape:
/// a bare child id, or a `parent/child` path for a grandchild spawned by a
/// subagent rather than the root session. the host app's flat `child_agent_id`
/// namespace (`cli-agent-<token>`, see `cli_delegation.rs`) makes the
/// `parent` segment optional metadata rather than a required routing key,
/// but the parse/format contract is preserved so a future nested-spawn
/// lane can populate it without a schema break.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPath {
    pub parent_agent_id: Option<String>,
    pub agent_id: String,
}

impl AgentPath {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("agent path is empty".to_string());
        }
        match raw.split_once('/') {
            Some((parent, child)) if !parent.is_empty() && !child.is_empty() => Ok(Self {
                parent_agent_id: Some(parent.to_string()),
                agent_id: child.to_string(),
            }),
            _ => Ok(Self {
                parent_agent_id: None,
                agent_id: raw.to_string(),
            }),
        }
    }

    /// Canonical wire form understood by a host
    /// [`SubagentSupervisor`](crate::context::SubagentSupervisor).
    pub fn canonical(&self) -> String {
        match &self.parent_agent_id {
            Some(parent) => format!("{parent}/{}", self.agent_id),
            None => self.agent_id.clone(),
        }
    }
}

/// Blocking-wait request: one or more agent paths, all of which must reach
/// a terminal status before the wait resolves (Codex's donor semantics —
/// `wait` blocks on the full set, not the first arrival).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitForSubagentsRequest {
    pub targets: Vec<AgentPath>,
    pub timeout_ms: Option<u64>,
}

/// Aggregate a set of already-known terminal statuses the same way the
/// live wait tool eventually will: every target must be terminal for the
/// aggregate to resolve; any `Failed`/`Cancelled` target marks the
/// aggregate result accordingly rather than silently succeeding.
pub fn aggregate_wait_outcomes(
    statuses: &[(AgentPath, SubagentTerminalStatus)],
) -> Option<SubagentTerminalStatus> {
    if statuses.is_empty() {
        return None;
    }
    if statuses
        .iter()
        .any(|(_, s)| matches!(s, SubagentTerminalStatus::Failed))
    {
        return Some(SubagentTerminalStatus::Failed);
    }
    if statuses
        .iter()
        .any(|(_, s)| matches!(s, SubagentTerminalStatus::Cancelled))
    {
        return Some(SubagentTerminalStatus::Cancelled);
    }
    Some(SubagentTerminalStatus::Completed)
}

/// `wait_for_subagents` — block until every named child reaches a terminal
/// status, or until `timeoutMs` elapses. A host-provided supervisor must
/// return one terminal outcome for every exact requested path.
pub struct WaitForSubagentTool;

#[async_trait::async_trait]
impl Tool for WaitForSubagentTool {
    fn name(&self) -> &str {
        "wait_for_subagents"
    }

    fn description(&self) -> &str {
        if crate::tool_options::spawn_subagent_extended() {
            "Block until every named subagent reaches a terminal status (completed, failed, or \
             cancelled), or until an optional timeout elapses. Accepts bare child agent ids or \
             `parent/child` agent paths. For children started with `spawn_subagent` `background:true`, \
             each outcome also carries the child's final `response` (or `error`)."
        } else {
            "Block until every named subagent reaches a terminal status (completed, failed, or \
             cancelled), or until an optional timeout elapses. Accepts bare child agent ids or \
             `parent/child` agent paths."
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["agentPaths"],
            "properties": {
                "agentPaths": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minItems": 1,
                    "description": "Agent ids or `parent/child` agent paths to wait on. The wait resolves only after every listed target is terminal."
                },
                "timeoutMs": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional wait ceiling in milliseconds."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        match parse_wait_request(&args) {
            Ok(request) => {
                let started = std::time::Instant::now();
                let requested_paths = request
                    .targets
                    .iter()
                    .map(AgentPath::canonical)
                    .collect::<Vec<_>>();
                // F-11: ids spawned with `background:true` (only with
                // `set_spawn_subagent_extended`) resolve from the in-process mailbox
                // (event-driven, carries the final response); everything else goes through
                // the supervisor.
                let mut background = Vec::new();
                let mut host_paths = Vec::new();
                for (target, path) in request.targets.iter().zip(&requested_paths) {
                    let id = target
                        .agent_id
                        .rsplit('/')
                        .next()
                        .unwrap_or(&target.agent_id);
                    match background_subscribe(&ctx.session_id, id) {
                        Some(rx) => background.push((path.clone(), id.to_string(), rx)),
                        None => host_paths.push(path.clone()),
                    }
                }
                let had_background = !background.is_empty();
                let mut outcomes = Vec::with_capacity(requested_paths.len());
                let mut extras: std::collections::HashMap<String, (&'static str, String)> =
                    std::collections::HashMap::new();
                let mut consumed = Vec::new();
                for (path, id, rx) in background {
                    let remaining = request.timeout_ms.map(|ms| {
                        std::time::Duration::from_millis(ms).saturating_sub(started.elapsed())
                    });
                    let Some(result) = await_background(rx, remaining).await else {
                        return ToolResult::error(
                            "wait_for_subagents: supervisor wait failed: timed out waiting for subagent terminal status",
                        );
                    };
                    let (status, key, text) = match result {
                        Ok(text) => (SubagentTerminalStatus::Completed, "response", text),
                        Err(text) if background_error_is_cancelled(&text) => {
                            (SubagentTerminalStatus::Cancelled, "error", text)
                        }
                        Err(text) => (SubagentTerminalStatus::Failed, "error", text),
                    };
                    outcomes.push(SubagentWaitOutcome {
                        agent_path: path.clone(),
                        status,
                    });
                    extras.insert(path, (key, text));
                    consumed.push(id);
                }
                if !host_paths.is_empty() {
                    let Some(supervisor) = &ctx.subagent_supervisor else {
                        return ToolResult::error(
                            "wait_for_subagents: no SubagentSupervisor is wired on this ToolContext",
                        );
                    };
                    // With background children already waited for, the supervisor gets what is
                    // left of the ceiling; with none, the ceiling as given.
                    let remaining_ms = if had_background {
                        request.timeout_ms.map(|ms| {
                            ms.saturating_sub(started.elapsed().as_millis() as u64)
                                .max(1)
                        })
                    } else {
                        request.timeout_ms
                    };
                    match supervisor
                        .wait_for(&ctx.session_id, host_paths, remaining_ms)
                        .await
                    {
                        Ok(mut host) => outcomes.append(&mut host),
                        Err(error) => {
                            return ToolResult::error(format!(
                                "wait_for_subagents: supervisor wait failed: {error}"
                            ))
                        }
                    }
                }
                match validate_wait_outcomes(&requested_paths, &outcomes) {
                    Ok(status) => {
                        for id in &consumed {
                            background_take(id);
                        }
                        let outcomes_json = outcomes
                            .iter()
                            .map(|outcome| {
                                let mut value =
                                    serde_json::to_value(outcome).unwrap_or(Value::Null);
                                if let (Some(object), Some((key, text))) =
                                    (value.as_object_mut(), extras.get(&outcome.agent_path))
                                {
                                    object.insert((*key).to_string(), Value::String(text.clone()));
                                }
                                value
                            })
                            .collect::<Vec<_>>();
                        ToolResult::ok(
                            json!({"status": status, "outcomes": outcomes_json}).to_string(),
                        )
                    }
                    Err(error) => ToolResult::error(format!(
                        "wait_for_subagents: supervisor returned invalid result: {error}"
                    )),
                }
            }
            Err(e) => ToolResult::error(e),
        }
    }
}

fn validate_wait_outcomes(
    requested_paths: &[String],
    outcomes: &[SubagentWaitOutcome],
) -> Result<SubagentTerminalStatus, String> {
    if outcomes.len() != requested_paths.len() {
        return Err(format!(
            "expected {} terminal outcomes, got {}",
            requested_paths.len(),
            outcomes.len()
        ));
    }
    let mut remaining = requested_paths.to_vec();
    for outcome in outcomes {
        let canonical = AgentPath::parse(&outcome.agent_path)?.canonical();
        let Some(index) = remaining.iter().position(|path| path == &canonical) else {
            return Err(format!("unexpected or duplicate outcome for {canonical}"));
        };
        remaining.remove(index);
    }
    if !remaining.is_empty() {
        return Err(format!(
            "missing terminal outcomes for {}",
            remaining.join(", ")
        ));
    }
    let statuses = outcomes
        .iter()
        .map(|outcome| Ok((AgentPath::parse(&outcome.agent_path)?, outcome.status)))
        .collect::<Result<Vec<_>, String>>()?;
    aggregate_wait_outcomes(&statuses).ok_or_else(|| "no terminal outcomes".to_string())
}

fn parse_wait_request(args: &Value) -> Result<WaitForSubagentsRequest, String> {
    let raw_paths = args
        .get("agentPaths")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or_else(|| "wait_for_subagents: 'agentPaths' must be a non-empty array".to_string())?;
    let mut targets = Vec::with_capacity(raw_paths.len());
    for raw in raw_paths {
        let raw = raw.as_str().ok_or_else(|| {
            "wait_for_subagents: 'agentPaths' entries must be strings".to_string()
        })?;
        targets.push(AgentPath::parse(raw)?);
    }
    let timeout_ms = match args.get("timeoutMs") {
        Some(v) => Some(v.as_u64().filter(|t| *t > 0).ok_or_else(|| {
            "wait_for_subagents: 'timeoutMs' must be a positive integer".to_string()
        })?),
        None => None,
    };
    Ok(WaitForSubagentsRequest {
        targets,
        timeout_ms,
    })
}

/// `interrupt_subagent` — cancel one exact subtree by agent path, scoped
/// stop semantics (does not touch sibling children), matching Codex's
/// `interrupt` handler.
pub struct InterruptSubagentTool;

#[async_trait::async_trait]
impl Tool for InterruptSubagentTool {
    fn name(&self) -> &str {
        "interrupt_subagent"
    }

    fn description(&self) -> &str {
        "Cancel one running subagent (and only that subagent's subtree) by agent id or \
         `parent/child` agent path. Sibling children are unaffected."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["agentPath"],
            "properties": {
                "agentPath": {
                    "type": "string",
                    "description": "Agent id or `parent/child` agent path of the subagent to interrupt."
                },
                "reason": {
                    "type": "string",
                    "description": "Optional human-readable interrupt reason surfaced to the child."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let raw = match args.get("agentPath").and_then(|v| v.as_str()) {
            Some(r) => r,
            None => return ToolResult::error("interrupt_subagent: 'agentPath' is required"),
        };
        match AgentPath::parse(raw) {
            Ok(target) => {
                let Some(supervisor) = &ctx.subagent_supervisor else {
                    return ToolResult::error(
                        "interrupt_subagent: no SubagentSupervisor is wired on this ToolContext",
                    );
                };
                let reason = args
                    .get("reason")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|reason| !reason.is_empty())
                    .map(str::to_string);
                match supervisor
                    .interrupt_subtree(&ctx.session_id, target.canonical(), reason)
                    .await
                {
                    Ok(()) => ToolResult::ok("subagent subtree interrupted"),
                    Err(error) => ToolResult::error(format!(
                        "interrupt_subagent: supervisor interrupt failed: {error}"
                    )),
                }
            }
            Err(e) => ToolResult::error(format!("interrupt_subagent: {e}")),
        }
    }
}

/// Allocate a child session id for a `visible` or background spawn, matching the host's
/// `sub-<unix-millis>-<sequence>` shape (with `set_spawn_subagent_extended`, followed by a random
/// tail: `sub-<unix-millis>-<sequence>-<random>`) so the id is indistinguishable from a
/// host-minted one. Only used when the caller must know the child's identity
/// before the spawn future resolves.
fn child_session_id_for_visible_spawn() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    if crate::tool_options::spawn_subagent_extended() {
        let unique = rightkit_agent_engine_protocol::uuid_v7();
        let tail = &unique[unique.len().saturating_sub(8)..];
        format!("sub-{millis}-{sequence}-{tail}")
    } else {
        format!("sub-{millis}-{sequence}")
    }
}

// ---------------------------------------------------------------------------
// F-11 — non-blocking (`background: true`) spawn and its result mailbox.
// ---------------------------------------------------------------------------

type BackgroundResult = Result<String, String>;
/// Finished mailbox entries nobody collected are dropped after this long.
const BACKGROUND_RETENTION: std::time::Duration = std::time::Duration::from_secs(60 * 60);

struct BackgroundEntry {
    parent_session_id: String,
    tx: tokio::sync::watch::Sender<Option<BackgroundResult>>,
    finished_at: Option<std::time::Instant>,
}

type BackgroundMap = std::collections::HashMap<String, BackgroundEntry>;

fn background_mailbox() -> std::sync::MutexGuard<'static, BackgroundMap> {
    static BACKGROUND: std::sync::OnceLock<std::sync::Mutex<BackgroundMap>> =
        std::sync::OnceLock::new();
    BACKGROUND
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Insert before the child task is spawned so a following wait never races, pruning finished
/// entries older than [`BACKGROUND_RETENTION`] (bounded memory). Reports whether the new child will
/// wait for a slot: `true` when `cap` is `Some` and this parent already has at least `cap` unfinished background
/// children. The count and the insert happen under one lock, so concurrent spawns see each other.
fn background_register_counting(
    child_id: String,
    parent_session_id: String,
    tx: tokio::sync::watch::Sender<Option<BackgroundResult>>,
    cap: Option<usize>,
) -> bool {
    let mut map = background_mailbox();
    map.retain(|_, entry| {
        entry
            .finished_at
            .is_none_or(|finished| finished.elapsed() <= BACKGROUND_RETENTION)
    });
    // `cap`: the host's per-session concurrent cap. This parent's unfinished children (running or
    // already queued) at or over it means the new child waits for a slot.
    let queued = cap.is_some_and(|cap| {
        map.values()
            .filter(|entry| {
                entry.parent_session_id == parent_session_id && entry.finished_at.is_none()
            })
            .count()
            >= cap
    });
    map.insert(
        child_id,
        BackgroundEntry {
            parent_session_id,
            tx,
            finished_at: None,
        },
    );
    queued
}

fn background_finish(child_id: &str, result: BackgroundResult) {
    if let Some(entry) = background_mailbox().get_mut(child_id) {
        entry.finished_at = Some(std::time::Instant::now());
        entry.tx.send_replace(Some(result));
    }
}

/// Receiver for a child spawned in the background by `parent_session_id`.
fn background_subscribe(
    parent_session_id: &str,
    child_id: &str,
) -> Option<tokio::sync::watch::Receiver<Option<BackgroundResult>>> {
    background_mailbox()
        .get(child_id)
        .filter(|entry| entry.parent_session_id == parent_session_id)
        .map(|entry| entry.tx.subscribe())
}

fn background_take(child_id: &str) {
    background_mailbox().remove(child_id);
}

fn background_error_is_cancelled(text: &str) -> bool {
    // The daemon's exact interrupt texts (daemon_state.rs: "sub-agent
    // interrupted"; a queued spawn whose parent was interrupted).
    text == "sub-agent interrupted" || text.starts_with("sub-agent spawn cancelled while queued")
}

/// Event-driven wait for a mailbox result; `None` on timeout (or if the entry
/// vanished without a result).
async fn await_background(
    mut rx: tokio::sync::watch::Receiver<Option<BackgroundResult>>,
    timeout: Option<std::time::Duration>,
) -> Option<BackgroundResult> {
    let wait = async move {
        loop {
            let current = rx.borrow().clone();
            if current.is_some() {
                return current;
            }
            if rx.changed().await.is_err() {
                return None;
            }
        }
    };
    match timeout {
        Some(limit) => tokio::time::timeout(limit, wait).await.ok().flatten(),
        None => wait.await,
    }
}

fn spawn_in_background(
    spawner: &std::sync::Arc<dyn crate::context::SubagentSpawner>,
    role: &str,
    task: &str,
    ctx: &ToolContext,
    mut options: SubagentSpawnOptions,
) -> ToolResult {
    let child_id = options
        .child_session_id
        .clone()
        .unwrap_or_else(child_session_id_for_visible_spawn);
    options.child_session_id = Some(child_id.clone());
    // Only this path may park for a slot: it runs in a detached task, so no turn waits on it.
    options.queue_when_full = true;

    let tx = tokio::sync::watch::Sender::new(None);
    // Count this parent's unfinished background children and insert the new entry under one
    // lock, so two spawns issued in one response see each other. At or over the host's
    // concurrent cap the daemon parks the child until a slot frees: report `queued`.
    let queued = background_register_counting(
        child_id.clone(),
        ctx.session_id.clone(),
        tx,
        spawner.concurrent_cap(&ctx.session_id),
    );

    // `spawn` returns a `'static` boxed future, so it can move into the task.
    let fut = spawner.spawn(role, task, &ctx.session_id, options);
    // No parent-cancel select here: `ctx.cancel_rx` is scoped to this one tool
    // call and its sender drops as soon as this call returns `running`. The
    // child is stopped through `interrupt_subagent` (or ends on its own).
    let role = role.to_string();
    let parent_session_id = ctx.session_id.clone();
    let spawned_at_unix_ms = observation_now_unix_ms();
    let task_child_id = child_id.clone();
    tokio::spawn(async move {
        // The inner task isolates a panicking child so the mailbox still
        // receives a terminal result instead of leaving waiters hanging.
        let result = match tokio::spawn(fut).await {
            Ok(result) => result,
            Err(join_error) => Err(format!("sub-agent task aborted: {join_error}")),
        };
        match &result {
            Ok(_) => record_agent_observation(AgentObservationV1::ended(
                role.as_str(),
                parent_session_id,
                spawned_at_unix_ms,
                observation_now_unix_ms(),
                rightkit_agent_telemetry::agent_observability::AgentOutcome::Completed,
            )),
            Err(error) => record_agent_observation(AgentObservationV1::failed(
                role.as_str(),
                parent_session_id,
                spawned_at_unix_ms,
                observation_now_unix_ms(),
                error.clone(),
            )),
        }
        background_finish(&task_child_id, result);
    });
    let status = if queued { "queued" } else { "running" };
    ToolResult::ok(json!({"agentId": child_id, "status": status}).to_string())
}
