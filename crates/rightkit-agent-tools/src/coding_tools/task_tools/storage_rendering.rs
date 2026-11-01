fn string_array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_state(value: &str) -> Option<TaskState> {
    match value.to_ascii_lowercase().replace('-', "_").as_str() {
        "open" => Some(TaskState::Open),
        "in_progress" | "inprogress" => Some(TaskState::InProgress),
        "blocked" => Some(TaskState::Blocked),
        "done" => Some(TaskState::Done),
        "failed" => Some(TaskState::Failed),
        "cancelled" | "canceled" => Some(TaskState::Cancelled),
        "escalated" => Some(TaskState::Escalated),
        "split" => Some(TaskState::Split),
        "merged" => Some(TaskState::Merged),
        _ => None,
    }
}

fn is_terminal_state(state: &TaskState) -> bool {
    matches!(
        state,
        TaskState::Done | TaskState::Failed | TaskState::Cancelled | TaskState::Merged
    )
}

fn tasks_path(ctx: &ToolContext) -> PathBuf {
    ctx.workspace_root.join(".rightkit").join("tasks.json")
}

fn load_effective_records(ctx: &ToolContext) -> Result<HashMap<String, TaskRecord>, String> {
    let mut merged = HashMap::new();
    let path = tasks_path(ctx);
    if path.exists() {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read tasks '{}': {e}", path.display()))?;
        merged = serde_json::from_str(&text)
            .map_err(|e| format!("Invalid tasks JSON '{}': {e}", path.display()))?;
    }
    if let Some(store) = &ctx.task_store {
        for (id, record) in store.lock().unwrap().iter() {
            merged.insert(id.clone(), record.clone());
        }
    }
    Ok(merged)
}

fn save_records(ctx: &ToolContext, records: &HashMap<String, TaskRecord>) -> Result<(), String> {
    let path = tasks_path(ctx);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create tasks directory '{}': {e}",
                parent.display()
            )
        })?;
    }
    let text = serde_json::to_string_pretty(records)
        .map_err(|e| format!("Failed to serialize tasks: {e}"))?;
    std::fs::write(&path, text)
        .map_err(|e| format!("Failed to write tasks '{}': {e}", path.display()))
}

fn set_memory_records(ctx: &ToolContext, records: HashMap<String, TaskRecord>) {
    if let Some(store) = &ctx.task_store {
        *store.lock().unwrap() = records;
    }
}

fn render_record(record: &TaskRecord) -> String {
    let mut out = render_record_brief(record);
    if !record.node.done_when.is_empty() {
        out.push_str(&format!("\ndone_when: {}", record.node.done_when));
    }
    if let Some(command) = &record.command {
        out.push_str(&format!("\ncommand: {command}"));
    }
    if let Some(shell_id) = &record.shell_id {
        out.push_str(&format!("\nshell_id: {shell_id}"));
    }
    if !record.output.trim().is_empty() {
        out.push_str(&format!("\noutput:\n{}", record.output));
    }
    out
}

fn render_record_brief(record: &TaskRecord) -> String {
    format!(
        "{} [{} {:?}] {}",
        record.node.id, record.kind, record.node.state, record.node.title
    )
}

fn shell_is_attached(ctx: &ToolContext, shell_id: &str) -> bool {
    #[cfg(feature = "coding_tools-host")]
    {
        ctx.background_shells
            .as_ref()
            .map(|store| store.lock().unwrap().contains_key(shell_id))
            .unwrap_or(false)
    }
    // No shells can be spawned here, so a journalled shell_id from another
    // host is never attached to this one.
    #[cfg(not(feature = "coding_tools-host"))]
    {
        let _ = (ctx, shell_id);
        false
    }
}

fn historical_output_body(record: &TaskRecord, shell_id: &str) -> String {
    if record.output.trim().is_empty() {
        format!(
            "(no stored output recorded for detached shell task '{}', shell_id: {shell_id})",
            record.node.id
        )
    } else {
        record.output.clone()
    }
}

fn now_string() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    format!("unix-ms:{millis}")
}
