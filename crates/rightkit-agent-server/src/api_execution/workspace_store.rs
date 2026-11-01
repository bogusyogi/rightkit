fn load_execution_workspaces(
    workspace_root: &std::path::Path,
) -> Result<Vec<ExecutionWorkspaceRecord>, ExecutionError> {
    let path = execution_workspace_store_path(workspace_root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| ExecutionError::internal(format!("read execution workspaces: {e}")))?;
    serde_json::from_str(&content)
        .map_err(|e| ExecutionError::internal(format!("parse execution workspaces: {e}")))
}

fn save_execution_workspaces(
    workspace_root: &std::path::Path,
    records: &[ExecutionWorkspaceRecord],
) -> Result<(), ExecutionError> {
    let path = execution_workspace_store_path(workspace_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            ExecutionError::internal(format!("create execution workspace store dir: {e}"))
        })?;
    }
    let raw = serde_json::to_string_pretty(records)
        .map_err(|e| ExecutionError::internal(format!("serialize workspaces: {e}")))?;
    crate::store_write::write_store_file(&path, raw.as_bytes())
        .map_err(|e| ExecutionError::internal(format!("write execution workspaces: {e}")))
}

fn load_execution_process_records(
    workspace_root: &std::path::Path,
) -> Result<Vec<ExecutionProcessRecord>, ExecutionError> {
    let path = execution_process_store_path(workspace_root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| ExecutionError::internal(format!("read execution processes: {e}")))?;
    serde_json::from_str(&content)
        .map_err(|e| ExecutionError::internal(format!("parse execution processes: {e}")))
}

fn save_execution_process_records(
    workspace_root: &std::path::Path,
    records: &[ExecutionProcessRecord],
) -> Result<(), ExecutionError> {
    let path = execution_process_store_path(workspace_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            ExecutionError::internal(format!("create execution process store dir: {e}"))
        })?;
    }
    let raw = serde_json::to_string_pretty(records)
        .map_err(|e| ExecutionError::internal(format!("serialize processes: {e}")))?;
    crate::store_write::write_store_file(&path, raw.as_bytes())
        .map_err(|e| ExecutionError::internal(format!("write execution processes: {e}")))
}

fn persist_execution_process_snapshots(
    processes: &[ExecutionProcessRow],
) -> Result<(), ExecutionError> {
    let now = now_iso();
    let mut roots: std::collections::BTreeMap<PathBuf, Vec<ExecutionProcessRecord>> =
        std::collections::BTreeMap::new();
    for process in processes {
        roots
            .entry(PathBuf::from(&process.workspace_root))
            .or_default()
            .push(ExecutionProcessRecord {
                id: process.id.clone(),
                kind: process.kind.clone(),
                session_id: process.session_id.clone(),
                workspace_root: process.workspace_root.clone(),
                backend_id: process.backend_id.clone(),
                execution_workspace_id: process.execution_workspace_id.clone(),
                network_profile: process.network_profile.clone(),
                command: process.command.clone(),
                execution_channel: process.execution_channel.clone(),
                persistence: process.persistence.clone(),
                attachment: process.attachment.clone(),
                lease_epoch: process.lease_epoch,
                next_input_sequence: process.next_input_sequence,
                last_acked_input_sequence: process.last_acked_input_sequence,
                status: process.status.clone(),
                status_label: process.status_label.clone(),
                last_seen_at: now.clone(),
            });
    }
    for (root, snapshots) in roots {
        let mut records = load_execution_process_records(&root)?;
        for snapshot in snapshots {
            if let Some(existing) = records.iter_mut().find(|record| record.id == snapshot.id) {
                *existing = snapshot;
            } else {
                records.push(snapshot);
            }
        }
        records.sort_by(|a, b| a.id.cmp(&b.id));
        save_execution_process_records(&root, &records)?;
    }
    Ok(())
}

