fn default_time_zone() -> String {
    "UTC".to_string()
}

fn default_job_mode() -> String {
    "agent".to_string()
}

fn schedules_path(ctx: &ToolContext) -> PathBuf {
    ctx.workspace_root.join(".rightkit").join("schedules.json")
}

fn load_workspace_schedules(ctx: &ToolContext) -> Result<HashMap<String, ScheduleEntry>, String> {
    let path = schedules_path(ctx);
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read schedules '{}': {e}", path.display()))?;
    serde_json::from_str::<HashMap<String, ScheduleEntry>>(&text)
        .map_err(|e| format!("Invalid schedules JSON '{}': {e}", path.display()))
}

fn save_workspace_schedules(
    ctx: &ToolContext,
    schedules: &HashMap<String, ScheduleEntry>,
) -> Result<(), String> {
    let path = schedules_path(ctx);
    let text = serde_json::to_string_pretty(schedules)
        .map_err(|e| format!("Failed to serialize schedules: {e}"))?;
    rightkit_fs::atomic_write_with(
        &path,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..rightkit_fs::AtomicOptions::default()
        },
        |file| std::io::Write::write_all(file, text.as_bytes()),
    )
    .map_err(|e| format!("Failed to write schedules '{}': {e}", path.display()))
}

/// Parse a 5-field cron expression into a CronSchedule.
fn parse_cron(expr: &str) -> Result<CronSchedule, String> {
    let parts: Vec<&str> = expr.split_whitespace().collect();

    if parts.len() != 5 {
        return Err("Cron expression must have exactly 5 fields".to_string());
    }

    Ok(CronSchedule {
        minute: parse_cron_field(parts[0], 0, 59)?,
        hour: parse_cron_field(parts[1], 0, 23)?,
        day: parse_cron_field(parts[2], 1, 31)?,
        month: parse_cron_field(parts[3], 1, 12)?,
        weekday: parse_cron_field(parts[4], 0, 6)?,
    })
}

/// Parse a single cron field (handles *, numbers, ranges, steps).
fn parse_cron_field(field: &str, min: u8, max: u8) -> Result<Vec<u8>, String> {
    if field == "*" {
        return Ok((min..=max).collect());
    }

    // Handle step values (e.g., */5, 0-30/5)
    if field.contains('/') {
        let parts: Vec<&str> = field.split('/').collect();
        if parts.len() != 2 {
            return Err(format!("Invalid step syntax: {}", field));
        }

        let step: u8 = parts[1]
            .parse()
            .map_err(|_| format!("Invalid step value: {}", parts[1]))?;

        let base_field = if parts[0] == "*" {
            (min..=max).collect::<Vec<_>>()
        } else {
            parse_cron_field(parts[0], min, max)?
        };

        return Ok(base_field.iter().step_by(step as usize).copied().collect());
    }

    // Handle ranges (e.g., 0-5)
    if field.contains('-') {
        let parts: Vec<&str> = field.split('-').collect();
        if parts.len() != 2 {
            return Err(format!("Invalid range syntax: {}", field));
        }

        let start: u8 = parts[0]
            .parse()
            .map_err(|_| format!("Invalid range start: {}", parts[0]))?;
        let end: u8 = parts[1]
            .parse()
            .map_err(|_| format!("Invalid range end: {}", parts[1]))?;

        if start > end || start < min || end > max {
            return Err(format!(
                "Range {}-{} out of valid bounds {}-{}",
                start, end, min, max
            ));
        }

        return Ok((start..=end).collect());
    }

    // Handle single values
    let value: u8 = field
        .parse()
        .map_err(|_| format!("Invalid value: {}", field))?;

    if value < min || value > max {
        return Err(format!(
            "Value {} out of valid bounds {}-{}",
            value, min, max
        ));
    }

    Ok(vec![value])
}

// Simple UUID generation fallback (since uuid crate may not be available)
mod uuid {
    pub struct Uuid(String);

    impl Uuid {
        pub fn new_v4() -> Self {
            use std::time::{SystemTime, UNIX_EPOCH};
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .subsec_nanos();
            Uuid(format!("{:08x}-{:04x}", nanos, nanos >> 16))
        }

        pub fn to_string(&self) -> String {
            self.0.clone()
        }
    }
}
