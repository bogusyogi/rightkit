pub(crate) fn command_schedules(args: &[String]) -> CommandResult {
    let root = command_workspace_arg(args);
    let schedules = match load_api_schedules(&root) {
        Ok(value) => value,
        Err(e) => return command_err(format!("Failed to load schedules: {e}")),
    };
    let runs = load_api_schedule_runs(&root).unwrap_or_default();
    let mut rows: Vec<_> = schedules
        .values()
        .map(|schedule| {
            let latest = schedule_runs_for(&runs, &schedule.id)
                .first()
                .and_then(|v| v["status"].as_str().map(str::to_owned))
                .unwrap_or_else(|| "never run".into());
            format!(
                "- {} `{}` {} ({latest})",
                schedule.id, schedule.cron_expr, schedule.time_zone
            )
        })
        .collect();
    rows.sort();
    let mut output = format!(
        "**Schedules:** {} in {}\nStore: {}",
        rows.len(),
        root.display(),
        schedules_store_path(&root).display()
    );
    if rows.is_empty() {
        output.push_str("\nNo schedules.");
    } else {
        output.push('\n');
        output.push_str(&rows.join("\n"));
    }
    command_ok(output)
}

pub(crate) fn command_tasks(args: &[String]) -> CommandResult {
    let root = command_workspace_arg(args);
    let records = match load_workspace_task_records(&root) {
        Ok(value) => value,
        Err(e) => return command_err(format!("Failed to load tasks: {e}")),
    };
    let mut rows: Vec<_> = records
        .values()
        .map(|record| {
            format!(
                "- {} [{}] {}",
                record.node.id,
                record.kind,
                record
                    .command
                    .clone()
                    .unwrap_or_else(|| record.node.title.clone())
            )
        })
        .collect();
    rows.sort();
    let mut output = format!(
        "**Tasks:** {} in {}\nStore: {}",
        rows.len(),
        root.display(),
        workspace_tasks_path(&root).display()
    );
    if rows.is_empty() {
        output.push_str("\nNo persisted workspace task records.");
    } else {
        output.push('\n');
        output.push_str(&rows.join("\n"));
    }
    command_ok(output)
}
