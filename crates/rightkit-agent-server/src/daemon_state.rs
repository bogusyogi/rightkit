//! Trait implementations that express DaemonState's identity as a runtime —
//! sub-agent spawning, message dispatch, and team directory.
//!
//! Relocated verbatim from api.rs (2026-06-20) to shrink the daemon's monolith
//! and isolate the trait surface (SECURITY F11 auditability). Behaviour is
//! unchanged: the impl blocks moved as-is; api.rs still owns the
//! DaemonState struct, the router, and the free helpers these impls call.
//!
//! ponytail: the plan's optional decomposition of `spawn` into five named phase
//! helpers is deferred — its provided code dropped the timeout `interrupt_tx`
//! signal (a behaviour regression) and rebuilt the fork twice. Relocation alone
//! achieves the file-size/auditability goal with zero behaviour change; revisit
//! the decomposition as its own slice if spawn grows further.

use rightkit_agent_engine_core::conduit::ConduitReceive;
use rightkit_agent_engine_protocol::ProtocolEvent;
use rightkit_agent_engine_runtime_middleware::MiddlewarePhase;
use rightkit_agent_orchestration::coding::subagent_routing::{
    resolve_invocation, Profile, TaskSpec,
};

use crate::api::DaemonSubagentSupervisor;
use crate::api::{
    build_subagent_fork, cleanup_subagent_worktree, decorate_user_payload_with_system_reminder,
    dynamic_user_turn_reminder, lock_recover, native_spawn_route, now_iso, now_millis,
    prepare_subagent_workspace, publish_agent_roster, record_subagent_runtime_span,
    record_subagent_transcript_artifact, BudgetDecision, DaemonState, LiveSession,
    RunSubagentBudget,
};
use crate::api_stores::write_team_store;

static SUBAGENT_ID_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn next_subagent_id() -> String {
    let sequence = SUBAGENT_ID_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("sub-{}-{sequence}", now_millis())
}

/// Maximum sub-agent nesting depth. A sub-agent spawned directly by a root
/// session counts as one live ancestor; spawning is refused once the parent's
/// chain already reaches this many live ancestors — a backstop against runaway
/// recursive fan-out (a sub-agent that re-spawns itself without bound). Mirrors
/// Claude Code's 5-level background cap.
/// ponytail: const, not a setting — promote to DaemonSettings only if tuning is needed.
pub(crate) const MAX_SUBAGENT_DEPTH: usize = 5;

/// Count live ancestors of `session_id` inclusive by walking parent links.
/// Stops at `MAX_SUBAGENT_DEPTH + 1` so a corrupt parent cycle can't spin forever.
pub(crate) fn subagent_chain_depth(state: &DaemonState, session_id: &str) -> usize {
    let mut depth = 0;
    let mut current = Some(session_id.to_string());
    while let Some(id) = current {
        let Some(session) = state.get_session(&id) else {
            break;
        };
        depth += 1;
        if depth > MAX_SUBAGENT_DEPTH {
            break;
        }
        current = session.parent_session_id.clone();
    }
    depth
}

/// A short, human-readable title for a sub-agent's task.
///
/// Handoff briefs are markdown, so taking the first 60 raw characters produced
/// agent cards reading `## Handoff Brief\n\n### Goal\nRun the wh` — heading
/// syntax, a mid-word cut, and none of the actual task. Skip heading and label
/// lines, take the first real sentence, and cut on a word boundary.
fn handoff_task_title(task_text: &str) -> String {
    const MAX: usize = 60;
    let first_line = task_text
        .lines()
        .map(str::trim)
        .find(|line| {
            !line.is_empty()
                && !line.starts_with('#')
                && !line.starts_with("**")
                && !line.starts_with('-')
                && !line.starts_with('>')
        })
        .unwrap_or_else(|| task_text.trim());

    if first_line.chars().count() <= MAX {
        return first_line.to_string();
    }
    // Cut on the last word boundary that fits, so titles never end mid-word.
    let head: String = first_line.chars().take(MAX).collect();
    let trimmed = match head.rsplit_once(' ') {
        Some((left, _)) if left.chars().count() >= MAX / 2 => left,
        _ => head.as_str(),
    };
    format!("{}…", trimmed.trim_end_matches(['.', ',', ';', ':']))
}

fn ensure_parent_agent_roster(parent: &LiveSession, sub_session_id: &str, role: &str, model: &str) {
    lock_recover(&parent.agent_roster, "session.agent_roster").ensure(sub_session_id, role, model);
}

fn record_parent_agent_usage(
    parent: &LiveSession,
    sub_session_id: &str,
    prompt_tokens: i32,
    completion_tokens: i32,
    cache_read_tokens: i32,
) {
    lock_recover(&parent.agent_roster, "session.agent_roster").record_usage(
        sub_session_id,
        prompt_tokens.max(0) as u64,
        completion_tokens.max(0) as u64,
        cache_read_tokens.max(0) as u64,
    );
}

fn record_parent_agent_tool(parent: &LiveSession, sub_session_id: &str, tool_name: &str) {
    lock_recover(&parent.agent_roster, "session.agent_roster")
        .record_tool(sub_session_id, tool_name);
}

fn set_parent_agent_status(parent: &LiveSession, sub_session_id: &str, status: &str) {
    lock_recover(&parent.agent_roster, "session.agent_roster").set_status(sub_session_id, status);
}

