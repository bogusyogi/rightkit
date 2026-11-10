//! PR 5: CLI-backed subagent spawn helper.
//!
//! Wires the daemon's [`SubagentSpawner`] into the authenticated-CLI
//! runtime. When `SubagentSpawnOptions::runtime` is
//! [`SpawnRuntimeKind::AuthenticatedCli`], the spawn helper:
//!
//! 1. Resolves the requested runtime against the session's
//!    [`CliDelegationPolicy`] via [`resolve_child_runtime`]. A
//!    `DelegationError` rejects the spawn before any storage or
//!    adapter call.
//! 2. Records the parent's `idempotency_key` against the driver's
//!    `driver_event_inbox` table so a duplicate replay returns the
//!    existing child's id without spawning a second vendor process.
//! 3. Persists a new row in `driver_bindings` with `parent_agent_id`
//!    set so the UI's task graph can render the parent → child edge.
//! 4. Issues a new `session_capability_leases` row tied to the child
//!    binding so the child can call back into the daemon.
//! 5. Opens the child through [`AuthenticatedCliExecutor`], sends the
//!    [`CliHandoffBrief`] prompt, folds the vendor events into an
//!    [`AgentResult`], and returns the structured result alongside
//!    the child's `coderight_agent_id`.
//!
//! [`SpawnRuntimeKind::AuthenticatedCli`]: rightkit_agent_tools::coding_tools::SpawnRuntimeKind::AuthenticatedCli
//! [`CliDelegationPolicy`]: rightkit_agents::driver_host::cli_runtime::CliDelegationPolicy

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use rightkit_agent_engine_protocol::ProtocolEvent;
use rightkit_agent_engine_storage::{
    DriverBindingRow, DriverEventClaim, DriverEventInboxRow, SessionCapabilityLeaseRow, Storage,
    StorageError,
};
use rightkit_agent_orchestration::coding::supervision::{
    enter_worktree, exit_worktree, SystemGitWorktreeExecutor, WorktreeEnterRequest,
};
use rightkit_agent_tools::coding_tools::{
    SpawnRuntimeKind, SubagentContinuity, SubagentSpawnOptions,
};
use rightkit_agents::driver_host::cli_runtime::{
    resolve_child_runtime, AgentKind, AgentResult, AgentResultStatus, AgentRuntimeBackend,
    AgentRuntimeExecutor, AgentTurnRequest, AgentTurnStatus, AuthenticatedCliExecutor,
    CliAccountProfile, CliAccountProfileId, CliAuthMode, CliExecutorLaunchContext, CliHandoffBrief,
    CliMcpServerSpec, CliProviderId, DelegateTaskRequest, MutationPolicy, RegisteredProviders,
    RequestedRuntime, RuntimeResolution, RuntimeResolutionSource,
};
use serde_json::{json, Value};

use crate::api::DaemonState;

/// What the spawn helper returns. `child_agent_id` is the
/// RightKit-native id used by every downstream consumer (UI rail,
/// agent roster, follow-up `ContinueAgent` calls). `result` is the
/// structured [`AgentResult`] so the parent turn gets evidence refs
/// and the provider id without parsing `final_text` for hidden
/// metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliChildSpawnOutcome {
    pub child_agent_id: String,
    pub result: AgentResult,
}

/// Why a CLI spawn was rejected at the spawn-helper boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliSpawnRejection {
    /// The session's [`CliDelegationPolicy`] refused the request.
    /// Mirrors the resolver's [`DelegationError`] so the daemon can
    /// surface the reason in a single error envelope.
    Policy(String),
    /// A row in storage referenced a binding or session that does not
    /// exist. Surfaced verbatim so the caller can distinguish a
    /// permission rejection from a stale-state rejection.
    Storage(String),
    /// The executor returned a runtime error during `open_agent` or
    /// `run_turn`. The bridge surfaces bridge errors verbatim; the
    /// daemon does not strip their `code` field.
    Runtime(String),
}

impl std::fmt::Display for CliSpawnRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliSpawnRejection::Policy(message) => write!(f, "policy: {message}"),
            CliSpawnRejection::Storage(message) => write!(f, "storage: {message}"),
            CliSpawnRejection::Runtime(message) => write!(f, "runtime: {message}"),
        }
    }
}

impl std::error::Error for CliSpawnRejection {}

/// Configuration needed for a CLI-backed spawn. The daemon passes
/// its already-built [`AuthenticatedCliExecutor`] so the helper does
/// not have to know how the executor was constructed.
pub struct CliDelegationContext {
    pub executor: Arc<AuthenticatedCliExecutor>,
    pub registered_provider_ids: BTreeSet<CliProviderId>,
    pub parent_session_id: String,
    pub parent_agent_id: String,
    pub child_session_id: String,
    pub child_agent_id: String,
    pub role: String,
    pub task: String,
    pub safe_mode: bool,
    pub options: SubagentSpawnOptions,
}

