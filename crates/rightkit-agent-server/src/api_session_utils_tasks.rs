use super::*;

pub(crate) fn normalize_session_status(status: &str) -> Option<&'static str> {
    match status
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .as_str()
    {
        "archived" | "archive" => Some("archived"),
        "dormant" | "restore" | "restored" => Some("dormant"),
        "running" | "run" | "resume" => Some("running"),
        "done" => Some("done"),
        "interrupted" => Some("interrupted"),
        _ => None,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditGoal {
    pub text: Option<String>,
    pub budget_tokens: Option<i64>,
    pub budget_usd: Option<f64>,
}

/// F2: flip the active goal's status so the engine's `may_continue` honors it
/// on the next turn-end check (immediate — the engine reads shared state).
pub(crate) fn set_goal_status_field(live: &LiveSession, status: &str) -> bool {
    if let Some(goal) = lock_recover(&live.goal_state, "session.goal_state").as_mut() {
        goal.status = status.to_string();
        true
    } else {
        false
    }
}

pub(crate) fn task_node_json(task: &rightkit_agent_engine_protocol::TaskNode) -> serde_json::Value {
    let mut row = serde_json::to_value(task).unwrap_or(serde_json::Value::Null);
    if let Some(obj) = row.as_object_mut() {
        obj.insert(
            "source".into(),
            serde_json::Value::String("session-taskgraph".into()),
        );
    }
    row
}

pub(crate) fn task_record_json(
    record: &rightkit_agent_coding_tools::task_tools::TaskRecord,
    path: &FsPath,
    live_shell_statuses: &HashMap<String, String>,
) -> serde_json::Value {
    let mut row = serde_json::to_value(&record.node).unwrap_or(serde_json::Value::Null);
    if let Some(obj) = row.as_object_mut() {
        obj.insert(
            "source".into(),
            serde_json::Value::String("workspace-record".into()),
        );
        obj.insert(
            "kind".into(),
            serde_json::Value::String(record.kind.clone()),
        );
        obj.insert(
            "taskStorePath".into(),
            serde_json::Value::String(path.to_string_lossy().to_string()),
        );
        obj.insert(
            "outputBytes".into(),
            serde_json::Value::Number(serde_json::Number::from(record.output.len())),
        );
        obj.insert(
            "outputPreview".into(),
            serde_json::Value::String(task_output_preview(&record.output)),
        );
        if let Some(command) = &record.command {
            obj.insert("command".into(), serde_json::Value::String(command.clone()));
        }
        if let Some(shell_id) = &record.shell_id {
            let process_status = live_shell_statuses
                .get(shell_id)
                .cloned()
                .unwrap_or_else(|| "unavailable".into());
            let attachment = if process_status == "unavailable" {
                "detached"
            } else {
                "attached"
            };
            let can_poll_output = process_status != "unavailable";
            let can_stop = process_status == "running";
            obj.insert(
                "shellId".into(),
                serde_json::Value::String(shell_id.clone()),
            );
            obj.insert(
                "processStatus".into(),
                serde_json::Value::String(process_status.clone()),
            );
            obj.insert(
                "processAttachment".into(),
                serde_json::Value::String(attachment.into()),
            );
            obj.insert(
                "canPollOutput".into(),
                serde_json::Value::Bool(can_poll_output),
            );
            obj.insert("canStop".into(), serde_json::Value::Bool(can_stop));
            if !can_poll_output {
                obj.insert(
                    "processStatusReason".into(),
                    serde_json::Value::String(
                        "Shell is not attached to this daemon; stored task output remains available."
                            .into(),
                    ),
                );
            }
            if !can_stop {
                obj.insert(
                    "lifecycleStatusReason".into(),
                    serde_json::Value::String(task_lifecycle_reason(record, &process_status)),
                );
            }
        } else {
            obj.insert("canStop".into(), serde_json::Value::Bool(false));
            obj.insert(
                "lifecycleStatusReason".into(),
                serde_json::Value::String(task_lifecycle_reason(record, "none")),
            );
        }
    }
    row
}

fn task_lifecycle_reason(
    record: &rightkit_agent_coding_tools::task_tools::TaskRecord,
    process_status: &str,
) -> String {
    match record.node.state {
        rightkit_agent_engine_protocol::TaskState::Done
        | rightkit_agent_engine_protocol::TaskState::Failed
        | rightkit_agent_engine_protocol::TaskState::Cancelled
        | rightkit_agent_engine_protocol::TaskState::Merged => {
            format!(
                "Task is already {:?}; stop is not needed.",
                record.node.state
            )
        }
        _ if record.shell_id.is_some() && process_status == "unavailable" => {
            "Shell process is not attached to this daemon; use task_output for stored output."
                .into()
        }
        _ if record.shell_id.is_some() => {
            format!("Shell process is {process_status}; stop is only available while running.")
        }
        _ => format!(
            "Task kind '{}' has no live cancellation handle; use task_update to change lifecycle state.",
            record.kind
        ),
    }
}

pub(crate) fn live_shell_statuses_for_workspace(
    state: &DaemonState,
    root: &FsPath,
) -> HashMap<String, String> {
    let sessions: Vec<_> = lock_recover(&state.sessions, "sessions")
        .values()
        .cloned()
        .collect();
    let mut statuses = HashMap::new();
    for live in sessions {
        if !same_path(&live.workspace_root, root) {
            continue;
        }
        let shells = lock_recover(&live.background_shells, "session.background_shells");
        for (shell_id, shell) in shells.iter() {
            statuses.insert(shell_id.clone(), process_status_label(shell.status()));
        }
    }
    statuses
}

pub(crate) fn same_path(left: &str, right: &FsPath) -> bool {
    let left = PathBuf::from(left);
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => left == right,
    }
}

fn process_status_label(
    status: rightkit_agent_coding_tools::background_shell::BgStatus,
) -> String {
    match status {
        rightkit_agent_coding_tools::background_shell::BgStatus::Running => "running".into(),
        rightkit_agent_coding_tools::background_shell::BgStatus::Exited(_) => {
            "exited".into()
        }
        rightkit_agent_coding_tools::background_shell::BgStatus::Killed => "killed".into(),
        rightkit_agent_coding_tools::background_shell::BgStatus::SpawnFailed(_) => {
            "spawnFailed".into()
        }
        rightkit_agent_coding_tools::background_shell::BgStatus::Unknown => "unknown".into(),
    }
}

fn task_output_preview(output: &str) -> String {
    const MAX_CHARS: usize = 2_000;
    let mut preview = output.chars().rev().take(MAX_CHARS).collect::<Vec<_>>();
    preview.reverse();
    preview.into_iter().collect()
}

pub(crate) fn workspace_tasks_path(root: &FsPath) -> PathBuf {
    // The file the `task_tools` of rightkit-agent-tools write (`<workspace>/.rightkit/tasks.json`).
    root.join(".rightkit").join("tasks.json")
}

pub(crate) fn load_workspace_task_records(
    root: &FsPath,
) -> Result<HashMap<String, rightkit_agent_coding_tools::task_tools::TaskRecord>, String> {
    let path = workspace_tasks_path(root);
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read tasks '{}': {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("invalid tasks '{}': {e}", path.display()))
}