/// Fold one child event into its completion state. An assistant message only
/// records the latest content (it is interim: steer, tool turns and summaries
/// also emit them); only the typed terminal `SessionTurnFinished` status
/// settles the child. Returns `None` while the child is still running.
fn settle_subagent_event(
    event: &ProtocolEvent,
    last_content: &mut String,
) -> Option<Result<String, String>> {
    match event {
        ProtocolEvent::AssistantMessageCompleted {
            message: rightkit_agent_engine_protocol::TranscriptMessage::Assistant { content, .. },
            ..
        } => {
            *last_content = content.clone();
            None
        }
        ProtocolEvent::SessionTurnFinished { status, .. } => Some(match status.as_str() {
            "completed" => Ok(std::mem::take(last_content)),
            "cancelled" => Err("sub-agent interrupted".to_string()),
            other => Err(format!("sub-agent turn {other}")),
        }),
        _ => None,
    }
}

/// Text published when a session's engine task dies by panic. The prefix is
/// the typed marker `subagent_error_kind` classifies as a failed child (never
/// a resource-limit cutoff).
pub(crate) fn engine_task_panic_failure(
    session_id: &str,
    seq: i64,
    join_error: &tokio::task::JoinError,
) -> Option<ProtocolEvent> {
    if !join_error.is_panic() {
        return None;
    }
    Some(ProtocolEvent::RunFailed {
        session_id: session_id.to_string(),
        seq,
        created_at: now_iso(),
        error: format!(
            "{}: {join_error}",
            crate::orchestrator::SUBAGENT_PANIC_PREFIX
        ),
        kind: "other".to_string(),
        hint: String::new(),
    })
}

/// Watch a session's engine task. A panic would otherwise leave the conduit
/// open (the live session still holds its sender), so a supervising parent
/// would wait for the wall-clock ceiling and misreport the child as a limit
/// cutoff. Publish a typed `RunFailed` the moment the task panics (CR2).
pub(crate) fn supervise_session_engine_task(
    handle: tokio::task::JoinHandle<()>,
    conduit: std::sync::Arc<rightkit_agent_engine_core::conduit::Conduit>,
    session_id: String,
) {
    supervise_session_engine_task_persisting(handle, conduit, session_id, None);
}

/// Persist session status `failed` (so `terminal_status` and restart-safe
/// readers see the child as failed, never pending or completed).
pub(crate) async fn persist_session_failed(storage: &crate::api::SharedStorage, session_id: &str) {
    if let Err(error) = storage
        .lock()
        .await
        .update_session_status(session_id, "failed", &now_iso())
    {
        tracing::error!(session_id = %session_id, error = %error, "failed to persist failed status after engine task panic");
    }
}

/// Like [`supervise_session_engine_task`], and when `storage` is supplied also
/// persists session status `failed` on panic, before the typed `RunFailed` is
/// published, so no observer can see the failure event with a stale status.
pub(crate) fn supervise_session_engine_task_persisting(
    handle: tokio::task::JoinHandle<()>,
    conduit: std::sync::Arc<rightkit_agent_engine_core::conduit::Conduit>,
    session_id: String,
    storage: Option<crate::api::SharedStorage>,
) {
    tokio::spawn(async move {
        if let Err(error) = handle.await {
            let seq = conduit.next_seq();
            if let Some(event) = engine_task_panic_failure(&session_id, seq, &error) {
                tracing::error!(session_id = %session_id, error = %error, "session engine task panicked");
                if let Some(storage) = storage.as_ref() {
                    persist_session_failed(storage, &session_id).await;
                }
                conduit.publish(event);
            }
        }
    });
}

fn subagent_wall_clock_ceiling(state: &DaemonState) -> std::time::Duration {
    std::time::Duration::from_secs(
        (lock_recover(&state.settings, "settings").max_wall_clock_minutes as u64).max(1) * 60,
    )
}

/// Compute the effective run sub-agent budget for a parent session.
///
/// Source of truth:
/// - `max_workers` from `DaemonSettingsStore` (config.toml `budget.max_workers`)
/// - The team preset's `max_workers` if the goal mentions the team name.
///
/// Returns a [`RunSubagentBudget`] whose `concurrent_max` is `effective_max_workers`
/// and whose `cumulative_max` is 4× that value (the conservative lifetime
/// multiplier the bin has used since the per-run budget was introduced).
fn effective_run_budget(state: &DaemonState, parent_id: &str) -> RunSubagentBudget {
    let configured = lock_recover(&state.settings, "settings").max_workers as usize;
    // Look up the parent session's goal text to see if a team preset applies.
    let team_preset = state.get_session(parent_id).and_then(|parent| {
        let goal = lock_recover(&parent.goal, "session.goal").clone();
        goal.and_then(|goal_text| {
            let teams = lock_recover(&state.live_teams, "live_teams").clone();
            let snapshot: Vec<rightkit_agent_coding_tools::TeamDefinition> = teams
                .values()
                .filter_map(|team| team.max_workers.map(|_| team.clone()))
                .collect();
            crate::orchestrator::find_team_preset_for_goal(&goal_text, &snapshot)
        })
    });
    let concurrent = effective_max_workers(configured, team_preset.as_ref()).max(1) as u32;
    let cumulative = concurrent.saturating_mul(4).max(concurrent);
    RunSubagentBudget::default()
        .with_concurrent(concurrent)
        .with_cumulative(cumulative)
}

