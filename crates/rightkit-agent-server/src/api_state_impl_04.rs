/// Effective deny-glob lists: built-in defaults plus `[permissions]
/// deny-read-globs` / `deny-write-globs`. `None` when nothing is configured.
fn deny_globs_for_config(
    config: &rightkit_app_settings::agent_config::HostConfig,
) -> Option<Arc<rightkit_agent_policy::DenyGlobSet>> {
    let permissions = &config.permissions;
    if permissions.deny_read_globs.is_empty() && permissions.deny_write_globs.is_empty() {
        return None;
    }
    match rightkit_agent_policy::DenyGlobSet::with_additional_patterns(
        permissions.deny_read_globs.clone(),
        permissions.deny_write_globs.clone(),
    ) {
        Ok(set) => Some(Arc::new(set)),
        Err(error) => {
            // Reject only the malformed entries; well-formed user denies stay
            // in force on top of the defaults.
            tracing::warn!(%error, "permissions deny globs invalid; dropping malformed entries");
            let valid = |p: &String| {
                rightkit_agent_policy::DenyGlobSet::with_patterns([p.clone()], []).is_ok()
            };
            let read: Vec<String> = permissions
                .deny_read_globs
                .iter()
                .filter(|p| valid(p))
                .cloned()
                .collect();
            let write: Vec<String> = permissions
                .deny_write_globs
                .iter()
                .filter(|p| valid(p))
                .cloned()
                .collect();
            rightkit_agent_policy::DenyGlobSet::with_additional_patterns(read, write)
                .ok()
                .map(Arc::new)
        }
    }
}

struct DaemonHookProcessLauncher;

/// Providerless sessions still need the canonical engine-owned tool dispatcher.
/// This client makes any accidental model turn fail closed while leaving tool
/// admission, permission, execution, & result shaping under `SessionEngine`.
struct ProviderlessLlm;

#[async_trait::async_trait]
impl rightkit_agent_engine_core::llm::LlmClient for ProviderlessLlm {
    async fn complete(
        &self,
        _messages: &[rightkit_agent_engine_protocol::TranscriptMessage],
    ) -> Result<rightkit_agent_engine_core::llm::LlmResponse, String> {
        Err("provider is not configured for this session".to_string())
    }
}

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
#[cfg(windows)]
use std::os::windows::process::ExitStatusExt;

/// Bound on retained hook output. Hook stdout feeds deny/error messages and
/// transform envelopes; a chatty hook must not grow daemon memory without
/// limit. The stream is still drained to EOF so the child never stalls on a
/// full pipe.
const HOOK_STREAM_CAPTURE_LIMIT: u64 = 256 * 1024;

/// Binding identity for the host-issued compaction decision (see
/// `CompactionDecisionProvider`); stable across sessions of this host build.
const HOST_LOCAL_COMPACTION_BINDING: &str = "host-local:compaction-decision:v1";

