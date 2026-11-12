use super::lazy_loading::is_core_tool;
use super::native_types::{
    CapabilityDescriptor, ModelToolInterface, RegistrationScopeId, ScopedRegistrationError,
    ToolCapability, ToolRegistry,
};
use crate::tool_trait::Tool;
use std::path::PathBuf;

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            scope_owners: std::collections::BTreeMap::new(),
            lazy_loading: false,
            model_interface: ModelToolInterface::default(),
        }
    }

    /// Register a tool owned by a disposable scope (EXT-001).
    ///
    /// Re-registering the same tool name under the same scope replaces it, so
    /// a reconnect produces exactly one live registration rather than a
    /// duplicate. Registering a name currently owned by a *different* scope is
    /// refused: ownership is unambiguous, and silently stealing a name would
    /// make `unload_scope` unable to guarantee full removal.
    pub fn register_scoped(
        &mut self,
        scope: &RegistrationScopeId,
        tool: Box<dyn Tool>,
    ) -> Result<(), ScopedRegistrationError> {
        let name = tool.name().to_string();
        match self.scope_owners.get(&name) {
            Some(existing) if existing != scope => {
                return Err(ScopedRegistrationError::OwnedByAnotherScope {
                    tool: name,
                    owner: existing.clone(),
                })
            }
            None if name == "exec" || self.tools.iter().any(|existing| existing.name() == name) => {
                return Err(ScopedRegistrationError::ShadowsBuiltin { tool: name })
            }
            _ => {}
        }
        self.tools.retain(|existing| existing.name() != name);
        self.tools.push(tool);
        self.scope_owners.insert(name, scope.clone());
        Ok(())
    }

    /// Drop every registration owned by `scope`. Returns how many were removed.
    ///
    /// This is the only supported unload path; it cannot leave a partially
    /// unloaded scope behind, and it never touches built-ins or other scopes.
    pub fn unload_scope(&mut self, scope: &RegistrationScopeId) -> usize {
        let owned = self
            .scope_owners
            .iter()
            .filter(|(_, owner)| *owner == scope)
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        for name in &owned {
            self.tools.retain(|tool| tool.name() != name.as_str());
            self.scope_owners.remove(name);
        }
        owned.len()
    }

    /// Tool names currently owned by `scope`, sorted.
    pub fn scope_registrations(&self, scope: &RegistrationScopeId) -> Vec<String> {
        self.scope_owners
            .iter()
            .filter(|(_, owner)| *owner == scope)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// The scope owning `tool`, if it is a dynamic registration.
    pub fn owning_scope(&self, tool: &str) -> Option<&RegistrationScopeId> {
        self.scope_owners.get(tool)
    }

    /// All live scopes, sorted and deduplicated.
    pub fn live_scopes(&self) -> Vec<RegistrationScopeId> {
        let mut scopes = self.scope_owners.values().cloned().collect::<Vec<_>>();
        scopes.sort();
        scopes.dedup();
        scopes
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn replace(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_string();
        self.tools.retain(|existing| existing.name() != name);
        self.scope_owners.remove(&name);
        self.tools.push(tool);
    }

    pub fn remove(&mut self, name: &str) {
        self.tools.retain(|tool| tool.name() != name);
        self.scope_owners.remove(name);
        // Refresh the search catalog only while it is still registered —
        // rebuilding an absent tool_search would resurrect a removed tool.
        if self.get("tool_search").is_some() {
            self.register_tool_search_from_current_tools();
        }
    }

    pub fn with_lazy_loading(mut self, enabled: bool) -> Self {
        self.lazy_loading = enabled;
        self
    }

    pub fn lazy_loading_enabled(&self) -> bool {
        self.lazy_loading
    }

    pub fn is_prelude_tool(&self, name: &str) -> bool {
        !self.lazy_loading || is_core_tool(name)
    }

    pub fn with_model_interface(mut self, interface: ModelToolInterface) -> Self {
        self.model_interface = interface;
        match interface {
            ModelToolInterface::FixedV1 => {
                self.replace(Box::new(crate::exec::ExecTool::new()));
            }
            ModelToolInterface::LegacyPromoted => {
                self.tools.retain(|tool| tool.name() != "exec");
                self.scope_owners.remove("exec");
            }
        }
        if self.get("tool_search").is_some() {
            self.register_tool_search_from_current_tools();
        }
        self
    }

    pub fn model_interface(&self) -> ModelToolInterface {
        self.model_interface
    }

    pub fn capability_descriptor(&self, name: &str) -> Option<CapabilityDescriptor> {
        self.get(name).map(descriptor_for_tool)
    }

    pub fn resolve_capability(&self, id: &str, schema_digest: &str) -> Result<&dyn Tool, String> {
        let name = id
            .strip_prefix("tool:")
            .ok_or_else(|| format!("invalid capability id `{id}`: expected `tool:<name>`"))?;
        if name.is_empty() {
            return Err(format!("invalid capability id `{id}`: empty tool name"));
        }
        let tool = self
            .get(name)
            .ok_or_else(|| format!("capability `{id}` is unavailable: no live tool `{name}`"))?;
        let expected = capability_schema_digest(&tool.input_schema());
        if expected != schema_digest {
            return Err(format!(
                "stale schema digest for capability `{id}`: expected {expected}, got {schema_digest}"
            ));
        }
        Ok(tool)
    }

    pub fn capability_descriptors(&self) -> Vec<CapabilityDescriptor> {
        let mut descriptors: Vec<CapabilityDescriptor> = self
            .tools
            .iter()
            .map(|tool| descriptor_for_tool(tool.as_ref()))
            .collect();
        descriptors.sort_by(|left, right| left.id.cmp(&right.id));
        descriptors
    }

    fn register_tool_search_from_current_tools(&mut self) {
        use crate::tool_search::ToolSearchTool;

        self.tools.retain(|tool| tool.name() != "tool_search");
        let mut descriptors = self.capability_descriptors();
        descriptors.push(ToolSearchTool::self_descriptor());
        descriptors.sort_by(|left, right| left.id.cmp(&right.id));
        let tool_search = ToolSearchTool::from_descriptors(
            descriptors,
            self.model_interface == ModelToolInterface::FixedV1,
        );
        self.register(Box::new(tool_search));
    }

    /// Refresh the live `tool_search` snapshot after host-owned tools have
    /// been registered. This never creates `tool_search` when it is absent.
    pub fn refresh_tool_search_catalog(&mut self) {
        if self.get("tool_search").is_some() {
            self.register_tool_search_from_current_tools();
        }
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.as_ref())
    }

    pub fn all(&self) -> &[Box<dyn Tool>] {
        &self.tools
    }

    pub fn capabilities(&self) -> Vec<ToolCapability> {
        let mut caps: Vec<_> = self
            .tools
            .iter()
            .map(|tool| ToolCapability {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                category: tool_category(tool.name()).to_string(),
                read_only: tool.is_read_only(),
                effect_class: tool.effect_class().as_str().to_string(),
                approval_class: tool.approval_class().as_str().to_string(),
            })
            .collect();
        caps.sort_by(|left, right| {
            left.category
                .cmp(&right.category)
                .then(left.name.cmp(&right.name))
        });
        caps
    }

    pub fn render_classes(&self) -> Vec<(&str, crate::registry::ToolRenderClass)> {
        self.tools
            .iter()
            .map(|tool| {
                let name = tool.name();
                (name, crate::registry::tool_render_class(name))
            })
            .collect()
    }

    /// One descriptor/factory authority for every built-in tool (CQ-16).
    /// Entries are `(registered_name, factory)` in the historical
    /// `build_default` registration order. Selection happens on names BEFORE
    /// instantiation, so a narrow session catalog (`build_for_allowed`,
    /// mobile) constructs only the tools it ships — the previous build
    /// instantiated the full ~95-tool registry and then retained the allowed
    /// names. A factory returning `None` means the tool's backing runtime is
    /// unavailable on this host: it is never registered, so it never reaches
    /// any advertising surface (capabilities, tool_search metadata, request
    /// schemas). `tool_search` is not listed here — it is generated from the
    /// registered set by `register_tool_search_from_current_tools`.
    /// `(registered_name, available_probe, factory)`. The probe answers
    /// host availability without constructing the handler: a tool whose
    /// probe fails is never registered and never reaches an advertising
    /// surface (capabilities, render classes, tool_search metadata,
    /// request schemas).
    pub(super) fn builtin_tool_factories(
    ) -> Vec<(&'static str, fn() -> bool, fn() -> Option<Box<dyn Tool>>)> {
        use crate::apply_patch::ApplyPatchTool;
        use crate::artifact_crud::{
            ArtifactCreateTool, ArtifactDeleteTool, ArtifactListTool, ArtifactReadTool,
            ArtifactUpdateTool,
        };
        use crate::async_inbox::AsyncInboxTool;
        use crate::attach_verdict::AttachVerdictTool;
        #[cfg(feature = "host")]
        use crate::browser::{
            BrowserActTool, BrowserCloseTool, BrowserConsoleTool, BrowserOpenTool, BrowserReadTool,
            BrowserScreenshotTool,
        };
        use crate::browser_fetch::BrowserFetchTool;
        use crate::cloudflare::CloudflareTool;
        use crate::context7::Context7Tool;
        use crate::dev_tunnel_proxy::DevTunnelProxyTool;
        use crate::finalize_artifact::FinalizeArtifactTool;
        use crate::flyio::FlyioTool;
        use crate::git_clone::GitCloneTool;
        use crate::git_diff::GitDiffTool;
        use crate::git_fetch::GitFetchTool;
        use crate::git_history_ops::GitHistoryOpsTool;
        use crate::git_lfs::{GitLfsFetchTool, GitLfsScanTool};
        use crate::git_merge::GitMergeTool;
        use crate::git_read::GitReadTool;
        use crate::git_write::GitWriteTool;
        use crate::github::GitHubTool;
        use crate::image_generate::ImageGenerateTool;
        use crate::read_file::ReadFileTool;
        use crate::search::SearchTool;
        use crate::sequential_thinking::SequentialThinkingTool;
        #[cfg(feature = "host")]
        use crate::shell::{PowerShellTool, ShellTool};
        use crate::spawn_subagent::SpawnSubagentTool;
        use crate::time::TimeTool;
        use crate::tunnel::TunnelTool;
        use crate::wasm_compute::WasmComputeTool;
        use crate::web_search::WebSearchTool;
        use crate::write_file::WriteFileTool;

        #[cfg(feature = "host")]
        fn media(kind: rightkit_ffmpeg::agent_media::MediaToolKind) -> Option<Box<dyn Tool>> {
            Some(Box::new(
                rightkit_ffmpeg::agent_media::NativeMediaTool::new(kind),
            ))
        }

        let factories: Vec<(&'static str, fn() -> bool, fn() -> Option<Box<dyn Tool>>)> = vec![
            (
                "spawn_subagent",
                || true,
                || {
                    Some(Box::new(
                        crate::tool_trait::ProcessResourceSelectorTool::new(
                            SpawnSubagentTool,
                            |args: &serde_json::Value| {
                                let external_cli = args
                                    .get("runtime")
                                    .and_then(serde_json::Value::as_object)
                                    .and_then(|runtime| runtime.get("kind"))
                                    .and_then(serde_json::Value::as_str)
                                    == Some("authenticated_cli");
                                external_cli.then_some(
                                    rightkit_agent_engine_runtime_resources::ResourceKind::ChildProcess,
                                )
                            },
                        ),
                    ))
                },
            ),
            ("read_file", || true, || Some(Box::new(ReadFileTool))),
            (
                "view_image",
                || true,
                || Some(Box::new(crate::view_image::ViewImageTool)),
            ),
            (
                "computer",
                crate::computer_use::computer_use_available,
                || Some(Box::new(crate::computer_use::ComputerUseTool)),
            ),
            ("write_file", || true, || Some(Box::new(WriteFileTool))),
            (
                "file_service",
                || true,
                || Some(Box::new(crate::file_service::FileLifecycleTool)),
            ),
            (
                "edit_file",
                || true,
                || Some(Box::new(crate::edit_file::EditFileTool)),
            ),
            (
                "multi_edit",
                || true,
                || Some(Box::new(crate::multi_edit::MultiEditTool)),
            ),
            (
                "undo_edit",
                || true,
                || Some(Box::new(crate::undo_edit::UndoEditTool)),
            ),
            ("apply_patch", || true, || Some(Box::new(ApplyPatchTool))),
            #[cfg(feature = "host")]
            ("shell", || true, || Some(Box::new(ShellTool))),
            #[cfg(feature = "host")]
            ("powershell", crate::shell::powershell_available, || {
                Some(Box::new(PowerShellTool) as Box<dyn Tool>)
            }),
            #[cfg(feature = "host")]
            (
                "ocr",
                || true,
                || media(rightkit_ffmpeg::agent_media::MediaToolKind::Ocr),
            ),
            #[cfg(feature = "host")]
            (
                "ocr_onnx",
                || true,
                || media(rightkit_ffmpeg::agent_media::MediaToolKind::OcrOnnx),
            ),
            #[cfg(feature = "host")]
            (
                "web_extract",
                || true,
                || media(rightkit_ffmpeg::agent_media::MediaToolKind::WebExtract),
            ),
            #[cfg(feature = "host")]
            (
                "media_fetch",
                || true,
                || media(rightkit_ffmpeg::agent_media::MediaToolKind::MediaFetch),
            ),
            #[cfg(feature = "host")]
            (
                "ffmpeg_extract",
                || true,
                || media(rightkit_ffmpeg::agent_media::MediaToolKind::FfmpegExtract),
            ),
            #[cfg(feature = "host")]
            (
                "get_shell_output",
                || true,
                || Some(Box::new(crate::background_shell::GetShellOutputTool)),
            ),
            #[cfg(feature = "host")]
            (
                "kill_shell",
                || true,
                || Some(Box::new(crate::background_shell::KillShellTool)),
            ),
            #[cfg(feature = "host")]
            (
                "process",
                || true,
                || Some(Box::new(crate::background_shell::ProcessTool)),
            ),
            ("search", || true, || Some(Box::new(SearchTool))),
            (
                "list_files",
                || true,
                || Some(Box::new(crate::list_files::ListFilesTool)),
            ),
            #[cfg(feature = "host")]
            (
                "lsp",
                || true,
                || {
                    Some(Box::new(crate::tool_trait::ProcessResourceTool::new(
                        crate::lsp_tool::LspTool,
                        rightkit_agent_engine_runtime_resources::ResourceKind::Lsp,
                    )))
                },
            ),
            (
                "artifact_create",
                || true,
                || Some(Box::new(ArtifactCreateTool)),
            ),
            (
                "artifact_delete",
                || true,
                || Some(Box::new(ArtifactDeleteTool)),
            ),
            (
                "artifact_list",
                || true,
                || Some(Box::new(ArtifactListTool)),
            ),
            (
                "artifact_read",
                || true,
                || Some(Box::new(ArtifactReadTool)),
            ),
            (
                "artifact_update",
                || true,
                || Some(Box::new(ArtifactUpdateTool)),
            ),
            ("async_inbox", || true, || Some(Box::new(AsyncInboxTool))),
            (
                "image_generate",
                || true,
                || Some(Box::new(ImageGenerateTool::default())),
            ),
            ("flyio", || true, || Some(Box::new(FlyioTool))),
            ("tunnel", || true, || Some(Box::new(TunnelTool::new()))),
            (
                "dev_tunnel_proxy",
                || true,
                || Some(Box::new(DevTunnelProxyTool::new())),
            ),
            (
                "wasm_compute",
                crate::wasm_compute::wasm_runtime_configured,
                || Some(Box::new(WasmComputeTool::new())),
            ),
            ("git_lfs_fetch", || true, || Some(Box::new(GitLfsFetchTool))),
            ("git_lfs_scan", || true, || Some(Box::new(GitLfsScanTool))),
            ("git_clone", || true, || Some(Box::new(GitCloneTool))),
            (
                "git_history_ops",
                || true,
                || Some(Box::new(GitHistoryOpsTool)),
            ),
            ("git_merge", || true, || Some(Box::new(GitMergeTool))),
            ("git_fetch", || true, || Some(Box::new(GitFetchTool))),
            ("git_read", || true, || Some(Box::new(GitReadTool))),
            ("git_diff", || true, || Some(Box::new(GitDiffTool))),
            ("git_write", || true, || Some(Box::new(GitWriteTool))),
            (
                "browser_fetch",
                || true,
                || Some(Box::new(BrowserFetchTool)),
            ),
            #[cfg(feature = "host")]
            ("browser_open", || true, || Some(Box::new(BrowserOpenTool))),
            #[cfg(feature = "host")]
            ("browser_read", || true, || Some(Box::new(BrowserReadTool))),
            #[cfg(feature = "host")]
            ("browser_act", || true, || Some(Box::new(BrowserActTool))),
            #[cfg(feature = "host")]
            (
                "browser_screenshot",
                || true,
                || Some(Box::new(BrowserScreenshotTool)),
            ),
            #[cfg(feature = "host")]
            (
                "browser_console",
                || true,
                || Some(Box::new(BrowserConsoleTool)),
            ),
            #[cfg(feature = "host")]
            (
                "browser_close",
                || true,
                || Some(Box::new(BrowserCloseTool)),
            ),
            (
                "research_report",
                || true,
                || Some(Box::new(crate::research_report::ResearchReportTool)),
            ),
            ("web_search", || true, || Some(Box::new(WebSearchTool))),
            (
                "attach_verdict",
                || true,
                || Some(Box::new(AttachVerdictTool)),
            ),
            (
                "finalize_artifact",
                || true,
                || Some(Box::new(FinalizeArtifactTool)),
            ),
            ("github", || true, || Some(Box::new(GitHubTool))),
            ("cloudflare", || true, || Some(Box::new(CloudflareTool))),
            (
                "sequentialthinking",
                || true,
                || Some(Box::new(SequentialThinkingTool::new())),
            ),
            ("context7", || true, || Some(Box::new(Context7Tool))),
            ("get_time", || true, || Some(Box::new(TimeTool))),
            (
                "todo_write",
                || true,
                || Some(Box::new(crate::todo_write::TodoWriteTool)),
            ),
            (
                "ask_user",
                || true,
                || Some(Box::new(crate::ask_user::AskUserTool)),
            ),
            (
                "exit_plan_mode",
                || true,
                || Some(Box::new(crate::exit_plan_mode::ExitPlanModeTool)),
            ),
            (
                "list_mcp_tools",
                || true,
                || Some(Box::new(crate::mcp_tools::ListMcpToolsTool)),
            ),
            (
                "list_mcp_sources",
                || true,
                || Some(Box::new(crate::mcp_tools::ListMcpSourcesTool)),
            ),
            (
                "mcp_call",
                || true,
                || Some(Box::new(crate::mcp_tools::McpCallTool)),
            ),
            (
                "list_mcp_resources",
                || true,
                || Some(Box::new(crate::mcp_tools::ListMcpResourcesTool)),
            ),
            (
                "read_mcp_resource",
                || true,
                || Some(Box::new(crate::mcp_tools::ReadMcpResourceTool)),
            ),
            (
                "sleep",
                || true,
                || Some(Box::new(crate::sleep_tool::SleepTool)),
            ),
            (
                "brief",
                || true,
                || Some(Box::new(crate::brief_tool::BriefTool)),
            ),
            (
                "task_create",
                || true,
                || Some(Box::new(crate::task_tools::TaskCreateTool)),
            ),
            (
                "task_update",
                || true,
                || Some(Box::new(crate::task_tools::TaskUpdateTool)),
            ),
            (
                "task_get",
                || true,
                || Some(Box::new(crate::task_tools::TaskGetTool)),
            ),
            (
                "task_list",
                || true,
                || Some(Box::new(crate::task_tools::TaskListTool)),
            ),
            (
                "task_output",
                || true,
                || Some(Box::new(crate::task_tools::TaskOutputTool)),
            ),
            (
                "task_stop",
                || true,
                || Some(Box::new(crate::task_tools::TaskStopTool)),
            ),
            (
                "notebook",
                || true,
                || Some(Box::new(crate::notebook_tool::NotebookTool)),
            ),
            (
                "workspace_checkpoint",
                || true,
                || {
                    Some(Box::new(
                        crate::workspace_checkpoint::WorkspaceCheckpointTool,
                    ))
                },
            ),
            #[cfg(feature = "host")]
            (
                "schedule_cron",
                || true,
                || Some(Box::new(crate::schedule_cron_tool::ScheduleCronTool::new())),
            ),
            #[cfg(feature = "host")]
            (
                "repl",
                || true,
                || Some(Box::new(crate::repl_tool::ReplTool::new())),
            ),
            (
                "config",
                || true,
                || Some(Box::new(crate::config_tool::ConfigTool::new())),
            ),
            (
                "recursive_fetch",
                || true,
                || Some(Box::new(crate::recursive_fetch_tool::RecursiveFetchTool)),
            ),
            (
                "send_message",
                || true,
                || Some(Box::new(crate::send_message::SendMessageTool)),
            ),
            (
                "invoke_skill",
                || true,
                || Some(Box::new(crate::skill_tool::SkillTool)),
            ),
            (
                "team_create",
                || true,
                || Some(Box::new(crate::team_tools::TeamCreateTool)),
            ),
            (
                "team_delete",
                || true,
                || Some(Box::new(crate::team_tools::TeamDeleteTool)),
            ),
            (
                "followup_subagent",
                || true,
                || Some(Box::new(crate::send_message::FollowupSubagentTool)),
            ),
            (
                "wait_for_subagents",
                || true,
                || Some(Box::new(crate::spawn_subagent::WaitForSubagentTool)),
            ),
            (
                "interrupt_subagent",
                || true,
                || Some(Box::new(crate::spawn_subagent::InterruptSubagentTool)),
            ),
            #[cfg(feature = "host")]
            (
                "worktree_enter",
                || true,
                || Some(Box::new(crate::worktree_tools::EnterWorktreeTool)),
            ),
            #[cfg(feature = "host")]
            (
                "worktree_exit",
                || true,
                || Some(Box::new(crate::worktree_tools::ExitWorktreeTool)),
            ),
        ];
        factories
    }

    /// Instantiate the selected built-ins and register `tool_search` over the
    /// resulting set. `None` selects every factory (the default catalog);
    /// `Some(permitted)` constructs only factories whose registered name is
    /// in the set — tools are never built just to be dropped.
    fn build_selected(permitted: Option<&std::collections::HashSet<String>>) -> Self {
        let mut reg = Self::new();
        for (name, available, build) in Self::builtin_tool_factories() {
            if permitted.is_none_or(|set| set.contains(name)) && available() {
                if let Some(tool) = build() {
                    reg.register(tool);
                }
            }
        }
        #[cfg(feature = "code-mode")]
        if permitted.is_none_or(|set| set.contains(crate::code_mode::RUN_PROGRAM_NAME)) {
            reg.register(Box::new(crate::code_mode::RunProgramTool::new(&reg)));
        }
        reg.register_tool_search_from_current_tools();
        reg
    }

    pub fn build_default() -> Self {
        Self::build_selected(None)
    }

    pub fn build_default_for_evaluation() -> Self {
        let mut reg = Self::build_default();
        reg.register(Box::new(crate::synthetic_output_tool::SyntheticOutputTool));
        reg.register_tool_search_from_current_tools();
        reg
    }

    #[cfg(feature = "host")]
    pub fn build_default_with_plugins(plugin_dir: impl Into<PathBuf>) -> Self {
        Self::build_with_plugins_for_session(None, plugin_dir, &[])
    }

    #[cfg(feature = "host")]
    pub fn build_for_allowed_with_plugins(
        allowed: &[String],
        plugin_dir: impl Into<PathBuf>,
    ) -> Self {
        Self::build_with_plugins_for_session(Some(allowed), plugin_dir, &[])
    }

    /// Rebuild a session catalog without changing global plugin activation.
    /// Session overrides never override the caller's tool allow-list.
    #[cfg(feature = "host")]
    pub fn build_with_plugins_for_session(
        allowed: Option<&[String]>,
        plugin_dir: impl Into<PathBuf>,
        enabled_plugins: &[String],
    ) -> Self {
        let mut reg = allowed
            .map(Self::build_for_allowed)
            .unwrap_or_else(Self::build_default);
        let permitted = allowed.map(|names| names.iter().cloned().collect());
        reg.register_plugin_tools_from_dir(plugin_dir, permitted.as_ref(), enabled_plugins);
        reg.register_tool_search_from_current_tools();
        reg
    }

    #[cfg(feature = "host")]
    fn register_plugin_tools_from_dir(
        &mut self,
        plugin_dir: impl Into<PathBuf>,
        allowed: Option<&std::collections::HashSet<String>>,
        enabled_plugins: &[String],
    ) {
        for (manifest, root) in enabled_plugin_manifests(plugin_dir.into(), enabled_plugins) {
            let entry = PathBuf::from(&manifest.entry);
            // EXT-001: every plugin's tools belong to that plugin's disposable
            // scope, so unloading the plugin can remove all of them and leave
            // nothing stale behind.
            let scope = Self::plugin_scope(&manifest.id);
            for tool_name in manifest.provides_tools {
                if allowed.map(|a| a.contains(&tool_name)).unwrap_or(true) {
                    let metadata = manifest.tools.get(&tool_name).cloned();
                    let tool = Box::new(crate::plugin_tool::PluginTool::new(
                        tool_name.clone(),
                        manifest.id.clone(),
                        root.clone(),
                        entry.clone(),
                        metadata,
                        manifest.sandbox.clone(),
                    ));
                    if let Err(error) = self.register_scoped(&scope, tool) {
                        // Refusing is correct: a plugin silently shadowing a
                        // built-in or another plugin's tool would break the
                        // total-removal guarantee that makes unload safe.
                        tracing::warn!(
                            plugin_id = %manifest.id,
                            tool = %tool_name,
                            error = %error,
                            "refused plugin tool registration"
                        );
                    }
                }
            }
        }
    }

    /// The disposable owner scope for a plugin's dynamic registrations.
    pub fn plugin_scope(plugin_id: &str) -> RegistrationScopeId {
        RegistrationScopeId::new(format!("plugin:{plugin_id}"))
    }

    /// Unload every tool a plugin registered. Returns how many were removed.
    ///
    /// EXT-001: this is the production unload path. It is total (no
    /// registration owned by the plugin survives) and idempotent, so a
    /// disable followed by a re-enable yields exactly one live registration.
    pub fn unload_plugin(&mut self, plugin_id: &str) -> usize {
        let removed = self.unload_scope(&Self::plugin_scope(plugin_id));
        // tool_search advertises the current tool set; it must not keep
        // offering tools that were just unloaded. Refresh only while it is
        // still registered — rebuilding an absent tool_search would
        // resurrect a removed tool.
        if removed > 0 && self.get("tool_search").is_some() {
            self.register_tool_search_from_current_tools();
        }
        removed
    }

    /// Drop registrations owned by plugins disabled for this scope, advancing
    /// the catalog to a generation that no longer contains them.
    pub fn unload_disabled_plugins(&mut self, disabled: &[String]) -> usize {
        disabled
            .iter()
            .map(|plugin_id| self.unload_plugin(plugin_id))
            .sum()
    }

    /// The allowed-name expansion shared by the narrow builders: the
    /// ALWAYS_ON meta set plus each alias expanded to the tool names it
    /// grants. Selection happens on this name set; instantiation follows in
    /// `build_selected`.
    pub(super) fn permitted_names_for(allowed: &[String]) -> std::collections::HashSet<String> {
        const ALWAYS_ON: &[&str] = &[
            "tool_search",
            "get_time",
            "sequentialthinking",
            "context7",
            "finalize_artifact",
            "todo_write",
            "ask_user",
            "exit_plan_mode",
            "list_mcp_tools",
            "list_mcp_sources",
            "mcp_call",
            "list_mcp_resources",
            "read_mcp_resource",
        ];
        let mut permitted: std::collections::HashSet<String> =
            ALWAYS_ON.iter().map(|s| s.to_string()).collect();
        for entry in allowed {
            match entry.as_str() {
                "git" => {
                    permitted.insert("git_read".into());
                    permitted.insert("git_diff".into());
                    permitted.insert("git_write".into());
                }
                "web" => {
                    permitted.insert("browser_fetch".into());
                    permitted.insert("web_search".into());
                }
                "browser" => {
                    permitted.insert("browser_open".into());
                    permitted.insert("browser_read".into());
                    permitted.insert("browser_act".into());
                    permitted.insert("browser_screenshot".into());
                    permitted.insert("browser_console".into());
                    permitted.insert("browser_close".into());
                }
                "write_file" => {
                    permitted.insert("write_file".into());
                    permitted.insert("edit_file".into());
                    permitted.insert("multi_edit".into());
                    permitted.insert("undo_edit".into());
                    permitted.insert("file_service".into());
                }
                "read_file" => {
                    permitted.insert("read_file".into());
                    permitted.insert("view_image".into());
                    permitted.insert("list_files".into());
                    permitted.insert("lsp".into());
                    permitted.insert("file_service".into());
                }
                "shell" => {
                    permitted.insert("shell".into());
                    permitted.insert("powershell".into());
                    permitted.insert("get_shell_output".into());
                    permitted.insert("kill_shell".into());
                    permitted.insert("task_create".into());
                    permitted.insert("task_update".into());
                    permitted.insert("task_get".into());
                    permitted.insert("task_list".into());
                    permitted.insert("task_output".into());
                    permitted.insert("task_stop".into());
                    permitted.insert("process".into());
                }
                "media" => {
                    permitted.insert("ocr".into());
                    permitted.insert("ocr_onnx".into());
                    permitted.insert("web_extract".into());
                    permitted.insert("media_fetch".into());
                    permitted.insert("ffmpeg_extract".into());
                }
                other => {
                    permitted.insert(other.to_string());
                }
            }
        }
        permitted
    }

    /// Narrow catalog: expand aliases + ALWAYS_ON into the permitted name
    /// set, then instantiate only those tools (CQ-16 — select first; the
    /// full registry is never built just to be filtered down).
    pub fn build_for_allowed(allowed: &[String]) -> Self {
        Self::build_selected(Some(&Self::permitted_names_for(allowed)))
    }

    pub fn build_for_allowed_mobile(allowed: &[String], mobile: MobileToolCapabilities) -> Self {
        // Same selection contract as `build_for_allowed`: fold the mobile
        // capability disallows out of the permitted set BEFORE instantiation.
        let mut permitted = Self::permitted_names_for(allowed);
        if !mobile.allow_ask_user_mobile {
            permitted.remove("ask_user");
        }
        if !mobile.allow_plan_review_mobile {
            permitted.remove("exit_plan_mode");
        }
        if !mobile.allow_subagents_mobile {
            permitted.remove("spawn_subagent");
        }
        if !mobile.allow_mcp_mobile {
            for name in [
                "list_mcp_tools",
                "list_mcp_sources",
                "mcp_call",
                "list_mcp_resources",
                "read_mcp_resource",
            ] {
                permitted.remove(name);
            }
        }
        if !mobile.allow_wasm_compute_mobile {
            permitted.remove("wasm_compute");
        }
        if !mobile.allow_image_generation_mobile {
            permitted.remove("image_generate");
        }
        if !mobile.allow_tunnel_mobile {
            permitted.remove("tunnel");
            permitted.remove("dev_tunnel_proxy");
        }
        if !mobile.allow_async_inbox_mobile {
            permitted.remove("async_inbox");
        }
        Self::build_selected(Some(&permitted))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MobileToolCapabilities {
    pub allow_ask_user_mobile: bool,
    pub allow_plan_review_mobile: bool,
    pub allow_subagents_mobile: bool,
    pub allow_mcp_mobile: bool,
    pub allow_wasm_compute_mobile: bool,
    pub allow_image_generation_mobile: bool,
    pub allow_tunnel_mobile: bool,
    pub allow_async_inbox_mobile: bool,
    pub fetched_resource_packs: Vec<String>,
}

impl MobileToolCapabilities {
    pub fn capabilities(&self) -> &[String] {
        &self.fetched_resource_packs
    }
}

pub(crate) fn capability_schema_digest(input_schema: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(input_schema).unwrap_or_default();
    rightkit_fs::digest::sha256_bytes(&bytes)
}

fn descriptor_for_tool(tool: &dyn Tool) -> CapabilityDescriptor {
    let input_schema = tool.input_schema();
    CapabilityDescriptor {
        id: format!("tool:{}", tool.name()),
        name: tool.name().to_string(),
        description: tool.description().to_string(),
        schema_digest: capability_schema_digest(&input_schema),
        input_schema,
        read_only: tool.is_read_only(),
        availability: "available".to_string(),
    }
}

pub(crate) fn tool_category(name: &str) -> &'static str {
    match name {
        "read_file"
        | "view_image"
        | "write_file"
        | "edit_file"
        | "multi_edit"
        | "undo_edit"
        | "apply_patch"
        | "search"
        | "list_files"
        | "notebook"
        | "workspace_checkpoint"
        | "file_service" => "files",
        "shell" | "powershell" | "get_shell_output" | "kill_shell" | "repl" | "ocr" | "process"
        | "ocr_onnx" | "web_extract" | "media_fetch" | "ffmpeg_extract" => "execution",
        "git_read" | "git_diff" | "git_write" | "enter_worktree" | "exit_worktree" => "git",
        "browser_fetch" | "web_search" | "recursive_fetch" | "github" => "web",
        "browser_open" | "browser_read" | "browser_act" | "browser_screenshot"
        | "browser_console" | "browser_close" => "browser",
        "list_mcp_tools" | "list_mcp_sources" | "mcp_call" | "list_mcp_resources"
        | "read_mcp_resource" | "mcp_auth" => "mcp",
        "spawn_subagent" | "send_message" | "team_create" | "team_delete" | "followup_subagent"
        | "wait_for_subagents" | "interrupt_subagent" => "agents",
        "task_create" | "task_update" | "task_get" | "task_list" | "task_output" | "task_stop"
        | "todo_write" => "tasks",
        "lsp" => "code-intel",
        "invoke_skill" | "config" | "schedule_cron" | "ask_user" | "exit_plan_mode" | "brief"
        | "sleep" => "workflow",
        "tool_search" | "sequentialthinking" | "context7" | "get_time" | "exec" => "meta",
        "attach_verdict" | "finalize_artifact" | "research_report" => "verification",
        _ => "other",
    }
}

#[cfg(feature = "host")]
fn enabled_plugin_manifests(
    plugin_dir: PathBuf,
    enabled_plugins: &[String],
) -> Vec<(rightkit_agent_plugins::PluginManifest, PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(plugin_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let root = entry.path();
        if !root.is_dir() {
            continue;
        }
        let manifest_path = root.join("plugin.json");
        let Ok(raw) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = rightkit_agent_plugins::PluginManifest::from_json(&raw) else {
            continue;
        };
        if manifest.enabled || enabled_plugins.contains(&manifest.id) {
            out.push((manifest, root));
        }
    }
    out
}

/// Static capability metadata for every catalog tool: the authority
/// for metadata-only queries so they never construct a handler.
/// Drift-guarded by metadata_table_matches_constructed_tools.
/// (name, description, read_only)
pub(crate) const BUILTIN_CAPABILITY_META: &[(&str, &str, bool)] = &[
    ("apply_patch", "Apply a custom patch to files. Format: lines starting with '*** ' set the target file, '--- ' remove a line, '+++ ' add a line, and bare lines are context that must match.", false),
    ("artifact_create", "Create a new first-class artifact (independent of the transcript) with an initial content version. Returns the artifact's id, which artifact_read/artifact_update/artifact_delete take.", false),
    ("artifact_delete", "Delete an artifact by id. Moves it to a workspace-local trash directory rather than removing it outright, mirroring FileService's file-delete trash convention.", false),
    ("artifact_list", "List artifacts in the current workspace, optionally filtered by session_id or kind.", true),
    ("artifact_read", "Read an artifact's current metadata and content by id.", true),
    ("artifact_update", "Update an artifact's content, creating a new version. Rejects the write if expected_version no longer matches the artifact's current version (someone else edited it since it was last read), and separately rejects it if the on-disk content changed underneath the version guard.", false),
    ("ask_user", "Block and ask operator a structured question (1-4 questions, 2-4 options each; multiSelect for multi-choice). Use only for operator decisions, not facts you can find. Fail: headless/no questioner driver.", true),
    ("async_inbox", "Read or acknowledge messages that arrived for this session outside the live turn \u{2014} completed background work, deferred tool-call resolutions, webhook or callback events. Read returns the oldest pending messages first (bounded, default 20) without removing them; acknowledge removes every pending message through one returned id.", true),
    ("attach_verdict", "Attach trust verdict (approved/rejected) to artifact with reasoning + confidence [0-1]. Fail: no trust store or artifact not found.", false),
    ("brief", "Produce concise briefing summary of context text: first sentence per paragraph + key lines (- or * prefix). Returns markdown summary. Fail: context field missing.", true),
    ("browser_act", "Act on an open browser session: click, type, press a key, navigate, scroll, wait for an element to appear, choose a dropdown option, hover, or go back/forward/reload. Get selectors from browser_read first. Returns the page state after the action, flagging any console errors it triggered. Fail: unknown session, or no element matches the selector.", false),
    ("browser_close", "Close a browser session and shut down its Chrome process. Call this when done \u{2014} an open session holds a real browser.", true),
    ("browser_console", "Read everything an open browser session has logged since it opened \u{2014} console output and uncaught exceptions. This is where a broken frontend actually tells you what is wrong. Fail: unknown session id.", true),
    ("browser_fetch", "Fetch a URL and extract readable text (title+body). tier='auto' (default) tries plain HTTP, retries with stealth headers, then falls back to JS rendering if the site blocks both. tier='native' stops at HTTP; tier='js' renders JavaScript via TinyFish (needs a TinyFish key). Fail: network error, SSRF-guarded host, or a site that blocks every tier. This tool reads pages \u{2014} it cannot click, type, or run a browser session.", true),
    ("browser_open", "Open a real Chrome browser at a URL and return a session id. Use this to look at a running app \u{2014} including a local dev server (http://localhost:5173) \u{2014} then drive it with browser_read, browser_act, browser_screenshot, and browser_console. Close it with browser_close when done. To reuse the user's signed-in session, pass `consent` with the id of a consent the user already granted (you cannot grant one); the page then runs on a private copy of their profile, limited to the domains they consented to. Fail: no Chrome or Edge installed, the URL does not load, or the consent is unknown, revoked, expired or does not cover the URL.", false),
    ("browser_read", "Read what an open browser session is currently showing: the rendered page text, and every interactive element with the CSS selector needed to act on it. Pass those selectors to browser_act. Fail: unknown session id.", true),
    ("browser_screenshot", "Capture what an open browser session looks like right now, as a PNG you can see. Use it to check layout, styling, and visual regressions \u{2014} things page text cannot tell you. Fail: unknown session id.", true),
    ("cloudflare", "Cloudflare account API: inspect Pages, R2 & Workers; upload one workspace file to R2; retry/rollback Pages deployments; trigger an existing Workers Build. Uses host-stored OAuth/API-token credentials. Git-linked Pages/Workers deploy automatically after git push.", false),
    ("computer", "Operate the desktop: screenshot, move, click, type, press keys, scroll (Linux X11 with xdotool and ImageMagick). Every action returns a screenshot; actions other than screenshot need approval.", false),
    ("config", "Read/set/list daemon config keys. Ops: get(key), set(key, value), list(). Returns JSON values. Fail: key not found (get), invalid key format (set).", false),
    ("context7", "Fetch up-to-date library documentation from context7.com. Use 'resolve-library-id' first to find the library's context7 ID, then 'get-library-docs' to retrieve focused docs. Always use this before answering questions about library APIs, configuration options, or version-specific behaviour.", true),
    ("dev_tunnel_proxy", "Expose a local development server through a registered tunnel provider and return the provider's public handle.", false),
    ("edit_file", "Replace exact substring in a file. Hashline-prefixed old_string values from read_file are validated against their original line anchors and stale anchors are rejected with fresh anchors. Plain old_string must be unique unless replace_all=true.", false),
    ("exit_plan_mode", "Present your implementation plan and ask the operator to approve leaving Plan mode so you can start making changes. Call this only after you've finished researching and have a concrete plan.", true),
    ("ffmpeg_extract", "Run ffmpeg to extract a clip, audio stream, frames, or another derived media file.", false),
    ("file_service", "Typed workspace filesystem lifecycle: stat, mkdir, copy, move, trash/delete, or conflict-checked batch. Canonical path, symlink, overwrite, receipt, and workspace-boundary checks apply.", false),
    ("finalize_artifact", "Mark an artifact as finalized (shippable). Only valid in trust-gated mode.", false),
    ("flyio", "Fly.io Machines API: list apps/machines in the user's own Fly org, create or resume a user-owned Machine backed by a persistent Volume, stop/destroy a Machine, and list/create Volumes. Uses a host-stored, user-supplied Fly.io API token. No Agent account, proxy, or control plane is involved; all Fly spend and ownership stay in the user's Fly.io account.", false),
    ("followup_subagent", "Steer an already-running subagent with a new instruction instead of spawning a fresh child. Addressed by agent id or `parent/child` agent path; delivered through the same live message channel as send_message.", false),
    ("interrupt_subagent", "Cancel one running subagent (and only that subagent's subtree) by agent id or `parent/child` agent path. Sibling children are unaffected.", false),
    ("wait_for_subagents", "Block until every named subagent reaches a terminal status (completed, failed, or cancelled), or until an optional timeout elapses. Accepts bare child agent ids or `parent/child` agent paths.", false),
    ("get_shell_output", "Return output from a background shell started with shell(run_in_background=true), along with its run status and typed exit status. By default returns only output produced since the last check; pass `since` (absolute byte offset) to re-read from an offset. Output per call is bounded by `max_bytes`; `next_offset` and `more` tell you how to continue.", true),
    ("get_time", "Current time/date, timezone conversion (IANA names like America/New_York, Asia/Tokyo, UTC or offset like +05:30), and duration calc. Operations: current_time, convert (ISO 8601), diff.", true),
    ("git_clone", "Clone an exact HTTPS repository URL into an empty app-owned workspace directory, with progress, cancellation, and cleanup of the partial clone on failure. Force and non-HTTPS remotes are refused.", false),
    ("git_diff", "Unified diff of working tree (or staged with staged=true) vs HEAD (max 100KB). Fail: not a git repo.", true),
    ("git_fetch", "Authenticated, cancellable Git fetch that updates remote-tracking refs only. Never touches the working tree, index, or HEAD.", false),
    ("git_history_ops", "Guarded Git stash (save/pop/apply/drop/list), revert, tag create/delete, and paged commit history/detail, scoped to an exact repository with machine-readable approval/effect receipts.", false),
    ("git_lfs_fetch", "Fetch exact, named Git LFS objects (by pointer path) up to a caller-declared byte ceiling. Refuses to run if the named objects' declared sizes exceed max_bytes, and never performs an unbounded whole-repo LFS pull.", false),
    ("git_lfs_scan", "Read-only: list Git LFS pointer files tracked at HEAD (oid, declared size, path). Performs no network access and downloads nothing.", false),
    ("git_merge", "Guarded Git merge/rebase with explicit conflict surfacing \u{2014} conflicts are always listed by path and stage, never auto-resolved. Supports ours/theirs/manual resolution per file, continue, and abort.", false),
    ("git_read", "Typed, read-only Git status/log/show/blame/branch operations scoped to an exact repository, ref, and path.", true),
    ("git_write", "Guarded Git stage, commit, and push with exact repository/path/ref/remote inputs and machine-readable approval/effect receipts. Force and wildcard pushes are refused.", false),
    ("github", "GitHub API (forge-aware, GitHub App installation-token compatible): list_prs, get_file, add_comment, get_pr_files, get_pr_comments, get_pr_review_comments, get_repo, get_branch, list_branches, get_commit, list_commits, list_issues, get_issue, list_releases, get_pr, get_pr_diff, resolve_pr_for_branch, create_pr, update_pr, set_pr_draft, add_review_comment, submit_review, resolve_review_thread, merge_pr, get_combined_status, get_check_runs, list_issue_comments. Supports forge=github (default) and base_url override for GHES. Requires github_token config (PAT or ghs_ installation token). Mutating actions (create_pr, update_pr, set_pr_draft, add_comment, add_review_comment, submit_review, resolve_review_thread, merge_pr) require confirm=true and will not execute otherwise. merge_pr additionally requires an explicit merge_method. Fail: no token, unsupported forge, missing confirmation, or API error.", false),
    ("image_generate", "Generate images through the agent's dedicated image-model catalog. Requires a configured image-provider account; image models never use chat completion routes.", false),
    ("invoke_skill", if cfg!(feature = "host") { "Invoke a named skill by slug. Reads the skill definition from $RIGHTKIT_SKILLS_DIR/<slug>.md or ~/.rightkit/skills/<slug>.md and returns its content for the agent to act on. Returns 'Skill not found: {slug}' if missing." } else { "Invoke a named skill by slug. Reads the skill definition from ~/.rightkit/skills/<slug>.md and returns its content for the agent to act on. Returns 'Skill not found: {slug}' if missing." }, true),
    ("kill_shell", "Terminate a background shell started with shell(run_in_background=true).", false),
    ("list_files", "List workspace files (gitignore-aware, default 500, hard max 5000). Bare glob '*.rs' matches filename; path glob 'src/**/*.rs' matches full path. Fail: path escapes workspace root.", true),
    ("list_mcp_resources", "List the resources available from a configured MCP server. Use read_mcp_resource to fetch the content of a specific resource by URI.", true),
    ("list_mcp_sources", "Report the state of each MCP tool source in this session: ready, no-tools, failed, or pending (still starting or not connected yet), with the tool count. It reads state the session already holds; it does not start a server, call a tool, or show a URL, header or token. Use it to tell a server that is not connected from one that has no tools. Call list_mcp_tools for the tools themselves.", true),
    ("list_mcp_tools", "List the tools available from configured MCP servers, with their qualified names and descriptions. Call mcp_call to invoke one.", true),
    ("lsp", "Semantic code navigation. Ops: status, workspace_symbols(query), goto_definition(path,line,character), references(path,line,character), hover(path,line,character), diagnostics(path). Reuses one live language server per workspace. Definition and hover can use a local fallback; workspace symbols, references and diagnostics require a real server. Line is 1-based to match read_file output.", true),
    ("mcp_call", "Invoke a tool on a configured MCP server by its qualified name (mcp__server__tool). Use list_mcp_tools first to discover names and schemas.", false),
    ("media_fetch", "Run yt-dlp to fetch media or media metadata from a URL.", false),
    ("multi_edit", "Apply multiple edits atomically to one file. Edits array: each {old_string, new_string, replace_all?} runs on prior result. Hashline-prefixed old_string values from read_file are validated against their line anchors. All succeed or none written.", false),
    ("notebook", "Cell-aware .ipynb editing. Legacy mode uses path/operation/index/kind/source; Claude-compatible mode uses notebook_path/cell_id/new_source/cell_type/edit_mode (replace, insert, delete). Preserves nbformat.", false),
    ("ocr", "Run Tesseract OCR on an image/PDF page via the installed tesseract binary.", false),
    ("ocr_onnx", "In-process OCR using the operating system's own text-recognition framework (Apple Vision on macOS, Windows.Media.Ocr on Windows). No models to download or bundle. This is the default OCR backend; use the ocr tool only if you specifically need Tesseract. Name retained for tool-contract stability \u{2014} it no longer uses ONNX.", false),
    ("powershell", "Execute a PowerShell command in the workspace using powershell on Windows or pwsh elsewhere. Uses the same destructive guard, permission checks, timeout, output limit, and background-shell support as shell.", false),
    ("process", "List/get/output/input/stop/reattach daemon-owned process handles with bounded output and typed exit state.", false),
    ("read_file", "Read file content with hashline prefixes (e.g., '     1:a3f: fn main() {'). Use offset/limit to page through large files (default cap: 2000 lines). Fail: passes absolute path outside workspace root, or path climbs with ../ past root.", true),
    ("read_mcp_resource", "Read a resource from a configured MCP server by URI. Use list_mcp_resources to discover available URIs first.", true),
    ("recursive_fetch", "Crawl a URL and follow same-origin links breadth-first. Returns {url, text}[] up to max_depth and max_pages. Fail: invalid URL, max exceeded, network error.", true),
    ("repl", "Evaluate code snippets in persistent REPL sessions. Ops: start(language), eval(session_id, code), status([session_id]), reset(session_id), close(session_id). Languages: only runtimes resolvable on this host are advertised (python, node). Fail: unsupported language, session not found.", false),
    ("research_report", "Finalize a deep-research run into a cited markdown report. Provide title, an optional summary, sections [{heading, body}] whose bodies cite sources inline as [1], [2]\u{2026}, and sources [{title, url, note?}]. Returns the rendered report; write it to a file or finalize it as an artifact. Cite every non-obvious claim.", true),
    ("schedule_cron", "Register/list/cancel durable cron-style scheduled prompt records in .rightkit/schedules.json. Ops: register(cron_expr, prompt), list(), cancel(id). Cron: '*/5 * * * *' = every 5min; '0 9 * * *' = 9am daily. The daemon runner claims due records once per minute and submits prompts into schedule-<id> sessions. Fail: invalid cron syntax.", false),
    ("search", "Search files for regex pattern with bounded streaming byte I/O; respects .gitignore. Modes: 'content' (lines, default), 'files_with_matches' (paths), 'count' (per-file). Returns execution/evidence output only: Agent performs no semantic ranking or context admission. Returns explicit Complete/Truncated/Cancelled/Deadline status and a continuation cursor when resumable.", true),
    ("send_message", if cfg!(feature = "host") { "Send a message to a specific session or broadcast to all. Writes the message to the messages drop-dir ($RIGHTKIT_MESSAGES_DIR or ~/.rightkit/messages) as a JSON file for the daemon to forward. Returns confirmation with the target." } else { "Send a message to a specific session or broadcast to all. Writes the message to the messages drop-dir (~/.rightkit/messages) as a JSON file for the daemon to forward. Returns confirmation with the target." }, false),
    ("sequentialthinking", "Step-by-step reasoning chain (use for complex analysis). Each call adds one thought; revise or branch earlier ones. Final thought: nextThoughtNeeded=false.", true),
    ("shell", "Execute shell command; returns stdout+stderr combined (30s timeout, max 50KB). use run_in_background=true for dev servers/watchers (returns shell_id to poll). Fail: command blocked by destructive guard (rm -rf, format disk) or permission denied.", false),
    ("sleep", "Sleep for N milliseconds (capped at 60s). Returns elapsed time. Use for timing, polling, test delays. Fail: duration > 60000ms.", true),
    ("spawn_subagent", "Spawn a specialist sub-agent to handle a specific task. The sub-agent runs to completion and returns its final response. Use when delegating a focused subtask to a role like 'challenger', 'designer', or 'lens'.", false),
    ("task_create", "Create a durable background task record. If `command` is provided, starts it with the background shell runner and returns both task_id and shell_id. If `kind` is `local_agent`, delegates the task to a configured sub-agent role and stores the result.", false),
    ("task_get", "Return one durable task record by id.", true),
    ("task_list", "List durable task records for the current workspace.", true),
    ("task_output", "Return output for a task. For shell-backed tasks, polls the background shell and appends new output to the task record.", false),
    ("task_stop", "Stop an active shell-backed task. Non-shell records are not mutated; use task_update for manual lifecycle changes.", false),
    ("task_update", "Update a durable task record's title, state, done_when, assigned_role, category, or appended output.", false),
    ("team_create", "Create an agent team configuration file at ~/.rightkit/teams/<name>.toml. Defines a named group of agent roles that can be spawned together. Returns 'Team created: {name}' on success.", false),
    ("team_delete", "Delete an agent team configuration file at ~/.rightkit/teams/<name>.toml. Returns 'Team deleted: {name}' on success or an error if not found.", false),
    ("todo_write", "Record and update your task list for the current work. Call this at the start of any multi-step task and after each step completes. Pass the COMPLETE list every time \u{2014} it replaces the previous list. Each item has `content` (imperative, e.g. 'Add the login route'), `status` (pending | in_progress | completed), and optional `activeForm` (present continuous, e.g. 'Adding the login route'). Keep exactly one item in_progress at a time. Mark an item completed the moment it is done \u{2014} don't batch completions.", true),
    ("tool_search", "Discover available tools with a ranked shortlist from case-insensitive substring matching over tool names and descriptions.", true),
    ("tunnel", "Optional, guarded exposure of a local host:port over an external tunnel provider. Lists providers, stores a managed token, and starts/stops/checks a public exposure. Exposure never survives the session that opened it unless keep_alive is set.", false),
    ("undo_edit", "Restore file to its snapshot before the most recent edit (edit_file/write_file/multi_edit). Fail: no edit history for this file, or (when target_receipt_id is given) a later conflicting effect has already landed on this path.", false),
    ("view_image", "Load a local image file and render it as an inline image in the transcript. Takes a path (relative to workspace or absolute within workspace). Supports PNG, JPEG, GIF, WebP, BMP, SVG, TIFF, ICO. The image must be under the workspace root; read-deny glob rules apply.", true),
    ("wasm_compute", "Run a module under explicit memory, fuel/step, and wall-clock bounds; unavailable when no WASM runtime is configured.", false),
    ("web_extract", "Run an installed web extraction/scraping CLI against a URL and return its output.", false),
    ("web_search", "Search the web via keyless DuckDuckGo by default, or configured Brave/Tavily/Exa/TinyFish (default 10 results, max 20). Returns title+snippet+url.", true),
    ("workspace_checkpoint", "Whole-workspace snapshot/restore via a private git index: create(label?) snapshots every tracked+untracked file into a write-tree/commit-tree object without moving HEAD; restore(checkpoint_id) transactionally re-applies a prior snapshot (validated fully in memory before any file is touched); list() enumerates snapshots. Fail: not a git repo, unknown checkpoint_id, corrupt tree.", false),
    ("worktree_enter", "Create a git worktree for a branch, enabling isolated work without switching branches. Runs `git worktree add {path} {branch}`. Uses a temp dir if no path given. Returns the worktree path.", false),
    ("worktree_exit", "Remove a git worktree at the given path. Runs `git worktree remove {path}`. Returns 'Worktree removed: {path}' on success.", false),
    ("write_file", "Write content to a file (creates or overwrites). Auto-creates parent directories. Fail if path escapes workspace or permission denied.", false),
];

/// Names currently advertised on this host: available builtin descriptors
/// plus the generated `tool_search` catalog tool. An unavailable runtime
/// (powershell off-Windows, unconfigured WASM) advertises nothing.
fn advertised_builtin_names() -> std::collections::HashSet<&'static str> {
    let mut names: std::collections::HashSet<&'static str> = ToolRegistry::builtin_tool_factories()
        .into_iter()
        .filter(|(_, available, _)| available())
        .map(|(name, _, _)| name)
        .collect();
    names.insert("tool_search");
    names
}

