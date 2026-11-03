fn write_schedule(root: &Path, id: &str, cron_expr: &str, prompt: &str) {
    write_schedule_with_time_zone(root, id, cron_expr, prompt, "UTC");
}

fn write_schedule_with_time_zone(
    root: &Path,
    id: &str,
    cron_expr: &str,
    prompt: &str,
    time_zone: &str,
) {
    let path = schedules_path(root);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut schedules = HashMap::new();
    schedules.insert(
        id.to_string(),
        ScheduleEntry {
            id: id.to_string(),
            cron_expr: cron_expr.to_string(),
            prompt: prompt.to_string(),
            time_zone: time_zone.to_string(),
            skills: Vec::new(),
            workdir: None,
            toolset: None,
            execution_backend: None,
            delivery: Vec::new(),
            script_allowlist: Vec::new(),
            mode: default_job_mode(),
            paused: false,
            source_recipe_id: None,
            recipe_params: BTreeMap::new(),
            max_runtime_seconds: None,
        },
    );
    std::fs::write(path, serde_json::to_string_pretty(&schedules).unwrap()).unwrap();
}

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "coderight-{name}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn epoch_minute_from_civil(year: i32, month: u32, day: u32, hour: u8, minute: u8) -> i64 {
    rightkit_time::days_from_civil(i64::from(year), month, day) * 1440
        + hour as i64 * 60
        + minute as i64
}

fn base_entry(id: &str) -> ScheduleEntry {
    ScheduleEntry {
        id: id.to_string(),
        cron_expr: "* * * * *".to_string(),
        prompt: "do something".to_string(),
        time_zone: "UTC".to_string(),
        skills: Vec::new(),
        workdir: None,
        toolset: None,
        execution_backend: None,
        delivery: Vec::new(),
        script_allowlist: Vec::new(),
        mode: "agent".to_string(),
        paused: false,
        source_recipe_id: None,
        recipe_params: BTreeMap::new(),
        max_runtime_seconds: None,
    }
}
