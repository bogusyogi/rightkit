use rightkit_agent_engine_core::provider_turn_boundary::{
    BoundarySendOutcome, ProviderBoundarySender, ProviderBoundaryUpdate,
};
use rightkit_agent_tools::coding_tools::ToolRegistry;
use std::sync::{Arc, Mutex};

const TOOL_NAMES: [&str; 5] = [
    "blueprint_recall",
    "blueprint_search",
    "blueprint_expand",
    "membrane_memory_read",
    "membrane_source_read",
];

type Rebuild = Box<dyn Fn() -> ToolRegistry + Send + Sync>;

fn with_model_profile(
    registry: ToolRegistry,
    lazy_tool_loading: bool,
    model_interface: rightkit_agent_tools::coding_tools::ModelToolInterface,
) -> ToolRegistry {
    registry
        .with_lazy_loading(lazy_tool_loading)
        .with_model_interface(model_interface)
}

pub(crate) fn model_interface_from_config(
    config: rightkit_app_settings::agent_config::ModelToolInterfaceConfig,
) -> rightkit_agent_tools::coding_tools::ModelToolInterface {
    match config {
        rightkit_app_settings::agent_config::ModelToolInterfaceConfig::LegacyPromoted => {
            rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted
        }
        rightkit_app_settings::agent_config::ModelToolInterfaceConfig::FixedV1 => {
            rightkit_agent_tools::coding_tools::ModelToolInterface::FixedV1
        }
    }
}

/// Host-owned snapshot used by projection surfaces that only describe tools.
/// Answered from the static capability table — no tool handler is
/// constructed for a metadata query.
pub(crate) fn default_tool_capabilities(
) -> Vec<rightkit_agent_tools::coding_tools::registry::ToolCapability> {
    rightkit_agent_tools::coding_tools::registry::default_tool_capabilities()
}

/// Host composition owns executable session registries. API/config surfaces
/// delegate here so they cannot construct a peer registry authority.
pub(crate) fn build_session_tool_registry_for_plugins(
    plugin_scope: (std::path::PathBuf, &[String]),
    safe_mode: bool,
    role_allowed_tools: Option<&Vec<String>>,
    requested_allowed_tools: &[String],
    requested_disallowed_tools: &[String],
    lazy_tool_loading: bool,
    model_interface: rightkit_agent_tools::coding_tools::ModelToolInterface,
    memory: Option<(
        Arc<dyn rightkit_memory::backend::MemoryBackend>,
        rightkit_memory::memory_provenance::MemoryProvenanceStore,
        &str,
    )>,
) -> ToolRegistry {
    let (plugin_dir, enabled_plugins) = plugin_scope;
    let filter = crate::api::effective_tool_filter(
        role_allowed_tools,
        requested_allowed_tools,
        requested_disallowed_tools,
    );
    let filter = if safe_mode {
        Some(safe_mode_tool_filter(filter, requested_disallowed_tools))
    } else {
        filter
    };

    let mut registry = if let Some(selected) = &filter {
        with_model_profile(
            if safe_mode {
                ToolRegistry::build_for_allowed(selected)
            } else {
                ToolRegistry::build_with_plugins_for_session(
                    Some(selected),
                    plugin_dir,
                    enabled_plugins,
                )
            },
            lazy_tool_loading,
            model_interface,
        )
    } else if requested_disallowed_tools.is_empty() {
        with_model_profile(
            if safe_mode {
                ToolRegistry::build_default()
            } else {
                ToolRegistry::build_with_plugins_for_session(None, plugin_dir, enabled_plugins)
            },
            lazy_tool_loading,
            model_interface,
        )
    } else {
        let denied = requested_disallowed_tools
            .iter()
            .map(|tool| tool.as_str())
            .collect::<std::collections::HashSet<_>>();
        let base = if safe_mode {
            ToolRegistry::build_default()
        } else {
            ToolRegistry::build_with_plugins_for_session(None, plugin_dir.clone(), enabled_plugins)
        };
        let names = base
            .all()
            .iter()
            .map(|tool| tool.name().to_string())
            .filter(|name| !denied.contains(name.as_str()))
            .collect::<Vec<_>>();
        with_model_profile(
            if safe_mode {
                ToolRegistry::build_for_allowed(&names)
            } else {
                ToolRegistry::build_with_plugins_for_session(
                    Some(&names),
                    plugin_dir,
                    enabled_plugins,
                )
            },
            lazy_tool_loading,
            model_interface,
        )
    };

    // Canonical Membrane read aliases are always available unless explicitly
    // denied. They expose only existing read operations and remain gated by
    // the per-session attested transport and registry identity at execution.
    rightkit_memory::blueprint_tools::register(&mut registry, |name| {
        !requested_disallowed_tools.iter().any(|tool| tool == name)
            && (filter
                .as_ref()
                .is_none_or(|selected| selected.iter().any(|tool| tool == name))
                || TOOL_NAMES.contains(&name))
    });

    if registry.get("run_program").is_some() {
        registry.register(Box::new(
            rightkit_agent_tools::coding_tools::code_mode::RunProgramTool::new(&registry),
        ));
    }

    // memory_save is host-owned because it carries selected backend/provenance.
    if let Some((backend, provenance, workspace_root)) = memory {
        let allowed = match &filter {
            Some(selected) => selected.iter().any(|tool| tool == "memory_save"),
            None => !requested_disallowed_tools
                .iter()
                .any(|tool| tool == "memory_save"),
        };
        if allowed {
            registry.register(Box::new(
                rightkit_memory::memory_save_tool::MemorySaveTool::new(
                    backend,
                    provenance,
                    workspace_root,
                ),
            ));
        }
    }

    // Final authoritative policy, applied AFTER the builders: build_for_allowed
    // unions ALWAYS_ON tools and expands group aliases (shell -> powershell,
    // git -> git_read/...) onto the filtered selection, so a deny checked only
    // against the pre-expansion list can be reintroduced — and in safe mode the
    // union could add back tools that are not read-only (mcp_call). Denies and
    // the safe-mode read-only ceiling bind the registry that actually runs.
    apply_post_build_policy(&mut registry, safe_mode, requested_disallowed_tools);
    registry.refresh_tool_search_catalog();
    registry
}

