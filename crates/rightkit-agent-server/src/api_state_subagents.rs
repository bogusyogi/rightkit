/// Credentials resolve per route only: this route's env var, then this
/// provider's stored key. Another route's (global) key is never reused.
fn configured_provider_api_key(
    provider_id: &str,
    route: &rightkit_agent_config::ProviderRouteConfig,
    _fallback: &EngineConfig,
) -> String {
    configured_provider_api_key_without_fallback(provider_id, route)
}

pub(crate) fn configured_provider_api_key_without_fallback(
    provider_id: &str,
    route: &rightkit_agent_config::ProviderRouteConfig,
) -> String {
    if let Some(env_name) = route
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        if let Ok(value) = std::env::var(env_name) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    rightkit_llm::agent_providers::load_api_key(provider_id).unwrap_or_default()
}

/// Daemon-owned implementation of the engine's typed child-control seam.
/// It resolves paths through live sessions when present, then uses persisted
/// parent/status columns as restart-safe authority; it owns no second registry.
#[derive(Clone)]
pub(crate) struct DaemonSubagentSupervisor {
    state: DaemonState,
}

impl DaemonSubagentSupervisor {
    pub(crate) fn new(state: DaemonState) -> Self {
        Self { state }
    }

    async fn session_row(
        &self,
        session_id: &str,
    ) -> Result<rightkit_agent_engine_storage::SessionRow, String> {
        let Some(storage) = self.state.storage.as_ref() else {
            return Err("subagent supervision requires daemon session storage".to_string());
        };
        let storage = storage.lock().await;
        storage
            .get_session(session_id)
            .map_err(|error| format!("failed to read subagent session {session_id}: {error}"))?
            .ok_or_else(|| format!("unknown subagent session {session_id}"))
    }

    pub(crate) async fn resolve_path(
        &self,
        parent_session_id: &str,
        agent_path: &str,
    ) -> Result<String, String> {
        let segments = agent_path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        if segments.is_empty() {
            return Err("subagent path is empty".to_string());
        }
        let children = if segments.len() == 1 {
            segments.as_slice()
        } else {
            if segments[0] != parent_session_id {
                return Err(format!(
                    "subagent path {agent_path} is outside parent {parent_session_id}"
                ));
            }
            &segments[1..]
        };
        if children.is_empty() {
            return Err(format!(
                "subagent path {agent_path} names its parent, not a child"
            ));
        }

        let mut expected_parent = parent_session_id.to_string();
        for child_id in children {
            let live_parent = lock_recover(&self.state.sessions, "sessions")
                .get(*child_id)
                .and_then(|session| session.parent_session_id.clone());
            let actual_parent = match live_parent {
                Some(parent) => Some(parent),
                None => self.session_row(child_id).await?.parent_session_id,
            };
            if actual_parent.as_deref() != Some(expected_parent.as_str()) {
                return Err(format!(
                    "subagent path {agent_path} is not a child subtree of {parent_session_id}"
                ));
            }
            expected_parent = (*child_id).to_string();
        }
        Ok(expected_parent)
    }

    async fn terminal_status(
        &self,
        session_id: &str,
    ) -> Result<Option<rightkit_agent_tools::coding_tools::SubagentTerminalStatus>, String> {
        let Some(storage) = self.state.storage.as_ref() else {
            return Err("subagent supervision requires daemon session storage".to_string());
        };
        let storage = storage.lock().await;
        let status = storage
            .session_status(session_id)
            .map_err(|error| format!("failed to read subagent status {session_id}: {error}"))?
            .ok_or_else(|| format!("unknown subagent session {session_id}"))?;
        Ok(match status.as_str() {
            "completed" | "idle" => {
                Some(rightkit_agent_tools::coding_tools::SubagentTerminalStatus::Completed)
            }
            "failed" => Some(rightkit_agent_tools::coding_tools::SubagentTerminalStatus::Failed),
            "cancelled" => {
                Some(rightkit_agent_tools::coding_tools::SubagentTerminalStatus::Cancelled)
            }
            _ => None,
        })
    }

    async fn interrupt_tree(&self, session_id: &str) -> Result<(), String> {
        let children = {
            let Some(storage) = self.state.storage.as_ref() else {
                return Err("subagent supervision requires daemon session storage".to_string());
            };
            let storage = storage.lock().await;
            storage
                .child_sessions(session_id)
                .map_err(|error| format!("failed to read child subtree {session_id}: {error}"))?
                .into_iter()
                .map(|child| child.id)
                .collect::<Vec<_>>()
        };
        for child in children {
            Box::pin(self.interrupt_tree(&child)).await?;
        }
        let interrupt = lock_recover(&self.state.sessions, "sessions")
            .get(session_id)
            .and_then(|session| session.interrupt_tx.clone());
        let Some(interrupt) = interrupt else {
            return Err(format!("subagent session {session_id} is not live"));
        };
        interrupt
            .send(true)
            .map_err(|_| format!("subagent session {session_id} no longer accepts interrupts"))
    }
}

