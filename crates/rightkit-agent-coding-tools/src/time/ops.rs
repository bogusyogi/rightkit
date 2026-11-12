fn current_time(args: &Value) -> ToolResult {
    let unix = now_unix();

    // Build list of timezones to show.
    let mut zones: Vec<String> = Vec::new();

    if let Some(arr) = args.get("timezones").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(s) = v.as_str() {
                zones.push(s.to_string());
            }
        }
    }
    if let Some(tz) = args.get("timezone").and_then(|v| v.as_str()) {
        if !zones.contains(&tz.to_string()) {
            zones.push(tz.to_string());
        }
    }
    if zones.is_empty() {
        zones.push("UTC".to_string());
    }

    let mut results = serde_json::Map::new();
    for tz in &zones {
        match tz_offset(tz) {
            Ok((offset, label)) => {
                results.insert(
                    tz.clone(),
                    serde_json::Value::String(apply_offset(unix, offset, label)),
                );
            }
            Err(e) => {
                results.insert(tz.clone(), serde_json::Value::String(format!("error: {e}")));
            }
        }
    }

    let utc = from_unix(unix);
    let out = serde_json::json!({
        "unix_timestamp": unix,
        "utc": utc.format_iso(),
        "utc_human": utc.format_human(),
        "times": results,
    });
    ToolResult::ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

fn convert_time(args: &Value) -> ToolResult {
    let dt_str = match args.get("datetime").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ToolResult::error("'datetime' is required for convert operation"),
    };
    let to_tz = match args.get("to_timezone").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ToolResult::error("'to_timezone' is required for convert operation"),
    };

    // Parse ISO 8601 datetime → unix.
    let unix = match parse_iso8601(dt_str) {
        Some(u) => u,
        None => {
            return ToolResult::error(format!(
                "cannot parse datetime '{dt_str}'; use ISO 8601 e.g. '2026-06-02T14:30:00Z'"
            ))
        }
    };

    // If from_timezone is provided and the input has no 'Z' or offset, apply it.
    let unix = if dt_str.ends_with('Z')
        || dt_str.contains('+')
        || (dt_str.len() > 19 && dt_str.chars().nth(19) == Some('-'))
    {
        unix
    } else if let Some(from_tz) = args.get("from_timezone").and_then(|v| v.as_str()) {
        match tz_offset(from_tz) {
            Ok((offset, _)) => unix - offset, // convert local → UTC
            Err(e) => return ToolResult::error(e),
        }
    } else {
        unix // assume UTC
    };

    match tz_offset(to_tz) {
        Ok((offset, label)) => {
            let converted = apply_offset(unix, offset, label);
            ToolResult::ok(
                serde_json::json!({
                    "input": dt_str,
                    "to_timezone": to_tz,
                    "result": converted,
                    "unix_timestamp": unix,
                })
                .to_string(),
            )
        }
        Err(e) => ToolResult::error(e),
    }
}

fn time_diff(args: &Value) -> ToolResult {
    let start_str = match args.get("start").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ToolResult::error("'start' is required for diff operation"),
    };
    let end_str = match args.get("end").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ToolResult::error("'end' is required for diff operation"),
    };

    let start = match parse_iso8601(start_str) {
        Some(u) => u,
        None => return ToolResult::error(format!("cannot parse start datetime '{start_str}'")),
    };
    let end = match parse_iso8601(end_str) {
        Some(u) => u,
        None => return ToolResult::error(format!("cannot parse end datetime '{end_str}'")),
    };

    let diff = end - start;
    let abs = diff.unsigned_abs();
    let days = abs / 86400;
    let hours = (abs % 86400) / 3600;
    let minutes = (abs % 3600) / 60;
    let seconds = abs % 60;
    let direction = if diff >= 0 { "future" } else { "past" };

    ToolResult::ok(
        serde_json::json!({
            "start": start_str,
            "end": end_str,
            "direction": direction,
            "total_seconds": diff,
            "breakdown": {
                "days": days,
                "hours": hours,
                "minutes": minutes,
                "seconds": seconds,
            },
            "human": format!("{days}d {hours}h {minutes}m {seconds}s ({direction})"),
        })
        .to_string(),
    )
}