/// Names approved under safe mode: every read-only builtin capability plus
/// the host-owned Blueprint read paths in [`TOOL_NAMES`], which are
/// registered by the host outside the builtin factory table. This set is the
/// single authority for both the pre-build filter and the post-build policy
/// so the two cannot drift — the regression this fixes was the post-build
/// policy rebuilding the set from `build_default()` alone and stripping the
/// host-registered Blueprint tools it had just admitted.
fn approved_read_only_tool_names() -> std::collections::HashSet<String> {
    let mut read_only: std::collections::HashSet<String> = default_tool_capabilities()
        .into_iter()
        .filter(|capability| capability.read_only)
        .map(|capability| capability.name)
        .collect();
    read_only.insert("exec".to_string());
    read_only.extend(TOOL_NAMES.iter().map(|name| (*name).to_string()));
    read_only
}

/// Remove explicitly denied names and, under safe mode, every registration
/// that is not an approved read-only tool (builtin read-only capabilities
/// plus host-owned read paths). Runs after construction so it binds the
/// post-expansion registry; `remove` keeps the tool_search catalog in sync.
/// Plugin tools are never in the approved set, so safe mode still strips
/// them; denies still bind afterward regardless of read-only status.
fn apply_post_build_policy(
    registry: &mut ToolRegistry,
    safe_mode: bool,
    requested_disallowed_tools: &[String],
) {
    if safe_mode {
        let read_only = approved_read_only_tool_names();
        let non_read_only = registry
            .all()
            .iter()
            .map(|tool| tool.name().to_string())
            .filter(|name| !read_only.contains(name))
            .collect::<Vec<_>>();
        for name in non_read_only {
            registry.remove(&name);
        }
    }
    for name in requested_disallowed_tools {
        registry.remove(name);
    }
}

fn safe_mode_tool_filter(
    requested_filter: Option<Vec<String>>,
    requested_disallowed_tools: &[String],
) -> Vec<String> {
    let read_only = approved_read_only_tool_names();
    let denied = requested_disallowed_tools
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut selected = requested_filter.unwrap_or_else(|| read_only.iter().cloned().collect());
    selected.retain(|tool| read_only.contains(tool) && !denied.contains(tool.as_str()));
    selected.sort();
    selected.dedup();
    selected
}

#[derive(Default)]
struct CatalogState {
    rebuild: Option<Rebuild>,
    active: Option<Arc<ToolRegistry>>,
    revision: u64,
}

