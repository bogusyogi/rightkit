fn live_execution_processes(
    state: &DaemonState,
    workspace_filter: Option<&std::path::Path>,
    include_output: bool,
) -> Vec<ExecutionProcessRow> {
    live_execution_processes_filtered(
        state,
        workspace_filter,
        &ProcessListFilter::default(),
        include_output,
    )
}

/// Which live processes a list call wants, beyond the workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ProcessListFilter {
    session_id: Option<String>,
    include_children: bool,
    running_only: bool,
}

/// `root` and every session whose `parentSessionId` chain leads to it
/// (`include_children`), from `(session id, parent id)` pairs. A cycle in the
/// pairs cannot loop: each session is visited once.
fn session_tree_ids(
    root: &str,
    include_children: bool,
    sessions: &[(String, Option<String>)],
) -> std::collections::HashSet<String> {
    let mut found = std::collections::HashSet::new();
    found.insert(root.to_string());
    if !include_children {
        return found;
    }
    loop {
        let before = found.len();
        for (id, parent) in sessions {
            if parent.as_ref().is_some_and(|parent| found.contains(parent)) {
                found.insert(id.clone());
            }
        }
        if found.len() == before {
            return found;
        }
    }
}

fn started_by_label(role: Option<&str>, parent: Option<&str>) -> String {
    match (parent, role.map(str::trim).filter(|role| !role.is_empty())) {
        (None, _) => "Main".to_string(),
        (Some(_), Some(role)) => role.to_string(),
        (Some(_), None) => "Sub-agent".to_string(),
    }
}

fn live_execution_processes_filtered(
    state: &DaemonState,
    workspace_filter: Option<&std::path::Path>,
    filter: &ProcessListFilter,
    include_output: bool,
) -> Vec<ExecutionProcessRow> {
    let sessions = lock_recover(&state.sessions, "state.sessions");
    let normalized_filter = workspace_filter.map(normalize_lexical);
    let wanted_sessions = filter.session_id.as_deref().map(|root| {
        let edges: Vec<(String, Option<String>)> = sessions
            .values()
            .map(|live| (live.id.clone(), live.parent_session_id.clone()))
            .collect();
        session_tree_ids(root, filter.include_children, &edges)
    });
    let mut rows = Vec::new();
    for live in sessions.values() {
        if let Some(wanted) = &wanted_sessions {
            if !wanted.contains(&live.id) {
                continue;
            }
        }
        if let Some(filter) = &normalized_filter {
            if normalize_lexical(std::path::Path::new(&live.workspace_root)) != *filter {
                continue;
            }
        }
        let shells = lock_recover(&live.background_shells, "live.background_shells");
        for (shell_id, shell) in shells.iter() {
            if filter.running_only
                && !matches!(
                    shell.status(),
                    rightkit_agent_coding_tools::background_shell::BgStatus::Running
                )
            {
                continue;
            }
            rows.push(execution_process_row(live, shell_id, shell, include_output));
        }
    }
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows
}

fn rfc3339_from_ns(ns: u128) -> String {
    i64::try_from(ns / 1_000_000_000)
        .ok()
        .and_then(|secs| rightkit_time::DateTime::from_unix(secs, 0))
        .map(|dt| dt.format_rfc3339())
        .unwrap_or_default()
}

