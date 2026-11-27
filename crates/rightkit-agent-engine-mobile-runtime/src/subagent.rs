//! In-process, model-only sub-agent spawner (ENG-37).
//!
//! `spawn_subagent` runs one bounded child turn inside the host process: a
//! child [`SessionEngine`] on the parent's provider route, the same workspace
//! root, the parent's permission mode and workspace trust, and a tool registry
//! filtered by `is_mobile_supported_tool` minus `spawn_subagent` (no recursion)
//! and minus the two tools that need a person (`ask_user`, `exit_plan_mode`).
//!
//! Wired only when the host turned on [`crate::host_options::set_subagents`].
//!
//! # Authority
//!
//! The child inherits the parent's authority and never widens it:
//!
//! * It is built from the parent's permission mode and workspace trust, never
//!   from `SessionEngineConfig::default()` (Bypass, Trusted). A permission mode
//!   named in the spawn options is ignored.
//! * It has its own private conduit with no human subscriber, so any approval
//!   card it would raise is denied as `unattended` instead of reaching a
//!   person who is looking at the parent's card.
//! * It is bounded to 8 iterations and a cost ceiling no larger than the
//!   parent's ceiling less what earlier children spent (the parent's own
//!   spend is read best-effort from its latest `usageUpdated` event).
//! * It listens to the parent's interrupt channel, so interrupting the parent
//!   interrupts the child.
//! * It gets neither the GitHub nor the Cloudflare credential, no MCP servers
//!   and no worktree; only one child runs at a time. A caller-chosen child
//!   session id is ignored.
//!
//! The parent sees `subAgentSpawned` and then `subAgentCompleted` or
//! `subAgentCancelled`, emitted on the parent's conduit.

use crate::{llm, ProviderConfig};
use rightkit_agent_coding_tools::context::{
    SpawnRuntimeKind, SubagentContinuity, SubagentSpawnOptions,
};
use rightkit_agent_coding_tools::registry::ToolRegistry;
use rightkit_agent_engine_core::conduit::Conduit;
use rightkit_agent_engine_core::engine::{EngineError, SessionEngine, SessionEngineConfig};
use rightkit_agent_engine_protocol::{now_iso, ProtocolEvent, TranscriptMessage};
use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Hard iteration bound for one child turn.
const MAX_CHILD_ITERATIONS: usize = 8;
/// Children that may run at once for one parent.
const MAX_CONCURRENT_CHILDREN: usize = 1;
const SUMMARY_CHARS: usize = 400;
const TITLE_CHARS: usize = 80;

/// Everything a spawner needs from its parent session.
pub struct SubagentSpawnerSettings {
    /// The only session this spawner serves; a spawn naming another is refused.
    pub parent_session_id: String,
    /// The parent's provider route; every child uses it.
    pub provider: ProviderConfig,
    /// The parent's workspace root; every child works in it.
    pub workspace_root: PathBuf,
    /// The parent's workspace trust; every child inherits it.
    pub workspace_trust: WorkspaceTrust,
    /// The parent's own ceiling in USD; `0.0` means none.
    pub parent_ceiling_usd: f64,
    /// The parent's live permission mode; a child starts in its current value.
    pub mode_rx: tokio::sync::watch::Receiver<PermissionMode>,
    /// The parent's turn interrupt; interrupting the parent interrupts the child.
    pub interrupt: tokio::sync::watch::Receiver<bool>,
    /// The parent's conduit, on which the child's lifecycle events are published.
    pub conduit: Arc<Conduit>,
}

/// Spawns one in-process child session at a time for one parent session.
pub struct MobileSubagentSpawner {
    parent_session_id: String,
    provider: ProviderConfig,
    workspace_root: PathBuf,
    workspace_trust: WorkspaceTrust,
    /// The parent's own ceiling in USD; `0.0` means none.
    parent_ceiling_usd: f64,
    mode_rx: tokio::sync::watch::Receiver<PermissionMode>,
    interrupt: tokio::sync::watch::Receiver<bool>,
    conduit: Arc<Conduit>,
    counter: AtomicU64,
    running: Arc<AtomicBool>,
    child_spent_microusd: Arc<AtomicU64>,
}