/// Dispatch a CLI-backed spawn on behalf of `DaemonState`. This is
/// the entry point the `SubagentSpawner::spawn` impl calls when the
/// caller asks for an authenticated CLI runtime. Owns the full
/// lifecycle: policy resolution, idempotency claim, child binding,
/// capability lease, executor open + run, structured result
/// projection.
pub async fn dispatch_cli_spawn(
    state: DaemonState,
    role: String,
    task_text: String,
    parent_id: String,
    options: SubagentSpawnOptions,
) -> Result<String, String> {
    let executor = state
        .cli_executor
        .clone()
        .ok_or_else(|| "cli_executor: not wired on this daemon".to_string())?;
    let registered = state.cli_registry_provider_ids.clone();
    let policy =
        crate::api::lock_recover(&state.cli_delegation_policies, "cli_delegation_policies")
            .get(&parent_id)
            .cloned()
            .unwrap_or_else(|| state.cli_delegation_policy.clone());
    let parent_session = state.get_session(&parent_id);
    let Some(parent) = parent_session else {
        return Err(format!("parent session not found: {parent_id}"));
    };

    // Step 1: resolve the requested runtime against policy.
    let parent_kind = if parent.parent_session_id.is_some() {
        AgentKind::Child
    } else {
        AgentKind::Root
    };
    let request = build_delegate_request(parent_kind, &options, None);
    let registered_view = RegisteredProviders::from(registered.clone());
    let resolution = match resolve_for_session(&request, &policy, &registered_view) {
        Ok(resolution) => resolution,
        Err(rejection) => return Err(format!("{rejection}")),
    };
    if matches!(resolution.source, RuntimeResolutionSource::NativeFallback) {
        // Caller asked for a CLI runtime but the policy resolved to
        // native — bounce back to the legacy path rather than
        // double-spawning.
        return Err(
            "delegate_task: policy resolved to native fallback but runtime requested a CLI child; \
             refusing to spawn a CLI child without an explicit CLI runtime"
                .into(),
        );
    }

    // Step 2: build the child session + agent ids. PR 5 uses the
    // session id as the agent id; a stable mapping lands with the
    // parent-id-column migration follow-up in PR 6.
    let child_agent_id = format!("cli-agent-{}", crate::startup::generate_token());
    let child_session_id = format!("cli-child-{parent_id}-{child_agent_id}");
    let parent_agent_id = parent_id.clone();

    // Step 3: exact-once authority belongs to `call_capability`, which
    // claimed this gateway call before it reached `delegate_task`. Do not
    // create a second internal claim with a generated id: it cannot replay
    // the gateway's persisted structured result.

    let binding_id = format!("binding-{child_agent_id}");
    // FM-161: an explicitly requested worktree is entered before any child
    // session or vendor process is admitted. The lease owns only the checkout
    // directory; the child's branch commits remain its deliverable.
    let entered_worktree = enter_child_worktree(&parent, &options, &child_agent_id)?;
    let mut child_runtime_options = match child_session_options(&parent, &options) {
        Ok(options) => options,
        Err(error) => {
            leave_child_worktree(entered_worktree.as_ref());
            return Err(error);
        }
    };
    let child_workspace_root = entered_worktree
        .as_ref()
        .map(|worktree| worktree.path.clone())
        .unwrap_or_else(|| PathBuf::from(&parent.workspace_root));
    if entered_worktree.is_some() {
        child_runtime_options.execution_workspace_path = Some(child_workspace_root.clone());
    }
    let prepared = match prepare_child_runtime(
        &state,
        &parent,
        &resolution,
        &binding_id,
        &child_session_id,
        &child_agent_id,
        &parent_agent_id,
        &child_runtime_options,
    )
    .await
    {
        Ok(prepared) => prepared,
        Err(error) => {
            leave_child_worktree(entered_worktree.as_ref());
            return Err(error);
        }
    };
    // A real child session is required for capability routing; it is created
    // only after every persisted provider/attestation check has passed.
    let effective_spawn_options = SubagentSpawnOptions {
        allowed_tools: child_runtime_options.allowed_tools.clone(),
        disallowed_tools: child_runtime_options.disallowed_tools.clone(),
        ..options.clone()
    };
    state.create_session_with_options(
        &child_session_id,
        Some(&role),
        &child_workspace_root.to_string_lossy(),
        &format!("{role} subtask"),
        Some(&parent_id),
        child_runtime_options,
    );
    let launch_contexts = match state.cli_launch_contexts.as_ref() {
        Some(contexts) => contexts,
        None => {
            cleanup_child_runtime(&state, &binding_id, &prepared.lease_id).await;
            crate::api::lock_recover(&state.sessions, "sessions").remove(&child_session_id);
            leave_child_worktree(entered_worktree.as_ref());
            return Err("cli launch contexts: unavailable".to_string());
        }
    };
    if let Err(error) = launch_contexts.insert(
        binding_id.clone(),
        child_session_id.clone(),
        child_agent_id.clone(),
        prepared.context,
    ) {
        cleanup_child_runtime(&state, &binding_id, &prepared.lease_id).await;
        crate::api::lock_recover(&state.sessions, "sessions").remove(&child_session_id);
        leave_child_worktree(entered_worktree.as_ref());
        return Err(format!("runtime: {error}"));
    }

    // Step 6: drive the executor. `spawn_cli_child` is the pure
    // helper that runs `open_agent` + `run_turn` and returns the
    // structured result.
    let ctx = CliDelegationContext {
        executor,
        registered_provider_ids: registered,
        parent_session_id: parent_id.clone(),
        parent_agent_id: parent_agent_id.clone(),
        child_session_id: child_session_id.clone(),
        child_agent_id: child_agent_id.clone(),
        role: role.clone(),
        task: task_text.clone(),
        safe_mode: parent.safe_mode,
        options: effective_spawn_options,
    };
    match spawn_cli_child(ctx, resolution).await {
        Ok(outcome) => {
            if let Some(storage) = state.storage.as_ref() {
                let store = storage.lock().await;
                let _ = store.update_driver_binding_progress(
                    &binding_id,
                    outcome.result.vendor_session_id.as_deref(),
                    None,
                    None,
                    "completed",
                    None,
                    None,
                    &crate::api::now_iso(),
                );
            }
            // Publish the completed event onto the parent's conduit
            // so the UI rail picks it up.
            let seq = parent.conduit.next_seq();
            let event = child_completed_event(
                &parent_id,
                seq,
                &crate::api::now_iso(),
                &outcome.child_agent_id,
                &role,
                &outcome.result,
            );
            parent.conduit.publish(event);
            if matches!(
                outcome.result.status,
                AgentResultStatus::Failed | AgentResultStatus::Cancelled
            ) {
                if let Some(contexts) = state.cli_launch_contexts.as_ref() {
                    contexts.remove(&binding_id);
                }
                cleanup_child_runtime(&state, &binding_id, &prepared.lease_id).await;
            }
            leave_child_worktree(entered_worktree.as_ref());
            Ok(json!({
                "agentId": outcome.child_agent_id,
                "provider": outcome.result.provider_id,
                "model": Value::Null,
                "vendorSessionId": outcome.result.vendor_session_id,
                "evidence": outcome.result.evidence_refs,
                "status": outcome.result.status,
                "finalText": outcome.result.final_text,
            })
            .to_string())
        }
        Err(rejection) => {
            if let Some(contexts) = state.cli_launch_contexts.as_ref() {
                contexts.remove(&binding_id);
            }
            cleanup_child_runtime(&state, &binding_id, &prepared.lease_id).await;
            crate::api::lock_recover(&state.sessions, "sessions").remove(&child_session_id);
            leave_child_worktree(entered_worktree.as_ref());
            Err(format!("{rejection}"))
        }
    }
}

