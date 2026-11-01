impl DaemonState {
    pub(crate) fn create_scheduled_session_with_toolset_policy(
        &self,
        id: &str,
        workspace_root: &str,
        title: &str,
        toolset_id: Option<&str>,
        active_skills: Vec<String>,
        execution_backend: Option<String>,
    ) -> Result<Arc<LiveSession>, StatusCode> {
        let cleaned_toolset = toolset_id.map(str::trim).filter(|value| !value.is_empty());
        let execution_backend = {
            let registry = lock_recover(&self.execution_backends, "execution_backends");
            validate_execution_backend(&registry, execution_backend.as_deref())?
        };
        let mut options = if let Some(toolset_id) = cleaned_toolset {
            let workspace_config =
                load_workspace_config_with_env(FsPath::new(workspace_root.trim()));
            let profile = resolve_toolset_profile_with_config(toolset_id, &workspace_config)?;
            let (allowed_tools, disallowed_tools) =
                apply_toolset_profile(Some(&profile), Vec::new(), Vec::new());
            SessionRuntimeOptions {
                allowed_tools,
                disallowed_tools,
                toolset: Some(profile.id),
                ..SessionRuntimeOptions::default()
            }
        } else {
            SessionRuntimeOptions::default()
        };

        if options.toolset.as_deref() != Some(SCHEDULE_ADMIN_TOOLSET)
            && !options
                .disallowed_tools
                .iter()
                .any(|tool| tool == "schedule_cron")
        {
            options.disallowed_tools.push("schedule_cron".to_string());
        }
        options.active_skills = active_skills;
        options.execution_backend = execution_backend;
        options.mcp_channel = "schedule".to_string();

        Ok(self.create_session_with_options(id, None, workspace_root, title, None, options))
    }

    pub(crate) fn create_recipe_session_with_toolset_policy(
        &self,
        id: &str,
        workspace_root: &str,
        title: &str,
        toolset_id: Option<&str>,
        active_skills: Vec<String>,
        execution_backend: Option<String>,
    ) -> Result<Arc<LiveSession>, StatusCode> {
        let cleaned_toolset = toolset_id.map(str::trim).filter(|value| !value.is_empty());
        let execution_backend = {
            let registry = lock_recover(&self.execution_backends, "execution_backends");
            validate_execution_backend(&registry, execution_backend.as_deref())?
        };
        let mut options = if let Some(toolset_id) = cleaned_toolset {
            let workspace_config =
                load_workspace_config_with_env(FsPath::new(workspace_root.trim()));
            let profile = resolve_toolset_profile_with_config(toolset_id, &workspace_config)?;
            let (allowed_tools, disallowed_tools) =
                apply_toolset_profile(Some(&profile), Vec::new(), Vec::new());
            SessionRuntimeOptions {
                allowed_tools,
                disallowed_tools,
                toolset: Some(profile.id),
                ..SessionRuntimeOptions::default()
            }
        } else {
            SessionRuntimeOptions::default()
        };
        options.active_skills = active_skills;
        options.execution_backend = execution_backend;
        options.mcp_channel = "recipe".to_string();

        Ok(self.create_session_with_options(id, None, workspace_root, title, None, options))
    }

    fn resolve_session_execution_network_profile(
        &self,
        workspace_root: &str,
        execution_backend: Option<&str>,
        explicit_profile: Option<&str>,
    ) -> Option<String> {
        if let Some(profile) = explicit_profile
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(profile.to_string());
        }
        let backend_id = execution_backend
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        let registry = lock_recover(&self.execution_backends, "execution_backends").clone();
        match resolve_execution_backend_network_profile(
            FsPath::new(workspace_root),
            &registry,
            Some(backend_id),
        ) {
            Ok(profile) => profile,
            Err(err) => {
                tracing::warn!(workspace_root = %workspace_root, backend_id = %backend_id, error = %err, "execution backend profile lookup failed");
                registry.get(backend_id).and_then(|backend| {
                    backend.network_profile.as_ref().and_then(|profile| {
                        let trimmed = profile.trim();
                        (!trimmed.is_empty()).then(|| trimmed.to_string())
                    })
                })
            }
        }
    }

    fn resolve_session_execution_resource_limits(
        &self,
        workspace_root: &str,
        execution_backend: Option<&str>,
    ) -> Option<rightkit_agent_tools::coding_tools::ExecutionResourceLimits> {
        let backend_id = execution_backend
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        let registry = lock_recover(&self.execution_backends, "execution_backends").clone();
        match resolve_execution_backend_resource_limits(
            FsPath::new(workspace_root),
            &registry,
            Some(backend_id),
        ) {
            Ok(limits) => limits,
            Err(err) => {
                tracing::warn!(workspace_root = %workspace_root, backend_id = %backend_id, error = %err, "execution backend resource-limit lookup failed");
                registry.get(backend_id).map(|backend| {
                    let limits = &backend.resource_limits;
                    rightkit_agent_tools::coding_tools::ExecutionResourceLimits {
                        cpu: limits.cpu,
                        memory_mb: limits.memory_mb,
                        disk_mb: limits.disk_mb,
                    }
                })
            }
        }
    }

    pub(crate) fn create_session_with_options(
        &self,
        id: &str,
        role: Option<&str>,
        workspace_root: &str,
        title: &str,
        parent_session_id: Option<&str>,
        options: SessionRuntimeOptions,
    ) -> Arc<LiveSession> {
        self.create_session_seeded_with_options(
            id,
            role,
            workspace_root,
            title,
            parent_session_id,
            None,
            None,
            0,
            0,
            options,
        )
    }
}
