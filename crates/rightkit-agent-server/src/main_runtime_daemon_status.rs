// ── `{app} status|stop|ps|agents|resume|fork` ────────────────────────────

async fn daemon_status() -> i32 {
    let resolution = resolve_daemon_state();
    let (base_url, token) = match resolution {
        DaemonResolution::Resolved(resolved) => resolved,
        DaemonResolution::Stopped => {
            emit_cli_json_line(&status_stopped_json());
            return 0;
        }
        DaemonResolution::Unreachable { base_url, error } => {
            emit_cli_json_line(&status_unreachable_json(&base_url, &error));
            return 0;
        }
    };

    let client = match daemon_status_client(&base_url) {
        Ok(client) => client,
        Err(error) => {
            emit_cli_json_line(&status_unreachable_json(&base_url, &error));
            return 0;
        }
    };
    let health = match rightkit_http::http_client::send_any(
        &client,
        &rightkit_http::Request::get(format!("{base_url}/health")),
    )
    .await
    {
        Ok(resp) if rightkit_http::http_client::is_success(resp.status) => resp
            .json::<serde_json::Value>()
            .unwrap_or(serde_json::Value::Null),
        Ok(resp) => {
            emit_cli_json_line(&status_degraded_json(
                &base_url,
                &format!("daemon health probe returned {}", resp.status),
            ));
            return 1;
        }
        Err(e) => {
            emit_cli_json_line(&status_unreachable_json(&base_url, &e));
            return 0;
        }
    };

    let authed_get = |path: &str| {
        let request = rightkit_http::Request::get(format!("{base_url}{path}")).bearer(&token);
        let client = &client;
        async move { rightkit_http::http_client::send_any(client, &request).await }
    };

    let sessions = match authed_get("/sessions").await {
        Ok(resp) if rightkit_http::http_client::is_success(resp.status) => resp
            .json::<serde_json::Value>()
            .unwrap_or_else(|_| serde_json::json!([])),
        Ok(resp) => {
            emit_cli_json_line(&status_degraded_json(
                &base_url,
                &format!("daemon sessions probe returned {}", resp.status),
            ));
            return 1;
        }
        Err(e) => {
            emit_cli_json_line(&status_unreachable_json(&base_url, &e));
            return 1;
        }
    };

    let session_count = sessions.as_array().map(Vec::len).unwrap_or(0);
    let mut status = serde_json::json!({
        "running": true,
        "status": daemon_machine_status_from_health(&health).as_str(),
        "daemonUrl": base_url,
        "health": health,
        "sessionCount": session_count,
        "sessions": sessions,
    });
    // Reuse the daemon's authenticated native binding projection. A missing
    // metrics response is retained as unavailable; status never invents a
    // healthy context from daemon reachability alone.
    if let Ok(resp) = authed_get("/runtime/resource-metrics").await {
        if rightkit_http::http_client::is_success(resp.status) {
            if let Ok(metrics) = resp.json::<serde_json::Value>() {
                status["context"] =
                    serde_json::to_value(crate::context_status::from_metrics(&metrics))
                        .unwrap_or_else(|_| serde_json::json!({"state":"unavailable"}));
            }
        }
    }
    // Client-presence passthrough (client-presence v1): older daemons without
    // the `/clients` route simply omit this key rather than failing status.
    if let Ok(resp) = authed_get("/clients").await {
        if rightkit_http::http_client::is_success(resp.status) {
            if let Ok(clients) = resp.json::<serde_json::Value>() {
                status["clients"] = clients;
            }
        }
    }
    emit_cli_json_line(&status);
    0
}

/// DL-014: `{app} stop`. Routes directly through the shared `authority()`
/// singleton's `Stop` command; no independent stop path is implemented here.
async fn daemon_stop(force: bool) -> i32 {
    use rightkit_agent_daemon::LifecycleCommand;

    match crate::daemon::lifecycle_backend::authority()
        .execute_cli(LifecycleCommand::Stop { force })
        .await
    {
        Ok(report) => {
            println!("{}", report.detail);
            0
        }
        Err(error) => {
            eprintln!("Failed to stop daemon: {error}");
            1
        }
    }
}

/// Status probes verify the complete session projection for upgrade safety;
/// persisted history can exceed a liveness probe's 500 ms budget.
fn daemon_status_client(base_url: &str) -> Result<rightkit_http::AsyncClient, String> {
    rightkit_http::http_client::daemon_client_probe(
        base_url,
        std::time::Duration::from_millis(300),
        std::time::Duration::from_secs(5),
    )
}

fn status_unreachable_json(base_url: &str, error: &str) -> serde_json::Value {
    serde_json::json!({
        "running": false,
        "status": DaemonMachineStatus::Unreachable.as_str(),
        "daemonUrl": base_url,
        "error": error,
    })
}

fn status_degraded_json(base_url: &str, error: &str) -> serde_json::Value {
    serde_json::json!({
        "running": true,
        "status": DaemonMachineStatus::Degraded.as_str(),
        "daemonUrl": base_url,
        "error": error,
    })
}

fn status_stopped_json() -> serde_json::Value {
    serde_json::json!({
        "running": false,
        "status": DaemonMachineStatus::Stopped.as_str(),
    })
}

fn daemon_machine_status_from_health(health: &serde_json::Value) -> DaemonMachineStatus {
    if health.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        DaemonMachineStatus::Healthy
    } else {
        DaemonMachineStatus::Degraded
    }
}

async fn daemon_ps() -> i32 {
    daemon_session_list(None, None).await
}