#[async_trait::async_trait]
impl rightkit_hooks::agent_hooks::ProcessLaunchAdapter for DaemonHookProcessLauncher {
    async fn run(
        &self,
        command: String,
        envs: Vec<(&'static str, String)>,
        context: rightkit_sandbox::execution::ProcessLaunchContext,
    ) -> Result<std::process::Output, String> {
        let spec = rightkit_sandbox::execution::CommandLaunchSpec {
            program: if cfg!(windows) { "cmd" } else { "sh" }.to_string(),
            argv: if cfg!(windows) {
                vec!["/C".into(), command]
            } else {
                vec!["-c".into(), command]
            },
            cwd: context.workspace_root.clone(),
            context,
            env: envs
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        };
        let launcher = crate::main_runtime::governed_child_process_launcher()
            .map_err(|error| format!("hook process admission denied: {error}"))?;
        let mut child =
            rightkit_sandbox::execution::ProcessLaunchAdapter::spawn(launcher.as_ref(), spec)
                .await
                .map_err(|error| format!("hook launch failed: {error:?}"))?;
        // Both pipes are drained concurrently with the wait: an unread pipe
        // can deadlock the child, and capture is bounded so a runaway hook
        // cannot grow daemon memory without limit.
        let mut stdout_read = child.take_stdout().map(|stream| {
            tokio::spawn(rightkit_sandbox::execution::capture_stream_bounded(
                stream,
                HOOK_STREAM_CAPTURE_LIMIT,
            ))
        });
        let mut stderr_read = child.take_stderr().map(|stream| {
            tokio::spawn(rightkit_sandbox::execution::capture_stream_bounded(
                stream,
                HOOK_STREAM_CAPTURE_LIMIT,
            ))
        });
        let code = child
            .wait()
            .await
            .map_err(|error| format!("hook wait failed: {error:?}"))?;
        let stdout = match &mut stdout_read {
            Some(task) => task
                .await
                .map_err(|error| error.to_string())?
                .map(|(bytes, _truncated)| bytes)
                .map_err(|error| error.to_string())?,
            None => Vec::new(),
        };
        let stderr = match &mut stderr_read {
            Some(task) => task
                .await
                .map_err(|error| error.to_string())?
                .map(|(bytes, _truncated)| bytes)
                .map_err(|error| error.to_string())?,
            None => Vec::new(),
        };
        // `ManagedChild::wait` returns a bare exit code, but Unix
        // `ExitStatus::from_raw` expects a *wait status* — the raw waitpid
        // word in which a normal exit encodes `code << 8`. Passing the code
        // itself made every nonzero-exit hook report `code() == None`,
        // which the outcome matcher treated like a clean exit — a deny
        // (exit 2) would have been silently honored as Proceed. A negative
        // code means the child died on a signal and no exit evidence exists.
        #[cfg(windows)]
        let status = std::process::ExitStatus::from_raw(code as u32);
        #[cfg(unix)]
        let status = {
            if code < 0 {
                return Err("hook terminated by signal (no exit status)".to_string());
            }
            std::process::ExitStatus::from_raw((code & 0xff) << 8)
        };
        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    }
}

struct DaemonCompactionRouteResolver {
    state: DaemonState,
    selection: crate::inference_runtime::InferenceSelection,
    scheduler: crate::inference_runtime::InferenceScheduler,
    tools: std::sync::Arc<rightkit_agent_tools::coding_tools::ToolRegistry>,
    active_requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// Admit one idle automatic retry after its wake has been observed.
///
/// Keeping reserve, refresh, eligibility, send, & pending reset together is
/// important: a full queue must suspend only at `reserve`, while each wake is
/// consumed once the queue has capacity.
fn process_idle_compaction_retry<Prepare>(
    permit: Result<
        tokio::sync::mpsc::Permit<'_, rightkit_agent_engine_core::CompactionCommandV1>,
        tokio::sync::mpsc::error::SendError<()>,
    >,
    wake_pending: &mut bool,
    mut prepare: Prepare,
) -> Result<bool, tokio::sync::mpsc::error::SendError<()>>
where
    Prepare: FnMut() -> bool,
{
    if !*wake_pending {
        return Ok(false);
    }
    let permit = permit?;
    let admitted = prepare();
    if admitted {
        permit.send(rightkit_agent_engine_core::CompactionCommandV1 {
            id: rightkit_agent_engine_core::compactor::next_compaction_command_id(),
            kind: rightkit_agent_engine_core::CompactionCommandKindV1::AutomaticRetry,
            response: None,
        });
    }
    *wake_pending = false;
    Ok(admitted)
}

#[async_trait::async_trait]
impl rightkit_agent_engine_core::CompactionModelRouteResolver for DaemonCompactionRouteResolver {
    async fn resolve(
        &self,
        slug: &str,
    ) -> Result<std::sync::Arc<dyn rightkit_agent_engine_core::llm::LlmClient>, String> {
        let mut selection = self.selection.clone();
        selection.model_id = slug.to_string();

        let route =
            crate::inference_runtime::ensure_inference_selection_qualified(&self.state, &selection)
                .await
                .map_err(|error| error.to_string())?;
        let (_updates_tx, updates_rx) = tokio::sync::mpsc::channel(1);
        let request_authority = std::sync::Arc::new(
            crate::provider_client::HostRequestAuthority::new(self.state.clone()),
        );
        let client = match route.selection.authority {
            crate::inference_runtime::InferenceAuthority::Direct => {
                crate::provider_client::ProviderLlmClient::new_direct(
                    route,
                    self.scheduler.clone(),
                    self.tools.clone(),
                    updates_rx,
                    self.active_requests.clone(),
                )
                .map(|client| client.with_host_request_authority(request_authority.clone()))
            }
        }
        .map_err(|error| error.to_string())?;
        Ok(std::sync::Arc::new(client))
    }
}

fn native_provider_fallback_route(
    provider_id: &str,
    model_id: &str,
    effort: Option<String>,
    thinking_budget_tokens: Option<u32>,
    disable_thinking: bool,
    fallback_models: Vec<String>,
) -> crate::provider_client::ProviderRoute {
    let (base_url, api_key) = rightkit_llm::agent_providers::find_provider(provider_id)
        .map(|provider| {
            (
                provider.base_url.to_string(),
                rightkit_llm::agent_providers::load_api_key(provider_id).unwrap_or_default(),
            )
        })
        .unwrap_or_default();
    // OAuth/API-key material is resolved asynchronously by provider_client at
    // request construction; keep catalog route usable even before login.
    let mut route = crate::provider_client::ProviderRoute {
        api_key,
        model_id: rightkit_llm::agent_providers::provider_model_id(provider_id, model_id),
        base_url,
        effort: None,
        thinking_budget_tokens: None,
        disable_thinking: false,
        fallback_models: Vec::new(),
        approved_private: false,
    };
    route.effort = effort;
    route.thinking_budget_tokens = thinking_budget_tokens;
    route.disable_thinking = disable_thinking;
    route.fallback_models = fallback_models;
    route.approved_private = false;
    route
}

impl DaemonState {
    #[allow(clippy::too_many_arguments)]
    #[rustfmt::skip]
    fn create_session_seeded_with_options(
        &self,
        id: &str,
        role: Option<&str>,
        workspace_root: &str,
        title: &str,
        parent_session_id: Option<&str>,
        seed: Option<Vec<TranscriptMessage>>,
        engine_seed: Option<Vec<TranscriptMessage>>,
        high_water: i64,
        compaction_epoch: u64,
        options: SessionRuntimeOptions,
    ) -> Arc<LiveSession> {
        // CR3 single-flight: the existence check and the final registry insert
        // below must be one critical section. Without it two concurrent
        // creates of the same id both pass the check, both spawn an engine
        // task, and the second insert silently replaces the first live
        // session (two engines, one durable row). The constructor is
        // synchronous and never re-enters session creation, so one gate held
        // for its whole body is deadlock-free.
        let _single_flight = SESSION_CREATE_GATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = lock_recover(&self.sessions, "sessions").get(id).cloned() {
        return existing;
        }
        // Resume path passes the persisted high-water so the first event
        // published after restart has seq > persisted_high_water and the
        // SSE catch-up loop sees no overlap with the storage log.
        // Fresh-create callers pass 0 (== Conduit::new).
        let conduit = Arc::new(Conduit::with_high_water_mark(512, high_water));
        let persistence_state = rightkit_agent_engine_storage::EventPersistenceState::new();
        let now = now_iso();
        let title_state = Arc::new(Mutex::new(title.to_string()));
        let updated_at_state = Arc::new(Mutex::new(now.clone()));
        let auto_title_started = Arc::new(AtomicBool::new(false));
        let workspace_config = load_workspace_config_with_env(FsPath::new(workspace_root.trim()));
        let mut explicit_native_provider = options
            .provider_id
            .as_deref()
            .map(str::trim)
            .filter(|provider| !provider.is_empty());
        let mut selected_model = options.model.as_deref().map(str::trim).filter(|model| !model.is_empty());
        // A route-qualified `provider::model` selection (the picker / CLI
        // form) names one route; providers and the model catalog only know
        // the bare model id.
        if explicit_native_provider.is_none() {
            if let Some((provider, model)) = selected_model.and_then(|m| m.split_once("::")) {
                let known = workspace_config.provider_routes.contains_key(provider)
                    || rightkit_llm::agent_providers::find_provider(provider).is_some();
                if known && !model.is_empty() {
                    explicit_native_provider = Some(provider);
                    selected_model = Some(model);
                }
            }
        }
        let mut role_descriptor = role.and_then(|r| self.agent_descriptor_with_workspace_config(&workspace_config, r));
        let split_available = crate::model_policy::split_entitled(|feature| self.has_license_feature(feature));
        // Apply the same versioned role-pack assignment used by daemon child
        // admission to direct session creation too.  Without this seam, a
        // root session could retain a stale registry model while its child
        // spawn path correctly selected the pack's primary model.
        // Single-model mode must remain authoritative for free sessions: a
        // workspace agent override cannot reintroduce split role-pack routing.
        if selected_model.is_none()
            && !crate::model_policy::uses_single_role_model(&workspace_config, split_available)
        {
            if let (Some(role), Some(descriptor)) = (role, role_descriptor.as_mut()) {
                if let Ok(Some(resolved)) = crate::orchestrator::resolve_builtin_role_pack_for_spawn(
                    role,
                    &crate::orchestrator::live_role_pack_catalog(),
                ) {
                    descriptor.model_id = resolved.primary.id;
                    descriptor.provider_id = crate::model_policy::provider_id_for_model_slug(
                        &descriptor.model_id,
                    )
                    .to_string();
                }
            }
        }
        if let (Some(descriptor), Some(selected_model)) = (
            role_descriptor.as_mut(),
            selected_model,
        ) {
            descriptor.model_id = selected_model.to_string();
            descriptor.provider_id = explicit_native_provider
                .map(str::to_string)
                .unwrap_or_else(|| crate::model_policy::provider_id_for_model_slug(selected_model).to_string());
        }
        let model_label = if let Some(model) = selected_model {
        model.to_string() } else if let Some(descriptor) = &role_descriptor {
        descriptor.model_id.clone() } else {
        self.engine_cfg.as_ref().map(|config| config.model_id.clone()).unwrap_or_default()
        };
        let mcp_channel = normalize_mcp_channel(Some(options.mcp_channel.clone()));
        let driver_kind = match options.driver_kind.trim().to_ascii_lowercase().as_str() {
            "codex" => "codex".to_string(),
            "claude" => "claude".to_string(),
            _ => "native".to_string(),
        };
        let allowed_tools_for_metadata = options.allowed_tools.clone();
        let disallowed_tools_for_metadata = options.disallowed_tools.clone();
        let toolset_for_metadata = options.toolset.clone();
        let active_skills_for_metadata = options.active_skills.clone();
        let execution_backend_for_metadata = options.execution_backend.clone();
        let execution_workspace_id_for_metadata = options.execution_workspace_id.clone();
        let resolved_execution_network_profile = self.resolve_session_execution_network_profile( workspace_root, options.execution_backend.as_deref(), options.execution_network_profile.as_deref(), );
        let resolved_execution_resource_limits = self.resolve_session_execution_resource_limits( workspace_root, options.execution_backend.as_deref(), );
        let execution_network_profile_for_metadata = resolved_execution_network_profile.clone();
        let execution_resource_limits_for_metadata = resolved_execution_resource_limits.clone();
        let driver_kind_for_metadata = driver_kind.clone();
        let mcp_channel_for_metadata = mcp_channel.clone();
        let spec_id_for_metadata = options.spec_id.clone();
        // CG-A06: bind the incoming cwd before looking up its persisted trust
        // posture. Unresolvable roots fail closed rather than inheriting a
        // trust grant through a spelling/canonicalization mismatch.
        let project_binding = crate::project_binding::ProjectBinding::bind(workspace_root);
        let workspace_trust = match project_binding.evaluate(workspace_root) {
            crate::project_binding::NavigationOutcome::Inside => {
                self.workspace_trust_for(project_binding.raw_root())
            }
            crate::project_binding::NavigationOutcome::Outside
            | crate::project_binding::NavigationOutcome::Unresolvable => {
                rightkit_agent_policy::WorkspaceTrust::Untrusted
            }
        };
        let workspace_trust_for_metadata = workspace_trust_label(workspace_trust);
        // FM-034 S-2: wire TrustPolicy::requires_isolation at session-start routing.
        // Must be before any git worktree creation seam so Bypass on non-sandboxed
        // workspaces forces isolation even if caller requested in-place.
        let initial_mode_for_isolation = options.permission_mode.unwrap_or(if parent_session_id.is_some() {
        rightkit_agent_policy::PermissionMode::Bypass } else {
        rightkit_agent_policy::PermissionMode::Ask });
        let requires_isolation = rightkit_agent_policy::TrustPolicy::requires_isolation(initial_mode_for_isolation, workspace_trust);
        if requires_isolation {
            tracing::warn!(
                session_id = %id,
                permission_mode = ?initial_mode_for_isolation,
                workspace_trust = ?workspace_trust,
                workspace_root = %workspace_root,
                "TrustPolicy requires isolation: Bypass on non-sandboxed workspace forces worktree isolation"
            );
        }
        // Forced worktree path when isolation required — seam before git worktree add.
        // Best-effort directory preparation; actual git worktree add is performed by
        // the worktree tool / daemon seam after LiveSession exists. The log above
        // proves the decision was evaluated; the path derivation proves it is wired
        // before the creation seam.
        let forced_worktree_path: Option<String> = if requires_isolation {
            let base = workspace_root.trim();
            // Isolated path under the workspace when it is a valid dir, else temp.
            let candidate = if base.is_empty() || base == "/" {
                std::env::temp_dir().join(format!("coderight-isolated-{}", id))
            } else {
                std::path::PathBuf::from(base).join(".coderight").join("worktrees").join(id)
            };
            if let Err(e) = std::fs::create_dir_all(&candidate) {
                tracing::warn!(session_id = %id, error = %e, path = %candidate.display(), "failed to create forced worktree directory");
                None
            } else if candidate.join(".git").exists() {
                // Resumed session: the isolated checkout already exists.
                tracing::info!(session_id = %id, path = %candidate.display(), "forced isolation worktree reused");
                Some(candidate.to_string_lossy().into_owned())
            } else if base.is_empty() || base == "/" {
                Some(candidate.to_string_lossy().into_owned())
            } else {
                // An isolated workspace must contain the repository: an empty
                // directory would hide every project file from a bypass-mode
                // agent. Materialize a real detached git worktree at HEAD; if the
                // workspace is not a git repository (or has no commit), isolation
                // is unavailable and the session runs in place, visibly logged.
                let added = rightkit_child_process::command_std("git")
                    .arg("-C")
                    .arg(base)
                    .args(["worktree", "add", "--detach", "--force"])
                    .arg(&candidate)
                    .arg("HEAD")
                    .output();
                match added {
                    Ok(output) if output.status.success() => {
                        if let Err(error) = rightkit_agent_tools::coding_tools::git_read::ignore_runtime_worktrees(std::path::Path::new(base)) {
                            tracing::warn!(session_id = %id, %error, "could not exclude engine-owned isolation worktrees from Git status");
                        }
                        tracing::info!(session_id = %id, path = %candidate.display(), "forced isolation git worktree created");
                        Some(candidate.to_string_lossy().into_owned())
                    }
                    other => {
                        tracing::warn!(session_id = %id, path = %candidate.display(), result = ?other.map(|o| String::from_utf8_lossy(&o.stderr).into_owned()), "isolation unavailable (not a git repository with a commit); running in place");
                        let _ = std::fs::remove_dir(&candidate);
                        None
                    }
                }
            }
        } else {
            None
        };
        let effective_workspace_root: String = forced_worktree_path.clone().unwrap_or_else(|| workspace_root.to_string());
        // EFF-006: reconcile any multi-file mutation batch that never settled.
        // A journal stuck at `prepared` proves only that a batch was about to
        // start - the process may have died after any number of commits - so
        // the resulting state has to be observed, not assumed. Session open is
        // the first moment a workspace root is known (the daemon has none at
        // startup).
        //
        // Reconciliation reads the journal directory and, for an unsettled
        // batch, digests every intended path. That is blocking IO which can be
        // slow on large files, and this runs under an async axum handler - so
        // it is done OFF the request path in a blocking task. Session creation
        // does not depend on the outcome; it is diagnostic. The result is
        // recorded on shared state so the sweep is provable, not merely logged.
        {
            let workspace_for_recovery = effective_workspace_root.clone();
            let session_for_recovery = id.to_string();
            let reports_handle = self.mutation_journal_reports.clone();
            tokio::spawn(async move {
                let scanned = tokio::task::spawn_blocking(move || {
                    rightkit_agent_tools::coding_tools::file_service::recover_mutation_journals(
                        std::path::Path::new(&workspace_for_recovery),
                    )
                })
                .await;
                match scanned {
                    Ok(Ok(reports)) => {
                        for report in &reports {
                            if report.observed_partial_application() {
                                tracing::warn!(
                                    session_id = %session_for_recovery,
                                    journal_id = %report.journal_id,
                                    entries = ?report.entries,
                                    "interrupted mutation batch was PARTIALLY applied; a `prepared` journal is not evidence that nothing ran"
                                );
                            } else {
                                tracing::info!(
                                    session_id = %session_for_recovery,
                                    journal_id = %report.journal_id,
                                    "interrupted mutation batch left no observed change"
                                );
                            }
                        }
                        lock_recover(&reports_handle, "state.mutation_journal_reports")
                            .insert(session_for_recovery, reports);
                    }
                    Ok(Err(error)) => tracing::warn!(
                        session_id = %session_for_recovery,
                        error = %error,
                        "mutation journal reconciliation failed"
                    ),
                    Err(error) => tracing::warn!(
                        session_id = %session_for_recovery,
                        error = %error,
                        "mutation journal reconciliation task failed"
                    ),
                }
            });
        }
        // Resolved durable provider identity for this session. Qualified
        // sessions report their connection id; dynamic sessions report the
        // provider the descriptor/route was resolved against, falling back to
        // the provider the model name implies so resume reproduces the same
        // provider instead of re-deriving it from future defaults.
        let initial_provider_id: Option<String> = options
            .inference_selection
            .as_ref()
            .map(|selection| selection.connection_id.clone())
            .or_else(|| {
                explicit_native_provider.map(str::to_string)
            })
            .or_else(|| {
                role_descriptor
                    .as_ref()
                    .map(|descriptor| descriptor.provider_id.clone())
                    .filter(|provider| !provider.trim().is_empty())
            })
            .or_else(|| {
                selected_model.and_then(|model| {
                    self.resolved_provider_id_for_model(&workspace_config, model)
                })
            });
        let provider_id_for_metadata = initial_provider_id.clone();
        let effort_for_metadata = options.effort.clone();
        let thinking_budget_tokens_for_metadata = options.thinking_budget_tokens;
        let disable_thinking_for_metadata = options.disable_thinking;
        let safe_mode_for_metadata = options.safe_mode;
        let forked_from_session_id_for_metadata = options.forked_from_session_id.clone();
        let effective_budget_ceiling_usd = {
        let settings = lock_recover(&self.settings, "settings");
        let configured = if parent_session_id.is_some() {
        settings.per_agent_call_max_usd } else {
        settings.per_session_max_usd };
        match options.budget_ceiling_usd.filter(|value| value.is_finite() && *value > 0.0) {
        Some(explicit) if configured > 0.0 => explicit.min(configured),
        Some(explicit) => explicit,
        None => configured,
        }
        };
        let requested_lineage_budget = options.lineage_budget.clone().or_else(|| {
        parent_session_id.and_then(|parent_id| self.get_session(parent_id).map(|parent| parent.lineage_budget.clone()))
        }).unwrap_or_else(|| rightkit_agent_engine_core::LineageBudget::from_usd(
        (effective_budget_ceiling_usd > 0.0 && effective_budget_ceiling_usd.is_finite()).then_some(effective_budget_ceiling_usd), 0.0));
        let lineage_budget = if parent_session_id.is_none() {
        let mut lineages = lock_recover(&self.lineage_budgets, "lineage_budgets");
        if let Some(existing) = lineages.get(id).and_then(std::sync::Weak::upgrade) {
        existing } else {
        lineages.insert(id.to_string(), Arc::downgrade(&requested_lineage_budget));
        requested_lineage_budget }
        } else {
        requested_lineage_budget };
        // RightKit owns durable session/event history independently from
        // Membrane memory. Missing Membrane keeps memory fail-closed, but must
        // not disable transcript persistence required by session APIs.
        if let Some(store) = self.storage.clone() {
        if seed.is_none() {
        let row = rightkit_agent_engine_storage::SessionRow {
        id: id.to_string(), title: title.to_string(), workspace_root: workspace_root.to_string(), created_at: now.clone(), updated_at: now.clone(), parent_session_id: parent_session_id.map(|s| s.to_string()), role: role.map(|s| s.to_string()), status: "idle".to_string(), config_json: serde_json::json!({
        "model": model_label, "providerId": provider_id_for_metadata, "inferenceSelection": options.inference_selection.clone(), "inferenceFingerprint": options.inference_fingerprint.clone(), "allowedTools": allowed_tools_for_metadata, "disallowedTools": disallowed_tools_for_metadata, "toolset": toolset_for_metadata, "activeSkills": active_skills_for_metadata, "executionBackend": execution_backend_for_metadata, "executionWorkspaceId": execution_workspace_id_for_metadata, "executionNetworkProfile": execution_network_profile_for_metadata, "executionResourceLimits": execution_resource_limits_for_metadata, "driverKind": driver_kind_for_metadata, "mcpChannel": mcp_channel_for_metadata, "specId": spec_id_for_metadata, "workspaceTrust": workspace_trust_for_metadata, "effort": effort_for_metadata, "thinkingBudgetTokens": thinking_budget_tokens_for_metadata, "disableThinking": disable_thinking_for_metadata, "safeMode": safe_mode_for_metadata, "forkedFromSessionId": forked_from_session_id_for_metadata, "budgetCeilingUsd": effective_budget_ceiling_usd, })
        .to_string(), tags: String::new(), recovery_marker: None, };
        let store2 = store.clone();
        let deleted_session_ids = self.deleted_session_ids.clone();
        let id_for_persist = id.to_string();
        let id_for_tombstone = id.to_string();
        let persistence_state_for_row = persistence_state.clone();
        // Acknowledged/visibly-failed persist with retry + backoff.
        // Uses spawn + spawn_blocking (never block_in_place) so it works
        // on both multi_thread and current_thread runtimes. Retries with
        // exponential backoff and surfaces failure via persistence_state
        // and error log so the HTTP handler can return 503 instead of warn!.
        tokio::spawn(async move {
        let mut last_error: Option<String> = None;
        let mut succeeded = false;
        for attempt in 0..3 {
        if attempt > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(50 * (1u64 << attempt))).await;
        }
        let store_clone = store2.clone();
        let deleted_clone = deleted_session_ids.clone();
        let row_clone = row.clone();
        let id_clone = id_for_tombstone.clone();
        let result = tokio::task::spawn_blocking(move || {
        let deleted = lock_recover(&deleted_clone, "deleted_session_ids");
        if deleted.contains(&id_clone) {
        return Ok(());
        }
        let s = store_clone.blocking_lock();
        s.create_session(&row_clone).map_err(|e| e.to_string())
        })
        .await;
        match result {
        Ok(Ok(())) => {
        succeeded = true;
        last_error = None;
        break;
        }
        Ok(Err(e)) => last_error = Some(e),
        Err(e) => last_error = Some(format!("session row persist task panicked: {e}")),
        }
        }
        if let Some(err) = last_error {
        tracing::error!(session_id = %id_for_persist, error = %err, "session row persist failed after retries");
        let previous = persistence_state_for_row.snapshot();
        persistence_state_for_row.update(rightkit_agent_engine_storage::EventPersistenceSnapshot {
        last_published_seq: previous.last_published_seq,
        last_persisted_seq: previous.last_persisted_seq,
        last_error: Some(err),
        });
        } else if succeeded {
        // Clear any prior error on success
        let previous = persistence_state_for_row.snapshot();
        if previous.last_error.is_some() {
        persistence_state_for_row.update(rightkit_agent_engine_storage::EventPersistenceSnapshot {
        last_published_seq: previous.last_published_seq,
        last_persisted_seq: previous.last_persisted_seq,
        last_error: None,
        });
        }
        }
        });
        }
        spawn_event_persister(
        id.to_string(),
        conduit.clone(),
        store.clone(),
        self.deleted_session_ids.clone(),
        persistence_state.clone(),
        );
        }
        let (interrupt_tx, interrupt_rx) = tokio::sync::watch::channel(false);
        // CR3: a persistence latch cancels tools already running for this session.
        let _ = spawn_latch_cancel(&conduit, id.to_string(), interrupt_tx.clone());
        let initial_mode = initial_mode_for_isolation;
        let (permission_mode_tx, permission_mode_rx) = tokio::sync::watch::channel(initial_mode);
        let mut permission_mode_idle_rx = permission_mode_rx.clone();
        let permission_rules: Arc<Mutex<rightkit_agent_policy::RuleStore>> = Arc::new(Mutex::new(rightkit_agent_policy::RuleStore::new()));
        let user_surfaces_enabled = session_user_surfaces_enabled(&options);
        if user_surfaces_enabled {
        let mut rules = lock_recover(&permission_rules, "session.permission_rules");
        self.seed_persistent_permission_rules(&mut rules, workspace_root);
        }
        let hook_bus = rightkit_hooks::agent_hooks::HookBus::with_process_launcher(
            256,
            std::sync::Arc::new(DaemonHookProcessLauncher),
            rightkit_sandbox::execution::ProcessLaunchContext {
                workspace_root: workspace_root.to_string(),
                sandbox_profile: options.safe_mode.then(|| "hook".to_string()),
                sandbox_required: options.safe_mode,
                network_required: false,
            },
        );
        if user_surfaces_enabled {
        crate::hooks::spawn_hook_runner_for_workspace(
            &hook_bus,
            FsPath::new(workspace_root),
            options.safe_mode,
        );
        }
        let hook_bus_for_engine = hook_bus.clone();
        let pending_approvals: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>> = Arc::new(Mutex::new(HashMap::new()));
        let pending_approval_contexts: Arc<Mutex<HashMap<String, rightkit_agent_engine_protocol::PendingApprovalContext>>> = Arc::new(Mutex::new(HashMap::new()));
        let cancel_tokens: Arc<Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>> = Arc::new(Mutex::new(HashMap::new()));
        let pending_questions: Arc< Mutex<HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>, > = Arc::new(Mutex::new(HashMap::new()));
        let sandbox_grant_prompts = Arc::new(AtomicBool::new(true));
        let pending_elicitations: Arc<Mutex<HashMap<String, crate::api::PendingElicitation>>> = Arc::new(Mutex::new(HashMap::new()));
        let pending_credential_requests: Arc<Mutex<rightkit_agent_engine_core::credential_request::PendingCredentialRequests>> = Arc::new(Mutex::new(rightkit_agent_engine_core::credential_request::PendingCredentialRequests::new()));
        let pending_plans: crate::plan_exit::PendingPlans = Arc::new(Mutex::new(HashMap::new()));
        let transcript: Arc<Mutex<Vec<TranscriptMessage>>> = Arc::new(Mutex::new(seed.clone().unwrap_or_default()));
        let tasks: Arc<Mutex<Vec<rightkit_agent_engine_protocol::TaskNode>>> = Arc::new(Mutex::new(Vec::new()));
        let background_shells: rightkit_agent_tools::coding_tools::background_shell::BgStore = Arc::new(Mutex::new(HashMap::new()));
        let process_launch_adapter = crate::main_runtime::governed_child_process_launcher().ok();
        let process_launch_context = rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: workspace_root.to_string(),
            sandbox_profile: options.safe_mode.then(|| "workspace".to_string()),
            sandbox_required: options.safe_mode,
            network_required: false,
        };
        let browser_sessions: rightkit_agent_tools::coding_tools::browser::BrowserStore = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let task_store: rightkit_agent_tools::coding_tools::task_tools::TaskStore = Arc::new(Mutex::new(HashMap::new()));
        let artifacts: Arc<Mutex<Vec<rightkit_agent_engine_protocol::ArtifactRow>>> = Arc::new(Mutex::new(Vec::new()));
        let (steer_tx, steer_rx) = tokio::sync::mpsc::channel::<String>(16);
        let (provider_boundary_tx, provider_boundary_rx) =
            rightkit_agent_engine_core::provider_turn_boundary::provider_boundary_channel(32);
        let tool_catalog = Arc::new(crate::session_tool_catalog::SessionToolCatalog::new(
            id.to_string(), provider_boundary_tx.clone(),
        ));
        // r5.20: bind the one canonical per-session catalog independently of
        // provider configuration. Inline MCP projection is a host surface,
        // so providerless sessions must expose the same creation-time policy
        // registry instead of waiting for an engine task that never starts.
        let catalog_session_id = id.to_string();
        let catalog_workspace = effective_workspace_root.clone();
        let catalog_session_plugin_activations = self.session_plugin_activations.clone();
        let catalog_plugin_unloads = self.session_plugin_unloads.clone();
        let catalog_plugin_dir = self.plugin_dir.clone();
        let catalog_safe_mode = options.safe_mode;
        let catalog_role_allowed_tools: Option<Vec<String>> =
            role_descriptor.as_ref().map(|d| d.allowed_tools.clone());
        let catalog_requested_allowed_tools = options.allowed_tools.clone();
        let catalog_requested_disallowed_tools = options.disallowed_tools.clone();
        let catalog_lazy_tool_loading = workspace_config.tools.lazy_tool_loading();
        let catalog_model_interface = crate::session_tool_catalog::model_interface_from_config(
            workspace_config.tools.model_interface(),
        );
        let catalog_memory = self.memory_backend.clone();
        let catalog_memory_provenance = self.memory_provenance.clone();
        let initial_tools = tool_catalog.bind(move || {
            let (enabled_plugins, disabled_plugins) = lock_recover(
                &catalog_session_plugin_activations,
                "session.plugin_activations",
            )
            .overrides_for_session(&catalog_session_id);
            let mut session_registry = build_session_tool_registry_for_plugins(
                (catalog_plugin_dir.clone(), &enabled_plugins),
                catalog_safe_mode,
                catalog_role_allowed_tools.as_ref(),
                &catalog_requested_allowed_tools,
                &catalog_requested_disallowed_tools,
                catalog_lazy_tool_loading,
                catalog_model_interface,
                Some((
                    catalog_memory.clone(),
                    catalog_memory_provenance.clone(),
                    catalog_workspace.as_str(),
                )),
            );
            if !disabled_plugins.is_empty() {
                let removed = session_registry.unload_disabled_plugins(&disabled_plugins);
                lock_recover(&catalog_plugin_unloads, "state.session_plugin_unloads").insert(
                    catalog_session_id.clone(),
                    (disabled_plugins.clone(), removed),
                );
                tracing::info!(
                    session_id = %catalog_session_id,
                    disabled_plugins = ?disabled_plugins,
                    removed_tools = removed,
                    "unloaded session-disabled plugin tools"
                );
            }
            session_registry
        });
        let (rewind_tx, mut rewind_rx) = tokio::sync::mpsc::channel::<crate::api_sessions::rewind::RewindRequest>(4);
        let (compact_tx, mut compact_rx) = tokio::sync::mpsc::channel::<rightkit_agent_engine_core::CompactionCommandV1>(4);
        let compaction_wake_notify = Arc::new(tokio::sync::Notify::new());
        let compaction_revision_notifier: Arc<dyn Fn() + Send + Sync> = {
            let wake_notify = compaction_wake_notify.clone();
            Arc::new(move || wake_notify.notify_one())
        };
        let (context_fold_tx, mut context_fold_rx) = tokio::sync::mpsc::channel::<crate::api_context_folds::ContextFoldCommand>(4);
        let fold_archive_dir = crate::api_context_folds::session_fold_archive_dir(&self.database_path, id).ok();
        let initial_compaction_policy =
            crate::provider_client::compaction_policy_for_model(&model_label);
        let (compaction_policy_tx, compaction_policy_rx) =
            tokio::sync::watch::channel(initial_compaction_policy.clone());
        let mut compaction_policy_idle_rx = compaction_policy_rx.clone();
        // H8 uses the same active policy the engine receives.  Keep it in a
        // small shared cell so route resolution can replace the initial model
        // before the first provider usage event is observed.
        let h8_policy = Arc::new(Mutex::new(initial_compaction_policy));
        let goal: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let goal_state: Arc<Mutex<Option<rightkit_agent_engine_core::GoalState>>> = Arc::new(Mutex::new(None));
        let goal_gap_tracker = Arc::new(Mutex::new(rightkit_agent_engine_core::GoalGapTracker::default()));
        let cost_usd: Arc<Mutex<Option<f64>>> = Arc::new(Mutex::new(None));
        {
        let transcript_bg = transcript.clone();
        let tasks_bg = tasks.clone();
        let state_for_denials = self.clone();
        let title_for_auto = title_state.clone();
        let updated_at_for_auto = updated_at_state.clone();
        let auto_title_started = auto_title_started.clone();
        let storage_for_auto = self.storage.clone();
        let engine_cfg_for_auto = self.engine_cfg.clone();
        let session_id_for_auto = id.to_string();
        let workspace_root_for_auto = workspace_root.to_string();
        let conduit_for_auto = conduit.clone();
        let is_top_level_session = parent_session_id.is_none();
        let cost_usd_bg = cost_usd.clone();
        let h8_policy_bg = h8_policy.clone();
        let h8_session_id = id.to_string();
        let mut sub = conduit.subscribe();
        tokio::spawn(async move {
        // Durable cursor for this projection: every event kind applied below is
        // a durable fact, so the SQLite event log already holds whatever the
        // live broadcast dropped. Lag catches up from that cursor instead of
        // silently serving a partial transcript/tasks/cost projection.
        let mut last_applied_seq: i64 = high_water;
        loop {
        let mut pending: Vec<ProtocolEvent> = Vec::new();
        match sub.recv().await {
            rightkit_agent_engine_core::conduit::ConduitReceive::Event(event) => pending.push(event),
            rightkit_agent_engine_core::conduit::ConduitReceive::Lagged { missed } => {
            match storage_for_auto.clone() {
            Some(store) => {
            let rebuild_session_id = session_id_for_auto.clone();
            let after = last_applied_seq;
            let rows = tokio::task::spawn_blocking(move || {
                store.blocking_lock().events_after(&rebuild_session_id, after)
            })
            .await;
            match rows {
            Ok(Ok(rows)) => {
            let replayed = rows.len();
            for row in rows {
                match serde_json::from_str::<ProtocolEvent>(&row.payload_json) {
                Ok(event) => pending.push(event),
                Err(error) => tracing::warn!(
                    session_id = %session_id_for_auto, seq = row.seq, %error,
                    "skipping undecodable durable event during lag rebuild"),
                }
            }
            tracing::warn!(session_id = %session_id_for_auto, missed, replayed, "conduit lagged; rebuilt projection from durable event log");
            }
            Ok(Err(error)) => tracing::warn!(session_id = %session_id_for_auto, missed, %error, "conduit lagged and durable replay failed; projection may be partial"),
            Err(error) => tracing::warn!(session_id = %session_id_for_auto, missed, %error, "conduit lagged and durable replay task failed; projection may be partial"),
            }
            }
            None => tracing::warn!(session_id = %session_id_for_auto, missed, "conduit lagged with no durable store; projection may be partial"),
            }
            }
            rightkit_agent_engine_core::conduit::ConduitReceive::Closed => break,
        }
        for event in pending {
        let event_seq = rightkit_agent_engine_core::conduit::seq_of(&event);
        // Only durable facts advance the replay cursor. Live projections use
        // the current durable high-water mark as an anchor; after lag, a
        // projection carrying that anchor must not suppress the durable rows
        // that the log replay is meant to restore.
        if event.is_durable_fact() {
            if event_seq <= last_applied_seq {
                continue;
            }
            last_applied_seq = event_seq;
        }
        match event {
        ProtocolEvent::UsageUpdated { cost_usd: usd, cost_known, context_tokens, .. } => {
        let mut slot = lock_recover(&cost_usd_bg, "session.cost_usd");
        *slot = if cost_known { Some(usd) } else { None };
        let policy = lock_recover(&h8_policy_bg, "session.h8_policy").clone();
        let active_task_id = lock_recover(&tasks_bg, "session.tasks")
            .iter()
            .find(|task| task.state == rightkit_agent_engine_protocol::TaskState::InProgress)
            .map(|task| task.id.clone());
        rightkit_logs::agent_telemetry::record_remaining_context_ceiling(
            rightkit_logs::agent_telemetry::RemainingContextInput {
                session_id: h8_session_id.clone(),
                task_id: active_task_id,
                model_id: policy.model_id,
                context_window_tokens: policy.context_window_tokens,
                reserve_tokens: policy.reserve_tokens,
                observed_context_tokens: u64::try_from(context_tokens).ok(),
                requested_at_unix_ms: rightkit_logs::agent_telemetry::now_unix_ms(),
            },
        );
        }
        ProtocolEvent::AssistantMessageCompleted { message, .. } => {
        lock_recover(&transcript_bg, "session.transcript") .push(message.clone());
        // CQ-09: run every eligibility check before cloning the transcript —
        // the snapshot is only needed once the CAS proves this session will
        // actually spawn the title task.
        let title_eligible = is_top_level_session
            && engine_cfg_for_auto.is_some()
            && !auto_title_started.load(Ordering::SeqCst)
            && should_auto_title_for_workspace(
                &lock_recover(&title_for_auto, "session.title"),
                &workspace_root_for_auto,
            )
            && session_title_prompt(&lock_recover(&transcript_bg, "session.transcript"))
                .is_some();
        if title_eligible && auto_title_started.compare_exchange( false, true, Ordering::SeqCst, Ordering::SeqCst, ).is_ok() {
        let transcript_snapshot = lock_recover(&transcript_bg, "session.transcript").clone();
        spawn_auto_title_task( session_id_for_auto.clone(), title_for_auto.clone(), updated_at_for_auto.clone(), conduit_for_auto.clone(), workspace_root_for_auto.clone(), storage_for_auto.clone(), engine_cfg_for_auto.clone().unwrap(), transcript_snapshot, );
        }
        }
        ProtocolEvent::ToolResultAdded {
        session_id, created_at, result, .. } => {
        let _ = state_for_denials.record_permission_denial_from_tool_result( &session_id, &created_at, &result, );
        lock_recover(&transcript_bg, "session.transcript").push(result);
        }
        ProtocolEvent::TaskGraphUpdated {
        tasks: new_tasks, .. } => {
        *lock_recover(&tasks_bg, "session.tasks") = new_tasks;
        }
        ProtocolEvent::SessionCompacted { seq, created_at, summary, dropped_count, .. } => {
        lock_recover(&transcript_bg, "session.transcript").push(TranscriptMessage::Compact {
        id: format!("compact-{seq}"), created_at, summary, dropped_count, pinned: true, });
        }
        ProtocolEvent::SessionContextFolded { start_marker, end_marker, fold, .. } => {
        crate::rehydrate::apply_context_fold(
        &mut lock_recover(&transcript_bg, "session.transcript"),
        &start_marker,
        &end_marker,
        fold,
        );
        }
        _ => {}
        }
        }
        }
        });
        }
        let system_prompt: Option<&'static str> = match role {
        Some("designer") => Some(WEBSITE_DESIGNER_SYSTEM_PROMPT), _ => None, };
        let instruction_context = if options.bare {
            None
        } else {
            crate::main_runtime::load_cli_instruction_context(
                std::path::Path::new(workspace_root),
                &options.add_dir,
            )
        };
        let mut live_model_route: Option<crate::provider_client::SharedProviderRoute> = None;
        let inference_active_requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let inference_active_requests_for_engine = inference_active_requests.clone();
        let (inference_route_tx, inference_route_rx) = tokio::sync::mpsc::channel(1);
        let live_inference_route_tx = options
            .resolved_inference_route
            .is_some()
            .then_some(inference_route_tx);
        let typed_mutations: Arc<std::sync::atomic::AtomicU32> =
            Arc::new(std::sync::atomic::AtomicU32::new(0));
        let typed_mutations_for_engine = typed_mutations.clone();
        let live_turn_handle = LiveTurnHandle::default();
        let live_turn_handle_for_engine = live_turn_handle.clone();
        // Turns handed to the engine channel but not yet begun. Admission
        // increments before `try_send`; the engine decrements the moment it
        // dequeues. Route updates read this (under `route_update_gate`) to
        // treat a queued turn as busy.
        let route_update_gate = Arc::new(tokio::sync::Mutex::new(()));
        let route_update_gate_for_engine = route_update_gate.clone();
        let pending_admissions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let pending_admissions_for_engine = pending_admissions.clone();
        let (driver_tool_tx, mut driver_tool_rx) =
            tokio::sync::mpsc::channel::<DriverToolRequest>(16);
        // Shared by engine task & returned LiveSession, including providerless
        // sessions where no engine task is started.
        let compaction_snapshot_feed = Arc::new(std::sync::RwLock::new(
            rightkit_agent_engine_core::CompactionSnapshotV1 {
                request_generation: 0,
                transcript_high_water: high_water,
                transcript_digest: String::new(),
                pin_revision: None,
                model_route_revision: None,
                tool_catalog_revision: None,
                promotion_revision: None,
                capacity_revision: None,
                reserve_revision: None,
                accounting_revision: None,
                federation_revision: None,
                binding_revision: None,
                h9_revision: None,
                policy_revision: None,
                repository_scope_revision: None,
                decision_expires_at_unix_ms: 0,
            },
        ));
        // An explicit session route can start the native engine without a
        // configured default. ChatGPT OAuth resolves at request construction.
        let session_engine_cfg = self.engine_cfg.clone().or_else(|| {
            explicit_native_provider.zip(selected_model).and_then(|(provider, model)| {
                crate::main_runtime::resolve_engine_config(
                    &workspace_config,
                    Some(model),
                    Some(provider),
                )
            })
        });
        let msg_tx = if let Some(global_cfg) = session_engine_cfg.as_ref() {
        let (tx, mut msg_rx) =
            tokio::sync::mpsc::channel::<String>(SESSION_MSG_CHANNEL_CAPACITY);
        let session_id = id.to_string();
        let conduit_clone = conduit.clone();
        let approval_store = pending_approvals.clone();
        let cancel_store = cancel_tokens.clone();
        let questioner_conduit = conduit.clone();
        let questioner_store = pending_questions.clone();
        let sandbox_grant_prompts_for_engine = sandbox_grant_prompts.clone();
        let plan_conduit = conduit.clone();
        let plan_pending = pending_plans.clone();
        let plan_mode_tx = permission_mode_tx.clone();
        let permission_rules_for_engine = permission_rules.clone();
        let web_search_config = web_search_config_from_config(&workspace_config);
        let mut descriptor = role_descriptor.clone();
        // R1a-L3 spawn-seam wiring (ADR §D2 seam 1). When the capability
        // router is enabled, the descriptor's model_id may be overridden
        // here BEFORE the live provider route resolves. The seam honors
        // pin precedence (role config override > session pin > router),
        // caches the decision per `(conductor_run_id, role)` for
        // sibling-stability, and is licensed-gated on the `routing`
        // Pro feature.
        if selected_model.is_none() {
        if let Some(d) = descriptor.as_mut() {
        let goal_text_owned = lock_recover(&goal, "session.goal").clone().unwrap_or_default();
        let goal_text: &str = goal_text_owned.as_str();
        let scoped_files: Vec<String> = Vec::new();
        // R1a-L3 SPEC 2 — populate-vs-default note for `TaskSignal`:
        //  - `budget_remaining_usd`: derived as (settings.per_session_max_usd
        //    minus session.cost_usd, or f64::MAX when either is unset).
        //    At spawn time we do not yet have a session id-bound cost_usd
        //    cell, so we conservatively treat remaining budget as the full
        //    session ceiling and let the heuristic downgrade only if other
        //    signals suggest budget pressure (none currently do at spawn).
        //  - `current_context_tokens`: 0 at spawn (no prior turns); the
        //    context-window hard constraint is therefore inactive here, by
        //    design — the spawn-time pick happens before any turn runs.
        //  - `tool_call_density`: 0 (no prior turns yet).
        //  - `failure_streak`: 0 (the spawn itself is not a retry; a
        //    conductor retry would re-enter with a populated streak).
        //  - `session_pin`: None — no per-session pin plumbing yet at this
        //    seam; the per-agent override check above is what blocks
        //    role-pinned roles today.
        // The settings guard is held only for this read, not to the end of the function.
        let budget_ceiling = {
            let settings_snapshot = lock_recover(&self.settings, "settings");
            if parent_session_id.is_some() {
                settings_snapshot.per_agent_call_max_usd
            } else {
                settings_snapshot.per_session_max_usd
            }
        };
        // Extract role name BEFORE taking &mut d — `apply_capability_routing`
        // needs `&mut descriptor`, which conflicts with `d.name.as_str()`.
        let role_name_owned: String = d.name.clone();
        // Root sessions have no parent id, so there is no already-registered
        // `DaemonState.sessions` entry the conductor_run_id-keyed lookup
        // could resolve against (this session's own id is not inserted into
        // `self.sessions` until after this constructor returns — see the
        // early-return-on-existing-id check above). S:F6: rather than let
        // that always-miss lookup silently drop context to zero, pull live
        // signals directly from THIS spawn's own already-built
        // `tasks`/`transcript`/`lineage_budget` locals, which are non-empty
        // exactly when this is a resume/re-entry seeded from storage.
        let routing_run_id = parent_session_id.or(Some(id));
        let root_context_signals = if parent_session_id.is_none() {
        let tasks_snapshot = lock_recover(&tasks, "session.tasks").clone();
        let transcript_snapshot = lock_recover(&transcript, "session.transcript").clone();
        crate::router_seam::runtime_task_signals_from_parts( &tasks_snapshot, &transcript_snapshot, &lineage_budget, &role_name_owned, )
        } else {
        crate::router_seam::LiveSessionContextSignals::default()
        };
        let routing_inputs = crate::router_seam::RoutingInputs {
        role: Some(role_name_owned.as_str()),
        workspace_config: &workspace_config,
        goal_text,
        scoped_files: &scoped_files,
        tool_call_density: root_context_signals.tool_call_density,
        failure_streak: root_context_signals.failure_streak,
        budget_remaining_usd: root_context_signals.budget_remaining_usd.unwrap_or(budget_ceiling),
        current_context_tokens: root_context_signals.current_context_tokens,
        session_pin: None,
        conductor_run_id: routing_run_id,
        };
        let routing_outcome = crate::router_seam::apply_capability_routing( self, d, &routing_inputs, );
        // R1a-L4: publish the routing chip through the SAME session conduit
        // every other transcript event flows through (no new store). Only a
        // fresh triage decision carries a payload — bypass/cache-hit paths
        // return None and stay silent, matching the tracing::info! line in
        // router_seam.rs which only fires on that same branch.
        if let Some(decision) = routing_outcome.protocol_event {
        conduit.publish(ProtocolEvent::ModelRouteDecision {
        session_id: id.to_string(),
        seq: conduit.next_seq(),
        created_at: now_iso(),
        decision,
        });
        }
        }
        }
        let requested_effort = apply_fable_mode(options.fable_mode, options.effort.clone(), None).0;
        let (agent_model_id, agent_base_url, agent_api_key, agent_approved_private) = match &descriptor {
        Some(d) => {
        let configured_route = if explicit_native_provider.is_some() {
            self.configured_provider_route_exact(
                &workspace_config,
                &d.provider_id,
                &d.model_id,
                global_cfg,
                requested_effort.clone(),
                options.thinking_budget_tokens,
                options.disable_thinking,
            )
        } else {
            self.configured_provider_route(
                &workspace_config,
                &d.provider_id,
                &d.model_id,
                global_cfg,
                requested_effort.clone(),
                options.thinking_budget_tokens,
                options.disable_thinking,
            )
        };
        if let Some(route) = configured_route {
        (route.model_id, route.base_url, route.api_key, route.approved_private) } else {
        if explicit_native_provider.is_some() {
        let route = native_provider_fallback_route(
            &d.provider_id,
            &d.model_id,
            requested_effort.clone(),
            options.thinking_budget_tokens,
            options.disable_thinking,
            global_cfg.fallback_models.clone(),
        );
        (route.model_id, route.base_url, route.api_key, route.approved_private)
        } else {
        // Credentials belong to the route they were issued for: the global
        // key is only paired with the global base URL, never with another
        // provider's catalog route.
        let catalog_provider = rightkit_llm::agent_providers::find_provider(&d.provider_id);
        let api_key = if catalog_provider.is_some() {
            rightkit_llm::agent_providers::load_api_key(&d.provider_id).unwrap_or_default()
        } else {
            global_cfg.api_key.clone()
        };
        let base_url = catalog_provider .map(|p| p.base_url.to_string()) .unwrap_or_else(|| global_cfg.base_url.clone());
        (rightkit_llm::agent_providers::provider_model_id(&d.provider_id, &d.model_id), base_url, api_key, false) }
        }
        }
        None => {
        let model_id = selected_model.unwrap_or(&global_cfg.model_id).to_string();
        let route = if let Some(provider_id) = explicit_native_provider
        {
            self.configured_provider_route_exact(
                &workspace_config,
                provider_id,
                &model_id,
                global_cfg,
                requested_effort.clone(),
                options.thinking_budget_tokens,
                options.disable_thinking,
            )
            .unwrap_or_else(|| {
                native_provider_fallback_route(
                    provider_id,
                    &model_id,
                    requested_effort.clone(),
                    options.thinking_budget_tokens,
                    options.disable_thinking,
                    global_cfg.fallback_models.clone(),
                )
            })
        } else {
            self.provider_route_for_model(
                &workspace_config,
                &model_id,
                global_cfg,
                requested_effort.clone(),
                options.thinking_budget_tokens,
                options.disable_thinking,
            )
        };
        (route.model_id, route.base_url, route.api_key, route.approved_private)
        }
        };
        let provider_route = Arc::new(Mutex::new(crate::provider_client::ProviderRoute {
        api_key: agent_api_key.clone(), model_id: agent_model_id.clone(), base_url: agent_base_url.clone(), effort: requested_effort.clone(), thinking_budget_tokens: options.thinking_budget_tokens, disable_thinking: options.disable_thinking, fallback_models: if explicit_native_provider.is_some() { Vec::new() } else if options.fallback_models.is_empty() {
        global_cfg.fallback_models.clone() } else {
        options.fallback_models.clone() },
        approved_private: agent_approved_private,
        }));
        live_model_route = Some(provider_route.clone());
        let active_compaction_policy =
            crate::provider_client::compaction_policy_for_model(&agent_model_id);
        let _ = compaction_policy_tx.send(active_compaction_policy.clone());
        *lock_recover(&h8_policy, "session.h8_policy") = active_compaction_policy.clone();
        // Before the first completed provider turn there is no measured
        // conversation footprint. Emit that state explicitly instead of
        // exposing a fabricated zero-capacity value.
        rightkit_logs::agent_telemetry::record_remaining_context_ceiling(
            rightkit_logs::agent_telemetry::RemainingContextInput::unavailable_for_session(
                id.to_string(),
                active_compaction_policy.model_id,
                active_compaction_policy.context_window_tokens,
                active_compaction_policy.reserve_tokens,
                rightkit_logs::agent_telemetry::now_unix_ms(),
            ),
        );
        let role_pack_persona = (!options.bare).then_some(role).flatten().and_then(|role| {
            crate::orchestrator::resolve_builtin_role_pack_for_spawn(
                role,
                &crate::orchestrator::live_role_pack_catalog(),
            )
            .ok()
            .flatten()
            .and_then(|resolved| resolved.persona.map(|persona| persona.system_prompt))
        });
        let agent_persona = role_pack_persona.or_else(|| (!options.bare).then(|| {
            descriptor
                .as_ref()
                .filter(|d| !d.system_prompt.is_empty())
                .map(compose_agent_persona)
        }).flatten());
        let workspace = effective_workspace_root.clone();
        let goal_for_engine = goal.clone();
        let goal_state_for_engine = goal_state.clone();
        let goal_for_small_model_audit = goal.clone();
        let goal_state_for_small_model_audit = goal_state.clone();
        let continuation_enabled = lock_recover(&self.settings, "settings").goal_continuation_enabled;
        let seed_for_engine = engine_seed.clone().or_else(|| seed.clone());
        let state_for_spawner = self.clone();
        let state_for_live_tools = self.clone();
        let routing_state_for_engine = self.clone();
        let mcp_pool_for_engine = self.mcp_pool.clone();
        let requested_permission_mode = options.permission_mode;
        let requested_execution_backend = options.execution_backend.clone();
        let requested_execution_workspace_id = options.execution_workspace_id.clone();
        let requested_execution_workspace_path = options.execution_workspace_path.clone();
        let requested_execution_network_profile = resolved_execution_network_profile.clone();
        let requested_execution_resource_limits = resolved_execution_resource_limits.clone();
        let requested_mcp_channel = mcp_channel.clone();
        let requested_toolset = options.toolset.clone();
        let requested_web_search_config = web_search_config.clone();
        let active_skills = options.active_skills.clone();
        let journal_dir_for_engine = self.journal_dir.clone();
        let storage_for_marker = self.storage.clone();
        // Lane-C native transcript export: the engine task fires a bounded
        // export at every turn boundary and once at session end. The durable
        // cursor lives in storage; triggers only name the session.
        let transcript_exporter_for_engine = self.transcript_exporter.clone();
        let deleted_session_ids_for_engine = self.deleted_session_ids.clone();
        let persistence_state_for_engine = persistence_state.clone();
        let skill_dirs_for_engine = self.skill_dirs.clone();
        let runtime_middleware_for_engine = if !user_surfaces_enabled {
        None } else {
        crate::api_plugins::sync_runtime_middleware_from_plugins(self);
        Some(self.runtime_middleware.clone()) };
        let background_shells_for_engine = background_shells.clone();
        let browser_sessions_for_engine = browser_sessions.clone();
        let task_store_for_engine = task_store.clone();
        let toolset_behavioral_skills = options .toolset .as_deref() .and_then(|toolset| resolve_toolset_profile(toolset).ok()) .map(|profile| profile.default_behavioral_skills);
        let active_skill_ids = merge_active_skill_ids( toolset_behavioral_skills.as_deref(), active_skills.as_slice(), );
        let skill_context_for_engine = if options.bare { None } else { skill_runtime_context_with_behavioral( &self.skill_dirs, effective_workspace_root.as_str(), role, (!active_skill_ids.is_empty()).then_some(active_skill_ids.as_slice()), ) };
        let plugin_prompt_context_for_engine = if options.bare || !user_surfaces_enabled {
        None } else {
        plugin_prompt_context(&self.plugin_dir, role) };
        let is_subagent = parent_session_id.is_some();
        let (max_iterations, max_worker_tool_calls, budget_ceiling_usd) = {
        let s = lock_recover(&self.settings, "settings");
        let ceiling = effective_budget_ceiling_usd;
        ( options.max_iterations.unwrap_or(s.max_iterations), s.max_workers as usize, ceiling, ) };
        let learned_lessons = if is_subagent {
        self.learning.lessons() } else {
        Vec::new() };
        let instinct_block = (!options.bare).then(|| instinct_guidance_block(self)).flatten();
        let environment_snapshot = (!options.bare).then(|| build_session_environment_snapshot(&workspace)).flatten();
        let execution_backend_registry_for_engine = Arc::new(lock_recover(&self.execution_backends, "execution_backends").clone());
        let compaction_policy_tx_for_engine = compaction_policy_tx.clone();
        let storage_for_compaction_archive = self.storage.clone();
        let storage_for_run_origin = self.storage.clone();
        let lineage_budget_for_engine = lineage_budget.clone();
        let denial_tracker_for_engine = self.denial_tracker.clone();
        let daemon_state_for_provider = self.clone();
        let resolved_inference_route_for_engine = options.resolved_inference_route.clone();
        let inference_scheduler_for_engine = self.inference_scheduler.clone();
        let mutation_diagnostics_for_engine = self.mutation_diagnostics.clone();
        let workspace = workspace.to_string();
        let workspace_root = workspace_root.to_string();
        let pending_approval_contexts_for_engine = Arc::clone(&pending_approval_contexts);
        let provider_boundary_tx_for_engine = provider_boundary_tx.clone();
        let initial_tools_for_engine = initial_tools.clone();
        let initial_model_route_revision = resolved_inference_route_for_engine
            .as_ref()
            .and_then(|route| route.direct_connection.as_ref())
            .map(|connection| connection.configuration_revision);
        let initial_model_route_fingerprint = resolved_inference_route_for_engine
            .as_ref()
            .map(|route| route.fingerprint.clone());
        let initial_tool_catalog_revision = tool_catalog.generation();
        let compaction_snapshot_feed_for_engine = compaction_snapshot_feed.clone();
        let compaction_enqueue_tx = compact_tx.clone();
        let compaction_revision_notifier_for_engine = compaction_revision_notifier.clone();
        let elicitation_conduit_for_engine = conduit.clone();
        let pending_elicitations_for_engine = pending_elicitations.clone();
        let pending_credential_requests_for_engine = pending_credential_requests.clone();
        if let Ok(mut snapshot) = compaction_snapshot_feed.write() {
            snapshot.model_route_revision = initial_model_route_revision;
            snapshot.tool_catalog_revision = Some(initial_tool_catalog_revision);
        }
        let panic_watch_conduit = conduit.clone();
        let panic_watch_session_id = id.to_string();
        let storage_for_supervisor = storage_for_marker.clone();
        let mcp_allowed_tools = options.allowed_tools.clone();
        let mcp_disallowed_tools = options.disallowed_tools.clone();
        let engine_task = tokio::spawn(async move {
        use crate::provider_client::ProviderLlmClient;
        use rightkit_agent_engine_core::engine::{SessionEngine, SessionEngineConfig};
        use rightkit_agent_engine_core::llm::LlmClient;
        use rightkit_agent_policy::PermissionMode;
        let environment_snapshot = environment_snapshot;
        let mut environment_runtime =
            crate::api::api_session_context::SessionEnvironmentContextRuntime::default();
        let mut environment_boundary_revision = 0_u64;
        let mut environment_boundary_overlay = String::new();
        if !options.bare {
            let _ = environment_runtime.turn_zero(std::path::Path::new(&workspace));
        }
        // Membrane is the single memory authority: no turn-0 generic memory
        // block is injected into the system prompt (federation delivers
        // context per request; context off means no injection at all).
        let memory_block: Option<String> = None;
        let tools = initial_tools_for_engine;
        let request_authority_for_engine = Arc::new(
            crate::provider_client::HostRequestAuthority::new(daemon_state_for_provider.clone()),
        );
        let compaction_route_resolver = resolved_inference_route_for_engine
            .as_ref()
            .map(|route| {
                Arc::new(DaemonCompactionRouteResolver {
                    state: daemon_state_for_provider.clone(),
                    selection: route.selection.clone(),
                    scheduler: inference_scheduler_for_engine.clone(),
                    tools: tools.clone(),
                    active_requests: inference_active_requests_for_engine.clone(),
                }) as Arc<dyn rightkit_agent_engine_core::CompactionModelRouteResolver>
            });
        let llm: Arc<dyn LlmClient> = match resolved_inference_route_for_engine {
            Some(route) if matches!(route.selection.authority, crate::inference_runtime::InferenceAuthority::Direct) => match ProviderLlmClient::new_direct(route, inference_scheduler_for_engine, tools.clone(), inference_route_rx, inference_active_requests_for_engine).map(|client| client.with_host_request_authority(request_authority_for_engine.clone()).with_anthropic_reasoning(crate::provider_client::AnthropicReasoning::from(&*provider_route.lock().unwrap_or_else(|e| e.into_inner())))) {
                Ok(client) => Arc::new(client),
                Err(error) => { tracing::error!(session_id = %session_id, error = %error, "qualified Direct client construction failed"); return; }
            },
            _ => Arc::new(ProviderLlmClient::new_dynamic_with_compaction_policy_and_lineage_budget(
                provider_route.clone(), tools.clone(), compaction_policy_tx_for_engine, lineage_budget_for_engine,
            ).with_route_admission(route_update_gate_for_engine, inference_active_requests_for_engine)
             .with_host_request_authority(request_authority_for_engine.clone())),
        };
        // FM-026 ABAC PDP — consulted before RuleStore at the permission_rules seam.
        // Precedence: inline `permissions.abac_yaml` (from_yaml_like) wins over
        // `<workspace>/.coderight/policy.abac.yaml` file; first-match-wins; abac:<id>.
        let abac_pdp_for_engine = {
            let inline = workspace_config
                .permissions
                .abac_yaml
                .clone()
                .filter(|s| !s.trim().is_empty());
            if let Some(yaml) = inline {
                match rightkit_agent_policy::PolicyDecisionPoint::from_yaml_like(&yaml) {
                    Ok(pdp) => Some(Arc::new(pdp)),
                    Err(e) => {
                        tracing::warn!(error = %e, "permissions.abac_yaml parse failed");
                        None
                    }
                }
            } else {
                let p = std::path::Path::new(workspace_root.trim())
                    .join(".coderight")
                    .join("policy.abac.yaml");
                if p.exists() {
                    match std::fs::read_to_string(&p) {
                        Ok(text) => match rightkit_agent_policy::PolicyDecisionPoint::from_yaml_like(&text) {
                            Ok(pdp) => Some(Arc::new(pdp)),
                            Err(e) => {
                                tracing::warn!(path = %p.display(), error = %e, "policy.abac.yaml parse failed");
                                None
                            }
                        },
                        Err(e) => {
                            tracing::warn!(path = %p.display(), error = %e, "policy.abac.yaml read failed");
                            None
                        }
                    }
                } else {
                    None
                }
            }
        };
        let engine_cfg = SessionEngineConfig {
        session_id: session_id.clone(), permission_mode: requested_permission_mode.unwrap_or(if is_subagent {
        PermissionMode::Bypass } else {
        PermissionMode::Ask }),
        workspace_trust, workspace_root: std::path::PathBuf::from(&workspace), max_iterations: max_iterations as usize, auto_post_edit_diagnostics: true, max_worker_tool_calls, budget_ceiling_usd, effort: requested_effort, permission_rules: Some(permission_rules_for_engine), abac_pdp: abac_pdp_for_engine, deny_globs: deny_globs_for_config(&workspace_config), skill_dirs: skill_dirs_for_engine, execution_backend: requested_execution_backend, execution_workspace_id: requested_execution_workspace_id, execution_workspace_path: requested_execution_workspace_path, execution_network_profile: requested_execution_network_profile, execution_resource_limits: requested_execution_resource_limits, execution_backend_registry: Some(execution_backend_registry_for_engine), mcp_channel: requested_mcp_channel, toolset: requested_toolset, runtime_middleware: runtime_middleware_for_engine, background_shells: Some(background_shells_for_engine), browser_sessions: Some(browser_sessions_for_engine), task_store: Some(task_store_for_engine), day_key: String::new(), context_fold_enabled: fold_archive_dir.is_some(), fold_archive_dir, loop_detector_config: rightkit_agent_engine_core::engine::LoopDetectorConfig::default(), denial_tracker: Some(denial_tracker_for_engine), compaction_route_resolver, };
        // Set up self-review fork (needs its own conduit handle before conduit_clone moves into the engine).
        let self_review_fork = crate::daemon_self_review::DaemonSelfReviewFork::new(llm.clone(), conduit_clone.clone());
        // H9 is published only by the canonical host-retention callback. Keep
        // its full observed identity as the change key, then expose a
        // session-local monotonic revision; callback absence clears H9.
        let h9_observed = Arc::new(std::sync::Mutex::new(None::<String>));
        let h9_revision = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let h9_feed = compaction_snapshot_feed_for_engine.clone();
        let h9_wake_notify = compaction_wake_notify.clone();
        request_authority_for_engine.set_retention_callback(Arc::new(move |observation| {
            let changed = {
                let Ok(mut feed) = h9_feed.write() else { return; };
                let Ok(mut previous) = h9_observed.lock() else { return; };
                let Some(observation) = observation else {
                    let changed = previous.is_some() || feed.h9_revision.is_some();
                    feed.h9_revision = None;
                    *previous = None;
                    drop(previous);
                    drop(feed);
                    if changed {
                        h9_wake_notify.notify_one();
                    }
                    return;
                };
                let key = match serde_json::to_string(&(
                    &observation.session_id,
                    &observation.task_id,
                    &observation.representation_handle,
                    &observation.source_ref,
                    &observation.source_digest,
                    &observation.serialized_digest,
                    observation.context_epoch,
                )) {
                    Ok(key) => key,
                    Err(_) => return,
                };
                if previous.as_deref() == Some(key.as_str()) {
                    false
                } else {
                    let Some(revision) = h9_revision
                        .try_update(
                            std::sync::atomic::Ordering::Relaxed,
                            std::sync::atomic::Ordering::Relaxed,
                            |current| current.checked_add(1),
                        )
                        .ok()
                        .and_then(|previous| previous.checked_add(1))
                    else {
                        return;
                    };
                    *previous = Some(key);
                    feed.h9_revision = Some(revision);
                    true
                }
            };
            if changed {
                h9_wake_notify.notify_one();
            }
        }));
        let mut engine = SessionEngine::new(llm.clone(), tools, conduit_clone.clone(), engine_cfg);
        if let Ok(Ok(Some(token))) = tokio::task::spawn_blocking(
            crate::github_signin::resolve_access_token,
        ).await {
            engine.set_github_token(token);
        }
        engine.restore_compaction_epoch(compaction_epoch);
        engine.set_compaction_snapshot_feed(compaction_snapshot_feed_for_engine.clone());
        engine.set_compaction_revision_notifier(compaction_revision_notifier_for_engine);
        // Seed only authorities observed from this session's canonical inputs.
        // The engine tracker advances its session revision when a later
        // observation differs; unavailable authorities remain absent.
        engine.observe_host_revision(
            "model_route",
            initial_model_route_fingerprint.as_deref(),
        );
        engine.observe_host_revision(
            "tool_catalog",
            Some(&initial_tool_catalog_revision.to_string()),
        );
        if let (Some(window), Some(reserve)) = (
            active_compaction_policy.context_window_tokens,
            active_compaction_policy.reserve_tokens,
        ) {
            engine.observe_host_revision("capacity", Some(&window.to_string()));
            engine.observe_host_revision("reserve", Some(&reserve.to_string()));
        }
        engine.observe_host_revision("repository_scope", Some(workspace_root.as_str()));
        // The compaction decision is issued host-locally until the Membrane
        // CLI gains a federation command (Orthic-Labs/pulse#129); its
        // binding is this host issuer, stable for the session.
        engine.observe_host_revision("binding", Some(HOST_LOCAL_COMPACTION_BINDING));
        if !daemon_state_for_provider.uses_direct_context() {
            if let (Some(window), Some(reserve)) = (
                active_compaction_policy.context_window_tokens,
                active_compaction_policy.reserve_tokens,
            ) {
                engine.set_compaction_decision_provider(Arc::new(
                    crate::provider_client::CompactionDecisionProvider::new(
                        request_authority_for_engine.clone(),
                        rightkit_llm::agent_providers::client::CapacityProfileV1::new(
                            window,
                            reserve,
                            agent_model_id.clone(),
                            agent_base_url.clone(),
                            None,
                        ),
                        session_id.clone(),
                        workspace.clone(),
                    ),
                ));
            }
        }
        // Bind run-change undo to this engine-owned authority, then make all
        // later engines adopt the same process authority before execution.
        let run_changes = crate::api_run_changes::shared_run_changes_store();
        let file_service = run_changes.bind_file_service(engine.file_service());
        engine.set_file_service(file_service);
        engine.set_run_changes_sink(Arc::new(crate::api_run_changes::DaemonRunChangesSink::shared()));
        if let Some(diagnostics) = mutation_diagnostics_for_engine {
            engine.set_mutation_diagnostics(diagnostics);
        }
        // P2-2 rolling observation decay: a read-time wire projection that
        // clears stale large tool outputs from the model request without
        // touching canonical history, turn structure, exempt tool classes, or
        // open obligations. keep_recent mirrors the compaction masker's
        // default; min_decay_bytes is the P1-6/DEFAULT masking threshold.
        engine.set_wire_projection(std::sync::Arc::new(
            rightkit_agent_engine_core::compaction_archive::DecayingObservationProjection::new(8, 30_000),
        ));
        engine.set_self_review_fork(std::sync::Arc::new(self_review_fork));
        engine.set_self_review_turn_interval(rightkit_agent_engine_core::self_review::DEFAULT_SELF_REVIEW_TURN_INTERVAL);
        // Post-compaction memory re-injection: the provider is inert because
        // Membrane federation is the only memory authority (no generic
        // memory block exists to re-inject).
        if !options.bare {
            engine.set_memory_block_provider(
                crate::api::api_session_context::daemon_memory_block_provider(
                    daemon_state_for_provider,
                ),
            );
        }

        if let Some(config) = requested_web_search_config {
            engine.set_web_search_config(config);
        }
        let (pp, cp) = rightkit_llm::agent_providers::price_per_1k(&agent_model_id);
        let model_is_priced = rightkit_llm::agent_providers::is_priced_model(&agent_model_id)
        && !rightkit_llm::agent_providers::is_subscription_billed_route(&agent_base_url);
        engine.set_pricing_with_known(pp, cp, model_is_priced);
        engine.set_compaction_policy(
            crate::provider_client::compaction_policy_for_model(&agent_model_id),
        );
        engine.set_compaction_policy_receiver(compaction_policy_rx);
        // Repair-telemetry attribution (master-change 5.3): stamp every
        // repair/validation outcome the engine seam records with the
        // session's live provider + model so counters carry all six
        // dimensions.
        engine.set_repair_attribution(
            descriptor.as_ref().map(|d| d.provider_id.clone()),
            Some(agent_model_id.clone()),
        );
        if let Some(storage) = storage_for_compaction_archive {
            engine.set_compaction_archive_sink(Arc::new(
                crate::api_compaction_archives::SqliteCompactionArchiveSink::new(storage).with_snapshot_feed(compaction_snapshot_feed_for_engine.clone()),
            ));
        }
        let system_prompt = options.system_prompt.as_deref().or(system_prompt);
        let combined_system = compose_session_system_prompt( instruction_context.as_deref(), skill_context_for_engine.as_deref(), plugin_prompt_context_for_engine.as_deref(), agent_persona.as_deref(), system_prompt, instinct_block.as_deref(), environment_snapshot.as_deref(), memory_block.as_deref(), );
        let combined_system = crate::main_runtime::compose_cli_system_prompt(
            combined_system.as_deref(),
            None,
            options.append_system_prompt.as_deref(),
        );
        let combined_system = apply_fable_mode(options.fable_mode, None, combined_system).1;
        let combined_system = apply_brief_default(options.brief_default, combined_system);
        if let Some(prompt) = combined_system.as_ref() {
        engine.set_system_prompt(prompt.clone());
        }
        if let Some(msgs) = seed_for_engine {
        engine.seed_transcript(msgs);
        }
        engine.set_interrupt_receiver(interrupt_rx);
        engine.set_permission_mode_receiver(permission_mode_rx);
        engine.set_hook_bus(hook_bus_for_engine);
        engine.set_approval_store(approval_store);
        engine.set_approval_context_store(pending_approval_contexts_for_engine);
        engine.set_credential_request_store(pending_credential_requests_for_engine.clone());
        engine.set_cancel_token_store(cancel_store);
        engine.set_steer_receiver(steer_rx);
        engine.set_provider_boundary_receiver(provider_boundary_rx);
        engine.set_goal_arc(goal_for_engine);
        engine.set_goal_state_arc(goal_state_for_engine);
        engine.set_continuation_enabled(continuation_enabled);
        engine.set_spawner(Arc::new(state_for_spawner.clone()));
        engine.set_subagent_supervisor(Arc::new(
            DaemonSubagentSupervisor::new(state_for_spawner),
        ));
        // Lane C3 (task 1): wire a SQLite-backed origin sink so every
        // pre-effect `ToolCallStarted` row commits durably before the tool
        // side effect fires. When the daemon has no storage wired (rare
        // dev/test paths), the sink still installs but reports `Deferred`
        // so the engine's existing seam translates that into a retry.
        engine.set_run_origin_sink(crate::run_origin_storage::StorageRunOriginSink::new(
            storage_for_run_origin,
            session_id.clone(),
        ));
        // Lane C3 (task 3): wire the shared typed-mutations counter so the
        // engine's `add_typed_mutations` keeps the live session's counter
        // in lockstep. `finalize_goal` reads `live.typed_mutations` to drive
        // the unbacked-victory gate; without this wiring the count would
        // stay at 0 and the gate would always refuse `Accepted`.
        engine.set_typed_mutations_counter(typed_mutations_for_engine);
        engine.set_message_dispatcher(Arc::new(state_for_live_tools.clone()));
        engine.set_team_directory(Arc::new(state_for_live_tools));
        engine.set_questioner(Arc::new(DaemonQuestioner {
        session_id: session_id.clone(), conduit: questioner_conduit, store: questioner_store, sandbox_grant_prompts: sandbox_grant_prompts_for_engine, }));
        engine.set_plan_exit(Arc::new(crate::plan_exit::DaemonPlanExit {
        session_id: session_id.clone(), conduit: plan_conduit, pending: plan_pending, permission_mode_tx: plan_mode_tx, }));
        let mut mcp_manager = if options.no_mcp || options.bare {
            rightkit_mcp::agent_mcp::McpManager::new()
        } else {
            // Canonical pooling (master-change 3.4): sessions in this
            // workspace that configure the same server share one transport.
            let launcher = crate::main_runtime::governed_mcp_process_launcher()
                .expect("MCP process authority must be installed by host composition");
            let launch_context = rightkit_sandbox::execution::ProcessLaunchContext {
                workspace_root: workspace.clone(),
                sandbox_profile: options.safe_mode.then(|| "mcp".to_string()),
                sandbox_required: options.safe_mode,
                network_required: false,
            };
            // A203: project-scoped MCP declarations (`.mcp.json`,
            // `.coderight/mcp.json`) load only for a workspace this session
            // resolved as explicitly Trusted; every other posture is fail-closed.
            let project_trust = if workspace_trust == rightkit_agent_policy::WorkspaceTrust::Trusted {
                rightkit_mcp::agent_mcp::ProjectTrust::Trusted
            } else {
                rightkit_mcp::agent_mcp::ProjectTrust::Untrusted
            };
            rightkit_mcp::agent_mcp::load_for_session(
                Some(std::path::Path::new(&workspace)),
                options.mcp_config.as_deref().map(std::path::Path::new),
                &mcp_pool_for_engine,
                launcher,
                launch_context,
                project_trust,
            )
            .await
        };
        {
            let pending = pending_elicitations_for_engine.clone();
            let conduit = elicitation_conduit_for_engine.clone();
            let mcp_session_id = session_id.clone();
            mcp_manager.set_elicitation_handler(mcp_session_id, Arc::new(move |session_id, request| {
                let pending = pending.clone();
                let conduit = conduit.clone();
                let key = format!("{}:{}", request.server_name, request.id);
                Box::pin(async move {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let requested_schema = match &request.request {
                        rightkit_agent_engine_protocol::ElicitationRequest::Form { requested_schema, .. } => Some(requested_schema.clone()),
                        rightkit_agent_engine_protocol::ElicitationRequest::Url { .. } => None,
                    };
                    pending.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone(), crate::api::PendingElicitation { response: tx, requested_schema });
                    let _cleanup = crate::api::PendingElicitationCleanup::new(pending.clone(), key);
                    let seq = conduit.next_seq();
                    conduit.publish(ProtocolEvent::McpElicitationRequested {
                        session_id,
                        seq,
                        created_at: now_iso(),
                        request: request.clone(),
                    });
                    let result = match tokio::time::timeout(std::time::Duration::from_secs(30 * 60), rx).await {
                        Ok(result) => result.map_err(|_| "elicitation was cancelled".to_string()),
                        Err(_) => Err("elicitation timed out awaiting operator response".to_string()),
                    };
                    result
                })
            }));
        }
        // `--tools` / `--exclude-tools` entries naming qualified MCP tools
        // (`mcp__server__tool`, `*` globs) filter the manager's tools; they are
        // discovered at runtime so only the manager can match them.
        mcp_manager.set_tool_name_filter(rightkit_mcp::agent_mcp::McpToolNameFilter {
            include: mcp_allowed_tools
                .iter()
                .filter(|name| name.starts_with("mcp__"))
                .cloned()
                .collect(),
            exclude: mcp_disallowed_tools
                .iter()
                .filter(|name| name.starts_with("mcp__"))
                .cloned()
                .collect(),
        });
        let mcp_manager = Arc::new(mcp_manager);
        request_authority_for_engine.set_session_mcp(mcp_manager.clone());
        engine.set_mcp(mcp_manager);
        engine.set_context_tool_dispatcher(Arc::new(crate::code_mode_context::CodeModeContextAdapter));
        // A204: opt-in per-turn git checkpoint and auto-commit on exit. Both
        // stay off unless the merged config opts in AND this session resolved
        // the workspace as explicitly Trusted (an untrusted project's own
        // `.coderight/config.toml` can never switch them on).
        let (turn_checkpoints_enabled, auto_commit_on_exit_enabled) =
            if options.bare || workspace_trust != rightkit_agent_policy::WorkspaceTrust::Trusted {
                (false, false)
            } else {
                let features = rightkit_app_settings::agent_config::ConfigLoader::new()
                    .load_files_for_workspace(std::path::Path::new(&workspace))
                    .features;
                (
                    features.turn_checkpoints_enabled(),
                    features.auto_commit_on_exit_enabled(),
                )
            };
        let mut turn_checkpoint_seq: u64 = if turn_checkpoints_enabled {
            rightkit_agent_tools::coding_tools::workspace_checkpoint::latest_turn_number(
                std::path::Path::new(&workspace),
                &session_id,
            )
        } else {
            0
        };
        let mut compaction_wake_pending = false;
        let mut permission_mode_watch_open = true;
        let mut compaction_policy_watch_open = true;
        loop {
        while let Ok(request) = rewind_rx.try_recv() {
        request.apply(&mut engine);
        }
        tokio::select! {
        _ = compaction_wake_notify.notified() => {
        compaction_wake_pending = true;
        }
        permit = compaction_enqueue_tx.reserve(), if compaction_wake_pending => {
        if process_idle_compaction_retry(
        permit,
        &mut compaction_wake_pending,
        || {
        engine.refresh_compaction_revision_inputs();
        compaction_snapshot_feed_for_engine.read().ok().map(|snapshot| snapshot.clone())
            .is_some_and(|snapshot| engine.compaction_retry_wake_eligible(&snapshot))
        },
        ).is_err() { break; }
        }
        changed = permission_mode_idle_rx.changed(), if permission_mode_watch_open => {
        if changed.is_err() {
        permission_mode_watch_open = false;
        } else {
        compaction_wake_pending = true;
        }
        }
        changed = compaction_policy_idle_rx.changed(), if compaction_policy_watch_open => {
        if changed.is_err() {
        compaction_policy_watch_open = false;
        } else {
        compaction_wake_pending = true;
        }
        }
        msg = msg_rx.recv() => {
        match msg {
        Some(text) => {
        let _live_turn_guard = live_turn_handle_for_engine.begin();
        // Consume one admission recorded by the gated user-prompt path.
        // Internal producers send without counting, so saturate at zero
        // instead of underflowing the busy signal.
        let _ = pending_admissions_for_engine.try_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |count| count.checked_sub(1),
        );
        if turn_checkpoints_enabled {
            // Snapshot BEFORE the turn runs so the checkpoint is the state this
            // turn started from. Bounded: a slow snapshot never stalls the turn.
            turn_checkpoint_seq = turn_checkpoint_seq.saturating_add(1);
            let snapshot_workspace = workspace.clone();
            let snapshot_session = session_id.to_string();
            let snapshot_turn = turn_checkpoint_seq;
            let snapshot = tokio::task::spawn_blocking(move || {
                rightkit_agent_tools::coding_tools::workspace_checkpoint::snapshot_turn(
                    std::path::Path::new(&snapshot_workspace),
                    &snapshot_session,
                    snapshot_turn,
                )
            });
            match tokio::time::timeout(std::time::Duration::from_secs(10), snapshot).await {
                Ok(Ok(Ok(Some(checkpoint_id)))) => tracing::debug!(
                    session_id = %session_id,
                    turn = snapshot_turn,
                    checkpoint_id = %checkpoint_id,
                    "per-turn workspace checkpoint recorded"
                ),
                Ok(Ok(Ok(None))) => {}
                Ok(Ok(Err(error))) => tracing::warn!(
                    session_id = %session_id,
                    turn = snapshot_turn,
                    error = %error,
                    "per-turn workspace checkpoint failed; continuing the turn"
                ),
                Ok(Err(error)) => tracing::warn!(
                    session_id = %session_id,
                    error = %error,
                    "per-turn workspace checkpoint task failed; continuing the turn"
                ),
                Err(_) => tracing::warn!(
                    session_id = %session_id,
                    turn = snapshot_turn,
                    "per-turn workspace checkpoint timed out; continuing the turn"
                ),
            }
        }
        if !options.bare {
        if let Some(refresh) = environment_runtime.refresh(std::path::Path::new(&workspace)) {
        // This is a complete current-state snapshot, so bounded-queue
        // coalescing can safely keep the newest unpublished revision. Earlier
        // revisions already sent remain in canonical transcript history.
        if !environment_boundary_overlay.is_empty() {
        environment_boundary_overlay.push_str("\n\n");
        }
        environment_boundary_overlay.push_str(&refresh);
        environment_boundary_revision = environment_boundary_revision.saturating_add(1);
        // HCTX-002: accepted provider-visible host state must never vanish
        // because the boundary queue is full. `send_latest` never blocks (so it
        // cannot deadlock behind the in-flight model stream) and never drops:
        // on backpressure it coalesces to the newest revision for the category,
        // which is promoted at the next provider-turn boundary.
        match provider_boundary_tx_for_engine.send_latest(
            rightkit_agent_engine_core::provider_turn_boundary::ProviderBoundaryUpdate::WorldState(
                rightkit_agent_engine_core::provider_turn_boundary::BoundaryTextUpdate::new(
                    environment_boundary_revision,
                    environment_boundary_overlay.clone(),
                ),
            ),
        ) {
            rightkit_agent_engine_core::provider_turn_boundary::BoundarySendOutcome::Queued
            | rightkit_agent_engine_core::provider_turn_boundary::BoundarySendOutcome::Coalesced => {}
            rightkit_agent_engine_core::provider_turn_boundary::BoundarySendOutcome::Closed => {
                tracing::warn!(
                    session_id = %session_id,
                    revision = environment_boundary_revision,
                    "provider boundary receiver closed; environment refresh cannot become provider-visible"
                );
            }
        }
        }
        }
        if let Some(store) = storage_for_marker.as_ref() {
        let store = store.lock().await;
        if let Err(error) = store.update_session_status(&session_id, "running", &now_iso()) {
        tracing::warn!(session_id = %session_id, error = %error, "failed to persist running session status");
        }
        }
        let turn_id = format!("turn-{}", now_millis());
        let mut journal = if let Some(jdir) = journal_dir_for_engine.as_deref() {
        let mut j = crate::journal::ExecutionJournal::open(jdir, &session_id).await;
        j.append(crate::journal::entry( "run_start", &session_id, &turn_id, serde_json::json!({
        "user_message_preview": crate::journal::text_preview(&text, 1000) })
        )).await;
        // Mid-life crash: stamp the persistent recovery marker on the
        // session row now, so the UI can badge "recoverable" before the
        // daemon restarts (the resume-time transcript note is independent).
        if let Some(store) = storage_for_marker.as_ref() {
        crate::journal::reconcile_persistent_marker(store, jdir, &session_id).await;
        }
        Some(j) } else {
        None };
        let reminder = if learned_lessons.is_empty() {
        None } else {
        Some(dynamic_user_turn_reminder(None, &learned_lessons)) };
        let engine_text = reminder
            .as_deref()
            .map(|block| decorate_user_payload_with_system_reminder(&text, block))
            .unwrap_or_else(|| text.clone());
        let result = engine.submit_user_message(&engine_text).await;
        // ENG-82: every successful goal turn receives one bounded daemon-side
        // progress audit through the separate `SmallModelClient` transport.
        // It runs in this daemon task after UI submission, so a disconnected
        // desktop client cannot cancel or substitute its result. This is an
        // audit only: it cannot perform tools or declare a goal achieved.
        let goal_for_audit = {
            lock_recover(&goal_for_small_model_audit, "session.goal").clone()
        };
        if let (Ok(answer), Some(goal)) = (result.as_ref(), goal_for_audit) {
            if !goal.trim().is_empty() {
                let audit_context = format!(
                    "User turn:\n{text}\n\nCompleted turn result:\n{answer}"
                );
                let client = crate::provider_client::DaemonSmallModelClient::new(
                    provider_route.clone(),
                );
                let config = rightkit_agent_orchestration::coding::goal_loop::GoalLoopConfig {
                    max_iterations: 1,
                    max_wall_clock: std::time::Duration::from_secs(10),
                    input_byte_cap: rightkit_agent_orchestration::coding::small_model_utility::DEFAULT_INPUT_BYTE_CAP,
                    interval: std::time::Duration::ZERO,
                };
                let budget = rightkit_agent_orchestration::coding::small_model_utility::SmallModelBudget::new(
                    1,
                    rightkit_agent_orchestration::coding::small_model_utility::DEFAULT_CALL_TOKEN_ESTIMATE,
                );
                match rightkit_agent_orchestration::coding::goal_loop::run_goal_loop(
                    &client,
                    &goal,
                    &audit_context,
                    budget,
                    config,
                    rightkit_agent_orchestration::coding::watchdog::RunWatchdog::new(
                        rightkit_agent_engine_core::GoalState::MAX_NO_PROGRESS as u32,
                    ),
                )
                .await
                {
                    Ok(report) => {
                        if report.audits.iter().any(|audit| !audit.on_track) {
                            if let Some(state) = lock_recover(
                                &goal_state_for_small_model_audit,
                                "session.goal_state",
                            )
                            .as_mut()
                            {
                                state.no_progress_count = state.no_progress_count.saturating_add(1);
                            }
                        }
                    }
                    Err(error) => {
                        tracing::debug!(
                            session_id = %session_id,
                            error = %error,
                            "goal progress audit unavailable"
                        );
                    }
                }
            }
        }
        // EVT-004: terminal turn state is a durable semantic fact, not an
        // inference from the final streaming delta or transport lifetime. The
        // EVT-003 persistence gate keeps this event invisible until SQLite
        // commit succeeds.
        let terminal_status = match &result {
            Ok(_) => "completed",
            Err(rightkit_agent_engine_core::engine::EngineError::Interrupted) => "cancelled",
            Err(_) => "failed",
        };
        let terminal_seq = conduit_clone.next_seq();
        conduit_clone.publish(ProtocolEvent::SessionTurnFinished {
            session_id: session_id.clone(),
            seq: terminal_seq,
            created_at: now_iso(),
            turn_id: turn_id.clone(),
            status: terminal_status.to_string(),
        });
        // Lane-C: turn settle (completed, cancelled, or failed — all go
        // through this terminal publish) drains the durable tail to the
        // absorbed stream once the persistence gate has committed it.
        if let Some(exporter) = transcript_exporter_for_engine.as_ref() {
            crate::native_transcript_exporter::spawn_session_export(
                exporter.clone(),
                storage_for_marker.clone(),
                deleted_session_ids_for_engine.clone(),
                persistence_state_for_engine.clone(),
                session_id.clone(),
                terminal_seq,
            );
        }
        // `session_id` is the conductor-run id supplied as
        // `parent_session_id` to every sibling spawned during this turn.
        // Once the authoritative engine turn completes, its stable routing
        // decisions must not leak into the next turn on the same session.
        crate::router_seam::evict_routing_cache_for_run(
        &routing_state_for_engine,
        &session_id,
        );
        if let Some(ref mut j) = journal {
        let success = result.is_ok();
        j.append(crate::journal::entry("run_end", &session_id, &turn_id, serde_json::json!({ "success": success }))).await;
        j.close().await;
        if let (Some(store), Some(jdir)) = (storage_for_marker.as_ref(), journal_dir_for_engine.as_deref()) {
        crate::journal::reconcile_persistent_marker(store, jdir, &session_id).await;
        }
        }
        let persisted_status = if is_subagent {
            match &result {
                Ok(_) => "completed",
                Err(rightkit_agent_engine_core::engine::EngineError::Interrupted) => "cancelled",
                Err(_) => "failed",
            }
        } else {
            "idle"
        };
        if let Err(e) = result {
        use rightkit_agent_engine_core::engine::EngineError;
        if !matches!(e, EngineError::Interrupted) {
        tracing::error!(session_id = %session_id, error = %e, "engine run error");
        }
        }
        if let Some(store) = storage_for_marker.as_ref() {
        let store = store.lock().await;
        if let Err(error) = store.update_session_status(&session_id, persisted_status, &now_iso()) {
        tracing::warn!(session_id = %session_id, error = %error, "failed to persist session status");
        }
        }
        while let Ok(request) = rewind_rx.try_recv() {
        request.apply(&mut engine);
        }
        }
        None => break, }
        }
        Some(request) = rewind_rx.recv() => {
        request.apply(&mut engine);
        }
        Some(mut command) = compact_rx.recv() => {
        let response = command.response.take();
        let result = engine
            .execute_compaction_command(command)
            .await
            .map_err(|error| error.to_string());
        if let Some(resp_tx) = response { let _ = resp_tx.send(result); }
        }
        Some(command) = context_fold_rx.recv() => {
        let result = engine.fold_subtrajectory(
        &command.subtask_id,
        &command.start_marker,
        &command.end_marker,
        command.summary,
        );
        let result = match result {
        Ok(folded) => if let Some(fold) = engine.transcript().messages().iter().find(|message| matches!(
        message,
        TranscriptMessage::Fold { archive_ref, .. } if archive_ref == &folded.ref_id
        )).cloned() {
        let seq = conduit_clone.next_seq();
        conduit_clone.publish(ProtocolEvent::SessionContextFolded {
        session_id: session_id.clone(), seq, created_at: now_iso(),
        subtask_id: command.subtask_id, start_marker: command.start_marker,
        end_marker: command.end_marker, fold,
        });
        Ok(folded)
        } else {
        Err(rightkit_agent_engine_core::ContextFoldError::ArchiveUnavailable)
        },
        Err(error) => Err(error),
        };
        let _ = command.response.send(result);
        }
        Some(request) = driver_tool_rx.recv() => {
        let call_id = request.call.id.clone();
        let mut response = request.response;
        let result = if request.permission_only {
            tokio::select! {
                result = engine.authorize_external_tool_call(request.call) => result,
                _ = response.closed() => {
                    engine.revoke_external_tool_approval(&call_id);
                    continue;
                }
            }
        } else {
            engine.dispatch_external_tool_call(request.call).await
        };
        if response.send(result).is_err() && request.permission_only {
            engine.revoke_external_tool_approval(&call_id);
        }
        }
        else => break, }
        }
        // A204: opt-in auto-commit on exit (workspace is trusted and the
        // config opted in; see the gate above). Best-effort and never fatal.
        if auto_commit_on_exit_enabled {
            let commit_workspace = workspace.clone();
            let message = format!("coderight: auto-commit on exit (session {session_id})");
            match tokio::task::spawn_blocking(move || {
                rightkit_agent_tools::coding_tools::workspace_checkpoint::auto_commit_workspace(
                    std::path::Path::new(&commit_workspace),
                    &message,
                )
            })
            .await
            {
                Ok(Ok(Some(commit))) => tracing::info!(
                    session_id = %session_id,
                    commit = %commit,
                    "auto-committed workspace changes on session exit"
                ),
                Ok(Ok(None)) => {}
                Ok(Err(error)) => tracing::warn!(
                    session_id = %session_id,
                    error = %error,
                    "auto-commit on exit skipped"
                ),
                Err(error) => tracing::warn!(
                    session_id = %session_id,
                    error = %error,
                    "auto-commit on exit task failed"
                ),
            }
        }
        // Channel closure is the session-removal lifecycle. Usually the
        // per-turn hook above already emptied this run, but this idempotent
        // cleanup covers removal before or between turns.
        crate::router_seam::evict_routing_cache_for_run(
        &routing_state_for_engine,
        &session_id,
        );
        // Lane-C: session end drains whatever durable tail remains after the
        // last turn's settle trigger — min_seq 0 waits only for the
        // persister to quiesce before reading.
        if let Some(exporter) = transcript_exporter_for_engine.as_ref() {
            crate::native_transcript_exporter::spawn_session_export(
                exporter.clone(),
                storage_for_marker.clone(),
                deleted_session_ids_for_engine.clone(),
                persistence_state_for_engine.clone(),
                session_id.clone(),
                0,
            );
        }
        });
        crate::daemon_state::supervise_session_engine_task_persisting(engine_task, panic_watch_conduit, panic_watch_session_id, storage_for_supervisor.clone());
        Some(tx) } else {
        rightkit_logs::agent_telemetry::record_remaining_context_ceiling(
            rightkit_logs::agent_telemetry::RemainingContextInput::unavailable_for_session(
                id.to_string(),
                model_label.clone(),
                None,
                None,
                rightkit_logs::agent_telemetry::now_unix_ms(),
            ),
        );
        // r5.21: Inline MCP is also available in providerless sessions. Keep
        // execution behind SessionEngine so the same registry, permission,
        // ABAC, execution-backend, run-origin, & result-shaping authorities
        // govern host calls. ProviderlessLlm fails closed if this driver-only
        // engine is ever asked to perform a model turn.
        let session_id = id.to_string();
        let workspace = effective_workspace_root.clone();
        let workspace_root_for_policy = workspace_root.to_string();
        let requested_permission_mode = options.permission_mode;
        let requested_execution_backend = options.execution_backend.clone();
        let requested_execution_workspace_id = options.execution_workspace_id.clone();
        let requested_execution_workspace_path = options.execution_workspace_path.clone();
        let requested_execution_network_profile = resolved_execution_network_profile.clone();
        let requested_execution_resource_limits = resolved_execution_resource_limits.clone();
        let requested_mcp_channel = mcp_channel.clone();
        let requested_toolset = options.toolset.clone();
        let requested_effort_for_driver = options.effort.clone();
        let permission_rules_for_driver = permission_rules.clone();
        let execution_backend_registry_for_driver = Arc::new(lock_recover(
            &self.execution_backends,
            "execution_backends",
        ).clone());
        let runtime_middleware_for_driver = if !user_surfaces_enabled {
            None
        } else {
            crate::api_plugins::sync_runtime_middleware_from_plugins(self);
            Some(self.runtime_middleware.clone())
        };
        let background_shells_for_driver = background_shells.clone();
        let browser_sessions_for_driver = browser_sessions.clone();
        let task_store_for_driver = task_store.clone();
        let denial_tracker_for_driver = self.denial_tracker.clone();
        let tools_for_driver = initial_tools.clone();
        let workspace_config_for_driver = workspace_config.clone();
        let approval_store_for_driver = pending_approvals.clone();
        let approval_context_store_for_driver = pending_approval_contexts.clone();
        let cancel_store_for_driver = cancel_tokens.clone();
        let hook_bus_for_driver = hook_bus.clone();
        let questioner_conduit_for_driver = conduit.clone();
        let questioner_store_for_driver = pending_questions.clone();
        let sandbox_grant_prompts_for_driver = sandbox_grant_prompts.clone();
        let plan_conduit_for_driver = conduit.clone();
        let plan_pending_for_driver = pending_plans.clone();
        let plan_mode_tx_for_driver = permission_mode_tx.clone();
        let state_for_driver = self.clone();
        let mutation_diagnostics_for_driver = self.mutation_diagnostics.clone();
        let storage_for_run_origin_driver = self.storage.clone();
        let typed_mutations_for_driver = typed_mutations.clone();
        let workspace_trust_for_driver = workspace_trust;
        let is_subagent_for_driver = parent_session_id.is_some();
        let skill_dirs_for_driver = self.skill_dirs.clone();
        let (max_iterations_for_driver, max_worker_tool_calls_for_driver) = {
            let settings = lock_recover(&self.settings, "settings");
            (
                options.max_iterations.unwrap_or(settings.max_iterations) as usize,
                settings.max_workers as usize,
            )
        };
        tokio::spawn(async move {
            use rightkit_agent_engine_core::engine::{SessionEngine, SessionEngineConfig};
            use rightkit_agent_policy::PermissionMode;

            let abac_pdp = {
                let inline = workspace_config_for_driver
                    .permissions
                    .abac_yaml
                    .clone()
                    .filter(|value| !value.trim().is_empty());
                if let Some(yaml) = inline {
                    match rightkit_agent_policy::PolicyDecisionPoint::from_yaml_like(&yaml) {
                        Ok(pdp) => Some(Arc::new(pdp)),
                        Err(error) => {
                            tracing::warn!(error = %error, "permissions.abac_yaml parse failed");
                            None
                        }
                    }
                } else {
                    let path = std::path::Path::new(workspace_root_for_policy.trim())
                        .join(".coderight")
                        .join("policy.abac.yaml");
                    if path.exists() {
                        match std::fs::read_to_string(&path) {
                            Ok(text) => match rightkit_agent_policy::PolicyDecisionPoint::from_yaml_like(&text) {
                                Ok(pdp) => Some(Arc::new(pdp)),
                                Err(error) => {
                                    tracing::warn!(path = %path.display(), error = %error, "policy.abac.yaml parse failed");
                                    None
                                }
                            },
                            Err(error) => {
                                tracing::warn!(path = %path.display(), error = %error, "policy.abac.yaml read failed");
                                None
                            }
                        }
                    } else {
                        None
                    }
                }
            };
            let config = SessionEngineConfig {
                session_id: session_id.clone(),
                permission_mode: requested_permission_mode.unwrap_or(if is_subagent_for_driver {
                    PermissionMode::Bypass
                } else {
                    PermissionMode::Ask
                }),
                workspace_trust: workspace_trust_for_driver,
                workspace_root: std::path::PathBuf::from(&workspace),
                max_iterations: max_iterations_for_driver,
                auto_post_edit_diagnostics: true,
                max_worker_tool_calls: max_worker_tool_calls_for_driver,
                budget_ceiling_usd: effective_budget_ceiling_usd,
                effort: requested_effort_for_driver,
                permission_rules: Some(permission_rules_for_driver),
                skill_dirs: skill_dirs_for_driver,
                execution_backend: requested_execution_backend,
                execution_workspace_id: requested_execution_workspace_id,
                execution_workspace_path: requested_execution_workspace_path,
                execution_network_profile: requested_execution_network_profile,
                execution_resource_limits: requested_execution_resource_limits,
                execution_backend_registry: Some(execution_backend_registry_for_driver),
                mcp_channel: requested_mcp_channel,
                toolset: requested_toolset,
                runtime_middleware: runtime_middleware_for_driver,
                background_shells: Some(background_shells_for_driver),
                browser_sessions: Some(browser_sessions_for_driver),
                task_store: Some(task_store_for_driver),
                day_key: String::new(),
                context_fold_enabled: false,
                fold_archive_dir: None,
                loop_detector_config: rightkit_agent_engine_core::engine::LoopDetectorConfig::default(),
                denial_tracker: Some(denial_tracker_for_driver),
                abac_pdp,
                deny_globs: deny_globs_for_config(&workspace_config_for_driver),
                compaction_route_resolver: None,
            };
            let mut engine = SessionEngine::new(
                Arc::new(ProviderlessLlm),
                tools_for_driver,
                questioner_conduit_for_driver.clone(),
                config,
            );
            let run_changes = crate::api_run_changes::shared_run_changes_store();
            let file_service = run_changes.bind_file_service(engine.file_service());
            engine.set_file_service(file_service);
            engine.set_run_changes_sink(Arc::new(
                crate::api_run_changes::DaemonRunChangesSink::shared(),
            ));
            if let Some(diagnostics) = mutation_diagnostics_for_driver {
                engine.set_mutation_diagnostics(diagnostics);
            }
            engine.set_hook_bus(hook_bus_for_driver);
            engine.set_approval_store(approval_store_for_driver);
            engine.set_approval_context_store(approval_context_store_for_driver);
            engine.set_cancel_token_store(cancel_store_for_driver);
            engine.set_interrupt_receiver(interrupt_rx);
            engine.set_permission_mode_receiver(permission_mode_rx);
            engine.set_run_origin_sink(crate::run_origin_storage::StorageRunOriginSink::new(
                storage_for_run_origin_driver,
                session_id.clone(),
            ));
            engine.set_typed_mutations_counter(typed_mutations_for_driver);
            engine.set_message_dispatcher(Arc::new(state_for_driver.clone()));
            engine.set_team_directory(Arc::new(state_for_driver.clone()));
            engine.set_spawner(Arc::new(state_for_driver.clone()));
            engine.set_subagent_supervisor(Arc::new(
                DaemonSubagentSupervisor::new(state_for_driver),
            ));
            engine.set_questioner(Arc::new(DaemonQuestioner {
                session_id: session_id.clone(),
                conduit: questioner_conduit_for_driver,
                store: questioner_store_for_driver,
                sandbox_grant_prompts: sandbox_grant_prompts_for_driver,
            }));
            engine.set_plan_exit(Arc::new(crate::plan_exit::DaemonPlanExit {
                session_id,
                conduit: plan_conduit_for_driver,
                pending: plan_pending_for_driver,
                permission_mode_tx: plan_mode_tx_for_driver,
            }));
            while let Some(request) = driver_tool_rx.recv().await {
                let call_id = request.call.id.clone();
                let mut response = request.response;
                let result = if request.permission_only {
                    tokio::select! {
                        result = engine.authorize_external_tool_call(request.call) => result,
                        _ = response.closed() => {
                            engine.revoke_external_tool_approval(&call_id);
                            continue;
                        }
                    }
                } else {
                    engine.dispatch_external_tool_call(request.call).await
                };
                if response.send(result).is_err() && request.permission_only {
                    engine.revoke_external_tool_approval(&call_id);
                }
            }
        });
        None };
        let compact_tx = msg_tx.as_ref().map(|_| compact_tx);
        let compaction_policy_tx = msg_tx.as_ref().map(|_| compaction_policy_tx);
        let context_fold_tx = msg_tx.as_ref().map(|_| context_fold_tx);
        let driver_tool_tx = Some(driver_tool_tx);
        // Shared typed-mutations counter: the engine bumps it on every
        // successful `WorkspaceMutation`; `finalize_goal` reads it from
        // `LiveSession::typed_mutations` so the unbacked-victory gate sees
        // the real count instead of a hardcoded zero.
        let live = Arc::new(LiveSession {
        tool_catalog,
        inference_selection: Arc::new(Mutex::new(options.inference_selection)),
        inference_fingerprint: Arc::new(Mutex::new(options.inference_fingerprint)),
        inference_route_tx: live_inference_route_tx,
        inference_active_requests,
        provider_id: Arc::new(Mutex::new(initial_provider_id)),
        route_update_gate,
        pending_admissions,
        compaction_snapshot_feed: compaction_snapshot_feed.clone(), id: id.to_string(), title: title_state, model: Arc::new(Mutex::new(model_label)), model_route: live_model_route, workspace_root: effective_workspace_root.clone(), role: role.map(|s| s.to_string()), parent_session_id: parent_session_id.map(|s| s.to_string()), forked_from_session_id: options.forked_from_session_id, allowed_tools: options.allowed_tools, disallowed_tools: options.disallowed_tools, toolset: options.toolset, active_skills: options.active_skills, execution_backend: options.execution_backend, execution_workspace_id: options.execution_workspace_id, execution_workspace_path: options.execution_workspace_path, execution_network_profile: resolved_execution_network_profile, driver_kind: Arc::new(Mutex::new(driver_kind)), mcp_channel, spec_id: Arc::new(Mutex::new(options.spec_id)), workspace_trust, effort: Arc::new(Mutex::new(options.effort)), thinking_budget_tokens: options.thinking_budget_tokens, disable_thinking: options.disable_thinking, safe_mode: options.safe_mode, budget_ceiling_usd: (effective_budget_ceiling_usd > 0.0 && effective_budget_ceiling_usd.is_finite()).then_some(effective_budget_ceiling_usd), lineage_budget, created_at: now.clone(), updated_at: updated_at_state, conduit, msg_tx, live_turn_handle, driver_tool_tx, compact_tx, context_fold_tx, compaction_policy_tx, interrupt_tx: Some(interrupt_tx), permission_mode_tx: Some(permission_mode_tx), permission_rules, pending_approvals, pending_approval_contexts: pending_approval_contexts.clone(), pending_credential_requests, cancel_tokens, pending_questions, sandbox_grant_prompts, pending_elicitations, pending_plans, transcript, persistence_state, tasks, background_shells, process_launch_adapter, process_launch_context, browser_sessions, artifacts, storage: self.storage.clone(), steer_tx: Some(steer_tx), provider_boundary_tx: Some(provider_boundary_tx), goal, goal_state, goal_gap_tracker, rewind_tx, hook_bus: hook_bus.clone(), agent_roster: Mutex::new(crate::agent_roster::AgentRoster::default()), cost_usd, typed_mutations, task_typed_evidence: Arc::new(Mutex::new(std::collections::HashMap::new())), task_job_ids: Arc::new(Mutex::new(std::collections::HashMap::new())), completion_evidence: Arc::new(Mutex::new(CompletionEvidence::default())),
        compaction_revision_notifier: Some(compaction_revision_notifier),
// Lane C3 (task 2): per-run sub-agent budget ledger. Default caps (4 concurrent / 64 cumulative) match RunSubagentBudget::default; the spawn gate consults this so a runaway conductor can never blow the budget.
run_budget: tokio::sync::Mutex::new(RunBudgetLedger::new(RunSubagentBudget::default())),
// Lane C3 (task 3): per-run governed completion arbiter. Tracks
// unbacked-victory-claim attempts and refuses a goal after the policy
// cap when the run claims done without typed mutations.
governed_arbiter: tokio::sync::Mutex::new(rightkit_agent_orchestration::coding::run_governance::GovernedArbiter::default()), });
        lock_recover(&self.sessions, "sessions").insert(id.to_string(), live.clone());
        live
    }
}

/// Serialises the check-then-insert window of session creation (CR3).
static SESSION_CREATE_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
