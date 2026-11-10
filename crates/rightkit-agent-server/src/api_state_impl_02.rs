impl DaemonState {
    pub(crate) fn record_permission_denial_from_tool_result(
        &self,
        session_id: &str,
        created_at: &str,
        result: &TranscriptMessage,
    ) -> Result<(), String> {
        let TranscriptMessage::Tool {
            tool_call_id,
            summary,
            is_error,
            ..
        } = result
        else {
            return Ok(());
        };
        if !*is_error || !summary.to_lowercase().contains("denied") {
            return Ok(());
        }
        let correlation =
            rightkit_agent_policy::denial_tracking::correlation_token(session_id, tool_call_id);
        let row = PermissionDenialRow {
            id: format!("deny-{}", now_millis()),
            created_at: created_at.to_string(),
            session_id: session_id.to_string(),
            tool_call_id: tool_call_id.clone(),
            correlation,
            reason: summary.clone(),
        };
        let snapshot = {
            let mut denials = lock_recover(&self.permission_denials, "permission_denials");
            // A tool result can be replayed by a reconnecting client or reach
            // this projection after an earlier denial gate recorded it. Keep
            // one durable API row for each identified call, matching the
            // engine tracker’s S:B5 correlation invariant.
            if !row.correlation.is_empty()
                && denials
                    .iter()
                    .any(|existing| existing.correlation == row.correlation)
            {
                return Ok(());
            }
            denials.push(row);
            if denials.len() > 500 {
                let keep_from = denials.len().saturating_sub(500);
                denials.drain(0..keep_from);
            }
            denials.clone()
        };
        write_permission_denial_store(&self.permission_denial_store_path, &snapshot)
    }

    #[allow(dead_code)]
    pub fn with_plugin_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.plugin_dir = dir.into();
        self
    }

    /// Register discovered provider extensions into live daemon state using
    /// explicit host policy. Invalid or colliding manifests fail atomically;
    /// discovery never grants core trust by itself.
    pub fn register_provider_extensions(&self) -> Result<usize, String> {
        let mut providers = self
            .provider_extensions
            .lock()
            .map_err(|_| "provider extension registry mutex poisoned".to_string())?;
        let mut routes = rightkit_llm::agent_providers::registry::ModelRegistry::new();
        let mut metadata = rightkit_llm::agent_providers::registry_meta::RegistryMeta::new();
        crate::provider_client::register_discovered_provider_extensions(
            &self.plugin_dir,
            &mut providers,
            &mut routes,
            &mut metadata,
        )
    }

    #[allow(dead_code)]
    pub fn with_plugin_marketplace_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.plugin_marketplace_path = path.into();
        self
    }

    #[allow(dead_code)]
    pub fn with_skills_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.skill_dirs = vec![dir.into()];
        self
    }

    #[allow(dead_code)]
    pub fn with_mcp_user_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.mcp_user_config_path = path.into();
        self
    }

    /// Attach the trajectory store (persistent), replacing the default.
    pub fn with_trajectories(mut self, t: crate::trajectory::TrajectoryStore) -> Self {
        self.trajectories = t;
        self
    }

    /// Attach the session/event store so deletes can purge persisted rows.
    pub fn with_storage<S>(mut self, storage: S) -> Self
    where
        S: Into<SharedStorage>,
    {
        let storage = storage.into();
        // `with_storage` is also called while a Tokio runtime is active
        // (including current-thread runtimes); never invoke
        // `SharedStorage::blocking_lock` during construction.
        if let Ok(store) = storage.try_lock() {
            if let Ok(Some(snapshot)) = store.load_observation_snapshot() {
                if let Ok(restored) =
                    crate::daemon::external_observations::ExternalObservationLoop::restore(
                        &snapshot,
                    )
                {
                    self.external_observations = Arc::new(Mutex::new(restored));
                }
            }
        }
        self.cli_registry
            .set_attestation_store(Some(Arc::new(StorageAttestationStore {
                storage: storage.clone(),
            })));
        let launch_contexts = Arc::new(crate::api::api_state::StoredCliLaunchContexts::new(
            storage.clone(),
        ));
        let cli_bridge = Arc::new(crate::driver_host_runtime::DaemonCliDriverBridge::new(
            self.driver_host.clone(),
            self.cli_registry.clone(),
        ));
        self.cli_executor = Some(Arc::new(
            rightkit_agents::driver_host::cli_runtime::AuthenticatedCliExecutor::new(
                self.cli_registry.clone(),
                cli_bridge.clone(),
            )
            .with_launch_context_provider(launch_contexts.clone())
            .with_resume_guard(Arc::new(
                crate::cli_recovery_runtime::CliRecoveryGuard::new(
                    storage.clone(),
                    self.cli_recovery_live_generations.clone(),
                    self.cli_recovery_live_epoch.clone(),
                ),
            )),
        ));
        self.cli_bridge = Some(cli_bridge);
        self.cli_launch_contexts = Some(launch_contexts);
        self.storage = Some(storage);
        self
    }

    /// PR 13: drain every deferred active-binding update at the turn
    /// boundary. Returns the [`DeferredApplyReport`] so the caller
    /// (engine task, journal sweep, or operator action) can observe
    /// how many updates landed and how many were dropped. Safe to
    /// call when no storage is attached: the helper short-circuits
    /// and the queue is left intact for the next call.
    pub async fn drain_cli_recovery_updates(
        &self,
    ) -> rightkit_agents::driver_host::cli_runtime::DeferredApplyReport {
        let Some(storage) = self.storage.clone() else {
            return rightkit_agents::driver_host::cli_runtime::DeferredApplyReport::default();
        };
        let drained = self.cli_recovery_deferred_updates.drain();
        crate::cli_recovery_runtime::apply_deferred_binding_updates(&storage, drained).await
    }

    /// Override the resolved workflow feature gate. The merged
    /// `HostConfig.features.workflows_enabled` is the canonical source
    /// for this value; tests may flip the gate at will.
    #[allow(dead_code)]
    pub fn with_workflows_feature(
        mut self,
        feature: rightkit_workflow::control_plane::prelude::FeatureGate,
    ) -> Self {
        self.workflows_feature = feature;
        self
    }

    /// Attach the per-router [`WorkflowApiState`] (WP-09 / ADR-0021). Built
    /// once at daemon startup by `main_runtime::serve` after storage is
    /// open; the router mount in [`crate::api_router::build_router`]
    /// consults the feature gate to decide whether to merge the surface.
    #[allow(dead_code)]
    pub fn with_workflow_api_state(
        mut self,
        state: Arc<crate::api_workflows::WorkflowApiState>,
    ) -> Self {
        self.workflow_api_state = Some(state);
        self
    }

    /// Attach a (possibly persistent) observation store, replacing the default.
    pub fn with_observations(mut self, obs: crate::observation::ObservationStore) -> Self {
        self.observations = obs;
        self
    }

    /// Attach durable runtime middleware span/observation storage.
    pub fn with_runtime_middleware_store_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.runtime_middleware = Arc::new(Mutex::new(
            rightkit_agent_engine_runtime_middleware::RuntimeMiddlewareRegistry::with_store_path(
                path,
            ),
        ));
        self
    }

    /// Attach causal provenance backed by the selected Hub memory binding.
    pub(crate) fn with_memory_provenance(
        mut self,
        provenance: rightkit_memory::memory_provenance::MemoryProvenanceStore,
    ) -> Self {
        self.memory_provenance = provenance;
        self
    }

    /// Inject the already-selected sticky backend used by production startup.
    pub(crate) fn with_memory_backend(
        mut self,
        backend: Arc<dyn rightkit_memory::backend::MemoryBackend>,
    ) -> Self {
        self.memory_backend = backend;
        self
    }

    /// Attach the Pulse Membrane CLI backend selected at startup.
    pub(crate) fn with_membrane(
        mut self,
        backend: Arc<dyn rightkit_memory::membrane::MembraneBackend + Send + Sync>,
    ) -> Self {
        self.membrane = backend;
        self
    }

    pub(crate) fn with_context_mode(mut self, mode: crate::cli::ContextMode) -> Self {
        self.context_mode = mode;
        self
    }

    pub(crate) fn with_context_degraded(mut self, reason: impl Into<String>) -> Self {
        self.context_degraded = Some(reason.into());
        self
    }

    /// Why context is off (Membrane unavailable), when turns run degraded.
    pub(crate) fn context_degraded_reason(&self) -> Option<&str> {
        self.context_degraded.as_deref()
    }

    pub(crate) fn uses_direct_context(&self) -> bool {
        // The removed direct mode is a compatibility no-op. Only an actual
        // unavailable binding selects the no-injection degraded path.
        self.context_degraded.is_some()
    }

    pub(crate) fn with_mutation_diagnostics(
        mut self,
        diagnostics: Arc<dyn rightkit_agent_engine_core::MutationDiagnostics>,
    ) -> Self {
        self.mutation_diagnostics = Some(diagnostics);
        self
    }

    /// Attach a (possibly persistent) audit log, replacing the default.
    pub fn with_audit(mut self, audit: crate::audit::AuditLog) -> Self {
        self.audit = audit;
        self
    }

    /// Attach a (possibly persistent) learning harness, replacing the default.
    pub fn with_learning(mut self, learning: crate::learning::LearningHarness) -> Self {
        self.learning = learning;
        self
    }

    /// Record the actual bound port and database path so `/health` can report
    /// the truth instead of a hardcoded guess.
    pub fn with_runtime_info(mut self, port: u16, database_path: impl Into<String>) -> Self {
        self.bound_port = port;
        self.database_path = database_path.into();
        self
    }

    pub fn with_engine(mut self, cfg: EngineConfig) -> Self {
        self.engine_cfg = Some(cfg);
        self
    }

    /// Attach the Lane-C transcript exporter built on the same authenticated
    /// native binding (`None` when the binding carried no service claim).
    pub fn with_transcript_exporter(
        mut self,
        exporter: Option<Arc<crate::native_transcript_exporter::NativeTranscriptExporter>>,
    ) -> Self {
        self.transcript_exporter = exporter;
        self
    }

    #[allow(clippy::too_many_arguments)]
    fn configured_provider_route(
        &self,
        config: &rightkit_app_settings::agent_config::HostConfig,
        provider_id: &str,
        model: &str,
        fallback: &EngineConfig,
        effort: Option<String>,
        thinking_budget_tokens: Option<u32>,
        disable_thinking: bool,
    ) -> Option<crate::provider_client::ProviderRoute> {
        let route = config.provider_routes.get(provider_id).or_else(|| {
            config
                .provider_routes
                .values()
                .find(|route| route.model.trim().eq_ignore_ascii_case(model.trim()))
        })?;
        let model_id = rightkit_llm::agent_providers::provider_model_id(provider_id, model.trim());
        let api_key = configured_provider_api_key(provider_id, route, fallback);
        Some(crate::provider_client::ProviderRoute {
            api_key,
            model_id,
            base_url: route.base_url.trim().trim_end_matches('/').to_string(),
            effort,
            thinking_budget_tokens,
            disable_thinking,
            fallback_models: fallback.fallback_models.clone(),
            approved_private: route.approved_private,
        })
    }

    /// Resolve only the named provider route. Explicit per-call provider
    /// selections must not fall through to another provider whose model name
    /// happens to match.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn configured_provider_route_exact(
        &self,
        config: &rightkit_app_settings::agent_config::HostConfig,
        provider_id: &str,
        model: &str,
        _fallback: &EngineConfig,
        effort: Option<String>,
        thinking_budget_tokens: Option<u32>,
        disable_thinking: bool,
    ) -> Option<crate::provider_client::ProviderRoute> {
        let route = config.provider_routes.get(provider_id)?;
        let model_id = rightkit_llm::agent_providers::provider_model_id(provider_id, model.trim());
        let api_key = configured_provider_api_key_without_fallback(provider_id, route);
        Some(crate::provider_client::ProviderRoute {
            api_key,
            model_id,
            base_url: route.base_url.trim().trim_end_matches('/').to_string(),
            effort,
            thinking_budget_tokens,
            disable_thinking,
            // Explicit provider/model selection cannot inherit another route's fallbacks.
            fallback_models: Vec::new(),
            approved_private: route.approved_private,
        })
    }

    /// Route for an explicitly selected provider: the configured route of that
    /// exact id, else the native catalog route. `None` when the provider is
    /// neither configured nor in the catalog, so callers can reject it instead
    /// of silently falling back to the default route.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn explicit_provider_route(
        &self,
        config: &rightkit_app_settings::agent_config::HostConfig,
        provider_id: &str,
        model: &str,
        fallback: &EngineConfig,
        effort: Option<String>,
        thinking_budget_tokens: Option<u32>,
        disable_thinking: bool,
    ) -> Option<crate::provider_client::ProviderRoute> {
        if let Some(route) = self.configured_provider_route_exact(
            config,
            provider_id,
            model,
            fallback,
            effort.clone(),
            thinking_budget_tokens,
            disable_thinking,
        ) {
            return Some(route);
        }
        rightkit_llm::agent_providers::find_provider(provider_id)?;
        Some(native_provider_fallback_route(
            provider_id,
            model,
            effort,
            thinking_budget_tokens,
            disable_thinking,
            Vec::new(),
        ))
    }

    pub(crate) fn provider_route_for_model(
        &self,
        workspace_config: &rightkit_app_settings::agent_config::HostConfig,
        model: &str,
        fallback: &EngineConfig,
        effort: Option<String>,
        thinking_budget_tokens: Option<u32>,
        disable_thinking: bool,
    ) -> crate::provider_client::ProviderRoute {
        let trimmed = model.trim();
        if let Some(descriptor) =
            self.agent_descriptor_with_workspace_config(workspace_config, trimmed)
        {
            if let Some(route) = self.configured_provider_route(
                workspace_config,
                &descriptor.provider_id,
                &descriptor.model_id,
                fallback,
                effort.clone(),
                thinking_budget_tokens,
                disable_thinking,
            ) {
                return route;
            }
            let base_url = rightkit_llm::agent_providers::find_provider(&descriptor.provider_id)
                .map(|provider| provider.base_url.to_string())
                .unwrap_or_else(|| fallback.base_url.clone());
            let api_key = rightkit_llm::agent_providers::load_api_key(&descriptor.provider_id)
                .unwrap_or_else(|| fallback.api_key.clone());
            return crate::provider_client::ProviderRoute {
                api_key,
                model_id: rightkit_llm::agent_providers::provider_model_id(
                    &descriptor.provider_id,
                    &descriptor.model_id,
                ),
                base_url,
                effort,
                thinking_budget_tokens,
                disable_thinking,
                fallback_models: fallback.fallback_models.clone(),
                approved_private: false,
            };
        }
        if let Some(route) = self.configured_provider_route(
            workspace_config,
            trimmed,
            trimmed,
            fallback,
            effort.clone(),
            thinking_budget_tokens,
            disable_thinking,
        ) {
            return route;
        }

        if crate::model_policy::provider_id_for_model_slug(trimmed) == "minimax" {
            let provider = rightkit_llm::agent_providers::find_provider("minimax")
                .expect("minimax provider is part of the built-in catalog");
            return crate::provider_client::ProviderRoute {
                api_key: rightkit_llm::agent_providers::load_api_key("minimax")
                    .unwrap_or_else(|| fallback.api_key.clone()),
                model_id: rightkit_llm::agent_providers::provider_model_id("minimax", trimmed),
                base_url: provider.base_url.to_string(),
                effort,
                thinking_budget_tokens,
                disable_thinking,
                fallback_models: fallback.fallback_models.clone(),
                approved_private: false,
            };
        }

        crate::provider_client::ProviderRoute {
            api_key: fallback.api_key.clone(),
            model_id: trimmed.to_string(),
            base_url: fallback.base_url.clone(),
            effort,
            thinking_budget_tokens,
            disable_thinking,
            fallback_models: fallback.fallback_models.clone(),
            approved_private: false,
        }
    }

    /// Provider identity that `provider_route_for_model` would resolve for
    /// `model`, or `None` when the consumed route carries no durable provider
    /// authority: a role/agent descriptor match (resume must re-resolve the
    /// descriptor because `model` is a role name, not a slug) or the generic
    /// engine-default fallback (the configured default route is the authority,
    /// so persisting a provider id would override it on resume). Used to
    /// persist `providerId` alongside model-only route updates.
    pub(crate) fn resolved_provider_id_for_model(
        &self,
        workspace_config: &rightkit_app_settings::agent_config::HostConfig,
        model: &str,
    ) -> Option<String> {
        let trimmed = model.trim();
        if trimmed.is_empty() {
            return None;
        }
        if self
            .agent_descriptor_with_workspace_config(workspace_config, trimmed)
            .is_some()
        {
            return None;
        }
        if workspace_config.provider_routes.contains_key(trimmed) {
            return Some(trimmed.to_string());
        }
        if let Some((provider_id, _)) = workspace_config
            .provider_routes
            .iter()
            .find(|(_, route)| route.model.trim().eq_ignore_ascii_case(trimmed))
        {
            return Some(provider_id.clone());
        }
        if crate::model_policy::provider_id_for_model_slug(trimmed) == "minimax" {
            return Some("minimax".to_string());
        }
        None
    }

    pub(crate) fn agent_registry_for_workspace(&self, workspace_root: &str) -> AgentRegistry {
        let workspace_config = load_workspace_config_with_env(FsPath::new(workspace_root.trim()));
        self.agent_registry_with_workspace_config(&workspace_config)
    }

    pub(crate) fn agent_registry_with_workspace_config(
        &self,
        workspace_config: &rightkit_app_settings::agent_config::HostConfig,
    ) -> AgentRegistry {
        let mut registry = AgentRegistry::new();
        for descriptor in self.agent_registry.all() {
            registry.register(descriptor.clone());
        }

        // Provider is passed through verbatim. An empty override must reach
        // `apply_config_overrides` empty so it can derive the provider from
        // the override's model; pre-filling a default provider id here would
        // defeat that derivation and force every overridden agent onto a
        // provider the user never configured.
        let agent_overrides: std::collections::HashMap<String, (String, String)> = workspace_config
            .orchestration
            .agents
            .iter()
            .map(|(name, ov)| (name.clone(), (ov.model_id.clone(), ov.provider_id.clone())))
            .collect();
        if !agent_overrides.is_empty() {
            registry.apply_config_overrides(&agent_overrides);
        }
        let split_available =
            crate::model_policy::split_entitled(|feature| self.has_license_feature(feature));
        crate::model_policy::apply_effective_role_model_policy(
            &mut registry,
            workspace_config,
            split_available,
            self.engine_cfg
                .as_ref()
                .map(|config| config.model_id.as_str()),
        );
        registry
    }

    pub(crate) fn agent_descriptor_with_workspace_config(
        &self,
        workspace_config: &rightkit_app_settings::agent_config::HostConfig,
        role: &str,
    ) -> Option<rightkit_agent_orchestration::coding::registry::AgentDescriptor> {
        self.agent_registry_with_workspace_config(workspace_config)
            .get(role)
            .cloned()
    }

    pub fn create_session(
        &self,
        id: &str,
        role: Option<&str>,
        workspace_root: &str,
        title: &str,
        parent_session_id: Option<&str>,
    ) -> Arc<LiveSession> {
        self.create_session_with_options(
            id,
            role,
            workspace_root,
            title,
            parent_session_id,
            SessionRuntimeOptions::default(),
        )
    }
}