impl rightkit_agent_tools::coding_tools::SubagentSupervisor for DaemonSubagentSupervisor {
    fn wait_for(
        &self,
        parent_session_id: &str,
        agent_paths: Vec<String>,
        timeout_ms: Option<u64>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Vec<rightkit_agent_tools::coding_tools::SubagentWaitOutcome>,
                        String,
                    >,
                > + Send,
        >,
    > {
        let supervisor = self.clone();
        let parent_session_id = parent_session_id.to_string();
        Box::pin(async move {
            let mut resolved = Vec::with_capacity(agent_paths.len());
            for path in agent_paths {
                let session_id = supervisor.resolve_path(&parent_session_id, &path).await?;
                resolved.push((path, session_id));
            }
            let started = std::time::Instant::now();
            loop {
                let mut outcomes = Vec::with_capacity(resolved.len());
                let mut pending = false;
                for (path, session_id) in &resolved {
                    match supervisor.terminal_status(session_id).await? {
                        Some(status) => {
                            outcomes.push(rightkit_agent_tools::coding_tools::SubagentWaitOutcome {
                                agent_path: path.clone(),
                                status,
                            })
                        }
                        None => pending = true,
                    }
                }
                if !pending {
                    return Ok(outcomes);
                }
                if timeout_ms.is_some_and(|timeout| {
                    started.elapsed() >= std::time::Duration::from_millis(timeout)
                }) {
                    return Err("timed out waiting for subagent terminal status".to_string());
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
    }

    fn interrupt_subtree(
        &self,
        parent_session_id: &str,
        agent_path: String,
        _reason: Option<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> {
        let supervisor = self.clone();
        let parent_session_id = parent_session_id.to_string();
        Box::pin(async move {
            let session_id = supervisor
                .resolve_path(&parent_session_id, &agent_path)
                .await?;
            supervisor.interrupt_tree(&session_id).await
        })
    }
}

#[derive(Clone, Debug)]
pub struct TelegramRemoteStatus {
    pub token_configured: bool,
    pub allowed_user_count: usize,
    pub long_poll_requested: bool,
    pub binding_store_path: PathBuf,
    pub runtime: crate::telegram_task::TelegramRuntimeStatusStore,
}

impl Default for TelegramRemoteStatus {
    fn default() -> Self {
        Self {
            token_configured: false,
            allowed_user_count: 0,
            long_poll_requested: false,
            binding_store_path: crate::telegram_task::telegram_binding_store_path(),
            runtime: crate::telegram_task::new_telegram_runtime_status_store(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SessionRuntimeOptions {
    pub(crate) model: Option<String>,
    /// Per-call native provider override. Transient child state only; never
    /// written back to an agent profile or workspace configuration.
    pub(crate) provider_id: Option<String>,
    /// Structured inference authority carried intact through child forks.
    pub(crate) inference_selection: Option<crate::inference_runtime::InferenceSelection>,
    pub(crate) inference_fingerprint: Option<String>,
    pub(crate) resolved_inference_route: Option<crate::inference_runtime::ResolvedInferenceRoute>,
    pub(crate) fallback_models: Vec<String>,
    pub(crate) allowed_tools: Vec<String>,
    pub(crate) disallowed_tools: Vec<String>,
    pub(crate) toolset: Option<String>,
    /// Independent verifier selected by a resolved role pack. This remains
    /// child-local until the engine's verifier runner consumes it.
    pub(crate) verifier_model: Option<String>,
    pub(crate) verifier_is_independent: Option<bool>,
    pub(crate) active_skills: Vec<String>,
    pub(crate) execution_backend: Option<String>,
    pub(crate) execution_workspace_id: Option<String>,
    pub(crate) execution_workspace_path: Option<PathBuf>,
    pub(crate) execution_network_profile: Option<String>,
    /// Root conductor for this session. Omitted/persisted legacy sessions are
    /// always native RightKit sessions, never implicitly delegated to a
    /// vendor CLI.
    pub(crate) driver_kind: String,
    pub(crate) mcp_channel: String,
    pub(crate) spec_id: Option<String>,
    pub(crate) effort: Option<String>,
    pub(crate) thinking_budget_tokens: Option<u32>,
    pub(crate) disable_thinking: bool,
    pub(crate) safe_mode: bool,
    pub(crate) forked_from_session_id: Option<String>,
    pub(crate) permission_mode: Option<rightkit_agent_policy::PermissionMode>,
    pub(crate) max_iterations: Option<u32>,
    pub(crate) budget_ceiling_usd: Option<f64>,
    /// Explicit CLI replacement prompt, scoped to this session.
    pub(crate) system_prompt: Option<String>,
    /// CLI prompt suffix, including structured-output constraints.
    pub(crate) append_system_prompt: Option<String>,
    /// Additional instruction roots for the canonical instruction loader.
    pub(crate) add_dir: Vec<String>,
    pub(crate) bare: bool,
    pub(crate) no_mcp: bool,
    pub(crate) mcp_config: Option<String>,
    pub(crate) lineage_budget: Option<Arc<rightkit_agent_engine_core::LineageBudget>>,
    /// Fable max-effort mode: force high reasoning effort and inject the
    /// constraint-rich [`FABLE_DIRECTIVE`] into the system prompt.
    pub(crate) fable_mode: bool,
    /// Brief default: inject [`BRIEF_DEFAULT_DIRECTIVE`] into every session
    /// and sub-agent prompt. Read from `prompt.brief-default` config (default
    /// true). Overridable per-workspace.
    pub(crate) brief_default: bool,
}

pub(crate) fn session_user_surfaces_enabled(options: &SessionRuntimeOptions) -> bool {
    !options.safe_mode
}

/// Why the daemon has no engine to route turns to. Recorded once at boot so
/// the user-facing message states the actual cause instead of always blaming a
/// missing API key — a configured-but-unresolvable provider route used to
/// report "add an OpenRouter key", sending users to fix something that was
/// never wrong.
static ENGINE_UNAVAILABLE_REASON: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub(crate) fn record_engine_unavailable_reason(reason: impl Into<String>) {
    let _ = ENGINE_UNAVAILABLE_REASON.set(reason.into());
}

pub(crate) fn missing_provider_key_message() -> String {
    match ENGINE_UNAVAILABLE_REASON.get() {
        Some(reason) => format!("Local engine is running, but no model provider is active: {reason}"),
        None => "Local engine is running, but no model provider is active. Configure a provider API key or a local provider route, then restart RightKit.".to_string(),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SubagentFork {
    pub(crate) sub_session_id: String,
    pub(crate) parent_session_id: String,
    pub(crate) role: String,
    task: String,
    pub(crate) workspace_root: String,
    execution_backend: Option<String>,
    execution_workspace_id: Option<String>,
    execution_workspace_path: Option<PathBuf>,
    execution_network_profile: Option<String>,
    mcp_channel: String,
    thinking_budget_tokens: Option<u32>,
    disable_thinking: bool,
    safe_mode: bool,
    max_iterations: Option<u32>,
    permission_mode: Option<rightkit_agent_policy::PermissionMode>,
    allowed_tools: Vec<String>,
    disallowed_tools: Vec<String>,
    budget_ceiling_usd: Option<f64>,
    lineage_budget: Option<Arc<rightkit_agent_engine_core::LineageBudget>>,
    brief_default: bool,
    provider_id: Option<String>,
    model: Option<String>,
    fallback_models: Vec<String>,
    toolset: Option<String>,
    verifier_model: Option<String>,
    verifier_is_independent: Option<bool>,
    effort: Option<String>,
    inference_selection: Option<crate::inference_runtime::InferenceSelection>,
    inference_fingerprint: Option<String>,
}

impl SubagentFork {
    pub(crate) fn runtime_options(&self) -> SessionRuntimeOptions {
        SessionRuntimeOptions {
            model: self.model.clone(),
            provider_id: self.provider_id.clone(),
            inference_selection: self.inference_selection.clone(),
            inference_fingerprint: self.inference_fingerprint.clone(),
            max_iterations: self.max_iterations,
            permission_mode: self.permission_mode,
            allowed_tools: self.allowed_tools.clone(),
            disallowed_tools: self.disallowed_tools.clone(),
            fallback_models: self.fallback_models.clone(),
            toolset: self.toolset.clone(),
            verifier_model: self.verifier_model.clone(),
            verifier_is_independent: self.verifier_is_independent,
            effort: self.effort.clone(),
            budget_ceiling_usd: self.budget_ceiling_usd,
            lineage_budget: self.lineage_budget.clone(),
            execution_backend: self.execution_backend.clone(),
            execution_workspace_id: self.execution_workspace_id.clone(),
            execution_workspace_path: self.execution_workspace_path.clone(),
            execution_network_profile: self.execution_network_profile.clone(),
            mcp_channel: self.mcp_channel.clone(),
            thinking_budget_tokens: self.thinking_budget_tokens,
            disable_thinking: self.disable_thinking,
            safe_mode: self.safe_mode,
            brief_default: self.brief_default,
            ..SessionRuntimeOptions::default()
        }
    }

    pub(crate) fn artifact_meta(&self, success: bool, message_count: usize) -> serde_json::Value {
        serde_json::json!({
            "parentSessionId": self.parent_session_id,
            "subSessionId": self.sub_session_id,
            "role": self.role,
            "taskTitle": self.task.chars().take(120).collect::<String>(),
            "transcriptRoute": format!("/sessions/{}/transcript", self.sub_session_id),
            "sideChain": true,
            "bodyStoredIn": "child-session-transcript",
            "parentTranscriptMerged": false,
            "cacheSafeFork": true,
            "providerCacheShared": false,
            "parentTranscriptSeeded": false,
            "seedMessageCount": 0,
            "success": success,
            "messageCount": message_count,
        })
    }
}

/// Return the native route requested for this one spawn, if any. A bare
/// native model keeps legacy provider derivation (model slug -> provider);
/// an explicit provider/model pair remains paired and wins over inheritance.
/// The result is carried on the child runtime options only, never persisted.
pub(crate) fn native_spawn_route(
    options: &rightkit_agent_tools::coding_tools::SubagentSpawnOptions,
) -> Option<(String, String)> {
    let runtime = options.runtime.as_ref()?;
    let rightkit_agent_tools::coding_tools::SpawnRuntimeKind::HostNative { model_id, .. } = runtime
    else {
        return None;
    };
    if model_id.trim().is_empty() {
        return None;
    }
    if let Some(route) = runtime.native_provider_model() {
        return Some(route);
    }
    Some((
        crate::model_policy::provider_id_for_model_slug(model_id).to_string(),
        model_id.trim().to_string(),
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_subagent_fork(
    sub_session_id: String,
    parent_session_id: String,
    role: String,
    task: String,
    workspace_root: String,
    parent_session: Option<&Arc<LiveSession>>,
    options: &rightkit_agent_tools::coding_tools::SubagentSpawnOptions,
    brief_default: bool,
    inherited_model: Option<String>,
) -> SubagentFork {
    let requested_budget = options
        .budget_microusd
        .map(|micros| micros as f64 / 1_000_000.0);
    let parent_budget = parent_session.and_then(|parent| parent.budget_ceiling_usd);
    let native_route = native_spawn_route(options);
    let (provider_id, model) = match native_route {
        Some((provider_id, model)) => (Some(provider_id), Some(model)),
        None => {
            let model = inherited_model;
            // Inheriting the parent's model keeps its resolved provider pin
            // (explicit or configured) so a fork reproduces the same route;
            // a different model re-derives provider from the slug instead.
            let parent_model =
                parent_session.map(|p| lock_recover(&p.model, "session.model").clone());
            let provider_id = if model.is_some() && model == parent_model {
                parent_session
                    .and_then(|p| lock_recover(&p.provider_id, "session.provider_id").clone())
            } else {
                None
            }
            .or_else(|| {
                model
                    .as_deref()
                    .map(crate::model_policy::provider_id_for_model_slug)
                    .map(str::to_string)
            });
            (provider_id, model)
        }
    };
    SubagentFork {
        sub_session_id,
        parent_session_id,
        role,
        task,
        workspace_root,
        execution_backend: parent_session.and_then(|p| p.execution_backend.clone()),
        execution_workspace_id: parent_session.and_then(|p| p.execution_workspace_id.clone()),
        execution_workspace_path: parent_session.and_then(|p| p.execution_workspace_path.clone()),
        execution_network_profile: parent_session.and_then(|p| p.execution_network_profile.clone()),
        mcp_channel: parent_session
            .map(|p| p.mcp_channel.clone())
            .unwrap_or_else(|| "desktop".to_string()),
        thinking_budget_tokens: parent_session.and_then(|p| p.thinking_budget_tokens),
        disable_thinking: parent_session.map(|p| p.disable_thinking).unwrap_or(false),
        safe_mode: parent_session.map(|p| p.safe_mode).unwrap_or(false),
        max_iterations: options.max_iterations,
        permission_mode: effective_child_permission_mode(parent_session, options.permission_mode),
        allowed_tools: effective_child_allowed_tools(parent_session, &options.allowed_tools),
        disallowed_tools: effective_child_disallowed_tools(
            parent_session,
            &options.disallowed_tools,
        ),
        budget_ceiling_usd: match (parent_budget, requested_budget) {
            (Some(parent), Some(requested)) => Some(parent.min(requested)),
            (Some(parent), None) => Some(parent),
            (None, requested) => requested,
        },
        lineage_budget: parent_session.map(|parent| parent.lineage_budget.clone()),
        brief_default,
        provider_id,
        model,
        fallback_models: Vec::new(),
        toolset: None,
        verifier_model: None,
        verifier_is_independent: None,
        effort: parent_session.and_then(|p| lock_recover(&p.effort, "session.effort").clone()),
        inference_selection: parent_session.and_then(|parent| {
            lock_recover(&parent.inference_selection, "session.inference_selection").clone()
        }),
        inference_fingerprint: parent_session.and_then(|parent| {
            lock_recover(
                &parent.inference_fingerprint,
                "session.inference_fingerprint",
            )
            .clone()
        }),
    }
}

impl SubagentFork {
    /// Bind runtime fields represented by `SessionRuntimeOptions`. Resolution
    /// already validated model, persona & verifier references; retain each
    /// assignment so a child cannot silently fall back after admission.
    pub(crate) fn apply_resolved_role_pack(
        &mut self,
        resolved: &rightkit_agent_orchestration::coding::role_pack::ResolvedRolePack,
    ) -> Result<(), String> {
        // Bind role-pack capabilities to the channel that will actually own
        // this child session.  Keep this at the spawn seam so a resolved pack
        // cannot be admitted while its MCP channel remains merely metadata.
        let available_channels =
            rightkit_agent_orchestration::coding::role_pack_mcp::McpChannelSet::from_channels([
                self.mcp_channel.as_str(),
            ]);
        let mcp_binding = rightkit_agent_orchestration::coding::role_pack_mcp::bind_role_pack_mcp(
            resolved,
            std::slice::from_ref(&self.mcp_channel),
            &available_channels,
        )
        .map_err(|error| format!("MCP channel binding failed: {error}"))?;
        if resolved.persona.is_none() {
            return Err(format!(
                "role pack '{}' has no bound persona",
                resolved.pack.role
            ));
        }
        if resolved.pack.toolset.len() != 1 {
            return Err(format!(
                "role pack '{}' has {} toolsets but child runtime accepts one",
                resolved.pack.role,
                resolved.pack.toolset.len(),
            ));
        }
        let toolset = resolved
            .pack
            .toolset
            .first()
            .filter(|toolset| !toolset.trim().is_empty())
            .cloned()
            .ok_or_else(|| format!("role pack '{}' has no runtime toolset", resolved.pack.role))?;
        let verifier_model = resolved.verifier.id.trim();
        if verifier_model.is_empty()
            || resolved.verifier_binding.model_id.trim().is_empty()
            || verifier_model != resolved.verifier_binding.model_id
        {
            return Err(format!(
                "role pack '{}' has an invalid verifier binding",
                resolved.pack.role
            ));
        }

        self.provider_id =
            Some(crate::model_policy::provider_id_for_model_slug(&resolved.primary.id).to_string());
        self.model = Some(resolved.primary.id.clone());
        self.fallback_models = resolved
            .fallbacks
            .iter()
            .map(|model| model.id.clone())
            .collect();
        self.toolset = Some(toolset);
        self.mcp_channel = mcp_binding.channels.first().cloned().ok_or_else(|| {
            format!(
                "role pack '{}' has no bound MCP channel",
                resolved.pack.role
            )
        })?;
        self.verifier_model = Some(verifier_model.to_string());
        self.verifier_is_independent = Some(resolved.verifier_binding.independent);
        self.effort = Some(resolved.pack.reasoning.clone());
        self.max_iterations = Some(
            self.max_iterations
                .map(|limit| limit.min(resolved.pack.max_turns))
                .unwrap_or(resolved.pack.max_turns),
        );
        self.budget_ceiling_usd = Some(
            self.budget_ceiling_usd
                .map(|limit| limit.min(resolved.pack.max_cost_usd))
                .unwrap_or(resolved.pack.max_cost_usd),
        );
        Ok(())
    }
}

fn effective_child_permission_mode(
    parent_session: Option<&Arc<LiveSession>>,
    requested: Option<rightkit_agent_policy::PermissionMode>,
) -> Option<rightkit_agent_policy::PermissionMode> {
    let parent = parent_session.and_then(|session| {
        session
            .permission_mode_tx
            .as_ref()
            .map(|sender| *sender.borrow())
    });
    let inherited = match (parent, requested) {
        (Some(parent), Some(requested)) => Some(
            if permission_mode_rank(requested) <= permission_mode_rank(parent) {
                requested
            } else {
                parent
            },
        ),
        (Some(parent), None) => Some(parent),
        (None, requested) => requested,
    };

    // A sub-agent has no interactive surface: its approval events go to its own
    // conduit, which no UI subscribes to, so an inherited `Ask` guarantees the
    // first gated tool blocks until it is denied by timeout. That is what wedged
    // conductor runs on their first `shell` call.
    //
    // Children therefore run non-interactively under `AutoSafe`: command-aware
    // classification lets read-only work (`ls`, `find`, `git status`) proceed,
    // while destructive and process-control commands still require an approval
    // — and, absent a responder, are denied rather than run. Deny rules,
    // workspace trust, the destructive guard and durable-write provenance all
    // still apply. `AcceptEdits` would not do: it still asks for every shell
    // call, which is the exact hang. Stricter explicit requests (Plan) are kept.
    match inherited {
        Some(rightkit_agent_policy::PermissionMode::Ask) => {
            Some(rightkit_agent_policy::PermissionMode::AutoSafe)
        }
        other => other,
    }
}

fn permission_mode_rank(mode: rightkit_agent_policy::PermissionMode) -> u8 {
    match mode {
        rightkit_agent_policy::PermissionMode::Plan => 0,
        rightkit_agent_policy::PermissionMode::Ask => 1,
        rightkit_agent_policy::PermissionMode::AcceptEdits => 2,
        rightkit_agent_policy::PermissionMode::AutoSafe => 3,
        rightkit_agent_policy::PermissionMode::Bypass => 4,
    }
}

fn effective_child_allowed_tools(
    parent_session: Option<&Arc<LiveSession>>,
    requested: &[String],
) -> Vec<String> {
    let parent = parent_session
        .map(|session| session.allowed_tools.as_slice())
        .unwrap_or_default();
    let mut allowed = match (parent.is_empty(), requested.is_empty()) {
        (false, false) => parent
            .iter()
            .filter(|tool| requested.iter().any(|requested| requested == *tool))
            .cloned()
            .collect(),
        (false, true) => parent.to_vec(),
        (true, false) => requested.to_vec(),
        (true, true) => Vec::new(),
    };
    allowed.sort();
    allowed.dedup();
    allowed
}

fn effective_child_disallowed_tools(
    parent_session: Option<&Arc<LiveSession>>,
    requested: &[String],
) -> Vec<String> {
    let mut disallowed = parent_session
        .map(|session| session.disallowed_tools.clone())
        .unwrap_or_default();
    disallowed.extend(requested.iter().cloned());
    disallowed.sort();
    disallowed.dedup();
    disallowed
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_subagent_runtime_span(
    state: &DaemonState,
    phase: MiddlewarePhase,
    status: &str,
    parent_session_id: &str,
    parent_span_id: Option<&str>,
    sub_session_id: &str,
    role: &str,
    task: &str,
    workspace: &str,
    execution_backend: Option<&str>,
    execution_workspace_id: Option<&str>,
    execution_network_profile: Option<&str>,
    mcp_channel: Option<&str>,
    message: Option<&str>,
    extra: serde_json::Value,
) -> Option<String> {
    let definitions =
        lock_recover(&state.runtime_middleware, "runtime_middleware").enabled_for_phase(phase);
    if definitions.is_empty() {
        return None;
    }
    let middleware_ids = definitions
        .iter()
        .map(|definition| definition.id.clone())
        .collect::<Vec<_>>();
    let now = now_iso();
    let id = format!(
        "runtime-{}-{}-{}",
        parent_session_id,
        api_middleware_phase_key(phase),
        now_millis()
    );
    let attributes = merge_json_objects(
        serde_json::json!({
            "middlewareIds": middleware_ids,
            "middlewareCount": definitions.len(),
            "parentSessionId": parent_session_id,
            "subSessionId": sub_session_id,
            "role": role,
            "taskTitle": task.chars().take(80).collect::<String>(),
            "workspaceRoot": workspace,
            "executionBackend": execution_backend,
            "executionWorkspaceId": execution_workspace_id,
            "executionNetworkProfile": execution_network_profile,
            "mcpChannel": mcp_channel,
            "message": message,
        }),
        extra,
    );
    lock_recover(&state.runtime_middleware, "runtime_middleware").record_span(RuntimeSpan {
        id: id.clone(),
        parent_id: parent_span_id.map(str::to_string),
        session_id: parent_session_id.to_string(),
        turn_id: None,
        phase,
        status: status.to_string(),
        started_at: now.clone(),
        ended_at: Some(now),
        attributes,
    });
    Some(id)
}

fn merge_json_objects(mut base: serde_json::Value, extra: serde_json::Value) -> serde_json::Value {
    if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    base
}

fn api_middleware_phase_key(phase: MiddlewarePhase) -> &'static str {
    match phase {
        MiddlewarePhase::SubagentStart => "subagentStart",
        MiddlewarePhase::SubagentStop => "subagentStop",
        _ => "middleware",
    }
}

// NUL cannot occur in a filesystem path, so this local-only marker cannot
// collide with an explicit caller-supplied worktree path.
const POOLED_WORKTREE_CLEANUP_PREFIX: &str = "\0pool:";

pub(crate) async fn cleanup_subagent_worktree(parent_workspace: &str, path: &str) {
    if let Some(pooled_path) = path.strip_prefix(POOLED_WORKTREE_CLEANUP_PREFIX) {
        let parent = PathBuf::from(parent_workspace);
        let pooled_path = PathBuf::from(pooled_path);
        // Match existing best-effort cleanup semantics: a failed release must
        // not hide the completed/failed subagent result from its parent.
        if let Ok(pool) = open_subagent_worktree_pool(&parent) {
            let _ = pool.release_async(&pooled_path).await;
        }
        return;
    }
    let _ = crate::api_environments::run_managed_process(
        "git",
        &["worktree", "remove", "--force", path],
        parent_workspace,
    )
    .await;
}

pub(crate) fn open_subagent_worktree_pool(
    parent_workspace: &std::path::Path,
) -> Result<rightkit_agent_tools::coding_tools::worktree_pool::WorktreePool, String> {
    let pool_dir = parent_workspace
        .join(".coderight")
        .join("subagent-worktree-pool");
    let state_dir = parent_workspace
        .join(".coderight")
        .join("subagent-worktree-pool-state");
    rightkit_agent_tools::coding_tools::worktree_pool::WorktreePool::open(
        parent_workspace,
        &pool_dir,
        &state_dir,
    )
    .map_err(|error| format!("failed to open sub-agent worktree pool: {error}"))
}

/// The workspace's shared worktree pool with the governed child-process
/// launcher attached. Required for any acquire, fill, or release, so git
/// children run under the same process authority as subagent worktrees.
pub(crate) fn governed_workspace_worktree_pool(
    workspace: &std::path::Path,
) -> Result<rightkit_agent_tools::coding_tools::worktree_pool::WorktreePool, String> {
    let process_adapter = crate::main_runtime::governed_child_process_launcher()
        .map_err(|error| format!("worktree pool process authority unavailable: {error}"))?;
    Ok(open_subagent_worktree_pool(workspace)?.with_process_launch(
        process_adapter,
        rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: workspace.to_string_lossy().into_owned(),
            sandbox_profile: Some("workspace".into()),
            sandbox_required: true,
            network_required: false,
        },
    ))
}

pub(crate) async fn prepare_subagent_workspace(
    parent_workspace: &str,
    role: &str,
    sub_id: &str,
    options: &rightkit_agent_tools::coding_tools::SubagentSpawnOptions,
) -> Result<(String, Option<String>), String> {
    let Some(worktree) = &options.worktree else {
        return Ok((parent_workspace.to_string(), None));
    };
    let path = worktree
        .path
        .clone()
        .unwrap_or_else(|| default_subagent_worktree_path(role, sub_id));
    let parent = PathBuf::from(parent_workspace);
    if parent_workspace.trim().is_empty() {
        return Err("sub-agent worktree requested but parent workspace is empty".into());
    }
    // Only anonymous detached worktrees owned by this cleanup path are
    // interchangeable. Explicit paths, branches, and retained checkouts keep
    // existing caller-selected git semantics below.
    if worktree.path.is_none()
        && worktree
            .branch
            .as_deref()
            .is_none_or(|branch| branch.trim().is_empty())
        && worktree.cleanup
    {
        let process_adapter =
            crate::main_runtime::governed_child_process_launcher().map_err(|error| {
                format!("sub-agent worktree process authority unavailable: {error}")
            })?;
        let pool = open_subagent_worktree_pool(&parent)?.with_process_launch(
            process_adapter,
            rightkit_sandbox::execution::ProcessLaunchContext {
                workspace_root: parent.to_string_lossy().into_owned(),
                sandbox_profile: Some("workspace".into()),
                sandbox_required: true,
                network_required: false,
            },
        );
        pool.fill_async(1)
            .await
            .map_err(|error| format!("failed to warm sub-agent worktree pool: {error}"))?;
        let worktree = pool
            .acquire_async(false)
            .await
            .map_err(|error| format!("failed to acquire pooled sub-agent worktree: {error}"))?;
        // Retain one ready checkout while this lease is in use. A refill
        // failure cannot invalidate an already-acquired child workspace.
        let _ = pool.fill_async(1).await;
        let workspace = worktree.to_string_lossy().into_owned();
        return Ok((
            workspace.clone(),
            Some(format!("{POOLED_WORKTREE_CLEANUP_PREFIX}{workspace}")),
        ));
    }
    if let Some(parent_dir) = PathBuf::from(&path).parent() {
        std::fs::create_dir_all(parent_dir)
            .map_err(|e| format!("failed to create worktree parent directory: {e}"))?;
    }

    let output = if let Some(branch) = worktree.branch.as_deref().filter(|s| !s.trim().is_empty()) {
        crate::api_environments::run_managed_process(
            "git",
            &["worktree", "add", "-b", branch, &path, "HEAD"],
            &parent.to_string_lossy(),
        )
        .await
    } else {
        crate::api_environments::run_managed_process(
            "git",
            &["worktree", "add", &path, "HEAD"],
            &parent.to_string_lossy(),
        )
        .await
    };

    match output {
        Ok(out) if out.success => {
            let cleanup = worktree.cleanup.then_some(path.clone());
            Ok((path, cleanup))
        }
        Ok(out) => Err(format!(
            "failed to create sub-agent worktree: {}",
            out.stderr
        )),
        Err(e) => Err(format!("failed to run git worktree: {e}")),
    }
}

fn default_subagent_worktree_path(role: &str, sub_id: &str) -> String {
    let safe_role: String = role
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    std::env::temp_dir()
        .join(format!("coderight-subagent-{safe_role}-{sub_id}"))
        .to_string_lossy()
        .into_owned()
}

async fn interrupted_journal_markers(
    journal_dir: &FsPath,
    session_id: &str,
) -> Vec<TranscriptMessage> {
    crate::journal::ExecutionJournal::interrupted_runs(journal_dir, session_id)
        .await
        .into_iter()
        .map(|run| {
            let mut content = format!(
                "Recovery note: the previous agent turn was interrupted before a clean run_end. turn_id={} started_at={}. Review the workspace, transcript, and tool side effects before retrying; do not assume partial side effects were rolled back.",
                run.turn_id, run.started_at
            );
            if let Some(preview) = run.user_message_preview.as_deref().filter(|s| !s.is_empty()) {
                content.push_str("\nInterrupted prompt preview: ");
                content.push_str(preview);
            }
            TranscriptMessage::System {
                id: format!("journal-interrupted-{}", run.turn_id),
                created_at: now_iso(),
                content,
                pinned: true,
            }
        })
        .collect()
}

// ── B3 / C3 run-budget ledger ───────────────────────────────────────────────────
//
// Per-run concurrent + cumulative sub-agent limits live here so the same
// module that builds a [`SubagentFork`] owns the gate that admits it. The
// ledger is self-contained (no storage seam), which keeps the failure
// path synchronous and machine-deterministic for tests.

/// The configuration a run may apply to cap its own sub-agent activity.
/// Carried on `SessionRuntimeOptions` once it leaves the fork builder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunSubagentBudget {
    /// Max **simultaneous** sub-agents the run may keep in flight. A spawn
    /// that would exceed this limit is refused synchronously.
    pub concurrent_max: u32,
    /// Max sub-agents the run may accept across its **entire lifetime**.
    /// Counts retries, restarts, and completions equally; only declined
    /// spawns do not count.
    pub cumulative_max: u32,
}

impl Default for RunSubagentBudget {
    fn default() -> Self {
        Self {
            concurrent_max: Self::DEFAULT_CONCURRENT,
            cumulative_max: Self::DEFAULT_CUMULATIVE,
        }
    }
}

impl RunSubagentBudget {
    pub const DEFAULT_CONCURRENT: u32 = 4;
    pub const DEFAULT_CUMULATIVE: u32 = 64;

    pub fn with_concurrent(mut self, n: u32) -> Self {
        self.concurrent_max = n;
        self
    }

    pub fn with_cumulative(mut self, n: u32) -> Self {
        self.cumulative_max = n;
        self
    }
}

/// Where the run sits inside the lifecycle — used to keep cumulative counts
/// correct on restart/recovery without double-counting previously completed
/// sub-agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentLifecycleMark {
    /// Sub-agent finished (success or failure); counts only toward the
    /// cumulative total.
    Completed,
}

/// Result of an admission decision. Errors carry enough context for the
/// caller to surface a useful message and pick whether to retry, queue, or
/// surface the failure as a typed [`RunGovernanceRejected`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetDecision {
    /// Spawn admitted; caller increments its in-flight count exactly once
    /// when it transitions to `InFlight`.
    Admit,
    /// Spawn refused because the concurrent cap would be exceeded.
    RefuseConcurrent {
        concurrent_limit: u32,
        concurrent_in_flight: u32,
        cumulative_limit: u32,
        cumulative_used: u32,
    },
    /// Spawn refused because the cumulative cap would be exceeded.
    RefuseCumulative {
        concurrent_limit: u32,
        concurrent_in_flight: u32,
        cumulative_limit: u32,
        cumulative_used: u32,
    },
}

/// Per-run sub-agent ledger.
#[derive(Debug)]
pub struct RunBudgetLedger {
    budget: RunSubagentBudget,
    /// Cumulative counts only move forward (except via cancellation, which
    /// rolls the entry back). Store as a flat counter rather than a HashMap
    /// so the lock overhead stays minimal.
    cumulative_used: u32,
    concurrent_in_flight: u32,
    /// Children whose slot has already been released, so cancel and
    /// completion racing on the same child release it exactly once.
    released: std::collections::HashSet<String>,
}

impl RunBudgetLedger {
    pub fn new(budget: RunSubagentBudget) -> Self {
        Self {
            budget,
            cumulative_used: 0,
            concurrent_in_flight: 0,
            released: std::collections::HashSet::new(),
        }
    }

    /// Release the concurrent slot held by `child_id` at most once. Returns
    /// `false` (and changes nothing) on a repeat release.
    pub fn mark_completed_once(&mut self, child_id: &str) -> bool {
        if !self.released.insert(child_id.to_string()) {
            return false;
        }
        self.mark(SubagentLifecycleMark::Completed);
        true
    }

    pub fn cumulative_used(&self) -> u32 {
        self.cumulative_used
    }

    pub fn concurrent_in_flight(&self) -> u32 {
        self.concurrent_in_flight
    }

    /// Refresh configured limits without losing run-scoped usage. On daemon
    /// restart there are no surviving in-flight workers, while the persisted
    /// cumulative count remains authoritative.
    pub fn configure(&mut self, budget: RunSubagentBudget, persisted_cumulative: Option<u32>) {
        self.budget = budget;
        if self.cumulative_used == 0 {
            self.cumulative_used = persisted_cumulative.unwrap_or(0);
        }
    }

    /// Try to admit a new sub-agent. Returns a [`BudgetDecision`] and
    /// advances the counters only when the call is `Admit`.
    pub fn try_admit(&mut self) -> BudgetDecision {
        if self.concurrent_in_flight >= self.budget.concurrent_max
            && self.budget.concurrent_max != u32::MAX
        {
            return BudgetDecision::RefuseConcurrent {
                concurrent_limit: self.budget.concurrent_max,
                concurrent_in_flight: self.concurrent_in_flight,
                cumulative_limit: self.budget.cumulative_max,
                cumulative_used: self.cumulative_used,
            };
        }
        if self.cumulative_used >= self.budget.cumulative_max
            && self.budget.cumulative_max != u32::MAX
        {
            return BudgetDecision::RefuseCumulative {
                concurrent_limit: self.budget.concurrent_max,
                concurrent_in_flight: self.concurrent_in_flight,
                cumulative_limit: self.budget.cumulative_max,
                cumulative_used: self.cumulative_used,
            };
        }
        self.cumulative_used = self.cumulative_used.saturating_add(1);
        self.concurrent_in_flight = self.concurrent_in_flight.saturating_add(1);
        BudgetDecision::Admit
    }

    /// Apply a lifecycle mark. The ledger refuses unexpected transitions
    /// so retry/cancel/restart/completion bookkeeping stays consistent.
    pub fn mark(&mut self, mark: SubagentLifecycleMark) {
        match mark {
            SubagentLifecycleMark::Completed => {
                self.concurrent_in_flight = self.concurrent_in_flight.saturating_sub(1);
            }
        }
    }

    /// Cancel-and-rollback for a spawn that was admitted but rejected by a
    /// downstream seam (e.g. the worktree failed to create). Returns the
    /// caller back to the state they were in before `try_admit`.
    pub fn rollback_after_admit(&mut self) {
        self.concurrent_in_flight = self.concurrent_in_flight.saturating_sub(1);
        self.cumulative_used = self.cumulative_used.saturating_sub(1);
    }
}