fn live_execution_process(
    state: &DaemonState,
    id: &str,
    include_output: bool,
) -> Result<
    Option<(
        ExecutionProcessRow,
        std::sync::Arc<rightkit_agent_coding_tools::background_shell::BgShell>,
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
    shell: &rightkit_agent_coding_tools::background_shell::BgShell,
    include_output: bool,
) -> ExecutionProcessRow {
    let status = shell.status();
    let target = shell.execution_target();
    // One read of the lease. While an input flush holds it nothing was read (`lease_snapshot_now`
    // panics then), so the lease fields are `null` for this row instead of invented values.
    let lease = shell.try_lease_snapshot_now();
    let activity = shell.activity();
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
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
        attachment: lease.as_ref().map(|lease| {
            if lease.attached {
                "attached"
            } else {
                "detached"
            }
            .to_string()
        }),
        lease_epoch: lease.as_ref().map(|lease| lease.epoch),
        next_input_sequence: lease.as_ref().map(|lease| lease.next_input_sequence),
        last_acked_input_sequence: lease.as_ref().map(|lease| lease.last_acked_input_sequence),
        status: process_status(&status).to_string(),
        status_label: process_status_label(&status),
        started_by: Some(ExecutionProcessStartedBy {
            session_id: live.id.clone(),
            role: live.role.clone(),
            parent_session_id: live.parent_session_id.clone(),
            label: started_by_label(live.role.as_deref(), live.parent_session_id.as_deref()),
        }),
        started_at: Some(rfc3339_from_ns(activity.started_at_ns)),
        running_seconds: Some(activity.running_seconds(now_ns)),
        last_output_at: activity.last_output_at_ns.map(rfc3339_from_ns),
        quiet_seconds: Some(activity.quiet_seconds(now_ns)),
        last_line: shell.last_line(),
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
        attachment: Some("detached".to_string()),
        lease_epoch: Some(record.lease_epoch),
        next_input_sequence: Some(record.next_input_sequence),
        last_acked_input_sequence: Some(record.last_acked_input_sequence),
        status: if record.persistence == "service" { "unknown" } else { "unavailable" }.to_string(),
        status_label: format!("last seen {}", record.status_label),
        last_output_line: String::new(),
        started_by: None,
        started_at: None,
        running_seconds: None,
        last_output_at: None,
        quiet_seconds: None,
        last_line: String::new(),
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
    status: &rightkit_agent_coding_tools::background_shell::BgStatus,
) -> &'static str {
    match status {
        rightkit_agent_coding_tools::background_shell::BgStatus::Running => "running",
        rightkit_agent_coding_tools::background_shell::BgStatus::Exited(_) => "exited",
        rightkit_agent_coding_tools::background_shell::BgStatus::Killed => "killed",
        rightkit_agent_coding_tools::background_shell::BgStatus::SpawnFailed(_) => "spawnFailed",
        rightkit_agent_coding_tools::background_shell::BgStatus::Unknown => "unknown",
    }
}

fn process_status_label(
    status: &rightkit_agent_coding_tools::background_shell::BgStatus,
) -> String {
    match status {
        rightkit_agent_coding_tools::background_shell::BgStatus::Running => "running".into(),
        rightkit_agent_coding_tools::background_shell::BgStatus::Exited(code) => {
            format!("exited (code {code})")
        }
        rightkit_agent_coding_tools::background_shell::BgStatus::Killed => "killed".into(),
        rightkit_agent_coding_tools::background_shell::BgStatus::SpawnFailed(error) => {
            format!("spawn failed: {error}")
        }
        rightkit_agent_coding_tools::background_shell::BgStatus::Unknown => {
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

#[cfg(test)]
mod process_filter_tests {
    use super::*;

    fn pairs(items: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
        items
            .iter()
            .map(|(id, parent)| (id.to_string(), parent.map(str::to_string)))
            .collect()
    }

    #[test]
    fn tree_includes_children_and_grandchildren_only_when_asked() {
        let sessions = pairs(&[
            ("chat", None),
            ("kid", Some("chat")),
            ("grandkid", Some("kid")),
            ("other", None),
            ("other-kid", Some("other")),
        ]);
        let all = session_tree_ids("chat", true, &sessions);
        assert_eq!(all.len(), 3);
        assert!(all.contains("chat") && all.contains("kid") && all.contains("grandkid"));
        let only = session_tree_ids("chat", false, &sessions);
        assert_eq!(only.len(), 1);
        assert!(!session_tree_ids("kid", true, &sessions).contains("chat"));
    }

    #[test]
    fn tree_walk_survives_a_cycle_and_unknown_root() {
        let sessions = pairs(&[("a", Some("b")), ("b", Some("a"))]);
        assert_eq!(session_tree_ids("a", true, &sessions).len(), 2);
        assert_eq!(session_tree_ids("missing", true, &sessions).len(), 1);
    }

    #[test]
    fn label_is_main_or_the_role() {
        assert_eq!(started_by_label(None, None), "Main");
        assert_eq!(started_by_label(Some("tests"), None), "Main");
        assert_eq!(started_by_label(Some("tests"), Some("chat")), "tests");
        assert_eq!(started_by_label(None, Some("chat")), "Sub-agent");
        assert_eq!(started_by_label(Some("  "), Some("chat")), "Sub-agent");
    }
}