struct StorageAttestationStore {
    storage: SharedStorage,
}

impl rightkit_agents::driver_host::cli_runtime::AttestationStore for StorageAttestationStore {
    fn latest_attestation(
        &self,
        provider_id: &rightkit_agents::driver_host::cli_runtime::CliProviderId,
    ) -> Result<
        Option<rightkit_agents::driver_host::cli_runtime::LatestAttestation>,
        rightkit_agents::driver_host::cli_runtime::AttestationStoreError,
    > {
        let store = self.storage.try_lock().map_err(|_| {
            rightkit_agents::driver_host::cli_runtime::AttestationStoreError::Backend(
                "attestation storage busy".into(),
            )
        })?;
        let Some(row) = store
            .latest_cli_harness_attestation(provider_id.as_str())
            .map_err(|error| {
                rightkit_agents::driver_host::cli_runtime::AttestationStoreError::Backend(
                    error.to_string(),
                )
            })?
        else {
            return Ok(None);
        };
        let verified_at_unix_secs = rfc3339_unix_seconds(&row.verified_at).ok_or_else(|| {
            rightkit_agents::driver_host::cli_runtime::AttestationStoreError::Backend(
                "invalid attestation verified_at".into(),
            )
        })?;
        let isolation_mode = match row.isolation_mode.as_str() {
            // `HarnessIsolationMode::label()` writes `exclusive_rightkit_gateway`; rows stored
            // before the rename carry the legacy `exclusive_coderight_gateway`.
            "exclusive_rightkit_gateway" | "exclusive_coderight_gateway" => rightkit_agents::driver_host::cli_runtime::HarnessIsolationMode::ExclusiveHostGateway,
            "native_read_only_compatibility" => rightkit_agents::driver_host::cli_runtime::HarnessIsolationMode::NativeReadOnlyCompatibility,
            _ => return Err(rightkit_agents::driver_host::cli_runtime::AttestationStoreError::Backend(
                "invalid attestation isolation_mode".into(),
            )),
        };
        let inputs = rightkit_agents::driver_host::cli_runtime::AttestationInputs {
            provider_id: provider_id.clone(),
            executable_sha256: row.executable_sha256,
            binary_version: row.binary_version,
            protocol_version: row.protocol_version,
            profile_hash: row.profile_hash,
            policy_hash: row.policy_hash,
            tool_schema_hash: row.tool_schema_hash,
            operating_system: row.operating_system,
            architecture: row.architecture,
            isolation_mode,
        };
        Ok(Some(
            rightkit_agents::driver_host::cli_runtime::LatestAttestation {
                attestation_id: row.id,
                inputs,
                failed_probes: if row.result == "passed" {
                    Vec::new()
                } else {
                    vec!["qualification_failed".into()]
                },
                verified_at_unix_secs,
            },
        ))
    }
}

fn rfc3339_unix_seconds(value: &str) -> Option<u64> {
    // Fixed shape `YYYY-MM-DDTHH:MM:SSZ` only (no fraction, no offset, no leap second).
    let bytes = value.as_bytes();
    if bytes.len() != 20 || bytes[10] != b'T' || bytes[19] != b'Z' {
        return None;
    }
    let parsed = rightkit_time::DateTime::parse_rfc3339(value)?;
    if parsed.second > 59 {
        return None;
    }
    u64::try_from(parsed.unix_seconds()).ok()
}