/// Expose the underlying `effective_max_workers` so the budget builder can
/// reuse the same `configured vs team.min(configured)` rule the conductor
/// task graph already uses.
fn effective_max_workers(
    configured: usize,
    team: Option<&rightkit_agent_coding_tools::TeamDefinition>,
) -> usize {
    match (
        configured,
        team.and_then(|t| t.max_workers).map(|n| n as usize),
    ) {
        (0, Some(team_max)) => team_max,
        (configured, Some(team_max)) => configured.min(team_max).max(1),
        _ => configured,
    }
}

fn native_spawn_task_spec(
    task_id: String,
    payload: String,
    options: &rightkit_agent_coding_tools::SubagentSpawnOptions,
    requested_tier: u8,
) -> TaskSpec {
    let task = TaskSpec::new(task_id, payload).with_tier(requested_tier);
    match native_spawn_route(options) {
        Some((provider_id, model_id)) => task.with_model_override(provider_id, model_id),
        None => task,
    }
}

impl DaemonState {
    /// Root-controller handoffs fence new daemon intake until ownership has
    /// moved to a destination runtime.
    pub(crate) fn handoff_intake_is_fenced(&self, session_id: &str) -> bool {
        let _controller = &self.handoff_controller;
        rightkit_agent_coding_tools::handoff::HandoffController::is_in_flight(session_id)
    }
}

/// Out-slot for the typed `RunFailed.kind` a sub-agent failed with. The wire
/// kind travels beside the failure text so outcome classification uses the
/// type; it stays `None` for failures that carried no kind (spawn refusals,
/// wall-clock ceiling, conduit closure), which classify by their text.
pub(crate) type SubagentFailureKindSlot = std::sync::Arc<std::sync::Mutex<Option<String>>>;

impl rightkit_agent_coding_tools::SubagentSpawner for DaemonState {
    fn spawn(
        &self,
        role: &str,
        task: &str,
        parent_session_id: &str,
        options: rightkit_agent_coding_tools::SubagentSpawnOptions,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
        self.spawn_with_failure_kind(
            role,
            task,
            parent_session_id,
            options,
            SubagentFailureKindSlot::default(),
        )
    }
}

