#[async_trait::async_trait]
impl Tool for TaskUpdateTool {
    fn name(&self) -> &str {
        "task_update"
    }

    fn description(&self) -> &str {
        "Update a durable task record's title, state, done_when, assigned_role, category, or appended output."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": { "type": "string" },
                "title": { "type": "string" },
                "state": { "type": "string", "enum": ["open", "in_progress", "blocked", "done", "failed", "escalated", "split", "merged"] },
                "done_when": { "type": "string" },
                "assigned_role": { "type": "string" },
                "category": { "type": "string" },
                "append_output": { "type": "string" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let id = match args["id"].as_str() {
            Some(id) => id,
            None => return ToolResult::error("Missing required argument: id"),
        };
        let mut records = match load_effective_records(ctx) {
            Ok(records) => records,
            Err(e) => return ToolResult::error(e),
        };
        let Some(record) = records.get_mut(id) else {
            return ToolResult::error(format!("No task with id '{id}'."));
        };
        if let Some(title) = args["title"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            record.node.title = title.to_string();
        }
        if let Some(state) = args["state"].as_str() {
            record.node.state = match parse_state(state) {
                Some(state) => state,
                None => return ToolResult::error(format!("Invalid task state '{state}'.")),
            };
        }
        if let Some(done_when) = args["done_when"].as_str() {
            record.node.done_when = done_when.to_string();
        }
        if let Some(role) = args["assigned_role"].as_str() {
            record.node.assigned_role = role.to_string();
        }
        if let Some(category) = args["category"].as_str() {
            record.node.category = category.to_string();
        }
        if let Some(output) = args["append_output"].as_str() {
            record.output.push_str(output);
        }
        record.node.updated_at = now_string();
        let record = record.clone();
        if let Err(e) = save_records(ctx, &records) {
            return ToolResult::error(e);
        }
        set_memory_records(ctx, records);
        ToolResult::ok(render_record(&record))
    }
}

#[async_trait::async_trait]
impl Tool for TaskGetTool {
    fn name(&self) -> &str {
        "task_get"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Return one durable task record by id."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["id"],
            "properties": { "id": { "type": "string" } }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let id = match args["id"].as_str() {
            Some(id) => id,
            None => return ToolResult::error("Missing required argument: id"),
        };
        let records = match load_effective_records(ctx) {
            Ok(records) => records,
            Err(e) => return ToolResult::error(e),
        };
        match records.get(id) {
            Some(record) => ToolResult::ok(render_record(record)),
            None => ToolResult::error(format!("No task with id '{id}'.")),
        }
    }
}

#[async_trait::async_trait]
impl Tool for TaskListTool {
    fn name(&self) -> &str {
        "task_list"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "List durable task records for the current workspace."
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    async fn execute(&self, _args: Value, ctx: &ToolContext) -> ToolResult {
        let records = match load_effective_records(ctx) {
            Ok(records) => records,
            Err(e) => return ToolResult::error(e),
        };
        if records.is_empty() {
            return ToolResult::ok("No tasks registered.");
        }
        let mut rows: Vec<_> = records.values().map(render_record_brief).collect();
        rows.sort();
        ToolResult::ok(format!("Tasks:\n{}", rows.join("\n")))
    }
}

#[async_trait::async_trait]
impl Tool for TaskOutputTool {
    fn name(&self) -> &str {
        "task_output"
    }

    fn description(&self) -> &str {
        "Return output for a task. For shell-backed tasks, polls the background shell and appends new output to the task record."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["id"],
            "properties": { "id": { "type": "string" } }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let id = match args["id"].as_str() {
            Some(id) => id,
            None => return ToolResult::error("Missing required argument: id"),
        };
        let mut records = match load_effective_records(ctx) {
            Ok(records) => records,
            Err(e) => return ToolResult::error(e),
        };
        let Some(record) = records.get_mut(id) else {
            return ToolResult::error(format!("No task with id '{id}'."));
        };
        if let Some(shell_id) = &record.shell_id {
            #[cfg(not(feature = "host"))]
            let _ = shell_id;
            #[cfg(feature = "host")]
            if shell_is_attached(ctx, shell_id) {
                let polled = GetShellOutputTool
                    .execute(json!({ "shell_id": shell_id }), ctx)
                    .await;
                if polled.is_error {
                    return polled;
                }
                record.output.push_str(&polled.content);
                if !record.output.ends_with('\n') {
                    record.output.push('\n');
                }
                if polled.content.contains("[exited (code 0)]") {
                    record.node.state = TaskState::Done;
                } else if polled.content.contains("[killed]")
                    || polled.content.contains("[exited (code ")
                {
                    record.node.state = TaskState::Failed;
                }
                record.node.updated_at = now_string();
            } else {
                let mut body = historical_output_body(record, shell_id);
                if let Err(e) = save_records(ctx, &records) {
                    return ToolResult::error(e);
                }
                set_memory_records(ctx, records);
                if !body.ends_with('\n') {
                    body.push('\n');
                }
                body.push_str(
                    "[detached] Background shell is not attached to this daemon; returning stored task output only.",
                );
                return ToolResult::ok(body);
            }
        }
        let body = if record.output.trim().is_empty() {
            "(no task output recorded)".to_string()
        } else {
            record.output.clone()
        };
        if let Err(e) = save_records(ctx, &records) {
            return ToolResult::error(e);
        }
        set_memory_records(ctx, records);
        ToolResult::ok(body)
    }
}

#[async_trait::async_trait]
impl Tool for TaskStopTool {
    fn name(&self) -> &str {
        "task_stop"
    }

    fn description(&self) -> &str {
        "Stop an active shell-backed task. Non-shell records are not mutated; use task_update for manual lifecycle changes."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["id"],
            "properties": { "id": { "type": "string" } }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let id = match args["id"].as_str() {
            Some(id) => id,
            None => return ToolResult::error("Missing required argument: id"),
        };
        let mut records = match load_effective_records(ctx) {
            Ok(records) => records,
            Err(e) => return ToolResult::error(e),
        };
        let Some(record) = records.get_mut(id) else {
            return ToolResult::error(format!("No task with id '{id}'."));
        };
        if is_terminal_state(&record.node.state) {
            return ToolResult::ok(format!(
                "Task '{id}' is already {:?}; no stop needed.",
                record.node.state
            ));
        }
        let Some(shell_id) = record.shell_id.clone() else {
            return ToolResult::error(format!(
                "task_stop only supports active shell-backed tasks. Task '{id}' is kind '{}'; use task_update to change its state.",
                record.kind
            ));
        };
        if !shell_is_attached(ctx, &shell_id) {
            return ToolResult::error(format!(
                "Cannot stop task '{id}' because shell '{shell_id}' is not attached to this daemon; stored output remains available via task_output."
            ));
        }
        #[cfg(feature = "host")]
        {
            let stopped = KillShellTool
                .execute(json!({ "shell_id": shell_id }), ctx)
                .await;
            if stopped.is_error {
                return stopped;
            }
            record.node.state = TaskState::Cancelled;
            record.node.updated_at = now_string();
            record.output.push_str(&stopped.content);
            record.output.push('\n');
            if let Err(e) = save_records(ctx, &records) {
                return ToolResult::error(e);
            }
            set_memory_records(ctx, records);
            ToolResult::ok(format!("Stopped task '{id}'.\n{}", stopped.content))
        }
        // Unreachable: the `shell_is_attached` guard above always returns first.
        #[cfg(not(feature = "host"))]
        {
            let _ = (&mut records, &shell_id);
            ToolResult::error(format!("Cannot stop task '{id}' on a host without shells."))
        }
    }
}
