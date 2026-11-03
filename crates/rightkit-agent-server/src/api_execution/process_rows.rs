fn live_execution_processes(
    state: &DaemonState,
    workspace_filter: Option<&std::path::Path>,
    include_output: bool,
) -> Vec<ExecutionProcessRow> {
    let sessions = lock_recover(&state.sessions, "state.sessions");
    let normalized_filter = workspace_filter.map(normalize_lexical);
    let mut rows = Vec::new();
    for live in sessions.values() {
        if let Some(filter) = &normalized_filter {
            if normalize_lexical(std::path::Path::new(&live.workspace_root)) != *filter {
                continue;
            }
        }
        let shells = lock_recover(&live.background_shells, "live.background_shells");
        for (shell_id, shell) in shells.iter() {
            rows.push(execution_process_row(live, shell_id, shell, include_output));
        }
    }
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows
}

fn live_execution_process(
    state: &DaemonState,
    id: &str,
    include_output: bool,
) -> Result<
    Option<(
        ExecutionProcessRow,
        std::sync::Arc<rightkit_agent_tools::coding_tools::background_shell::BgShell>,
    )>,
    ExecutionError,
> {
    let sessions = lock_recover(&state.sessions, "state.sessions");
    for live in sessions.values() {
        let shell = lock_recover(&live.background_shells, "live.background_shells")
            .get(id)
            .cloned();
        if let Some(shell) = shell {
            let process = execution_process_row(live, id, &shell, include_output);
            return Ok(Some((process, shell)));
        }
    }
    Ok(None)
}

fn execution_process_row(
    live: &crate::api::LiveSession,
    shell_id: &str,
    shell: &rightkit_agent_tools::coding_tools::background_shell::BgShell,
    include_output: bool,
) -> ExecutionProcessRow {
    let status = shell.status();
    let target = shell.execution_target();
    ExecutionProcessRow {
        id: shell_id.to_string(),
        kind: shell.process_kind().to_string(),
        session_id: live.id.clone(),
        workspace_root: live.workspace_root.clone(),
        backend_id: target
            .backend_id
            .clone()
            .or_else(|| live.execution_backend.clone())
            .unwrap_or_else(|| "local".to_string()),
        execution_workspace_id: target
            .workspace_id
            .clone()
            .or_else(|| live.execution_workspace_id.clone()),
        network_profile: target
            .network_profile
            .clone()
            .or_else(|| live.execution_network_profile.clone()),
        command: shell.command.clone(),
        execution_channel: match shell.execution_channel() {
            rightkit_sandbox::execution::ExecutionChannel::Pipe => "pipe".to_string(),
            rightkit_sandbox::execution::ExecutionChannel::Pty(_) => "pty".to_string(),
        },
        pty_size: match shell.execution_channel() {
            rightkit_sandbox::execution::ExecutionChannel::Pty(size) => Some(size),
            rightkit_sandbox::execution::ExecutionChannel::Pipe => None,
        },
        persistence: shell.persistence().label().to_string(),
        attachment: if shell.lease_snapshot_now().attached {
            "attached"
        } else {
            "detached"
        }
        .to_string(),
        lease_epoch: shell.lease_snapshot_now().epoch,
        next_input_sequence: shell.lease_snapshot_now().next_input_sequence,
        last_acked_input_sequence: shell.lease_snapshot_now().last_acked_input_sequence,
        status: process_status(&status).to_string(),
        status_label: process_status_label(&status),
        last_output_line: shell
            .output_snapshot()
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .chars()
            .rev()
            .take(100)
            .collect::<String>()
            .chars()
            .rev()
            .collect(),
        started_at_ns: Some(shell.started_at_ns()),
        live: true,
        control_mode: "liveProcess".to_string(),
        control_unavailable_reason: None,
        last_seen_at: None,
        output: include_output.then(|| shell.output_snapshot()),
    }
}

fn historical_process_row(record: ExecutionProcessRecord) -> ExecutionProcessRow {
    let last_status = record.status;
    ExecutionProcessRow {
        id: record.id,
        kind: record.kind,
        session_id: record.session_id,
        workspace_root: record.workspace_root,
        backend_id: record.backend_id,
        execution_workspace_id: record.execution_workspace_id,
        network_profile: record.network_profile,
        command: record.command,
        execution_channel: record.execution_channel,
        pty_size: None,
        persistence: record.persistence.clone(),
        attachment: "detached".to_string(),
        lease_epoch: record.lease_epoch,
        next_input_sequence: record.next_input_sequence,
        last_acked_input_sequence: record.last_acked_input_sequence,
        status: if record.persistence == "service" { "unknown" } else { "unavailable" }.to_string(),
        status_label: format!("last seen {}", record.status_label),
        last_output_line: String::new(),
        started_at_ns: None,
        live: false,
        control_mode: "historicalUnavailable".to_string(),
        control_unavailable_reason: Some(
            "Historical process controls require a live local process handle.".to_string(),
        ),
        last_seen_at: Some(record.last_seen_at),
        output: Some(format!(
            "Process history only; daemon no longer tracks live process state. Last recorded status: {last_status}."
        )),
    }
}

fn process_status(
    status: &rightkit_agent_tools::coding_tools::background_shell::BgStatus,
) -> &'static str {
    match status {
        rightkit_agent_tools::coding_tools::background_shell::BgStatus::Running => "running",
        rightkit_agent_tools::coding_tools::background_shell::BgStatus::Exited(_) => "exited",
        rightkit_agent_tools::coding_tools::background_shell::BgStatus::Killed => "killed",
        rightkit_agent_tools::coding_tools::background_shell::BgStatus::SpawnFailed(_) => {
            "spawnFailed"
        }
        rightkit_agent_tools::coding_tools::background_shell::BgStatus::Unknown => "unknown",
    }
}

fn process_status_label(
    status: &rightkit_agent_tools::coding_tools::background_shell::BgStatus,
) -> String {
    match status {
        rightkit_agent_tools::coding_tools::background_shell::BgStatus::Running => "running".into(),
        rightkit_agent_tools::coding_tools::background_shell::BgStatus::Exited(code) => {
            format!("exited (code {code})")
        }
        rightkit_agent_tools::coding_tools::background_shell::BgStatus::Killed => "killed".into(),
        rightkit_agent_tools::coding_tools::background_shell::BgStatus::SpawnFailed(error) => {
            format!("spawn failed: {error}")
        }
        rightkit_agent_tools::coding_tools::background_shell::BgStatus::Unknown => {
            "unknown (restart reconciliation required)".into()
        }
    }
}

fn tail_output_bytes(output: &str, tail_bytes: usize) -> (String, bool) {
    if tail_bytes >= output.len() {
        return (output.to_string(), false);
    }
    let mut start = output.len() - tail_bytes;
    while start < output.len() && !output.is_char_boundary(start) {
        start += 1;
    }
    (output[start..].to_string(), true)
}