/// Metadata-only view of the default tool catalog. Frontends consume this
/// instead of constructing the executable registry authority themselves —
/// it is answered from `BUILTIN_CAPABILITY_META`, so no tool handler is
/// instantiated for a capability listing.
pub fn default_tool_capabilities() -> Vec<ToolCapability> {
    let advertised = advertised_builtin_names();
    // Effect and approval class come from the tool itself. The factories build the same unit
    // handlers the registry holds; none of them is run.
    let classes: std::collections::HashMap<&'static str, (String, String)> =
        ToolRegistry::builtin_tool_factories()
            .into_iter()
            .filter(|(_, available, _)| available())
            .filter_map(|(name, _, factory)| {
                factory().map(|tool| {
                    (
                        name,
                        (
                            tool.effect_class().as_str().to_string(),
                            tool.approval_class().as_str().to_string(),
                        ),
                    )
                })
            })
            .collect();
    let mut caps: Vec<_> = BUILTIN_CAPABILITY_META
        .iter()
        .filter(|(name, _, _)| advertised.contains(name))
        .map(|(name, description, read_only)| {
            let (effect_class, approval_class) = classes.get(name).cloned().unwrap_or_else(|| {
                // Generated tools such as `tool_search` have no factory.
                let effect = rightkit_agent_policy::effect_class_for_tool(name, None, *read_only);
                (
                    effect.as_str().to_string(),
                    effect.approval_class().as_str().to_string(),
                )
            });
            ToolCapability {
                name: (*name).to_string(),
                description: (*description).to_string(),
                category: tool_category(name).to_string(),
                read_only: *read_only,
                effect_class,
                approval_class,
            }
        })
        .collect();
    caps.sort_by(|left, right| {
        left.category
            .cmp(&right.category)
            .then(left.name.cmp(&right.name))
    });
    caps
}

/// Metadata-only default render classes with owned names so presentation
/// adapters never need to retain or construct the executable ToolRegistry.
/// Order matches `ToolRegistry::render_classes` on a default build:
/// descriptor-table order, with the generated `tool_search` last.
pub fn default_tool_render_classes() -> Vec<(String, crate::registry::ToolRenderClass)> {
    let mut classes: Vec<(String, crate::registry::ToolRenderClass)> =
        ToolRegistry::builtin_tool_factories()
            .into_iter()
            .filter(|(_, available, _)| available())
            .map(|(name, _, _)| (name.to_string(), crate::registry::tool_render_class(name)))
            .collect();
    classes.push((
        "tool_search".to_string(),
        crate::registry::tool_render_class("tool_search"),
    ));
    classes
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