async fn daemon_agents(session_id: Option<&str>, json: bool, all: bool, cwd: bool) -> i32 {
    if let Some(session_id) = session_id.map(str::trim).filter(|s| !s.is_empty()) {
        return daemon_get_pretty_json(
            &format!("/sessions/{}/agents", url_encode_component(session_id)),
            "session agents",
        )
        .await;
    }

    let sessions = match daemon_get_json_value("/sessions", "sessions").await {
        Ok(value) => value.as_array().cloned().unwrap_or_default(),
        Err(code) => return code,
    };
    let rows = filter_agent_session_rows(sessions, all, cwd);
    if json {
        let processed_rows: Vec<serde_json::Value> = rows
            .into_iter()
            .map(|mut row| {
                if let Some(updated_at) = row.get("updatedAt").and_then(|v| v.as_str()) {
                    row["lastActivity"] = serde_json::Value::String(updated_at.to_string());
                }
                row
            })
            .collect();
        emit_cli_json_line(&serde_json::Value::Array(processed_rows));
    } else if rows.is_empty() {
        println!("No background sessions found.");
    } else {
        for row in rows {
            let id = row["id"].as_str().unwrap_or("-");
            let status = row["status"].as_str().unwrap_or("-");
            let model = row["model"].as_str().unwrap_or("-");
            let last_activity = row["updatedAt"].as_str().unwrap_or("-");
            let title = row["title"].as_str().unwrap_or("");
            let workspace = row["workspaceRoot"].as_str().unwrap_or("");
            // Unknown/unpriced cost prints as "-", never "0" or "0.00": an
            // unpriced model must read as unknown, not free.
            let cost = match row.get("costUsd").and_then(|v| v.as_f64()) {
                Some(usd) => format!("${usd:.4}"),
                None => "-".to_string(),
            };
            println!("{id}\t{status}\t{model}\t{cost}\t{last_activity}\t{title}\t{workspace}");
        }
    }
    0
}

async fn daemon_resume(
    session_id: Option<&str>,
    last: bool,
    all: bool,
    all_workspaces: bool,
) -> i32 {
    if let Some(id) = session_id.map(str::trim).filter(|id| !id.is_empty()) {
        return daemon_attach(id).await;
    }
    if !last {
        eprintln!("Usage: {app} resume <session-id> or {app} resume --last", app = crate::host_identity::app());
        return 2;
    }

    let rows = match daemon_get_json_value("/sessions", "sessions").await {
        Ok(value) => value.as_array().cloned().unwrap_or_default(),
        Err(code) => return code,
    };
    let cwd = if all_workspaces {
        None
    } else {
        std::env::current_dir()
            .ok()
            .map(|path| path.display().to_string())
    };
    let Some(id) = select_last_resume_session_id(&rows, all, cwd.as_deref()) else {
        eprintln!("No matching session found to resume.");
        return 1;
    };
    daemon_attach(&id).await
}

async fn daemon_fork(
    session_id: Option<&str>,
    last: bool,
    all: bool,
    all_workspaces: bool,
    title: Option<&str>,
) -> i32 {
    let parent_id = if let Some(id) = session_id.map(str::trim).filter(|id| !id.is_empty()) {
        id.to_string()
    } else if last {
        let rows = match daemon_get_json_value("/sessions", "sessions").await {
            Ok(value) => value.as_array().cloned().unwrap_or_default(),
            Err(code) => return code,
        };
        let cwd = if all_workspaces {
            None
        } else {
            std::env::current_dir()
                .ok()
                .map(|path| path.display().to_string())
        };
        match select_last_resume_session_id(&rows, all, cwd.as_deref()) {
            Some(id) => id,
            None => {
                eprintln!("No matching session found to fork.");
                return 1;
            }
        }
    } else {
        eprintln!("Usage: {app} fork <session-id> or {app} fork --last", app = crate::host_identity::app());
        return 2;
    };

    let mut body = serde_json::json!({});
    if let Some(title) = title.map(str::trim).filter(|value| !value.is_empty()) {
        body["title"] = serde_json::json!(title);
    }
    daemon_post_pretty_json(
        &format!("/sessions/{}/fork", url_encode_component(&parent_id)),
        body,
        "session fork",
    )
    .await
}

fn select_last_resume_session_id(
    rows: &[serde_json::Value],
    all: bool,
    cwd_path: Option<&str>,
) -> Option<String> {
    rows.iter()
        .filter(|row| all || !matches!(row["status"].as_str(), Some("archived" | "deleted")))
        .filter(|row| {
            if let Some(cwd_path) = cwd_path {
                same_cli_path(row["workspaceRoot"].as_str().unwrap_or(""), cwd_path)
            } else {
                true
            }
        })
        .filter_map(|row| {
            let id = row["id"].as_str().filter(|id| !id.trim().is_empty())?;
            let updated_at = row["updatedAt"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| {
                    row["createdAt"]
                        .as_str()
                        .filter(|value| !value.trim().is_empty())
                })
                .unwrap_or("");
            Some((
                updated_at.to_string(),
                row["createdAt"].as_str().unwrap_or("").to_string(),
                id.to_string(),
            ))
        })
        .max()
        .map(|(_, _, id)| id)
}

fn filter_agent_session_rows(
    rows: Vec<serde_json::Value>,
    all: bool,
    cwd: bool,
) -> Vec<serde_json::Value> {
    let cwd_path = cwd
        .then(|| std::env::current_dir().ok())
        .flatten()
        .map(|p| p.display().to_string());
    rows.into_iter()
        .filter(|row| all || !matches!(row["status"].as_str(), Some("archived" | "deleted")))
        .filter(|row| {
            if let Some(cwd_path) = cwd_path.as_deref() {
                same_cli_path(row["workspaceRoot"].as_str().unwrap_or(""), cwd_path)
            } else {
                true
            }
        })
        .collect()
}
