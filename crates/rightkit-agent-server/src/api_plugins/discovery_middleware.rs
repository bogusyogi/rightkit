// Daemon-state adapter for portable plugin discovery.

pub(crate) use rightkit_agent_engine_api_integrations::discovery::{
    discover_plugins, local_plugin_middleware_definitions, plugin_manifests_with_roots,
    provider_extension_documents,
};

pub(crate) fn sync_runtime_middleware_from_plugins(state: &DaemonState) -> usize {
    let definitions = local_plugin_middleware_definitions(&state.plugin_dir);
    let count = definitions.len();
    state
        .runtime_middleware
        .lock()
        .unwrap()
        .replace_local_plugin_middleware(definitions);
    count
}

fn validate_plugin_id(id: &str) -> bool {
    !id.trim().is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
}

/// Mutation routes call this after updating installed plugin state. Read-only
/// middleware inspection must not trigger catalog rebuilds or revisions.
fn refresh_runtime_plugins(state: &DaemonState) -> usize {
    let count = sync_runtime_middleware_from_plugins(state);
    // list_sessions snapshots handles and releases the daemon-wide lock.
    for session in state.list_sessions() {
        session.tool_catalog.refresh();
    }
    count
}

fn copy_plugin_tree(src: &FsPath, dst: &FsPath) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|error| error.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_dir() {
            copy_plugin_tree(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&src_path, &dst_path).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}