impl DaemonState {
    pub(crate) fn spawn_with_failure_kind(
        &self,
        role: &str,
        task: &str,
        parent_session_id: &str,
        options: rightkit_agent_coding_tools::SubagentSpawnOptions,
        failure_kind_slot: SubagentFailureKindSlot,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
        let state = self.clone();
        let role = role.to_string();
        let task_text = task.to_string();
        let parent_id = parent_session_id.to_string();
        Box::pin(async move {
            // PR 5 dispatch: when the caller asks for an authenticated
            // CLI runtime, route the spawn through the CLI helper
            // instead of the legacy native path. The helper honours
            // the session's `CliDelegationPolicy`, claims an
            // idempotency key in `driver_event_inbox`, persists the
            // child `driver_bindings` row with `parent_agent_id` set,
            // and returns the structured `AgentResult` projected onto
            // its `final_text`.
            if let Some(rightkit_agent_coding_tools::SpawnRuntimeKind::AuthenticatedCli {
                ..
            }) = &options.runtime
            {
                if state.cli_executor.is_none() {
                    return Err(
                        "authenticated CLI runtime is unavailable; refusing legacy-provider fallback"
                            .to_string(),
                    );
                }
                return crate::cli_delegation::dispatch_cli_spawn(
                    state, role, task_text, parent_id, options,
                )
                .await;
            }
            let parent_session = state.get_session(&parent_id);
            if parent_session.is_none() {
                return Err(format!("parent session not found: {parent_id}"));
            }
            // Codex's continuation form re-attaches to an existing child
            // instead of creating a second transcript. Validate ownership
            // before enqueueing so a parent cannot steer a foreign child.
            if let Some(rightkit_agent_coding_tools::SubagentContinuity::ContinueAgent {
                continue_agent_id,
            }) = &options.continuity
            {
                let child = state
                    .get_session(continue_agent_id)
                    .ok_or_else(|| format!("continuation child not found: {continue_agent_id}"))?;
                if child.parent_session_id.as_deref() != Some(parent_id.as_str()) {
                    return Err(format!(
                        "continuation child {continue_agent_id} is not owned by parent {parent_id}"
                    ));
                }
                if child.msg_tx.is_none() {
                    return Err(format!(
                        "continuation child {continue_agent_id} is not accepting input"
                    ));
                }
                child.send_route_bound_text(task_text).await.map_err(|_| {
                    format!("continuation child {continue_agent_id} rejected follow-up")
                })?;
                return Ok(format!("continued sub-agent {continue_agent_id}"));
            }
            // A caller that must know the child's identity up front (a
            // `visible` spawn, whose watch window attaches to the child's
            // session) allocates the id itself; otherwise mint one.
            let sub_id = options
                .child_session_id
                .clone()
                .unwrap_or_else(next_subagent_id);
            // Depth backstop: refuse to spawn deeper than MAX_SUBAGENT_DEPTH so a
            // sub-agent that re-spawns itself can't build an unbounded chain. The
            // per-agent wall-clock ceiling bounds runtime; this bounds nesting.
            let parent_depth = subagent_chain_depth(&state, &parent_id);
            if parent_depth >= MAX_SUBAGENT_DEPTH {
                return Err(format!(
                    "sub-agent nesting limit reached ({MAX_SUBAGENT_DEPTH} levels) — refusing to spawn deeper from {parent_id}"
                ));
            }
            // FM-088/FM-104/S:F8: resolve the role's built-in role pack
            // against the live model catalog BEFORE any admission or
            // workspace work, so an invalid pack (unknown/unqualified model,
            // malformed TOML) fails the spawn visibly with no partial
            // admission and no worktree ever created. A role with no
            // built-in pack (anything other than coder/orchestrator/
            // verifier) is unaffected — `resolved_role_pack` stays `None`
            // and the legacy per-agent-TOML/inherited-model
            // resolution below runs exactly as before.
            let role_pack_catalog = crate::orchestrator::live_role_pack_catalog();
            let resolved_role_pack = match crate::orchestrator::resolve_builtin_role_pack_for_spawn(
                &role,
                &role_pack_catalog,
            ) {
                Ok(Some(resolved)) => Some(resolved),
                Ok(None) => None,
                Err(error) => {
                    return Err(format!(
                        "role pack resolution failed for role '{role}': {error} — refusing to spawn"
                    ));
                }
            };
            // R13 (CR-42/43): tier-ceiling enforcement on the LIVE spawn path.
            //
            // `spawn_with_capabilities` (orchestration::spawn) checks a
            // requested tier against `Profile::max_tier_ceiling` via
            // `resolve_invocation`, but it is only ever called by its own
            // test — this daemon fork is the actual production spawn and
            // never consulted it, so the ceiling was open in production.
            //
            // This is a role-routed spawn, not a capability search, so we
            // call `resolve_invocation` directly (the one enforcement
            // primitive both callers share) rather than going through
            // `spawn_with_capabilities`'s capability-matching wrapper, which
            // has nothing to match here — the role is already fixed.
            //
            // The requesting agent's OWN registered escalation tier is its
            // ceiling: a bounded worker must not be able to spawn a child
            // registered at a higher tier than itself. A parent with no
            // role (a root/human-driven session) or a custom role absent
            // from the registry is not itself a bounded subagent, so it is
            // not tier-restricted here — there is no ceiling to check it
            // against.
            // Once the parent is known to be a bounded subagent, every spawn it
            // makes is checked -- including one naming a role the registry does
            // not know. An unknown child role has no tier to compare, and a
            // security boundary that skips the case it cannot evaluate is open
            // by exactly the input an attacker controls: the role string. Fail
            // closed instead.
            if let Some(parent_role) = parent_session.as_ref().and_then(|p| p.role.clone()) {
                if let Some(parent_descriptor) = state.agent_registry.get(&parent_role) {
                    let Some(child_descriptor) = state.agent_registry.get(&role) else {
                        return Err(format!(
                            "sub-agent role '{role}' is not registered, so its escalation tier \
                             cannot be checked against spawning agent '{parent_role}'s ceiling \
                             — refusing to spawn"
                        ));
                    };
                    let profile = Profile::new(parent_descriptor.escalation_tier, 3);
                    let task = native_spawn_task_spec(
                        sub_id.clone(),
                        task_text.clone(),
                        &options,
                        child_descriptor.escalation_tier,
                    );
                    if let Err(violation) = resolve_invocation(&task, &profile) {
                        return Err(format!(
                            "sub-agent role '{role}' (tier {}) exceeds spawning agent \
                             '{parent_role}'s tier ceiling: {violation}",
                            child_descriptor.escalation_tier
                        ));
                    }
                }
            }
            let effective_budget = effective_run_budget(&state, &parent_id);
            let persisted_cumulative = if let Some(storage) = state.storage.as_ref() {
                storage
                    .lock()
                    .await
                    .restore_run_budget(&parent_id)
                    .map_err(|error| {
                        format!("failed to restore sub-agent budget for {parent_id}: {error}")
                    })?
            } else {
                None
            };
            // Lane C3 (task 2): per-run sub-agent budget gate. Consult the
            // parent's `RunBudgetLedger` after the depth backstop — refuses
            // concurrent overloads and cumulative exhaustion with a typed
            // counterexample on the live bus so the UI can show the limit.
            // The admission guard holds the budget mutex across the rest of
            // Admission advances both counters. Any failure before the child
            // session is registered rolls the counters back explicitly.
            {
                let parent_for_budget = parent_session
                    .as_ref()
                    .expect("parent session existence checked above");
                let mut ledger = parent_for_budget.run_budget.lock().await;
                ledger.configure(effective_budget, persisted_cumulative);
                match ledger.try_admit() {
                    BudgetDecision::Admit => {}
                    BudgetDecision::RefuseConcurrent {
                        concurrent_limit,
                        concurrent_in_flight,
                        cumulative_limit,
                        cumulative_used,
                    } => {
                        return Err(format!(
                            "sub-agent concurrent budget exhausted (limit {concurrent_limit}, in-flight {concurrent_in_flight}, cumulative {cumulative_used}/{cumulative_limit}) — refusing to spawn {role} from {parent_id}"
                        ));
                    }
                    BudgetDecision::RefuseCumulative {
                        concurrent_limit,
                        concurrent_in_flight,
                        cumulative_limit,
                        cumulative_used,
                    } => {
                        return Err(format!(
                            "sub-agent cumulative budget exhausted (limit {cumulative_limit}, used {cumulative_used}, in-flight {concurrent_in_flight}/{concurrent_limit}) — refusing to spawn {role} from {parent_id}"
                        ));
                    }
                }
                if let Some(storage) = state.storage.as_ref() {
                    if let Err(error) = storage
                        .lock()
                        .await
                        .upsert_run_budget(&parent_id, ledger.cumulative_used())
                    {
                        ledger.rollback_after_admit();
                        return Err(format!(
                            "failed to persist sub-agent budget admission for {parent_id}: {error}"
                        ));
                    }
                }
            };
            // Inherit the parent's workspace so the worker operates on the same
            // project the conductor was given, not an empty root.
            let parent_workspace = parent_session
                .as_ref()
                .map(|p| p.workspace_root.clone())
                .unwrap_or_default();
            let parent_execution_backend = parent_session
                .as_ref()
                .and_then(|p| p.execution_backend.clone());
            let parent_execution_workspace_id = parent_session
                .as_ref()
                .and_then(|p| p.execution_workspace_id.clone());
            let parent_execution_network_profile = parent_session
                .as_ref()
                .and_then(|p| p.execution_network_profile.clone());
            let parent_mcp_channel = parent_session
                .as_ref()
                .map(|p| p.mcp_channel.clone())
                .unwrap_or_else(|| "desktop".to_string());
            let subagent_start_span_id = record_subagent_runtime_span(
                &state,
                MiddlewarePhase::SubagentStart,
                "started",
                &parent_id,
                None,
                &sub_id,
                &role,
                &task_text,
                &parent_workspace,
                parent_execution_backend.as_deref(),
                parent_execution_workspace_id.as_deref(),
                parent_execution_network_profile.as_deref(),
                Some(&parent_mcp_channel),
                None,
                serde_json::json!({
                    "worktreeRequested": options.worktree.is_some(),
                    "maxIterations": options.max_iterations,
                }),
            );
            let (workspace, cleanup_worktree) = match prepare_subagent_workspace(
                &parent_workspace,
                &role,
                &sub_id,
                &options,
            )
            .await
            {
                Ok(prepared) => prepared,
                Err(e) => {
                    if let Some(parent) = parent_session.as_ref() {
                        let mut ledger = parent.run_budget.lock().await;
                        ledger.rollback_after_admit();
                        if let Some(storage) = state.storage.as_ref() {
                            storage
                                    .lock()
                                    .await
                                    .upsert_run_budget(&parent_id, ledger.cumulative_used())
                                    .map_err(|error| {
                                        format!(
                                            "{e}; additionally failed to persist budget rollback: {error}"
                                        )
                                    })?;
                        }
                    }
                    record_subagent_runtime_span(
                        &state,
                        MiddlewarePhase::SubagentStop,
                        "failed",
                        &parent_id,
                        subagent_start_span_id.as_deref(),
                        &sub_id,
                        &role,
                        &task_text,
                        &parent_workspace,
                        parent_execution_backend.as_deref(),
                        parent_execution_workspace_id.as_deref(),
                        parent_execution_network_profile.as_deref(),
                        Some(&parent_mcp_channel),
                        Some(&e),
                        serde_json::json!({ "failureStage": "prepareWorkspace" }),
                    );
                    return Err(e);
                }
            };
            let fork_workspace_config = rightkit_agent_config::ConfigLoader::new()
                .load_files_for_workspace(std::path::Path::new(&workspace));
            // Per-agent TOML scoping (`.{app}/agents/<role>.toml`): role,
            // optional model override, and agent-scoped MCP servers. The
            // load is best-effort — a malformed file must never block a
            // spawn (the global config and the per-agent override map
            // remain authoritative). The model override here wins over the
            // inherited model when present, mirroring the per-agent
            // override precedence already used in the spawn-seam router.
            let agent_mcp_cfg =
                rightkit_agent_config::find_agents_dir(std::path::Path::new(&workspace));
            let agent_mcp = rightkit_agent_config::AgentMcpConfig::load_or_err(
                &agent_mcp_cfg,
                &role,
            )
            .map_err(|error| {
                format!("agent TOML config failed for role '{role}': {error}; refusing spawn")
            })?;
            let agent_model_override = agent_mcp
                .and_then(|cfg| cfg.model)
                .filter(|m| !m.trim().is_empty());
            let split_available =
                crate::model_policy::split_entitled(|feature| state.has_license_feature(feature));
            let inherited_model = if crate::model_policy::uses_single_role_model(
                &fork_workspace_config,
                split_available,
            ) {
                parent_session
                    .as_ref()
                    .map(|parent| lock_recover(&parent.model, "session.model").clone())
            } else {
                None
            };
            // Per-agent TOML override wins over both the inherited model and
            // the split-role default — kept here so the spawn seam has a
            // single, observable resolution order. The role pack's model
            // assignment (S:F8: "coder.v1.toml drives role→model assignment
            // through live spawn") sits above all of it: when the role has a
            // built-in pack, that pack — already resolved and validated
            // against the live catalog above — is the assignment, full stop.
            let inherited_model = agent_model_override.or(inherited_model);
            let inherited_model = resolved_role_pack
                .as_ref()
                .map(|resolved| resolved.primary.id.clone())
                .or(inherited_model);
            let mut fork = build_subagent_fork(
                sub_id.clone(),
                parent_id.clone(),
                role.clone(),
                task_text.clone(),
                workspace.clone(),
                parent_session.as_ref(),
                &options,
                fork_workspace_config.prompt.brief_default(),
                inherited_model,
            );
            if let Some(resolved) = resolved_role_pack.as_ref() {
                if let Err(error) = fork.apply_resolved_role_pack(resolved) {
                    if let Some(parent) = parent_session.as_ref() {
                        let mut ledger = parent.run_budget.lock().await;
                        ledger.rollback_after_admit();
                        if let Some(storage) = state.storage.as_ref() {
                            storage
                                .lock()
                                .await
                                .upsert_run_budget(&parent_id, ledger.cumulative_used())
                                .map_err(|storage_error| format!(
                                    "{error}; additionally failed to persist budget rollback: {storage_error}"
                                ))?;
                        }
                    }
                    if let Some(path) = cleanup_worktree.as_deref() {
                        cleanup_subagent_worktree(&parent_workspace, path).await;
                    }
                    return Err(format!(
                        "role pack binding failed for role '{role}': {error} — refusing to spawn"
                    ));
                }
            }
            // Parent cancellation has begun: admit no new children (CR2).
            let parent_cancelling = state.session_cancelling(&parent_id);
            if state.get_session(&parent_id).is_none() || parent_cancelling {
                if let Some(parent) = parent_session.as_ref() {
                    let mut ledger = parent.run_budget.lock().await;
                    ledger.rollback_after_admit();
                    if let Some(storage) = state.storage.as_ref() {
                        storage
                            .lock()
                            .await
                            .upsert_run_budget(&parent_id, ledger.cumulative_used())
                            .map_err(|error| {
                                format!(
                                    "failed to persist budget rollback for {parent_id}: {error}"
                                )
                            })?;
                    }
                }
                if let Some(path) = cleanup_worktree.as_deref() {
                    cleanup_subagent_worktree(&parent_workspace, path).await;
                }
                if parent_cancelling {
                    return Err(format!(
                        "parent session is cancelling; no new sub-agents: {parent_id}"
                    ));
                }
                return Err(format!(
                    "parent session was deleted before worker start: {parent_id}"
                ));
            }
            // The owner edge (`parent_session_id`) and the child's interrupt
            // channel are registered here, and the completion observation is
            // subscribed below, all BEFORE the task is dispatched (CR2).
            let live = state.create_session_with_options(
                &fork.sub_session_id,
                Some(&role),
                &fork.workspace_root,
                &format!("{role} subtask"),
                Some(&parent_id),
                fork.runtime_options(),
            );
            let mut sub = live.conduit.subscribe();
            // Close the race with a cancel that landed after the check above.
            if state.session_cancelling(&parent_id) {
                if let Some(tx) = &live.interrupt_tx {
                    let _ = tx.send(true);
                }
            }

            // Notify parent: sub-agent spawned.
            if let Some(parent) = state.get_session(&parent_id) {
                let seq = parent.conduit.next_seq();
                parent.conduit.publish(ProtocolEvent::SubAgentSpawned {
                    session_id: parent_id.clone(),
                    seq,
                    created_at: now_iso(),
                    sub_session_id: sub_id.clone(),
                    role: role.clone(),
                    task_title: handoff_task_title(&task_text),
                });
                // Carry the child's resolved model onto the rail so the user can
                // see which model each agent ran on — the difference Pro's
                // multi-model routing actually buys.
                let child_model = lock_recover(&live.model, "session.model").clone();
                ensure_parent_agent_roster(&parent, &sub_id, &role, &child_model);
                publish_agent_roster(&parent);
            }

            // A delegated task is admitted input too: keep its original text
            // in the canonical child transcript before execution can publish
            // later facts. Direct channel delivery alone made live child chat
            // empty until an assistant response completed.
            lock_recover(&live.transcript, "session.transcript").push(
                rightkit_agent_engine_protocol::TranscriptMessage::User {
                    id: format!("user-{}", now_millis()),
                    created_at: now_iso(),
                    content: task_text.clone(),
                    trust: None,
                    pinned: false,
                    attachments: Vec::new(),
                },
            );
            let seq = live.conduit.next_seq();
            live.conduit.publish(ProtocolEvent::UserMessageSubmitted {
                session_id: live.id.clone(),
                seq,
                created_at: now_iso(),
                text: task_text.clone(),
                nonce: None,
                trust: None,
                attachments: Vec::new(),
            });
            let memory_injection = crate::api::retrieve_session_memory(
                &state,
                &live.id,
                &live.workspace_root,
                &task_text,
                5,
                "subagent",
            )
            .await;
            let reminder = dynamic_user_turn_reminder(
                memory_injection
                    .as_ref()
                    .map(|injection| injection.context.as_str()),
                &[],
            );
            let briefing = decorate_user_payload_with_system_reminder(&task_text, &reminder);
            // Send the task.
            let sent = if live.msg_tx.is_some() {
                live.send_route_bound_text(briefing).await.is_ok()
            } else {
                false
            };
            if !sent {
                if let Some(path) = cleanup_worktree.as_deref() {
                    cleanup_subagent_worktree(&parent_workspace, path).await;
                }
                record_subagent_runtime_span(
                    &state,
                    MiddlewarePhase::SubagentStop,
                    "failed",
                    &parent_id,
                    subagent_start_span_id.as_deref(),
                    &sub_id,
                    &role,
                    &task_text,
                    &workspace,
                    parent_execution_backend.as_deref(),
                    parent_execution_workspace_id.as_deref(),
                    parent_execution_network_profile.as_deref(),
                    Some(&parent_mcp_channel),
                    Some("no engine for sub-agent"),
                    serde_json::json!({ "failureStage": "sendTask" }),
                );
                record_subagent_transcript_artifact(&state, &fork, false);
                return Err("no engine for sub-agent".into());
            }

            // Wait for the sub-agent to complete by listening to its conduit.
            // Bounded by a wall-clock ceiling so the conductor can never deadlock
            // on a wedged worker (defense in depth alongside the engine's
            // terminal-event guarantees and the provider request timeout).
            // (`sub` was subscribed before dispatch so an immediately-finishing
            // child cannot be missed.) Only the typed terminal turn event
            // settles the child; interim assistant messages just update the
            // latest content.
            let wait = async {
                let mut last_content = String::new();
                loop {
                    match sub.recv().await {
                        ConduitReceive::Event(
                            event @ (ProtocolEvent::AssistantMessageCompleted { .. }
                            | ProtocolEvent::SessionTurnFinished { .. }),
                        ) => {
                            if let Some(settled) = settle_subagent_event(&event, &mut last_content)
                            {
                                break settled;
                            }
                        }
                        ConduitReceive::Event(ProtocolEvent::RunFailed { error, kind, .. }) => {
                            // Carry the typed failure kind to the outcome
                            // classifier; text matching is only the fallback.
                            if !kind.trim().is_empty() {
                                *lock_recover(&failure_kind_slot, "failure_kind_slot") = Some(kind);
                            }
                            if error.starts_with(crate::orchestrator::SUBAGENT_PANIC_PREFIX) {
                                if let Some(storage) = state.storage.as_ref() {
                                    persist_session_failed(storage, &sub_id).await;
                                }
                            }
                            break Err(error);
                        }
                        ConduitReceive::Event(ProtocolEvent::RunInterrupted { .. }) => {
                            break Err("sub-agent interrupted".to_string())
                        }
                        ConduitReceive::Event(ProtocolEvent::BudgetExceeded {
                            current_usd,
                            ceiling_usd,
                            ..
                        }) => {
                            break Err(format!(
                                "session budget exceeded (${current_usd:.4} / ${ceiling_usd:.4})"
                            ))
                        }
                        ConduitReceive::Lagged { missed } => {
                            break Err(format!(
                                "sub-agent conduit lagged ({missed} events missed); resync required"
                            ))
                        }
                        ConduitReceive::Closed => break Err("sub-agent conduit closed".to_string()),
                        ConduitReceive::Event(other) => {
                            // Accumulate per-agent metrics for the parent's agent rail (F1).
                            // The child's usage/tool events flow on its own conduit, which
                            // this loop is subscribed to.
                            let changed = match &other {
                                ProtocolEvent::UsageUpdated {
                                    prompt_tokens,
                                    completion_tokens,
                                    cache_read_tokens,
                                    ..
                                } => {
                                    if let Some(parent) = state.get_session(&parent_id) {
                                        record_parent_agent_usage(
                                            &parent,
                                            &sub_id,
                                            *prompt_tokens,
                                            *completion_tokens,
                                            *cache_read_tokens,
                                        );
                                    }
                                    true
                                }
                                ProtocolEvent::ToolCallStarted { call, .. } => {
                                    if let Some(parent) = state.get_session(&parent_id) {
                                        record_parent_agent_tool(&parent, &sub_id, &call.name);
                                    }
                                    true
                                }
                                _ => false,
                            };
                            if changed {
                                if let Some(parent) = state.get_session(&parent_id) {
                                    publish_agent_roster(&parent);
                                }
                            }
                        }
                    }
                }
            };
            let ceiling =
                subagent_wall_clock_ceiling(&state).min(std::time::Duration::from_secs(120));
            let result: Result<String, String> = match tokio::time::timeout(ceiling, wait).await {
                Ok(r) => r,
                Err(_) => {
                    // Stop the wedged worker and report a clean failure.
                    if let Some(tx) = &live.interrupt_tx {
                        let _ = tx.send(true);
                    }
                    Err(format!(
                        "sub-agent exceeded the {}-minute wall-clock ceiling",
                        ceiling.as_secs() / 60
                    ))
                }
            };

            // Notify parent: sub-agent completed. `outcome`/`outcome_detail`
            // carry the sub-agent's TRUE completion state (parity P-2): a
            // rate-limit/wall-clock/budget cutoff is `partial_with_limit`
            // (never masqueraded as plain success/failure), and an
            // unrecovered provider/API error is `api_error` — reusing the
            // same classification the conductor's `WorkerResult::outcome`
            // uses, so the UI and the conductor agree on one taxonomy.
            let typed_failure_kind: Option<String> =
                lock_recover(&failure_kind_slot, "failure_kind_slot").clone();
            let (outcome_label, outcome_detail) = match &result {
                Ok(_) => ("complete".to_string(), String::new()),
                Err(e) => match crate::orchestrator::subagent_error_kind_typed(
                    typed_failure_kind.as_deref(),
                    e,
                ) {
                    crate::orchestrator::SubagentErrorKind::ResourceLimit(limit_kind) => {
                        ("partial_with_limit".to_string(), limit_kind)
                    }
                    crate::orchestrator::SubagentErrorKind::ApiError => {
                        ("api_error".to_string(), e.clone())
                    }
                },
            };
            if let Some(parent) = state.get_session(&parent_id) {
                let seq = parent.conduit.next_seq();
                parent.conduit.publish(ProtocolEvent::SubAgentCompleted {
                    session_id: parent_id.clone(),
                    seq,
                    created_at: now_iso(),
                    sub_session_id: sub_id.clone(),
                    role: role.clone(),
                    success: result.is_ok(),
                    summary: result
                        .as_deref()
                        .unwrap_or("task failed")
                        .chars()
                        .take(200)
                        .collect(),
                    outcome: outcome_label,
                    outcome_detail,
                });
                set_parent_agent_status(
                    &parent,
                    &sub_id,
                    if result.is_ok() { "done" } else { "failed" },
                );
                publish_agent_roster(&parent);
                // Lane C3 (task 2): mark the parent's budget ledger so the
                // concurrent count drops and the cumulative count holds.
                // `SubagentLifecycleMark::Completed` covers both Ok and Err
                // outcomes — the budget tracks spawns, not outcomes.
                // Release the slot exactly once per child, even if cancel
                // and completion both reach this point.
                parent.run_budget.lock().await.mark_completed_once(&sub_id);
            }

            // Fire SubagentStop hook on the sub-agent's own bus.
            if let Some(sub_live) = state.get_session(&sub_id) {
                sub_live
                    .hook_bus
                    .publish(rightkit_hooks::agent_hooks::HookEvent::SubagentStop {
                        session_id: sub_id.clone(),
                        role: role.clone(),
                        success: result.is_ok(),
                    });
            }

            record_subagent_transcript_artifact(&state, &fork, result.is_ok());

            if let Some(path) = cleanup_worktree.as_deref() {
                cleanup_subagent_worktree(&parent_workspace, path).await;
            }

            record_subagent_runtime_span(
                &state,
                MiddlewarePhase::SubagentStop,
                if result.is_ok() {
                    "completed"
                } else {
                    "failed"
                },
                &parent_id,
                subagent_start_span_id.as_deref(),
                &sub_id,
                &role,
                &task_text,
                &workspace,
                parent_execution_backend.as_deref(),
                parent_execution_workspace_id.as_deref(),
                parent_execution_network_profile.as_deref(),
                Some(&parent_mcp_channel),
                result.as_ref().err().map(String::as_str),
                serde_json::json!({ "success": result.is_ok() }),
            );

            result
        })
    }
}