impl MobileSubagentSpawner {
    /// A spawner for the parent session described by `settings`.
    pub fn new(settings: SubagentSpawnerSettings) -> Self {
        Self {
            parent_session_id: settings.parent_session_id,
            provider: settings.provider,
            workspace_root: settings.workspace_root,
            workspace_trust: settings.workspace_trust,
            parent_ceiling_usd: settings.parent_ceiling_usd,
            mode_rx: settings.mode_rx,
            interrupt: settings.interrupt,
            conduit: settings.conduit,
            counter: AtomicU64::new(0),
            running: Arc::new(AtomicBool::new(false)),
            child_spent_microusd: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Latest cumulative cost the parent reported, best effort: the conduit
    /// keeps a bounded history, so `0.0` when none is retained.
    fn parent_spent_usd(&self) -> f64 {
        self.conduit
            .events_after(0)
            .iter()
            .rev()
            .find_map(|event| match event {
                ProtocolEvent::UsageUpdated { cost_usd, .. } => Some(*cost_usd),
                _ => None,
            })
            .unwrap_or(0.0)
            .max(0.0)
    }

    fn prepare(
        &self,
        role: &str,
        task: &str,
        parent_session_id: &str,
        options: &SubagentSpawnOptions,
    ) -> Result<(Prepared, RunningGuard), String> {
        if parent_session_id != self.parent_session_id {
            return Err("sub-agent refused: unknown parent session".to_string());
        }
        if task.trim().is_empty() {
            return Err("sub-agent refused: the task is empty".to_string());
        }
        if options.worktree.is_some() {
            return Err("sub-agent refused: worktrees are unavailable on this device".to_string());
        }
        if options.visible {
            return Err(
                "sub-agent refused: visible sub-agent windows are unavailable on this device"
                    .to_string(),
            );
        }
        if options.fork || options.handoff_summary {
            return Err(
                "sub-agent refused: starting from the parent transcript is unavailable on this device"
                    .to_string(),
            );
        }
        if matches!(
            options.continuity,
            Some(SubagentContinuity::ContinueAgent { .. })
        ) {
            return Err(
                "sub-agent refused: follow-ups to an earlier sub-agent are unavailable on this device"
                    .to_string(),
            );
        }
        if !matches!(
            options.runtime,
            None | Some(SpawnRuntimeKind::HostNative {
                provider_id: None,
                ..
            })
        ) {
            return Err(
                "sub-agent refused: a sub-agent always uses this session's provider route"
                    .to_string(),
            );
        }
        if self.running.swap(true, Ordering::AcqRel) {
            return Err(
                "sub-agent refused: one sub-agent is already running on this device".to_string(),
            );
        }
        // From here on every exit path, including an early `Err`, clears the flag.
        let running = RunningGuard(self.running.clone());

        let safe_role: String = role
            .trim()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .take(32)
            .collect();
        let safe_role = if safe_role.is_empty() {
            "agent".to_string()
        } else {
            safe_role
        };
        let n = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let child_session_id = format!("{}/{safe_role}-{n}", self.parent_session_id);

        // Authority: the parent's mode right now, never the engine default.
        let permission_mode = *self.mode_rx.borrow();

        let child_ceiling_usd = if self.parent_ceiling_usd > 0.0 {
            let spent = self.parent_spent_usd()
                + self.child_spent_microusd.load(Ordering::Acquire) as f64 / 1_000_000.0;
            let remaining = self.parent_ceiling_usd - spent;
            if remaining <= 0.0 {
                return Err(
                    "sub-agent refused: the session cost ceiling is already spent".to_string(),
                );
            }
            match options.budget_microusd.filter(|value| *value > 0) {
                Some(requested) => remaining.min(requested as f64 / 1_000_000.0),
                None => remaining,
            }
        } else {
            options
                .budget_microusd
                .filter(|value| *value > 0)
                .map_or(0.0, |requested| requested as f64 / 1_000_000.0)
        };

        let max_iterations = options
            .max_iterations
            .map_or(MAX_CHILD_ITERATIONS, |value| value as usize)
            .clamp(1, MAX_CHILD_ITERATIONS);

        let mut provider = self.provider.clone();
        provider.session_id = Some(child_session_id.clone());

        let task_title: String = task
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .chars()
            .take(TITLE_CHARS)
            .collect();

        let prepared = Prepared {
            parent_session_id: self.parent_session_id.clone(),
            child_session_id,
            role: safe_role,
            task: task.to_string(),
            task_title,
            tool_call_id: options.tool_call_id.clone(),
            provider,
            workspace_root: self.workspace_root.clone(),
            workspace_trust: self.workspace_trust,
            permission_mode,
            max_iterations,
            child_ceiling_usd,
            allowed_tools: options.allowed_tools.clone(),
            disallowed_tools: options.disallowed_tools.clone(),
            interrupt: self.interrupt.clone(),
            parent_conduit: self.conduit.clone(),
            child_spent_microusd: self.child_spent_microusd.clone(),
        };
        Ok((prepared, running))
    }
}

/// Clears the single-child flag on every exit path, including a dropped future.
struct RunningGuard(Arc<AtomicBool>);

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct Prepared {
    parent_session_id: String,
    child_session_id: String,
    role: String,
    task: String,
    task_title: String,
    tool_call_id: Option<String>,
    provider: ProviderConfig,
    workspace_root: PathBuf,
    workspace_trust: WorkspaceTrust,
    permission_mode: PermissionMode,
    max_iterations: usize,
    child_ceiling_usd: f64,
    allowed_tools: Vec<String>,
    disallowed_tools: Vec<String>,
    interrupt: tokio::sync::watch::Receiver<bool>,
    parent_conduit: Arc<Conduit>,
    child_spent_microusd: Arc<AtomicU64>,
}

impl rightkit_agent_coding_tools::context::SubagentSpawner for MobileSubagentSpawner {
    fn spawn(
        &self,
        role: &str,
        task: &str,
        parent_session_id: &str,
        options: SubagentSpawnOptions,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> {
        match self.prepare(role, task, parent_session_id, &options) {
            Ok((prepared, running)) => Box::pin(async move {
                // `running` lives as long as this future: it clears the single-child flag when
                // the child ends and when the future is dropped before it ends.
                let result = run_child(prepared).await;
                drop(running);
                result
            }),
            Err(message) => Box::pin(async move { Err(message) }),
        }
    }

    fn concurrent_cap(&self, parent_session_id: &str) -> Option<usize> {
        // The cap of a parent this spawner does not serve is unknown (and a spawn for it is
        // refused anyway).
        (parent_session_id == self.parent_session_id).then_some(MAX_CONCURRENT_CHILDREN)
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push_str("...");
    out
}

fn emit_cancelled(prepared: &Prepared, cancelled_by: &str) {
    let session_id = prepared.parent_session_id.clone();
    let sub_session_id = prepared.child_session_id.clone();
    let role = prepared.role.clone();
    let cancelled_by = cancelled_by.to_string();
    prepared
        .parent_conduit
        .emit(|seq| ProtocolEvent::SubAgentCancelled {
            session_id,
            seq,
            created_at: now_iso(),
            sub_session_id,
            role,
            cancelled_by,
        });
}

fn emit_completed(
    prepared: &Prepared,
    success: bool,
    summary: String,
    outcome: &str,
    outcome_detail: String,
) {
    let session_id = prepared.parent_session_id.clone();
    let sub_session_id = prepared.child_session_id.clone();
    let role = prepared.role.clone();
    let outcome = outcome.to_string();
    let tool_call_id = prepared.tool_call_id.clone();
    prepared
        .parent_conduit
        .emit(|seq| ProtocolEvent::SubAgentCompleted {
            session_id,
            seq,
            created_at: now_iso(),
            sub_session_id,
            role,
            success,
            summary,
            outcome,
            outcome_detail,
            tool_call_id,
        });
}

/// Whether a tool may be offered to a child: a tool this host supports, never a
/// sub-agent spawner (no recursion), never a tool that needs a person (a child
/// has no human subscriber), and inside the caller's allow and deny profile.
fn child_may_use(name: &str, prepared: &Prepared) -> bool {
    !matches!(name, "spawn_subagent" | "ask_user" | "exit_plan_mode")
        && crate::is_mobile_supported_tool(name, false, false)
        && (prepared.allowed_tools.is_empty() || prepared.allowed_tools.iter().any(|n| n == name))
        && !prepared.disallowed_tools.iter().any(|n| n == name)
}

async fn run_child(prepared: Prepared) -> Result<String, String> {
    {
        let session_id = prepared.parent_session_id.clone();
        let sub_session_id = prepared.child_session_id.clone();
        let role = prepared.role.clone();
        let task_title = prepared.task_title.clone();
        let tool_call_id = prepared.tool_call_id.clone();
        prepared
            .parent_conduit
            .emit(|seq| ProtocolEvent::SubAgentSpawned {
                session_id,
                seq,
                created_at: now_iso(),
                sub_session_id,
                role,
                task_title,
                tool_call_id,
            });
    }

    // Model-only: no GitHub or Cloudflare credential reaches a child.
    let llm = match llm::client_for(&prepared.provider, false, false) {
        Ok(client) => client,
        Err(error) => {
            let message = format!("sub-agent could not start: {error}");
            emit_completed(
                &prepared,
                false,
                message.clone(),
                "api_error",
                message.clone(),
            );
            return Err(message);
        }
    };

    let mut tools = ToolRegistry::build_default();
    let names: Vec<String> = tools
        .all()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    for name in names {
        if !child_may_use(&name, &prepared) {
            tools.remove(&name);
        }
    }
    if crate::wasm::is_wasm_executor_installed() {
        tools.replace(Box::new(
            rightkit_agent_coding_tools::wasm_compute::WasmComputeTool::with_runtime(Arc::new(
                crate::wasm::MobileWasmRuntime,
            )),
        ));
    } else {
        tools.remove("wasm_compute");
    }

    let child_conduit = Arc::new(Conduit::new(256));
    let config = SessionEngineConfig {
        session_id: prepared.child_session_id.clone(),
        workspace_root: prepared.workspace_root.clone(),
        permission_mode: prepared.permission_mode,
        workspace_trust: prepared.workspace_trust,
        max_iterations: prepared.max_iterations,
        budget_ceiling_usd: prepared.child_ceiling_usd,
        auto_post_edit_diagnostics: false,
        ..SessionEngineConfig::default()
    };
    let mut engine = SessionEngine::new(llm, Arc::new(tools), child_conduit.clone(), config);
    engine.set_interrupt_receiver(prepared.interrupt.clone());
    engine.set_run_origin_sink(Arc::new(
        rightkit_agent_engine_core::InMemoryOriginSink::always_ready(),
    ));
    engine.set_run_changes_sink(Arc::new(
        rightkit_agent_engine_core::InMemoryRunChangesSink::default(),
    ));
    engine.set_system_prompt(format!(
        "You are a {} sub-agent (role: {}) running locally on this iPhone. Complete only the delegated task by inspecting and editing the configured workspace with the available tools. Use relative workspace paths. Never claim a file change unless a tool result confirms it. You cannot spawn further sub-agents or run shell commands, builds, tests, or browsers. Finish with a concise final answer.",
        rightkit_agent_config::app_name(),
        prepared.role
    ));

    let result = engine.submit_user_message(&prepared.task).await;

    // Account what the child spent so a later child sees a smaller ceiling.
    let events = child_conduit.events_after(0);
    let spent_usd = events
        .iter()
        .rev()
        .find_map(|event| match event {
            ProtocolEvent::UsageUpdated { cost_usd, .. } => Some(*cost_usd),
            _ => None,
        })
        .unwrap_or(0.0)
        .max(0.0);
    prepared
        .child_spent_microusd
        .fetch_add((spent_usd * 1_000_000.0).round() as u64, Ordering::AcqRel);
    let budget_exceeded = events
        .iter()
        .any(|event| matches!(event, ProtocolEvent::BudgetExceeded { .. }));

    match result {
        Ok(_) if budget_exceeded => {
            emit_cancelled(&prepared, "budget");
            Err("sub-agent stopped: its cost ceiling was reached".to_string())
        }
        Ok(text) => {
            let text = if text.trim().is_empty() {
                engine
                    .transcript()
                    .messages()
                    .iter()
                    .rev()
                    .find_map(|message| match message {
                        TranscriptMessage::Assistant { content, .. }
                            if !content.trim().is_empty() =>
                        {
                            Some(content.clone())
                        }
                        _ => None,
                    })
                    .unwrap_or_default()
            } else {
                text
            };
            emit_completed(
                &prepared,
                true,
                truncate(&text, SUMMARY_CHARS),
                "complete",
                String::new(),
            );
            Ok(text)
        }
        Err(EngineError::Interrupted) => {
            emit_cancelled(&prepared, "user");
            Err("sub-agent interrupted: the parent turn was stopped".to_string())
        }
        Err(EngineError::MaxIterations(limit)) => {
            let message =
                format!("sub-agent stopped: it reached its limit of {limit} tool iterations");
            emit_completed(
                &prepared,
                false,
                message.clone(),
                "partial_with_limit",
                "max_iterations".to_string(),
            );
            Err(message)
        }
        Err(error) => {
            let message = format!("sub-agent failed: {error}");
            emit_completed(
                &prepared,
                false,
                truncate(&message, SUMMARY_CHARS),
                "api_error",
                truncate(&error.to_string(), SUMMARY_CHARS),
            );
            Err(message)
        }
    }
}