fn merge_historical_processes(
    workspace_root: &std::path::Path,
    processes: &mut Vec<ExecutionProcessRow>,
) -> Result<(), ExecutionError> {
    let records = load_execution_process_records(workspace_root)?;
    for record in records {
        if processes.iter().any(|process| process.id == record.id) {
            continue;
        }
        processes.push(historical_process_row(record));
    }
    processes.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(())
}

fn historical_execution_process_from_query(
    query: &ExecutionProcessQuery,
    id: &str,
) -> Result<Option<ExecutionProcessRow>, ExecutionError> {
    let Some(root) = query
        .workspace_root
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    else {
        return Ok(None);
    };
    historical_execution_process(&root, id)
}

fn historical_execution_process_from_logs_query(
    query: &ExecutionProcessLogsQuery,
    id: &str,
) -> Result<Option<ExecutionProcessRow>, ExecutionError> {
    let Some(root) = query
        .workspace_root
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    else {
        return Ok(None);
    };
    historical_execution_process(&root, id)
}

fn historical_execution_process(
    workspace_root: &std::path::Path,
    id: &str,
) -> Result<Option<ExecutionProcessRow>, ExecutionError> {
    Ok(load_execution_process_records(workspace_root)?
        .into_iter()
        .find(|record| record.id == id)
        .map(historical_process_row))
}

fn clean_workspace_id(value: Option<&str>) -> Result<String, ExecutionError> {
    let id = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("workspace-{}", current_epoch_millis()));
    if id == "." || id == ".." {
        return Err(ExecutionError::bad_request(
            "workspaceId must not be '.' or '..'",
        ));
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(ExecutionError::bad_request(
            "workspaceId may contain only letters, numbers, '.', '-' and '_'",
        ));
    }
    Ok(id)
}

fn execution_workspace_paths(
    workspace_root: &std::path::Path,
    backend: &rightkit_sandbox::execution::ExecutionBackendConfig,
    workspace_id: &str,
) -> Result<ExecutionWorkspacePaths, ExecutionError> {
    let root_template = backend
        .workspace_layout
        .root
        .replace("{workspaceId}", workspace_id);
    let root_path = workspace_root.join(root_template);
    let root_path = normalize_lexical(&root_path);
    let normalized_workspace = normalize_lexical(workspace_root);
    if !root_path.starts_with(&normalized_workspace) {
        return Err(ExecutionError::bad_request(
            "execution workspace layout must stay inside workspaceRoot",
        ));
    }
    let uploads = root_path.join(&backend.workspace_layout.uploads);
    let workspace = root_path.join(&backend.workspace_layout.workspace);
    let outputs = root_path.join(&backend.workspace_layout.outputs);
    let logs = root_path.join(&backend.workspace_layout.logs);
    let skills = root_path.join(&backend.workspace_layout.skills);
    let metadata = root_path.join(&backend.workspace_layout.metadata);
    Ok(ExecutionWorkspacePaths {
        root: root_path.to_string_lossy().to_string(),
        uploads: uploads.to_string_lossy().to_string(),
        workspace: workspace.to_string_lossy().to_string(),
        outputs: outputs.to_string_lossy().to_string(),
        logs: logs.to_string_lossy().to_string(),
        skills: skills.to_string_lossy().to_string(),
        metadata: metadata.to_string_lossy().to_string(),
    })
}

fn create_execution_workspace_dirs(paths: &ExecutionWorkspacePaths) -> Result<(), ExecutionError> {
    for path in [
        &paths.uploads,
        &paths.workspace,
        &paths.outputs,
        &paths.logs,
        &paths.skills,
    ] {
        std::fs::create_dir_all(path)
            .map_err(|e| ExecutionError::internal(format!("create workspace dir: {e}")))?;
    }
    Ok(())
}

fn normalize_lexical(path: &std::path::Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn current_epoch_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn now_iso() -> String {
    rightkit_agent_engine_protocol::now_iso()
}
