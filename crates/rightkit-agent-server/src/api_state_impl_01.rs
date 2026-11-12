impl DaemonState {
    pub fn new(token: impl Into<String>) -> Self {
        let team_store_path = default_team_store_path();
        let live_teams = load_team_store(&team_store_path);
        let message_store_path = default_message_store_path();
        let message_history = load_message_history_store(&message_store_path);
        let permission_rule_store_path = default_permission_rule_store_path();
        let permission_rule_store = load_permission_rule_store(&permission_rule_store_path);
        let permission_denial_store_path = default_permission_denial_store_path();
        let permission_denials = load_permission_denial_store(&permission_denial_store_path);
        let workspace_trust_store_path = default_workspace_trust_store_path();
        let workspace_trust_store = load_workspace_trust_store(&workspace_trust_store_path);
        let memory_backend: Arc<dyn rightkit_memory_client::backend::MemoryBackend> = Arc::new(
            rightkit_memory_client::backend::unavailable::UnavailableMemoryBackend::new(
                "backend_unavailable",
                rightkit_memory_client::membrane::OFF_REASON,
            ),
        );
        let memory_provenance =
            rightkit_memory_client::memory_provenance::MemoryProvenanceStore::from_backend(
                memory_backend.clone(),
            );
        let instincts = crate::store_write::load_instincts(&default_instincts_store_path());
        let driver_host = match rightkit_agent_coding_tools::process_resource_authority::process_resource_governor() {
            Some(governor) => crate::driver_host_runtime::DriverHostRuntime::new().with_process_launch(
                Arc::new(
                    rightkit_agent_coding_tools::process_resource_authority::GovernedProcessLaunchAdapter::new(
                        rightkit_agent_coding_tools::process_launch::LocalProcessLaunchAdapter,
                        governor,
                        rightkit_agent_coding_tools::process_resource_authority::ResourceKind::ChildProcess,
                        true,
                    ),
                ),
                rightkit_sandbox::execution::ProcessLaunchContext {
                    workspace_root: String::new(),
                    sandbox_profile: Some("driver".into()),
                    sandbox_required: false,
                    network_required: false,
                },
            ),
            None => crate::driver_host_runtime::DriverHostRuntime::new(),
        };
        let cli_registry =
            Arc::new(rightkit_agents::driver_host::cli_runtime::CliAdapterRegistry::new());
        rightkit_agents::driver_host::cli_runtime::built_in::seed_registry(&cli_registry)
            .expect("built-in CLI registry must register atomically");
        let cli_registry_provider_ids = cli_registry.provider_ids().into_iter().collect();
        Self {
            context_mode: crate::cli::ContextMode::Membrane,
            context_degraded: None,
            external_observations: Arc::new(Mutex::new(
                crate::daemon::external_observations::ExternalObservationLoop::new(),
            )),
            fs_watch: Default::default(),
            memory_consolidation_status:
                rightkit_memory_client::consolidation_status::ConsolidationStatusState::new(),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            handoff_controller: Arc::new(
                rightkit_agent_coding_tools::handoff::HandoffController,
            ),
            mcp_pool: Arc::new(rightkit_mcp::agent_mcp::McpTransportPool::default()),
            driver_host,
            cli_registry,
            inference_scheduler: crate::inference_runtime::InferenceScheduler::new(4),
            cli_executor: None,
            cli_bridge: None,
            qualification_bindings: Arc::new(std::sync::RwLock::new(HashSet::new())),
            cli_launch_contexts: None,
            driver_authority_lock: Arc::new(tokio::sync::Mutex::new(())),
            cli_registry_provider_ids,
            cli_delegation_policy: rightkit_agents::driver_host::cli_runtime::CliDelegationPolicy {
                allowed_child_providers: [
                    "claude",
                    "codex",
                    "grok",
                    "cursor",
                    "cline",
                    "commandcode",
                ]
                .into_iter()
                .map(rightkit_agents::driver_host::cli_runtime::CliProviderId::new)
                .collect(),
                default_child_provider: None,
                role_routes: std::collections::BTreeMap::new(),
                permit_model_provider_hints: true,
                max_depth: 2,
                max_parallel_children: 4,
                max_parallel_per_provider: std::collections::BTreeMap::new(),
            },
            cli_delegation_policies: Arc::new(Mutex::new(HashMap::new())),
            lineage_budgets: Arc::new(Mutex::new(HashMap::new())),
            deleted_session_ids: Arc::new(Mutex::new(HashSet::new())),
            token: token.into(),
            engine_cfg: None,
            licensed_features: Arc::new(Mutex::new(initial_license_features())),
            license_gate: None,
            agent_registry: Arc::new(AgentRegistry::with_builtins()),
            settings: Arc::new(Mutex::new(DaemonSettingsStore::default())),
            bound_port: 0,
            database_path: String::new(),
            learning: crate::learning::LearningHarness::new(),
            skillopt_runs: Arc::new(Mutex::new(
                rightkit_agent_learning::SkillOptimizationStore::new(),
            )),
            skillopt_store_path: None,
            memory_backend,
            membrane: Arc::new(rightkit_memory_client::membrane::Unavailable),
            transcript_exporter: None,
            mutation_diagnostics: None,
            memory_provenance,
            instincts: Arc::new(Mutex::new(instincts)),
            audit: crate::audit::AuditLog::new(),
            observations: crate::observation::ObservationStore::new(),
            rollouts: crate::rollout_store::RolloutStore::new(),
            storage: None,
            cli_recovery_live_generations: Arc::new(
                crate::cli_recovery_runtime::LiveGenerationTable::new(),
            ),
            cli_recovery_live_epoch: Arc::new(
                crate::cli_recovery_runtime::LiveContextEpochState::default(),
            ),
            cli_recovery_deferred_updates: Arc::new(
                rightkit_agents::driver_host::cli_runtime::DeferredBindingUpdates::new(),
            ),
            comparisons: Arc::new(Mutex::new(HashMap::new())),
            trajectories: crate::trajectory::TrajectoryStore::new(),
            plugin_dir: default_plugin_dir(),
            provider_extensions: Arc::new(Mutex::new(
                rightkit_llm::agent_providers::ExtensionProviderRegistry::new(),
            )),
            plugin_marketplace_path: default_plugin_marketplace_path(),
            skill_dirs: default_skills_dirs(),
            mcp_user_config_path: default_mcp_user_config_path(),
            live_teams: Arc::new(Mutex::new(live_teams)),
            team_store_path,
            message_history: Arc::new(Mutex::new(message_history)),
            message_store_path,
            permission_rule_store: Arc::new(Mutex::new(permission_rule_store)),
            permission_rule_store_path,
            permission_denials: Arc::new(Mutex::new(permission_denials)),
            permission_denial_store_path,
            denial_tracker: Arc::new(Mutex::new(rightkit_agent_policy::DenialTracker::new())),
            workspace_trust_store: Arc::new(Mutex::new(workspace_trust_store)),
            workspace_trust_store_path,
            journal_dir: crate::journal::ExecutionJournal::journals_dir(),
            runtime_middleware: Arc::new(Mutex::new(
                rightkit_agent_engine_runtime_middleware::RuntimeMiddlewareRegistry::default(),
            )),
            tool_invocation_cancellations: Arc::new(Mutex::new(Vec::new())),
            telegram_status: TelegramRemoteStatus {
                binding_store_path: crate::telegram_task::telegram_binding_store_path(),
                runtime: crate::telegram_task::new_telegram_runtime_status_store(),
                ..TelegramRemoteStatus::default()
            },
            specs: Arc::new(Mutex::new(rightkit_evidence::SpecStore::new())),
            spec_store_path: None,
            execution_backends: Arc::new(Mutex::new(
                rightkit_sandbox::execution::ExecutionBackendRegistry::default(),
            )),
            recipes: Arc::new(Mutex::new(RecipeStore::new())),
            recipe_store_path: None,
            presence: Arc::new(rightkit_daemon::loopback::Presence::new(
                rightkit_daemon::loopback::PresenceConfig::default(),
            )),
            workflows_feature: rightkit_workflow::control_plane::prelude::FeatureGate::v1_default(),
            workflow_api_state: None,
            shutdown_tx: Arc::new(tokio::sync::watch::channel(false).0),
            routing_cache: Arc::new(Mutex::new(HashMap::new())),
            session_plugin_activations: Arc::new(Mutex::new(
                rightkit_agent_plugins::SessionPluginActivations::new(),
            )),
            session_plugin_unloads: Arc::new(Mutex::new(HashMap::new())),
            mutation_journal_reports: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(feature = "benchmarks")]
            executed_bakeoff_runtime: Arc::new(crate::api_benchmarks::DaemonExecutedBakeoffRuntime),
        }
    }

    #[allow(dead_code)]
    pub fn with_license_features<I, S>(self, features: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let normalized = features
            .into_iter()
            .map(|feature| feature.as_ref().trim().to_ascii_lowercase())
            .filter(|feature| !feature.is_empty())
            .collect::<HashSet<_>>();
        *lock_recover(&self.licensed_features, "licensed_features") = normalized;
        self
    }

    /// Attach the daemon's licence gate. From then on Pro features come only from the verified
    /// licence record, read live; the fixed set is ignored.
    pub fn with_license_gate(mut self, gate: Arc<crate::license::LicenseGate>) -> Self {
        self.license_gate = Some(gate);
        self
    }

    /// The Pro features in force right now.
    pub fn current_license_features(&self) -> HashSet<String> {
        match &self.license_gate {
            Some(gate) => gate.features(),
            None => lock_recover(&self.licensed_features, "licensed_features").clone(),
        }
    }

    pub fn has_license_feature(&self, feature: &str) -> bool {
        let feature = feature.trim().to_ascii_lowercase();
        match &self.license_gate {
            Some(gate) => gate.has_feature(&feature),
            None => lock_recover(&self.licensed_features, "licensed_features").contains(&feature),
        }
    }

    pub(crate) fn dispatch_classifier(&self) -> Option<Arc<dyn DispatchClassifier>> {
        let cfg = self.engine_cfg.clone()?;
        let client = rightkit_llm::agent_providers::OpenAiCompatClient::new(
            &cfg.api_key,
            &cfg.model_id,
            &cfg.base_url,
        );
        let llm: Arc<dyn rightkit_agent_engine_core::llm::LlmClient> =
            Arc::new(crate::provider_client::ProviderLlmClient::new(
                client,
                Arc::new(rightkit_agent_coding_tools::ToolRegistry::new()),
            ));
        Some(Arc::new(LlmDispatchClassifier { llm }))
    }

    pub(crate) fn active_instinct_rules(&self) -> Vec<String> {
        lock_recover(&self.instincts, "instincts")
            .active()
            .iter()
            .map(|instinct| instinct.rule.clone())
            .collect()
    }

    pub fn with_telegram_remote_config(
        mut self,
        config: rightkit_agent_config::RemoteConfig,
    ) -> Self {
        let token_configured = config
            .telegram_token
            .as_deref()
            .map(str::trim)
            .is_some_and(|token| !token.is_empty());
        self.telegram_status = TelegramRemoteStatus {
            token_configured,
            allowed_user_count: config.telegram_allowed_users.len(),
            long_poll_requested: token_configured,
            binding_store_path: crate::telegram_task::telegram_binding_store_path(),
            runtime: self.telegram_status.runtime.clone(),
        };
        self
    }

    #[allow(dead_code)]
    pub fn with_team_store_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let teams = load_team_store(&path);
        self.team_store_path = path;
        self.live_teams = Arc::new(Mutex::new(teams));
        self
    }

    #[allow(dead_code)]
    pub fn with_message_store_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let history = load_message_history_store(&path);
        self.message_store_path = path;
        self.message_history = Arc::new(Mutex::new(history));
        self
    }

    #[allow(dead_code)]
    pub fn with_permission_rule_store_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let store = load_permission_rule_store(&path);
        self.permission_rule_store_path = path;
        self.permission_rule_store = Arc::new(Mutex::new(store));
        self
    }

    #[allow(dead_code)]
    pub fn with_permission_denial_store_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let denials = load_permission_denial_store(&path);
        self.permission_denial_store_path = path;
        self.permission_denials = Arc::new(Mutex::new(denials));
        self
    }

    #[allow(dead_code)]
    pub fn with_workspace_trust_store_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let store = load_workspace_trust_store(&path);
        self.workspace_trust_store_path = path;
        self.workspace_trust_store = Arc::new(Mutex::new(store));
        self
    }

    #[allow(dead_code)]
    pub fn with_recipe_store_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let recipes = load_recipe_store(&path);
        self.recipe_store_path = Some(path);
        self.recipes = Arc::new(Mutex::new(recipes));
        self
    }

    #[allow(dead_code)]
    pub fn with_skillopt_store_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let runs = crate::api_runtime::load_skillopt_store(&path);
        self.skillopt_store_path = Some(path);
        self.skillopt_runs = Arc::new(Mutex::new(runs));
        self
    }

    #[allow(dead_code)]
    pub fn with_spec_store_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let specs = crate::api_specs::load_spec_store(&path);
        self.spec_store_path = Some(path);
        self.specs = Arc::new(Mutex::new(specs));
        self
    }

    #[allow(dead_code)]
    pub fn with_journal_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.journal_dir = Some(path.into());
        self
    }

    pub(crate) fn record_message_history(
        &self,
        message: rightkit_agent_coding_tools::MessageDispatch,
        delivery_status: &str,
        delivered_session_ids: Vec<String>,
        error: Option<String>,
    ) -> Result<(), String> {
        let row = MessageHistoryRow {
            id: format!("msg-{}", now_millis()),
            created_at: now_iso(),
            from_session_id: message.from_session_id,
            target_session_id: message.target_session_id,
            channel: message.channel,
            message: message.message,
            delivery_status: delivery_status.to_string(),
            delivered_session_ids,
            error,
        };
        let snapshot = {
            let mut history = lock_recover(&self.message_history, "message_history");
            history.push(row);
            if history.len() > 500 {
                let keep_from = history.len().saturating_sub(500);
                history.drain(0..keep_from);
            }
            history.clone()
        };
        write_message_history_store(&self.message_store_path, &snapshot)
    }

    pub(crate) fn seed_persistent_permission_rules(
        &self,
        rules: &mut rightkit_agent_policy::RuleStore,
        workspace_root: &str,
    ) {
        let store = lock_recover(&self.permission_rule_store, "permission_rule_store").clone();
        let empty = Vec::new();
        let project_rules = store.projects.get(workspace_root).unwrap_or(&empty);
        for (source, persisted) in [
            (rightkit_agent_policy::RuleSource::User, &store.user),
            (rightkit_agent_policy::RuleSource::Project, project_rules),
        ] {
            let parsed: Option<Vec<rightkit_agent_policy::Rule>> = persisted
                .iter()
                .map(|rule| {
                    parse_permission_decision(&rule.effect).map(|effect| {
                        rightkit_agent_policy::Rule::new(rule.pattern.clone(), effect, source)
                    })
                })
                .collect();
            let installed = match parsed {
                Some(batch) => rules.try_add_all(batch).map_err(|e| e.to_string()),
                None => Err("invalid permission rule effect".to_string()),
            };
            if let Err(error) = installed {
                // Never install a set containing a malformed rule, and never let
                // it authorize anything: keep only the well-formed deny/ask
                // rules so a bad entry cannot drop a valid deny.
                tracing::warn!(%error, ?source, "persisted permission rules invalid; keeping valid deny/ask rules only");
                for rule in persisted {
                    if let Some(effect) = parse_permission_decision(&rule.effect) {
                        let rule =
                            rightkit_agent_policy::Rule::new(rule.pattern.clone(), effect, source);
                        if effect != rightkit_agent_policy::PermissionDecision::Allow
                            && rule.validate().is_ok()
                        {
                            rules.add(rule);
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn workspace_trust_for(
        &self,
        workspace_root: &str,
    ) -> rightkit_agent_policy::WorkspaceTrust {
        let key = workspace_trust_key(workspace_root);
        let store = lock_recover(&self.workspace_trust_store, "workspace_trust_store");
        store
            .projects
            .get(&key)
            .and_then(|record| parse_workspace_trust_label(&record.trust))
            .unwrap_or(rightkit_agent_policy::WorkspaceTrust::Untrusted)
    }
}

pub(crate) struct StoredCliLaunchContexts {
    storage: crate::api::SharedStorage,
    contexts: std::sync::RwLock<HashMap<String, ScopedCliLaunchContext>>,
}

#[derive(Clone)]
struct ScopedCliLaunchContext {
    session_id: String,
    agent_id: String,
    context: rightkit_agents::driver_host::cli_runtime::CliExecutorLaunchContext,
}

impl StoredCliLaunchContexts {
    pub(crate) fn new(storage: crate::api::SharedStorage) -> Self {
        Self {
            storage,
            contexts: std::sync::RwLock::new(HashMap::new()),
        }
    }

    pub(crate) fn insert(
        &self,
        binding_id: String,
        session_id: String,
        agent_id: String,
        context: rightkit_agents::driver_host::cli_runtime::CliExecutorLaunchContext,
    ) -> Result<(), rightkit_agents::driver_host::cli_runtime::AgentRuntimeError> {
        self.contexts
            .write()
            .map_err(|_| {
                rightkit_agents::driver_host::cli_runtime::AgentRuntimeError::new(
                    "launch_context_lock",
                    "launch context map is poisoned",
                )
            })?
            .insert(
                binding_id,
                ScopedCliLaunchContext {
                    session_id,
                    agent_id,
                    context,
                },
            );
        Ok(())
    }

    pub(crate) fn remove(&self, binding_id: &str) {
        if let Ok(mut contexts) = self.contexts.write() {
            contexts.remove(binding_id);
        }
    }

    pub(crate) fn gateway_credentials(&self, binding_id: &str) -> Option<(String, String)> {
        let contexts = self.contexts.read().ok()?;
        let context = &contexts.get(binding_id)?.context;
        let capability = context
            .mcp_server
            .env
            .iter()
            .find(|(name, _)| name == "RIGHTKIT_GATEWAY_TOKEN")?
            .1
            .clone();
        let instance = context
            .mcp_server
            .env
            .iter()
            .find(|(name, _)| name == "RIGHTKIT_DRIVER_INSTANCE_SECRET")?
            .1
            .clone();
        Some((capability, instance))
    }
}

impl rightkit_agents::driver_host::cli_runtime::CliExecutorLaunchContextProvider
    for StoredCliLaunchContexts
{
    fn resolve(
        &self,
        provider_id: &rightkit_agents::driver_host::cli_runtime::CliProviderId,
        profile_id: &rightkit_agents::driver_host::cli_runtime::CliAccountProfileId,
        session_id: &str,
        agent_id: &str,
        binding_id: &str,
    ) -> Result<
        rightkit_agents::driver_host::cli_runtime::CliExecutorLaunchContext,
        rightkit_agents::driver_host::cli_runtime::AgentRuntimeError,
    > {
        let scoped = self
            .contexts
            .read()
            .map_err(|_| {
                rightkit_agents::driver_host::cli_runtime::AgentRuntimeError::new(
                    "launch_context_lock",
                    "launch context map is poisoned",
                )
            })?
            .get(binding_id)
            .cloned()
            .ok_or_else(|| {
                rightkit_agents::driver_host::cli_runtime::AgentRuntimeError::new(
                    "launch_context_missing",
                    "binding has no scoped launch context",
                )
            })?;
        if scoped.session_id != session_id || scoped.agent_id != agent_id {
            return Err(
                rightkit_agents::driver_host::cli_runtime::AgentRuntimeError::new(
                    "launch_context_identity_mismatch",
                    "launch context does not belong to requested session and agent",
                ),
            );
        }
        let store = self.storage.try_lock().map_err(|_| {
            rightkit_agents::driver_host::cli_runtime::AgentRuntimeError::new(
                "profile_store_busy",
                "profile storage is busy",
            )
        })?;
        let row = store
            .get_cli_account_profile(profile_id.as_str())
            .map_err(|error| {
                rightkit_agents::driver_host::cli_runtime::AgentRuntimeError::new(
                    "profile_store",
                    error.to_string(),
                )
            })?
            .ok_or_else(|| {
                rightkit_agents::driver_host::cli_runtime::AgentRuntimeError::new(
                    "profile_not_found",
                    "authenticated CLI profile is not persisted",
                )
            })?;
        if row.provider_id != provider_id.as_str()
            || row.auth_mode != "native_cli_account"
            || !row.signed_in
        {
            return Err(
                rightkit_agents::driver_host::cli_runtime::AgentRuntimeError::new(
                    "profile_not_eligible",
                    "profile is not a signed-in native CLI account",
                ),
            );
        }
        if scoped.context.profile.id.as_str() != profile_id.as_str()
            || scoped.context.profile.provider_id.as_str() != provider_id.as_str()
            || scoped.context.profile.managed_home.as_os_str()
                != std::ffi::OsStr::new(&row.managed_home)
        {
            return Err(
                rightkit_agents::driver_host::cli_runtime::AgentRuntimeError::new(
                    "launch_context_profile_mismatch",
                    "scoped launch profile no longer matches persisted profile",
                ),
            );
        }
        Ok(scoped.context)
    }
}