impl rightkit_agent_coding_tools::MessageDispatcher for DaemonState {
    fn dispatch(
        &self,
        message: rightkit_agent_coding_tools::MessageDispatch,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
        let state = self.clone();
        Box::pin(async move {
            let body = format!(
                "[Message from {} via {}]\n{}",
                message.from_session_id, message.channel, message.message
            );
            if let Some(target) = message.target_session_id.clone() {
                // `followup_task` carries a canonical AgentPath, unlike
                // ordinary send_message which addresses a raw session id.
                // Resolve it against sender lineage before touching the live
                // session, preserving exact subtree targeting for nested and
                // foreign-harness roots.
                let target = if message.channel == "followup_task" {
                    DaemonSubagentSupervisor::new(state.clone())
                        .resolve_path(&message.from_session_id, &target)
                        .await?
                } else {
                    target
                };
                match state.submit_relayed_message(&target, &body).await {
                    Ok(_) => {
                        crate::async_inbox_feed::feed_session_message(
                            &target,
                            &message.from_session_id,
                            &message.channel,
                            &message.message,
                        );
                        let _ = state.record_message_history(
                            message,
                            "delivered",
                            vec![target.clone()],
                            None,
                        );
                    }
                    Err(e) => {
                        let _ = state.record_message_history(
                            message,
                            "failed",
                            Vec::new(),
                            Some(e.clone()),
                        );
                        return Err(e);
                    }
                }
                return Ok(format!("delivered to {target}"));
            }

            let sessions = state.list_sessions();
            if sessions.is_empty() {
                let error = "no live sessions available for broadcast".to_string();
                let _ = state.record_message_history(
                    message,
                    "failed",
                    Vec::new(),
                    Some(error.clone()),
                );
                return Err(error);
            }
            let mut delivered = Vec::new();
            for session in sessions {
                if let Err(e) = state.submit_relayed_message(&session.id, &body).await {
                    let _ =
                        state.record_message_history(message, "failed", delivered, Some(e.clone()));
                    return Err(e);
                }
                crate::async_inbox_feed::feed_session_message(
                    &session.id,
                    &message.from_session_id,
                    &message.channel,
                    &message.message,
                );
                delivered.push(session.id.clone());
            }
            let delivered_count = delivered.len();
            let delivered_list = delivered.join(", ");
            let _ = state.record_message_history(message, "broadcast", delivered, None);
            Ok(format!(
                "broadcast to {} session(s): {}",
                delivered_count, delivered_list
            ))
        })
    }
}