#[cfg(test)]
mod live_plugin_catalog_tests {
    use super::*;
    use rightkit_agent_engine_core::provider_turn_boundary::{
        provider_boundary_channel, ProviderBoundaryProjection,
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn prebind_changes_and_full_queue_preserve_latest_catalog_without_mutating_active() {
        let (tx, mut rx) = provider_boundary_channel(1);
        let catalog = SessionToolCatalog::new("fixture".into(), tx);
        let include_search = Arc::new(AtomicBool::new(false));
        assert_eq!(catalog.refresh(), None);
        include_search.store(true, Ordering::SeqCst);
        let source = include_search.clone();
        let initial = catalog.bind(move || {
            let names = if source.load(Ordering::SeqCst) {
                vec!["search".into()]
            } else {
                vec![]
            };
            ToolRegistry::build_for_allowed(&names)
        });
        assert!(initial.get("search").is_some());
        include_search.store(false, Ordering::SeqCst);
        assert_eq!(catalog.refresh(), Some(BoundarySendOutcome::Queued));
        for _ in 0..8 {
            include_search.store(true, Ordering::SeqCst);
            assert_eq!(catalog.refresh(), Some(BoundarySendOutcome::Coalesced));
            include_search.store(false, Ordering::SeqCst);
            assert_eq!(catalog.refresh(), Some(BoundarySendOutcome::Coalesced));
        }
        assert!(
            initial.get("search").is_some(),
            "in-flight registry stays immutable"
        );
        let mut projection = ProviderBoundaryProjection::default();
        let promoted = projection.promote_pending(&mut rx).tools.unwrap();
        assert!(
            promoted.get("search").is_none(),
            "latest revision wins after overflow"
        );
        drop(rx);
        assert_eq!(catalog.refresh(), Some(BoundarySendOutcome::Closed));
    }

    /// The production reload closure calls the same session builder on every
    /// refresh, so the post-expansion deny/availability policy must bind each
    /// rebuilt registry — not just the initially bound one.
    #[test]
    fn refresh_rebuild_keeps_post_expansion_denies() {
        let (tx, _rx) = provider_boundary_channel(4);
        let catalog = SessionToolCatalog::new("fixture".into(), tx);
        let allowed = vec!["shell".to_string()];
        let denied = vec!["powershell".to_string(), "todo_write".to_string()];
        let initial = catalog.bind(move || {
            build_session_tool_registry_for_plugins(
                (std::path::PathBuf::new(), &[]),
                false,
                None,
                &allowed,
                &denied,
                false,
                rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
                None,
            )
        });
        assert!(initial.get("shell").is_some());
        assert!(initial.get("powershell").is_none());
        assert!(initial.get("todo_write").is_none());

        assert!(catalog.refresh().is_some());
        let rebuilt = catalog.snapshot().expect("rebuild publishes a snapshot");
        assert!(rebuilt.get("shell").is_some());
        assert!(
            rebuilt.get("powershell").is_none(),
            "alias expansion must not reintroduce a denied tool after reload"
        );
        assert!(
            rebuilt.get("todo_write").is_none(),
            "ALWAYS_ON union must not reintroduce a denied tool after reload"
        );
    }

    /// Regression: `apply_post_build_policy` used to rebuild the safe-mode
    /// read-only set from `build_default()` alone, stripping the Blueprint
    /// read tools the host registers outside the builtin table — after the
    /// pre-build filter had just admitted them. Both the initial
    /// construction and every reload must retain them.
    #[test]
    fn safe_mode_retains_host_blueprint_read_tools_on_build_and_reload() {
        let (tx, _rx) = provider_boundary_channel(4);
        let catalog = SessionToolCatalog::new("fixture".into(), tx);
        let initial = catalog.bind(|| {
            build_session_tool_registry_for_plugins(
                (std::path::PathBuf::new(), &[]),
                true,
                None,
                &[],
                &[],
                false,
                rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
                None,
            )
        });
        for name in ["blueprint_recall", "blueprint_search", "blueprint_expand"] {
            assert!(
                initial.get(name).is_some(),
                "safe mode must retain host-owned {name}"
            );
        }
        // The read-only ceiling still binds: mutating builtins are stripped.
        assert!(initial.get("write_file").is_none());
        assert!(initial.get("shell").is_none());
        assert!(initial.get("mcp_call").is_none());

        assert!(catalog.refresh().is_some());
        let rebuilt = catalog.snapshot().expect("rebuild publishes a snapshot");
        for name in ["blueprint_recall", "blueprint_search", "blueprint_expand"] {
            assert!(
                rebuilt.get(name).is_some(),
                "reload must not strip host-owned {name}"
            );
        }
        assert!(rebuilt.get("write_file").is_none());
    }

    #[test]
    fn model_interface_defaults_to_legacy_and_fixed_v1_is_explicit() {
        let lazy_default = build_session_tool_registry_for_plugins(
            (std::path::PathBuf::new(), &[]),
            false,
            None,
            &[],
            &[],
            true,
            rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
            None,
        );
        assert!(lazy_default.lazy_loading_enabled());
        assert_eq!(
            lazy_default.model_interface(),
            rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
            "lazy loading alone must not select FixedV1"
        );

        let fixed = build_session_tool_registry_for_plugins(
            (std::path::PathBuf::new(), &[]),
            false,
            None,
            &[],
            &[],
            true,
            rightkit_agent_tools::coding_tools::ModelToolInterface::FixedV1,
            None,
        );
        assert_eq!(
            fixed.model_interface(),
            rightkit_agent_tools::coding_tools::ModelToolInterface::FixedV1
        );
        let fixed_again = build_session_tool_registry_for_plugins(
            (std::path::PathBuf::new(), &[]),
            false,
            None,
            &[],
            &[],
            true,
            rightkit_agent_tools::coding_tools::ModelToolInterface::FixedV1,
            None,
        );
        assert_eq!(
            serde_json::to_vec(
                &rightkit_agent_tools::coding_tools::registry::fixed_v1_tool_defs(&fixed)
            )
            .unwrap(),
            serde_json::to_vec(
                &rightkit_agent_tools::coding_tools::registry::fixed_v1_tool_defs(&fixed_again)
            )
            .unwrap(),
            "FixedV1 projection is stable across rebuilds"
        );
    }

    #[test]
    fn model_interface_fixed_v1_safe_mode_admits_exec_and_deny_removes_it() {
        let safe_legacy = build_session_tool_registry_for_plugins(
            (std::path::PathBuf::new(), &[]),
            true,
            None,
            &[],
            &[],
            true,
            rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
            None,
        );
        assert!(safe_legacy.get("exec").is_none());

        let safe_fixed = build_session_tool_registry_for_plugins(
            (std::path::PathBuf::new(), &[]),
            true,
            None,
            &[],
            &[],
            true,
            rightkit_agent_tools::coding_tools::ModelToolInterface::FixedV1,
            None,
        );
        assert!(safe_fixed.get("exec").is_some());
        assert!(safe_fixed.get("exec").unwrap().is_read_only());

        let denied = vec!["exec".to_string()];
        let safe_fixed_denied = build_session_tool_registry_for_plugins(
            (std::path::PathBuf::new(), &[]),
            true,
            None,
            &[],
            &denied,
            true,
            rightkit_agent_tools::coding_tools::ModelToolInterface::FixedV1,
            None,
        );
        assert!(safe_fixed_denied.get("exec").is_none());
    }

    /// The approved read set admits host tools; it never weakens an explicit
    /// deny, and it never admits plugin-registered names.
    #[test]
    fn safe_mode_blueprint_deny_still_binds_on_build_and_reload() {
        let denied = vec!["blueprint_search".to_string()];
        let (tx, _rx) = provider_boundary_channel(4);
        let catalog = SessionToolCatalog::new("fixture".into(), tx);
        let initial = catalog.bind(move || {
            build_session_tool_registry_for_plugins(
                (std::path::PathBuf::new(), &[]),
                true,
                None,
                &[],
                &denied,
                false,
                rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
                None,
            )
        });
        assert!(initial.get("blueprint_search").is_none());
        assert!(initial.get("blueprint_recall").is_some());
        assert!(initial.get("blueprint_expand").is_some());

        assert!(catalog.refresh().is_some());
        let rebuilt = catalog.snapshot().expect("rebuild publishes a snapshot");
        assert!(
            rebuilt.get("blueprint_search").is_none(),
            "explicit deny must bind after reload"
        );
        assert!(rebuilt.get("blueprint_recall").is_some());
    }

    #[test]
    fn tool_search_discovers_live_membrane_aliases_and_respects_denies() {
        use rightkit_agent_tools::coding_tools::tool_trait::Tool;

        let registry = build_session_tool_registry_for_plugins(
            (std::path::PathBuf::new(), &[]),
            false,
            None,
            &[],
            &[],
            false,
            rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
            None,
        );
        let context = rightkit_agent_tools::coding_tools::ToolContext::new(
            "catalog-test",
            std::path::PathBuf::new(),
            rightkit_agent_policy::PermissionMode::Ask,
            rightkit_agent_policy::WorkspaceTrust::Trusted,
        );
        let runtime = tokio::runtime::Runtime::new().expect("test runtime");
        for name in TOOL_NAMES {
            let result = runtime.block_on(
                registry
                    .get("tool_search")
                    .expect("tool_search is registered")
                    .execute(serde_json::json!({"query":name}), &context),
            );
            assert!(!result.is_error, "{}", result.content);
            let found: Vec<serde_json::Value> =
                serde_json::from_str(&result.content).expect("tool_search returns JSON results");
            assert!(
                found
                    .iter()
                    .any(|entry| entry.get("name").and_then(|value| value.as_str()) == Some(name)),
                "missing {name}: {}",
                result.content
            );
        }

        let denied = vec!["blueprint_search".to_string()];
        let denied_registry = build_session_tool_registry_for_plugins(
            (std::path::PathBuf::new(), &[]),
            false,
            None,
            &[],
            &denied,
            false,
            rightkit_agent_tools::coding_tools::ModelToolInterface::LegacyPromoted,
            None,
        );
        let denied_result = runtime.block_on(
            denied_registry
                .get("tool_search")
                .expect("tool_search remains registered")
                .execute(serde_json::json!({"query":"blueprint_search"}), &context),
        );
        let denied_found: Vec<serde_json::Value> =
            serde_json::from_str(&denied_result.content).expect("JSON results");
        assert!(
            denied_found
                .iter()
                .all(|entry| entry.get("name").and_then(|name| name.as_str())
                    != Some("blueprint_search")),
            "denied alias must stay absent: {}",
            denied_result.content
        );
    }
}

/// Retains creation-time tool policy, not a second live registry. Both initial
/// construction and plugin refresh use the same builder. The engine owns the
/// active registry and adopts replacements at its existing provider boundary.
pub struct SessionToolCatalog {
    session_id: String,
    boundary: ProviderBoundarySender,
    state: Mutex<CatalogState>,
}

impl SessionToolCatalog {
    pub fn new(session_id: String, boundary: ProviderBoundarySender) -> Self {
        Self {
            session_id,
            boundary,
            state: Mutex::new(CatalogState::default()),
        }
    }