struct EnteredChildWorktree {
    repo_root: PathBuf,
    path: PathBuf,
    cleanup: bool,
}

fn enter_child_worktree(
    parent: &crate::api::LiveSession,
    options: &SubagentSpawnOptions,
    child_agent_id: &str,
) -> Result<Option<EnteredChildWorktree>, String> {
    let Some(requested) = options.worktree.as_ref() else {
        return Ok(None);
    };
    let repo_root = PathBuf::from(&parent.workspace_root);
    let branch = requested
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("coderight/{child_agent_id}"));
    let request = WorktreeEnterRequest {
        repo_root: repo_root.clone(),
        branch,
        base_ref: None,
        path: requested.path.as_deref().map(PathBuf::from),
    };
    let plan = enter_worktree(&request, &SystemGitWorktreeExecutor)
        .map_err(|error| format!("worktree enter rejected: {error}"))?;
    let path = if plan.worktree_path.is_absolute() {
        plan.worktree_path
    } else {
        repo_root.join(plan.worktree_path)
    };
    Ok(Some(EnteredChildWorktree {
        repo_root,
        path,
        cleanup: requested.cleanup,
    }))
}

fn leave_child_worktree(worktree: Option<&EnteredChildWorktree>) {
    let Some(worktree) = worktree else {
        return;
    };
    if let Err(error) = exit_worktree(
        &worktree.repo_root,
        &worktree.path,
        worktree.cleanup,
        &SystemGitWorktreeExecutor,
    ) {
        tracing::warn!(path = %worktree.path.display(), %error, "worktree exit failed after CLI child");
    }
}

struct PreparedChildRuntime {
    context: CliExecutorLaunchContext,
    lease_id: String,
}

fn child_session_options(
    parent: &crate::api::LiveSession,
    options: &SubagentSpawnOptions,
) -> Result<crate::api::SessionRuntimeOptions, String> {
    let use_safe_default =
        parent.safe_mode && parent.allowed_tools.is_empty() && options.allowed_tools.is_empty();
    let allowed_tools = if options.allowed_tools.is_empty() {
        parent.allowed_tools.clone()
    } else if parent.allowed_tools.is_empty() {
        options.allowed_tools.clone()
    } else {
        options
            .allowed_tools
            .iter()
            .filter(|tool| parent.allowed_tools.contains(tool))
            .cloned()
            .collect()
    };
    let mut disallowed_tools = parent.disallowed_tools.clone();
    for tool in &options.disallowed_tools {
        if !disallowed_tools.contains(tool) {
            disallowed_tools.push(tool.clone());
        }
    }
    let allowed_tools = enforce_child_tool_ceiling(
        parent.safe_mode,
        use_safe_default,
        allowed_tools,
        &disallowed_tools,
    )?;
    Ok(crate::api::SessionRuntimeOptions {
        allowed_tools,
        disallowed_tools,
        safe_mode: parent.safe_mode,
        execution_backend: parent.execution_backend.clone(),
        execution_workspace_id: parent.execution_workspace_id.clone(),
        execution_workspace_path: parent.execution_workspace_path.clone(),
        execution_network_profile: parent.execution_network_profile.clone(),
        budget_ceiling_usd: parent.budget_ceiling_usd,
        lineage_budget: Some(parent.lineage_budget.clone()),
        ..Default::default()
    })
}