impl rightkit_agent_coding_tools::TeamDirectory for DaemonState {
    fn create_team(
        &self,
        team: rightkit_agent_coding_tools::TeamDefinition,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
        let teams = self.live_teams.clone();
        let store_path = self.team_store_path.clone();
        Box::pin(async move {
            if team.name.trim().is_empty() {
                return Err("team name cannot be empty".to_string());
            }
            if team.members.is_empty() {
                return Err("team must include at least one member".to_string());
            }
            let name = team.name.clone();
            let member_count = team.members.len();
            let snapshot = {
                let mut guard = lock_recover(&teams, "live_teams");
                guard.insert(name.clone(), team);
                guard.clone()
            };
            write_team_store(&store_path, &snapshot)?;
            Ok(format!("{name} registered with {member_count} member(s)"))
        })
    }

    fn delete_team(
        &self,
        name: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
        let teams = self.live_teams.clone();
        let store_path = self.team_store_path.clone();
        Box::pin(async move {
            let snapshot = {
                let mut guard = lock_recover(&teams, "live_teams");
                if guard.remove(&name).is_none() {
                    return Err(format!("team not found: {name}"));
                }
                guard.clone()
            };
            write_team_store(&store_path, &snapshot)?;
            Ok(format!("{name} removed from live directory"))
        })
    }
}