    pub fn bind(
        &self,
        rebuild: impl Fn() -> ToolRegistry + Send + Sync + 'static,
    ) -> Arc<ToolRegistry> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        assert!(state.rebuild.is_none(), "session catalog already bound");
        let tools = Arc::new(rebuild());
        state.active = Some(Arc::clone(&tools));
        state.rebuild = Some(Box::new(rebuild));
        tracing::debug!(session_id = %self.session_id, "session_tool_catalog_bound");
        tools
    }

    /// Call after releasing manifest/activation/session locks. Serializing
    /// rebuild and publication prevents a slow older refresh winning a race.
    /// Before bind, startup will read current manifests and activation state.
    pub fn refresh(&self) -> Option<BoundarySendOutcome> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let rebuild = state.rebuild.as_ref()?;
        let tools = Arc::new(rebuild());
        state.revision = state.revision.saturating_add(1);
        state.active = Some(Arc::clone(&tools));
        let outcome = self
            .boundary
            .send_latest(ProviderBoundaryUpdate::ToolCatalog {
                revision: state.revision,
                tools,
            });
        match outcome {
            BoundarySendOutcome::Closed => tracing::warn!(
                session_id = %self.session_id, revision = state.revision,
                "session_tool_catalog_receiver_closed"
            ),
            _ => tracing::info!(
                session_id = %self.session_id, revision = state.revision,
                ?outcome, "session_tool_catalog_published"
            ),
        }
        Some(outcome)
    }

    /// Build a projection from the same creation-time policy closure used by
    /// the engine. Consumers use this only for host-facing schemas; execution
    /// still goes through the engine's immutable request registry.
    pub fn snapshot(&self) -> Option<Arc<ToolRegistry>> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.active.clone()
    }

    /// Monotonic generation of the live catalog published at the provider
    /// boundary. Generation zero is the bound startup catalog.
    pub fn generation(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .revision
    }
}
