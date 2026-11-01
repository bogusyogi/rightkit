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
//! The live wait/interrupt boundary is [`SubagentSupervisor`], carried on
//! [`ToolContext`]. These tools parse, validate, & invoke that typed contract
//! while failing closed when a host has not configured a supervisor. Host
//! ownership of child registration, lifecycle persistence & tool registry
//! exposure remains outside this module-local seam.

use crate::coding_tools::context::{
    SpawnRuntimeKind, SubagentContinuity, SubagentSpawnOptions, SubagentTerminalStatus,
    SubagentWaitOutcome, SubagentWorktreeOptions, ToolContext,
};
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_logs::agent_telemetry::agent_observability::{
    record_agent_observation, AgentObservationV1,
};
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
        "Spawn a specialist sub-agent to handle a specific task. The sub-agent runs to completion \
         and returns its final response. Use when delegating a focused subtask to a role like \
         'challenger', 'designer', or 'lens'."
    }

    fn input_schema(&self) -> Value {
        json!({
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
                            "description": "Optional worktree directory. Defaults to a temporary Agent worktree path."
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
                    "description": "Optional PR-5 runtime selector. When omitted the spawner falls back to the legacy Agent-native session. When set to `{ kind: \"authenticated_cli\", ... }` the daemon routes the spawn through the CLI child executor.",
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": ["authenticated_cli"],
                            "description": "Selector for the runtime that should host the sub-agent. PR 5 only ships `authenticated_cli`; `rightkit_native` falls through to the legacy path."
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
                    "description": "Opt-in only: open a real, visible `rightkit-agent attach` window (the actual interactive TUI) so an operator can watch this subagent work. Defaults to false — spawns are never visible unless explicitly requested. On non-Windows platforms an explicit `true` fails the spawn with a typed unsupported-platform error rather than silently spawning invisibly."
                }
            },
            "required": ["role", "task"]
        })
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
        let visible_window = if options.visible {
            let child_session_id = options
                .child_session_id
                .clone()
                .unwrap_or_else(child_session_id_for_visible_spawn);
            options.child_session_id = Some(child_session_id.clone());
            match crate::coding_tools::visible_subagent_window::spawn_visible_window(
                &child_session_id,
            ) {
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
                        rightkit_logs::agent_telemetry::agent_observability::AgentOutcome::Completed,
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

    let Some(worktree_value) = args.get("worktree") else {
        return Ok(SubagentSpawnOptions {
            worktree: None,
            max_iterations,
            runtime,
            continuity,
            visible,
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
        ..SubagentSpawnOptions::default()
    })
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

    /// Canonical wire form understood by a host [`SubagentSupervisor`].
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
        "Block until every named subagent reaches a terminal status (completed, failed, or \
         cancelled), or until an optional timeout elapses. Accepts bare child agent ids or \
         `parent/child` agent paths."
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
                let Some(supervisor) = &ctx.subagent_supervisor else {
                    return ToolResult::error(
                        "wait_for_subagents: no SubagentSupervisor is wired on this ToolContext",
                    );
                };
                let requested_paths = request
                    .targets
                    .iter()
                    .map(AgentPath::canonical)
                    .collect::<Vec<_>>();
                match supervisor
                    .wait_for(&ctx.session_id, requested_paths.clone(), request.timeout_ms)
                    .await
                {
                    Ok(outcomes) => match validate_wait_outcomes(&requested_paths, &outcomes) {
                        Ok(status) => ToolResult::ok(
                            json!({"status": status, "outcomes": outcomes}).to_string(),
                        ),
                        Err(error) => ToolResult::error(format!(
                            "wait_for_subagents: supervisor returned invalid result: {error}"
                        )),
                    },
                    Err(error) => ToolResult::error(format!(
                        "wait_for_subagents: supervisor wait failed: {error}"
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

/// Allocate a child session id for a `visible` spawn, matching the host's
/// `sub-<unix-millis>-<sequence>` shape so the id is indistinguishable from a
/// host-minted one. Only used when the caller must know the child's identity
/// before the spawn future resolves.
fn child_session_id_for_visible_spawn() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("sub-{millis}-{}", SEQUENCE.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_tools::context::{SubagentSpawnOptions, SubagentSpawner, ToolContext};
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    #[test]
    fn schema_exposes_worktree_isolation_options() {
        let schema = SpawnSubagentTool.input_schema();

        assert!(schema["properties"]["worktree"].is_object());
        assert_eq!(schema["properties"]["worktree"]["type"], "object");
        assert!(schema["properties"]["worktree"]["properties"]["branch"].is_object());
        assert!(schema["properties"]["worktree"]["properties"]["path"].is_object());
        assert!(schema["properties"]["worktree"]["properties"]["cleanup"].is_object());
        assert!(schema["properties"]["maxTurns"].is_object());
        // PR 5 additions: runtime + continuity.
        assert!(schema["properties"]["runtime"].is_object());
        assert_eq!(schema["properties"]["runtime"]["type"], "object");
        assert!(schema["properties"]["runtime"]["properties"]["kind"].is_object());
        assert!(schema["properties"]["runtime"]["properties"]["providerId"].is_object());
        assert!(schema["properties"]["continuity"].is_object());
        assert!(schema["properties"]["continuity"]["properties"]["kind"].is_object());
        assert!(schema["properties"]["continuity"]["properties"]["continueAgentId"].is_object());
        assert_eq!(schema["properties"]["visible"]["type"], "boolean");
    }

    // ── visible-window opt-in ───────────────────────────────────────────

    #[test]
    fn parse_spawn_options_defaults_visible_to_false() {
        let options = parse_spawn_options(&json!({"role": "coder", "task": "fix"})).unwrap();
        assert!(!options.visible);
    }

    #[test]
    fn parse_spawn_options_honors_explicit_visible_true() {
        let options =
            parse_spawn_options(&json!({"role": "coder", "task": "fix", "visible": true})).unwrap();
        assert!(options.visible);
    }

    #[test]
    fn parse_spawn_options_explicit_visible_false_stays_false() {
        let options =
            parse_spawn_options(&json!({"role": "coder", "task": "fix", "visible": false}))
                .unwrap();
        assert!(!options.visible);
    }

    #[test]
    fn parse_spawn_options_rejects_non_boolean_visible() {
        let err = parse_spawn_options(&json!({"role": "coder", "task": "fix", "visible": "yes"}))
            .unwrap_err();
        assert!(err.contains("'visible' must be a boolean"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_threads_visible_true_through_to_spawner_options() {
        let spawner = Arc::new(RecordingSpawner::default());
        let ctx = ToolContext::new(
            "parent",
            std::env::temp_dir(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_spawner(spawner.clone());

        let result = SpawnSubagentTool
            .execute(
                json!({"role": "coder", "task": "fix", "visible": true}),
                &ctx,
            )
            .await;

        // On non-Windows this call fails closed before ever reaching the
        // spawner (VisibleWindowError::Unsupported) — the assertion below
        // covers both platforms: either the spawner recorded visible=true,
        // or the call errored out and the spawner was never invoked at all
        // (never silently downgraded to an invisible spawn).
        if !result.is_error {
            let options = spawner.options.lock().unwrap().clone().unwrap();
            assert!(options.visible);
        } else {
            assert!(spawner.options.lock().unwrap().is_none());
            assert!(
                result.content.contains("visible window"),
                "got: {}",
                result.content
            );
        }
    }

    #[tokio::test]
    async fn execute_default_invisible_never_touches_visible_window_path() {
        let spawner = Arc::new(RecordingSpawner::default());
        let ctx = ToolContext::new(
            "parent",
            std::env::temp_dir(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_spawner(spawner.clone());

        let result = SpawnSubagentTool
            .execute(json!({"role": "coder", "task": "fix"}), &ctx)
            .await;

        assert!(!result.is_error, "{}", result.content);
        let options = spawner.options.lock().unwrap().clone().unwrap();
        assert!(!options.visible);
    }

    #[derive(Default)]
    struct RecordingSpawner {
        options: Mutex<Option<SubagentSpawnOptions>>,
    }

    impl SubagentSpawner for RecordingSpawner {
        fn spawn(
            &self,
            _role: &str,
            _task: &str,
            _parent_session_id: &str,
            options: SubagentSpawnOptions,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
            *self.options.lock().unwrap() = Some(options);
            Box::pin(async { Ok("done".to_string()) })
        }
    }

    #[tokio::test]
    async fn execute_passes_worktree_options_to_spawner() {
        let spawner = Arc::new(RecordingSpawner::default());
        let ctx = ToolContext::new(
            "parent",
            std::env::temp_dir(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_spawner(spawner.clone());

        let result = SpawnSubagentTool
            .execute(
                json!({
                    "role": "coder",
                    "task": "fix the bug",
                    "worktree": {
                        "branch": "codex/fix-bug",
                        "path": "D:/tmp/rightkit-agent-wt",
                        "cleanup": true
                    }
                }),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "{}", result.content);
        let options = spawner.options.lock().unwrap().clone().unwrap();
        let worktree = options.worktree.unwrap();
        assert_eq!(worktree.branch.as_deref(), Some("codex/fix-bug"));
        assert_eq!(worktree.path.as_deref(), Some("D:/tmp/rightkit-agent-wt"));
        assert!(worktree.cleanup);
    }

    #[tokio::test]
    async fn execute_passes_max_turns_to_spawner() {
        let spawner = Arc::new(RecordingSpawner::default());
        let ctx = ToolContext::new(
            "parent",
            std::env::temp_dir(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_spawner(spawner.clone());

        let result = SpawnSubagentTool
            .execute(
                json!({
                    "role": "coder",
                    "task": "fix the bug",
                    "maxTurns": 7
                }),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "{}", result.content);
        let options = spawner.options.lock().unwrap().clone().unwrap();
        assert_eq!(options.max_iterations, Some(7));
        assert!(options.worktree.is_none());
    }

    #[tokio::test]
    async fn execute_passes_authenticated_cli_runtime_to_spawner() {
        let spawner = Arc::new(RecordingSpawner::default());
        let ctx = ToolContext::new(
            "parent",
            std::env::temp_dir(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_spawner(spawner.clone());

        let result = SpawnSubagentTool
            .execute(
                json!({
                    "role": "reviewer",
                    "task": "audit",
                    "runtime": {
                        "kind": "authenticated_cli",
                        "providerId": "codex",
                        "modelId": "o3",
                        "accountProfileId": "codex/default"
                    }
                }),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "{}", result.content);
        let options = spawner.options.lock().unwrap().clone().unwrap();
        match options.runtime {
            Some(crate::coding_tools::context::SpawnRuntimeKind::AuthenticatedCli {
                provider_id,
                model_id,
                account_profile_id,
            }) => {
                assert_eq!(provider_id, "codex");
                assert_eq!(model_id.as_deref(), Some("o3"));
                assert_eq!(account_profile_id, "codex/default");
            }
            other => panic!("expected authenticated_cli runtime, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_passes_continue_agent_continuity_to_spawner() {
        let spawner = Arc::new(RecordingSpawner::default());
        let ctx = ToolContext::new(
            "parent",
            std::env::temp_dir(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_spawner(spawner.clone());

        let result = SpawnSubagentTool
            .execute(
                json!({
                    "role": "coder",
                    "task": "follow up",
                    "continuity": {
                        "kind": "continue_agent",
                        "continueAgentId": "child-abc"
                    }
                }),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "{}", result.content);
        let options = spawner.options.lock().unwrap().clone().unwrap();
        match options.continuity {
            Some(crate::coding_tools::context::SubagentContinuity::ContinueAgent {
                continue_agent_id,
            }) => {
                assert_eq!(continue_agent_id, "child-abc")
            }
            other => panic!("expected ContinueAgent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_rejects_unknown_runtime_kind() {
        let spawner = Arc::new(RecordingSpawner::default());
        let ctx = ToolContext::new(
            "parent",
            std::env::temp_dir(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_spawner(spawner.clone());

        let result = SpawnSubagentTool
            .execute(
                json!({
                    "role": "coder",
                    "task": "fix",
                    "runtime": {"kind": "kiro"}
                }),
                &ctx,
            )
            .await;
        assert!(result.is_error, "{}", result.content);
        assert!(
            result.content.contains("runtime.kind"),
            "got: {}",
            result.content
        );
    }

    // ── S:F10 typed addressing & aggregation (pure logic) ──────────────────

    #[test]
    fn agent_path_parses_bare_child_id() {
        let path = AgentPath::parse("cli-agent-abc123").unwrap();
        assert_eq!(path.parent_agent_id, None);
        assert_eq!(path.agent_id, "cli-agent-abc123");
    }

    #[test]
    fn agent_path_parses_parent_child_path() {
        let path = AgentPath::parse("root-agent/child-agent").unwrap();
        assert_eq!(path.parent_agent_id.as_deref(), Some("root-agent"));
        assert_eq!(path.agent_id, "child-agent");
    }

    #[test]
    fn agent_path_rejects_empty_input() {
        assert!(AgentPath::parse("   ").is_err());
    }

    #[test]
    fn aggregate_wait_outcomes_requires_all_targets() {
        assert_eq!(aggregate_wait_outcomes(&[]), None);
    }

    #[test]
    fn aggregate_wait_outcomes_prioritizes_failed_over_cancelled_and_completed() {
        let a = AgentPath::parse("a").unwrap();
        let b = AgentPath::parse("b").unwrap();
        let c = AgentPath::parse("c").unwrap();
        let statuses = vec![
            (a, SubagentTerminalStatus::Completed),
            (b, SubagentTerminalStatus::Cancelled),
            (c, SubagentTerminalStatus::Failed),
        ];
        assert_eq!(
            aggregate_wait_outcomes(&statuses),
            Some(SubagentTerminalStatus::Failed)
        );
    }

    #[test]
    fn aggregate_wait_outcomes_all_completed_resolves_completed() {
        let a = AgentPath::parse("a").unwrap();
        let b = AgentPath::parse("b").unwrap();
        let statuses = vec![
            (a, SubagentTerminalStatus::Completed),
            (b, SubagentTerminalStatus::Completed),
        ];
        assert_eq!(
            aggregate_wait_outcomes(&statuses),
            Some(SubagentTerminalStatus::Completed)
        );
    }

    #[tokio::test]
    async fn wait_for_subagents_rejects_empty_agent_paths() {
        let ctx = ToolContext::new(
            "parent",
            std::env::temp_dir(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        );
        let result = WaitForSubagentTool
            .execute(json!({"agentPaths": []}), &ctx)
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("agentPaths"));
    }

    #[tokio::test]
    async fn wait_for_subagents_accepts_typed_paths_but_reports_unwired_supervisor() {
        let ctx = ToolContext::new(
            "parent",
            std::env::temp_dir(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        );
        let result = WaitForSubagentTool
            .execute(
                json!({"agentPaths": ["root/child-1", "child-2"], "timeoutMs": 5000}),
                &ctx,
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("SubagentSupervisor"));
    }

    #[tokio::test]
    async fn interrupt_subagent_requires_agent_path() {
        let ctx = ToolContext::new(
            "parent",
            std::env::temp_dir(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        );
        let result = InterruptSubagentTool.execute(json!({}), &ctx).await;
        assert!(result.is_error);
        assert!(result.content.contains("agentPath"));
    }

    #[tokio::test]
    async fn interrupt_subagent_accepts_typed_target_but_reports_unwired_supervisor() {
        let ctx = ToolContext::new(
            "parent",
            std::env::temp_dir(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        );
        let result = InterruptSubagentTool
            .execute(json!({"agentPath": "root/child-1"}), &ctx)
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("SubagentSupervisor"));
    }
}
