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
    ctx.workspace_root
        .join(rightkit_agent_config::state_dir_name())
        .join("tasks.json")
}

/// `unix-ms:N` stamp as a number; 0 for anything else.
fn record_stamp(record: &TaskRecord) -> u128 {
    record
        .node
        .updated_at
        .strip_prefix("unix-ms:")
        .and_then(|ms| ms.parse().ok())
        .unwrap_or(0)
}

/// Serializes the read-merge-write of `tasks.json` between the parent and its parallel children
/// (host option `task_store_merged`).
static TASKS_FILE_LOCK: Mutex<()> = Mutex::new(());

fn read_tasks_file(path: &std::path::Path) -> Result<HashMap<String, TaskRecord>, String> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read tasks '{}': {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("Invalid tasks JSON '{}': {e}", path.display()))
}

fn load_effective_records(ctx: &ToolContext) -> Result<HashMap<String, TaskRecord>, String> {
    let merged_store = crate::tool_options::task_store_merged();
    let mut merged = read_tasks_file(&tasks_path(ctx))?;
    if let Some(store) = &ctx.task_store {
        for (id, record) in store.lock().unwrap().iter() {
            // With host option `task_store_merged`, a copy kept in memory never rolls back a newer
            // write by another session.
            if !merged_store
                || merged
                    .get(id)
                    .is_none_or(|on_disk| record_stamp(record) >= record_stamp(on_disk))
            {
                merged.insert(id.clone(), record.clone());
            }
        }
    }
    Ok(merged)
}

/// Writes the records. With host option `task_store_merged` they are written under a lock, merged
/// with what is on disk now, so a task another session created or changed since this caller loaded
/// is kept, and the file is replaced atomically; otherwise the file is overwritten as it is.
fn save_records(
    ctx: &ToolContext,
    records: &mut HashMap<String, TaskRecord>,
) -> Result<(), String> {
    let path = tasks_path(ctx);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create tasks directory '{}': {e}",
                parent.display()
            )
        })?;
    }
    if !crate::tool_options::task_store_merged() {
        let text = serde_json::to_string_pretty(&*records)
            .map_err(|e| format!("Failed to serialize tasks: {e}"))?;
        return std::fs::write(&path, text)
            .map_err(|e| format!("Failed to write tasks '{}': {e}", path.display()));
    }
    let _guard = TASKS_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Two processes on one workspace (two terminals, or a terminal and the daemon) would each
    // read, merge and replace the file; the lock file makes that one at a time across processes.
    let _file_guard = lock_tasks_file(&path)?;
    for (id, on_disk) in read_tasks_file(&path).unwrap_or_default() {
        match records.get(&id) {
            Some(mine) if record_stamp(mine) >= record_stamp(&on_disk) => {}
            _ => {
                records.insert(id, on_disk);
            }
        }
    }
    let text = serde_json::to_string_pretty(&*records)
        .map_err(|e| format!("Failed to serialize tasks: {e}"))?;
    rightkit_fs::atomic_write(&path, text.as_bytes())
        .map_err(|e| format!("Failed to write tasks '{}': {e}", path.display()))
}

/// Holds an exclusive lock on `tasks.json.lock` until dropped (the OS releases it if the process dies).
fn lock_tasks_file(path: &std::path::Path) -> Result<std::fs::File, String> {
    let lock_path = path.with_extension("json.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("Failed to open tasks lock '{}': {e}", lock_path.display()))?;
    file.lock()
        .map_err(|e| format!("Failed to lock tasks '{}': {e}", lock_path.display()))?;
    Ok(file)
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
    #[cfg(feature = "host")]
    {
        ctx.background_shells
            .as_ref()
            .map(|store| store.lock().unwrap().contains_key(shell_id))
            .unwrap_or(false)
    }
    // No shells can be spawned here, so a journalled shell_id from another
    // host is never attached to this one.
    #[cfg(not(feature = "host"))]
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