fn enforce_child_tool_ceiling(
    safe_mode: bool,
    use_safe_default: bool,
    mut allowed_tools: Vec<String>,
    disallowed_tools: &[String],
) -> Result<Vec<String>, String> {
    if safe_mode {
        let read_only = rightkit_agent_tools::coding_tools::ToolRegistry::build_default()
            .capabilities()
            .into_iter()
            .filter(|capability| capability.read_only)
            .map(|capability| capability.name)
            .collect::<BTreeSet<_>>();
        if allowed_tools.is_empty() && use_safe_default {
            allowed_tools.extend(read_only.iter().cloned());
        } else {
            allowed_tools.retain(|tool| read_only.contains(tool));
        }
    }
    allowed_tools.retain(|tool| !disallowed_tools.contains(tool));
    allowed_tools.sort();
    allowed_tools.dedup();
    if safe_mode && allowed_tools.is_empty() {
        return Err(
            "safe authenticated CLI child has no read-only tools after capability filtering"
                .to_string(),
        );
    }
    Ok(allowed_tools)
}

#[allow(clippy::too_many_arguments)]
async fn prepare_child_runtime(
    state: &DaemonState,
    parent: &std::sync::Arc<crate::api::LiveSession>,
    resolution: &RuntimeResolution,
    binding_id: &str,
    session_id: &str,
    agent_id: &str,
    parent_agent_id: &str,
    child_options: &crate::api::SessionRuntimeOptions,
) -> Result<PreparedChildRuntime, String> {
    let AgentRuntimeBackend::AuthenticatedCli {
        provider_id,
        model_id,
        account_profile_id,
        ..
    } = &resolution.selected
    else {
        return Err("CLI resolution has no authenticated provider".into());
    };
    let storage = state
        .storage
        .as_ref()
        .ok_or_else(|| "CLI storage is unavailable".to_string())?;
    let adapter = state.cli_registry.adapter(provider_id).ok_or_else(|| {
        format!(
            "provider adapter is not registered: {}",
            provider_id.as_str()
        )
    })?;
    let profile_row = storage
        .lock()
        .await
        .get_cli_account_profile(account_profile_id.as_str())
        .map_err(|error| format!("profile storage: {error}"))?;
    let default_profile = crate::api_drivers::default_cli_profile(provider_id.clone());
    let profile = match profile_row {
        Some(profile_row) => CliAccountProfile {
            id: CliAccountProfileId::new(profile_row.id),
            provider_id: provider_id.clone(),
            label: profile_row.label,
            managed_home: PathBuf::from(profile_row.managed_home),
            auth_mode: match profile_row.auth_mode.as_str() {
                "native_cli_account" => CliAuthMode::NativeCliAccount,
                "byok_or_external_api" => CliAuthMode::ByokOrExternalApi,
                _ => CliAuthMode::Unknown,
            },
            signed_in: profile_row.signed_in,
            account_fingerprint: profile_row.account_fingerprint,
            last_verified_at: profile_row.last_verified_at,
        },
        None if crate::api_drivers::missing_default_profile_is_allowed_current(
            provider_id,
            account_profile_id.as_str(),
        )
        .map_err(|error| error.code().to_string())? =>
        {
            default_profile
        }
        None => return Err("authenticated CLI profile is not persisted".into()),
    };
    let resolved = crate::api_drivers::resolve_cli_launch_route(
        state,
        provider_id,
        profile,
        model_id.as_deref(),
    )
    .await
    .map_err(|error| error.code().to_string())?;
    let profile = resolved.launch_profile.clone();
    if profile.provider_id.as_str() != provider_id.as_str()
        || profile.id.as_str() != account_profile_id.as_str()
        || !profile.managed_home.is_absolute()
        || !profile.managed_home.is_dir()
    {
        return Err("resolved CLI profile is not eligible".into());
    }
    let discovery = adapter
        .discover(&profile)
        .await
        .map_err(|error| format!("provider discovery: {error}"))?;
    let executable = discovery
        .executable
        .clone()
        .ok_or_else(|| "provider executable is unavailable".to_string())?;
    let digest = discovery
        .executable_sha256
        .clone()
        .ok_or_else(|| "provider executable digest is unavailable".to_string())?;
    let matched = crate::api_drivers::matching_route_attestation(
        state,
        provider_id,
        &discovery,
        adapter.as_ref(),
        &resolved,
    )
    .await
    .map_err(|error| error.code().to_string())?;
    let attestation = matched.row;
    let attestation_inputs = matched.inputs;
    let tools = child_options.allowed_tools.clone();
    let workspace = std::fs::canonicalize(&parent.workspace_root)
        .map_err(|error| format!("child workspace: {error}"))?;
    let now = crate::api::now_iso();
    let lease_id = format!("lease-{}", crate::startup::generate_token());
    let nonce = crate::startup::generate_token();
    let capability_token = format!(
        "{lease_id}.{nonce}.{}{}",
        crate::startup::generate_token(),
        crate::startup::generate_token()
    );
    let process_secret = format!(
        "{}{}",
        crate::startup::generate_token(),
        crate::startup::generate_token()
    );
    let binding = DriverBindingRow {
        id: binding_id.into(),
        driver_kind: provider_id.as_str().into(),
        session_id: session_id.into(),
        vendor_session_id: None,
        vendor_thread_id: None,
        vendor_turn_id: None,
        binary_version: discovery.binary_version.unwrap_or_default(),
        schema_hash: attestation.tool_schema_hash.clone(),
        status: "starting".into(),
        last_event_key: None,
        last_event_cursor: None,
        host_agent_id: Some(agent_id.into()),
        parent_agent_id: parent_agent_id.into(),
        provider_id: Some(provider_id.as_str().into()),
        transport_kind: Some(adapter.transport_kind().label().into()),
        account_profile_id: Some(profile.id.as_str().into()),
        auth_mode: Some(profile.auth_mode.label().into()),
        account_fingerprint: profile.account_fingerprint.clone(),
        model_id: resolved.model_id.clone(),
        executable_path: Some(executable.to_string_lossy().into_owned()),
        executable_sha256: Some(digest.clone()),
        protocol_version: Some(attestation.protocol_version.clone()),
        profile_hash: Some(resolved.effective_profile_hash.clone()),
        policy_hash: Some(attestation.policy_hash.clone()),
        tool_schema_hash: Some(attestation.tool_schema_hash.clone()),
        harness_attestation_id: Some(attestation.id.clone()),
        isolation_mode: Some(attestation.isolation_mode.clone()),
        resume_strategy: None,
        context_epoch: 0,
        process_generation: 1,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    let lease = SessionCapabilityLeaseRow {
        id: lease_id.clone(),
        binding_id: binding_id.into(),
        token_hash: token_hash(&capability_token),
        nonce,
        session_id: session_id.into(),
        canonical_workspace: workspace.to_string_lossy().into_owned(),
        tool_ceiling_json: serde_json::to_string(&tools)
            .map_err(|error| format!("tool ceiling: {error}"))?,
        created_at: now.clone(),
        expires_at: lease_expiry(),
        revoked_at: None,
        process_instance_hash: Some(token_hash(&process_secret)),
    };
    {
        let store = storage.lock().await;
        store
            .upsert_driver_binding(&binding)
            .map_err(|error| format!("binding storage: {error}"))?;
        if let Err(error) = store.upsert_session_capability_lease(&lease) {
            let _ = store.delete_driver_binding(binding_id);
            return Err(format!("lease storage: {error}"));
        }
    }
    let command = std::env::current_exe().map_err(|error| format!("daemon executable: {error}"))?;
    // Membrane is reached through Pulse's CLI; no binding identity is injected.
    let mcp_env = vec![
        (
            "RIGHTKIT_DAEMON_URL".into(),
            format!("http://127.0.0.1:{}", state.bound_port),
        ),
        ("RIGHTKIT_GATEWAY_TOKEN".into(), capability_token),
        ("RIGHTKIT_DRIVER_INSTANCE_SECRET".into(), process_secret),
    ];
    Ok(PreparedChildRuntime {
        lease_id,
        context: CliExecutorLaunchContext {
            executable,
            profile,
            mcp_server: CliMcpServerSpec {
                name: rightkit_agents::driver_host::agy_harness::HOST_MCP_SERVER_NAME.into(),
                command,
                args: vec![
                    "mcp-server".into(),
                    "--session-id".into(),
                    session_id.into(),
                ],
                env: mcp_env,
            },
            policy: rightkit_agents::driver_host::RUN_SCOPED_POLICY.into(),
            attestation_inputs,
        },
    })
}

async fn cleanup_child_runtime(state: &DaemonState, binding_id: &str, lease_id: &str) {
    if let Some(storage) = state.storage.as_ref() {
        let store = storage.lock().await;
        let _ = store.revoke_session_capability_lease(lease_id, &crate::api::now_iso());
        let _ = store.delete_driver_binding(binding_id);
    }
}

fn lease_expiry() -> String {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    format!(
        "{:020}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs()
            .saturating_add(15 * 60)
    )
}

/// Dispatch a subagent spawn through the CLI runtime. Returns the
/// structured [`CliChildSpawnOutcome`] on success, or a typed
/// [`CliSpawnRejection`] when policy, storage, or runtime refuses the
/// request. Pure helper — has no `DaemonState` dependency. The caller
/// owns the storage handle and the executor and is responsible for
/// rolling back any partial state on error.
pub async fn spawn_cli_child(
    ctx: CliDelegationContext,
    resolution: RuntimeResolution,
) -> Result<CliChildSpawnOutcome, CliSpawnRejection> {
    let CliDelegationContext {
        executor,
        registered_provider_ids,
        parent_session_id,
        parent_agent_id,
        child_session_id,
        child_agent_id,
        role,
        task,
        safe_mode,
        options,
    } = ctx;
    let _ = registered_provider_ids; // retained for the policy step the caller already performed.

    // Step 1: build the backend the executor should drive. The
    // caller resolved policy; the helper trusts the resolution and
    // reads its `selected` backend. A native-fallback resolution
    // short-circuits to `Policy` so the daemon routes to the legacy
    // spawn path rather than calling the executor with the wrong
    // backend.
    if matches!(resolution.source, RuntimeResolutionSource::NativeFallback) {
        return Err(CliSpawnRejection::Policy(
            "delegate_task: CLI runtime resolution fell back to native; caller must use the legacy spawn path".into(),
        ));
    }

    // Step 2: build the open-agent request. The parent_agent_id is
    // always Some(parent) on a child spawn; a None here would mean a
    // root session reached the CLI helper, which is a programmer
    // error rather than a runtime condition.
    let open_request = rightkit_agents::driver_host::cli_runtime::OpenAgentRequest {
        session_id: child_session_id.clone(),
        agent_id: child_agent_id.clone(),
        role: role.clone(),
        parent_agent_id: Some(parent_agent_id.clone()),
        backend: resolution.selected.clone(),
        tool_ceiling: options.allowed_tools.clone(),
        worktree_id: options
            .worktree
            .as_ref()
            .and_then(|wt| wt.path.clone().or_else(|| wt.branch.clone())),
    };

    let handle = executor
        .open_agent(open_request)
        .await
        .map_err(|error| CliSpawnRejection::Runtime(error.to_string()))?;

    // Step 3: shape the handoff brief the child will see as its
    // initial prompt. Safe parents force read-only mutation policy;
    // normal children retain scoped workspace writes.
    let mutation_policy = cli_child_mutation_policy(safe_mode);
    let handoff = CliHandoffBrief {
        parent_agent_id: parent_agent_id.clone(),
        child_agent_id: child_agent_id.clone(),
        task: task.clone(),
        done_when: Vec::new(),
        goal_summary: task.clone(),
        tool_ceiling: options.allowed_tools.clone(),
        mutation_policy,
    };
    let prompt = render_handoff_prompt(&handoff);

    // Step 4: run the turn and fold vendor output into AgentResult.
    let turn = executor
        .run_turn(
            &handle,
            AgentTurnRequest {
                agent_kind: AgentKind::Child,
                prompt,
                continuation_token: None,
            },
        )
        .await
        .map_err(|error| CliSpawnRejection::Runtime(error.to_string()))?;

    let result = AgentResult {
        coderight_agent_id: child_agent_id.clone(),
        provider_id: resolution.selected.provider_id().cloned(),
        vendor_session_id: handle.vendor_session_id.clone(),
        status: map_turn_status(turn.status),
        final_text: turn.final_text.clone(),
        evidence_refs: Vec::new(),
    };

    let _ = parent_session_id; // currently unused; reserved for the parent-event hook added in PR 6.
    Ok(CliChildSpawnOutcome {
        child_agent_id,
        result,
    })
}

fn cli_child_mutation_policy(safe_mode: bool) -> MutationPolicy {
    if safe_mode {
        MutationPolicy::ReadOnly
    } else {
        MutationPolicy::ScopedWrite
    }
}

/// Render a [`CliHandoffBrief`] as the markdown prompt the child
/// vendor process sees. The brief is markdown so the vendor's
/// standard first-user-message parsing handles it without a custom
/// protocol extension.
pub fn render_handoff_prompt(brief: &CliHandoffBrief) -> String {
    let tool_list = if brief.tool_ceiling.is_empty() {
        "(no tool ceiling; default vendor toolset)".to_string()
    } else {
        brief.tool_ceiling.join(", ")
    };
    let done_when = if brief.done_when.is_empty() {
        "(none provided)".to_string()
    } else {
        brief
            .done_when
            .iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "# Handoff Brief\n\n\
         ## Goal\n\
         {task}\n\n\
         ## Goal Summary\n\
         {goal_summary}\n\n\
         ## Done When\n\
         {done_when}\n\n\
         ## Mutation Policy\n\
         {mutation_policy:?}\n\n\
         ## Tool Ceiling\n\
         {tool_list}\n\n\
         ---\n\
         parent_agent_id={parent}\n\
         child_agent_id={child}\n",
        task = brief.task,
        goal_summary = brief.goal_summary,
        done_when = done_when,
        mutation_policy = brief.mutation_policy,
        tool_list = tool_list,
        parent = brief.parent_agent_id,
        child = brief.child_agent_id,
    )
}

fn map_turn_status(status: AgentTurnStatus) -> AgentResultStatus {
    match status {
        AgentTurnStatus::Completed => AgentResultStatus::Completed,
        AgentTurnStatus::Failed => AgentResultStatus::Failed,
        AgentTurnStatus::Interrupted => AgentResultStatus::Cancelled,
        AgentTurnStatus::WaitingForApproval | AgentTurnStatus::WaitingForChild => {
            AgentResultStatus::Partial
        }
    }
}

/// Build the [`DelegateTaskRequest`] the resolver consumes. The
/// caller passes the [`SubagentSpawnOptions`] (which carries both
/// the runtime and continuity fields) and the resolved user
/// instruction so this stays the only place that knows how to map
/// `SpawnRuntimeKind` → `RequestedRuntime`.
pub fn build_delegate_request(
    parent_kind: AgentKind,
    options: &SubagentSpawnOptions,
    user_instruction: Option<CliProviderId>,
) -> DelegateTaskRequest {
    let requested = match &options.runtime {
        Some(SpawnRuntimeKind::AuthenticatedCli {
            provider_id,
            model_id,
            account_profile_id,
        }) => Some(RequestedRuntime {
            provider_id: CliProviderId::new(provider_id.clone()),
            model_id: model_id.clone(),
            account_profile_id: account_profile_id.clone(),
        }),
        Some(SpawnRuntimeKind::HostNative { .. }) | None => None,
    };
    DelegateTaskRequest {
        parent_kind,
        requested,
        user_instruction,
    }
}

/// Run the resolver against the daemon's `RegisteredProviders` view
/// and the session's policy. This is a thin wrapper that maps the
/// resolver's typed error to [`CliSpawnRejection::Policy`].
pub fn resolve_for_session(
    request: &DelegateTaskRequest,
    policy: &rightkit_agents::driver_host::cli_runtime::CliDelegationPolicy,
    registered: &RegisteredProviders,
) -> Result<RuntimeResolution, CliSpawnRejection> {
    resolve_child_runtime(request, policy, registered)
        .map_err(|error| CliSpawnRejection::Policy(error.to_string()))
}

/// Issue an exact-once idempotency claim against `driver_event_inbox`.
/// Returns the existing child agent id when the same key is replayed
/// (replay is observable by the caller through `Some(existing)`); a
/// `None` value means the caller should proceed to spawn. A fresh
/// claim inserts a row in `status = "pending"` so the spawned child
/// can mark it `processed` on completion.
pub fn claim_delegation(
    storage: &mut Storage,
    binding_id: &str,
    event_key: &str,
    received_at: &str,
) -> Result<DelegationClaim, CliSpawnRejection> {
    let row = DriverEventInboxRow {
        binding_id: binding_id.to_string(),
        event_key: event_key.to_string(),
        event_cursor: None,
        event_kind: "delegate_task".to_string(),
        payload_json: "{}".to_string(),
        status: "pending".to_string(),
        received_at: received_at.to_string(),
        processed_at: None,
        result_json: None,
    };
    match storage.claim_driver_event(&row) {
        Ok(DriverEventClaim::Claimed) => Ok(DelegationClaim::Proceed),
        Ok(DriverEventClaim::Duplicate(existing)) => Ok(DelegationClaim::Duplicate(existing)),
        Err(error) => Err(map_storage_error(error)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum DelegationClaim {
    /// First time this idempotency key is seen — proceed with the
    /// spawn.
    Proceed,
    /// The key has already been processed; the caller should return
    /// the previously-issued child agent id without spawning again.
    Duplicate(DriverEventInboxRow),
}

/// Persist the child `driver_bindings` row with `parent_agent_id`
/// set so the task graph can draw the parent → child edge straight
/// out of storage.
pub fn upsert_child_binding(
    storage: &Storage,
    binding_id: &str,
    session_id: &str,
    parent_agent_id: &str,
    provider_label: &str,
    created_at: &str,
    updated_at: &str,
) -> Result<(), CliSpawnRejection> {
    let row = DriverBindingRow {
        id: binding_id.to_string(),
        driver_kind: provider_label.to_string(),
        session_id: session_id.to_string(),
        vendor_session_id: None,
        vendor_thread_id: None,
        vendor_turn_id: None,
        binary_version: "pending_attestation".to_string(),
        schema_hash: String::new(),
        status: "starting".to_string(),
        last_event_key: None,
        last_event_cursor: None,
        host_agent_id: Some(session_id.to_string()),
        parent_agent_id: parent_agent_id.to_string(),
        provider_id: Some(provider_label.to_string()),
        transport_kind: None,
        account_profile_id: None,
        auth_mode: None,
        account_fingerprint: None,
        model_id: None,
        executable_path: None,
        executable_sha256: None,
        protocol_version: None,
        profile_hash: None,
        policy_hash: None,
        tool_schema_hash: None,
        harness_attestation_id: None,
        isolation_mode: None,
        resume_strategy: None,
        context_epoch: 0,
        process_generation: 0,
        created_at: created_at.to_string(),
        updated_at: updated_at.to_string(),
    };
    storage
        .upsert_driver_binding(&row)
        .map_err(map_storage_error)
}

/// Issue a new `session_capability_leases` row tied to the child
/// binding. The caller already generated the random token+nonce;
/// this helper computes the token hash and persists the row.
#[allow(clippy::too_many_arguments)]
pub fn issue_child_lease(
    storage: &Storage,
    binding_id: &str,
    session_id: &str,
    canonical_workspace: &str,
    tool_ceiling: &[String],
    token: &str,
    nonce: &str,
    lease_id: &str,
    created_at: &str,
    expires_at: &str,
) -> Result<SessionCapabilityLeaseRow, CliSpawnRejection> {
    let row = SessionCapabilityLeaseRow {
        id: lease_id.to_string(),
        binding_id: binding_id.to_string(),
        token_hash: token_hash(token),
        nonce: nonce.to_string(),
        session_id: session_id.to_string(),
        canonical_workspace: canonical_workspace.to_string(),
        tool_ceiling_json: serde_json::to_string(tool_ceiling).unwrap_or_else(|_| "[]".into()),
        created_at: created_at.to_string(),
        expires_at: expires_at.to_string(),
        revoked_at: None,
        process_instance_hash: None,
    };
    storage
        .upsert_session_capability_lease(&row)
        .map_err(map_storage_error)?;
    Ok(row)
}

fn token_hash(token: &str) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(token.as_bytes());
    format!("sha256:{}", hasher.finalize())
}

fn map_storage_error(error: StorageError) -> CliSpawnRejection {
    CliSpawnRejection::Storage(error.to_string())
}

/// Resolve a [`SubagentContinuity::ContinueAgent`] request into the
/// stored binding id the executor should re-attach to. Returns
/// `Ok(binding_id)` when a child binding exists for
/// `continue_agent_id`; `Err(CliSpawnRejection::Storage)` otherwise.
pub fn resolve_continuation(
    storage: &Storage,
    continue_agent_id: &str,
) -> Result<String, CliSpawnRejection> {
    // ContinueAgent re-binds to the previously-issued child agent id,
    // which is the binding id we minted at spawn time.
    let binding = storage
        .get_driver_binding(continue_agent_id)
        .map_err(map_storage_error)?
        .ok_or_else(|| {
            CliSpawnRejection::Storage(format!(
                "ContinueAgent target has no driver binding: {continue_agent_id}"
            ))
        })?;
    Ok(binding.id)
}

/// Convenience constructor that emits a [`ProtocolEvent`] the
/// parent publishes on the child's spawn completion. The spawn
/// helper does not depend on a `DaemonState`; the caller feeds the
/// parent session's `conduit` and the parent's agent roster.
pub fn child_completed_event(
    parent_session_id: &str,
    seq: i64,
    created_at: &str,
    child_agent_id: &str,
    role: &str,
    result: &AgentResult,
) -> ProtocolEvent {
    ProtocolEvent::SubAgentCompleted {
        session_id: parent_session_id.to_string(),
        seq,
        created_at: created_at.to_string(),
        sub_session_id: child_agent_id.to_string(),
        role: role.to_string(),
        success: matches!(result.status, AgentResultStatus::Completed),
        summary: result
            .final_text
            .chars()
            .take(200)
            .collect::<String>()
            .if_empty_then("(no summary returned by child)"),
        outcome: match result.status {
            AgentResultStatus::Completed => "complete".to_string(),
            AgentResultStatus::Partial => "partial".to_string(),
            AgentResultStatus::Failed => "failed".to_string(),
            AgentResultStatus::Cancelled => "cancelled".to_string(),
        },
        outcome_detail: String::new(),
    }
}

/// Convenience constructor for the spawn-started event. Mirrors
/// the legacy `ProtocolEvent::SubAgentSpawned` payload the daemon
/// already publishes.
pub fn child_spawned_event(
    parent_session_id: &str,
    seq: i64,
    created_at: &str,
    child_agent_id: &str,
    role: &str,
    task_title: &str,
) -> ProtocolEvent {
    ProtocolEvent::SubAgentSpawned {
        session_id: parent_session_id.to_string(),
        seq,
        created_at: created_at.to_string(),
        sub_session_id: child_agent_id.to_string(),
        role: role.to_string(),
        task_title: task_title.to_string(),
    }
}

/// What a follow-up spawn request resolved to. Built by the daemon
/// from [`SubagentContinuity`] + the previously-issued child binding.
pub enum ContinuityTarget<'a> {
    New,
    Existing(&'a str),
}

impl<'a> From<&'a SubagentContinuity> for ContinuityTarget<'a> {
    fn from(value: &'a SubagentContinuity) -> Self {
        match value {
            SubagentContinuity::New => ContinuityTarget::New,
            SubagentContinuity::ContinueAgent { continue_agent_id } => {
                ContinuityTarget::Existing(continue_agent_id.as_str())
            }
        }
    }
}

/// Trait extension the daemon implements to expose its already-built
/// executor and registry to the spawn helper. The daemon's
/// `DaemonState` already carries the bridge; this trait lets the
/// spawn helper read it without taking a direct dependency on the
/// concrete struct.
pub trait CliDelegationEnvironment: Send + Sync {
    fn cli_executor(&self) -> Option<Arc<AuthenticatedCliExecutor>>;
    fn cli_registry_provider_ids(&self) -> BTreeSet<CliProviderId>;
    fn cli_delegation_policy(
        &self,
    ) -> rightkit_agents::driver_host::cli_runtime::CliDelegationPolicy;
}

impl CliDelegationEnvironment for DaemonState {
    fn cli_executor(&self) -> Option<Arc<AuthenticatedCliExecutor>> {
        self.cli_executor.clone()
    }
    fn cli_registry_provider_ids(&self) -> BTreeSet<CliProviderId> {
        self.cli_registry_provider_ids.clone()
    }
    fn cli_delegation_policy(
        &self,
    ) -> rightkit_agents::driver_host::cli_runtime::CliDelegationPolicy {
        self.cli_delegation_policy.clone()
    }
}

/// Helper: turn a `value: String` into either the value or the
/// fallback when the trimmed value is empty. Pulled out as a
/// `String::if_empty_then` extension so the spawn-completed event
/// builder above can read naturally.
trait IfEmptyThenExt {
    fn if_empty_then(self, fallback: &str) -> Self;
}

impl IfEmptyThenExt for String {
    fn if_empty_then(mut self, fallback: &str) -> Self {
        if self.trim().is_empty() {
            self = fallback.to_string();
        }
        self
    }
}

#[doc(hidden)]
pub fn _ensure_protocol_event_used(_event: &ProtocolEvent) -> serde_json::Value {
    // Keep the `ProtocolEvent` import alive without an unused
    // binding warning when no other call site references it. The
    // protocol event types are public API; this anchor guarantees
    // rustc sees the import even on minimal feature combinations.
    json!({})
}
