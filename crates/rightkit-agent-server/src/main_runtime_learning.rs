async fn cli_learning(command: LearningCommand) -> i32 {
    let (path, label) = learning_command_request(command);
    daemon_post_pretty_json(&path, serde_json::json!({}), label).await
}

async fn cli_insights(session_ids: Vec<String>, scope: String, min_recurrence: u32) -> i32 {
    daemon_post_pretty_json(
        "/runtime/context/adapt/insights",
        serde_json::json!({
            "sessionIds": session_ids,
            "workspaceRoot": std::env::current_dir().ok().map(|path| path.to_string_lossy().to_string()),
            "scope": scope,
            "minRecurrence": min_recurrence,
        }),
        "insights",
    )
    .await
}

fn learning_command_request(command: LearningCommand) -> (String, &'static str) {
    let (proposal_id, action, label) = match command {
        LearningCommand::Approve { proposal_id } => (proposal_id, "approve", "learning approve"),
        LearningCommand::Reject { proposal_id } => (proposal_id, "reject", "learning reject"),
    };
    (
        format!(
            "/runtime/learning/{}/{}",
            url_encode_component(&proposal_id),
            action
        ),
        label,
    )
}

async fn skill_lifecycle_action(
    slug: &str,
    action: &str,
    reason: Option<String>,
    operator_override: bool,
) -> i32 {
    let body = serde_json::json!({
        "reason": reason,
        "operatorOverride": operator_override,
    });
    daemon_post_pretty_json(
        &format!("/skills/{}/{}", url_encode_component(slug), action),
        body,
        &format!("skill {action}"),
    )
    .await
}

async fn daemon_get_pretty_json(path: &str, label: &str) -> i32 {
    match daemon_get_json_value(path, label).await {
        Ok(value) => {
            print_json_value(value);
            0
        }
        Err(code) => code,
    }
}

async fn daemon_get_json_value(path: &str, label: &str) -> Result<serde_json::Value, i32> {
    // Bounded like `{app} ps`: a daemon that never answers must fail
    // loudly, not hang the command.
    let resolved = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        crate::cli_presence::resolve_daemon_or_autostart(),
    )
    .await;
    let (base_url, token) = match resolved {
        Ok(Some(p)) => p,
        Ok(None) => {
            eprintln!(
                "No running daemon found. Start one with `{app} serve`.",
                app = crate::host_identity::app()
            );
            return Err(1);
        }
        Err(_) => {
            eprintln!("Timed out after 60s waiting for the daemon to become ready.");
            return Err(1);
        }
    };

    let url = format!("{base_url}{path}");
    let fetched = match rightkit_http::http_client::daemon_client_with(
        &url,
        std::time::Duration::from_secs(30),
    ) {
        Ok(client) => {
            rightkit_http::http_client::send_any(
                &client,
                &rightkit_http::Request::get(url.clone()).bearer(&token),
            )
            .await
        }
        Err(e) => Err(e),
    };
    match fetched {
        Ok(resp) if rightkit_http::http_client::is_success(resp.status) => {
            let value = resp
                .json::<serde_json::Value>()
                .unwrap_or(serde_json::Value::Null);
            Ok(value)
        }
        Ok(resp) => {
            let status = resp.status;
            let body = resp.text();
            eprintln!("Daemon returned {status} for {label}: {body}");
            Err(1)
        }
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            Err(1)
        }
    }
}

/// One daemon call (`method` on `path`, JSON `body` when present) whose JSON reply is
/// pretty-printed. Exit code 0 on a 2xx reply, 1 otherwise. Shared by the POST, PUT,
/// PATCH and DELETE verbs of the CLI.
async fn daemon_json_request(
    method: rightkit_http::Method,
    path: &str,
    body: Option<serde_json::Value>,
    label: &str,
) -> i32 {
    let (base_url, token) = match crate::cli_presence::resolve_daemon_or_autostart().await {
        Some(p) => p,
        None => {
            eprintln!(
                "No running daemon found. Start one with `{app} serve`.",
                app = crate::host_identity::app()
            );
            return 1;
        }
    };

    match rightkit_http::http_client::daemon_call(
        method,
        &format!("{base_url}{path}"),
        &token,
        body.as_ref(),
    )
    .await
    {
        Ok(resp) if rightkit_http::http_client::is_success(resp.status) => {
            let value = resp
                .json::<serde_json::Value>()
                .unwrap_or(serde_json::Value::Null);
            print_json_value(value);
            0
        }
        Ok(resp) => {
            let status = resp.status;
            let body = resp.text();
            eprintln!("Daemon returned {status} for {label}: {body}");
            1
        }
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            1
        }
    }
}

async fn daemon_post_pretty_json(path: &str, body: serde_json::Value, label: &str) -> i32 {
    daemon_json_request(rightkit_http::Method::Post, path, Some(body), label).await
}

async fn daemon_delete_pretty_json(path: &str, label: &str) -> i32 {
    daemon_json_request(rightkit_http::Method::Delete, path, None, label).await
}

fn read_json_file(path: &str) -> Result<serde_json::Value, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read JSON file {path}: {e}"))?;
    serde_json::from_str(&content).map_err(|e| format!("Invalid JSON in {path}: {e}"))
}

fn parse_skillopt_gates(gates: &[String]) -> Result<Vec<serde_json::Value>, String> {
    gates
        .iter()
        .map(|gate| {
            let mut parts = gate.splitn(3, ':');
            let name = parts.next().unwrap_or_default().trim();
            let kind = parts.next().unwrap_or_default().trim();
            let threshold = parts.next().unwrap_or_default().trim();
            if name.is_empty() || kind.is_empty() || threshold.is_empty() {
                return Err(format!(
                    "Invalid --gate '{gate}'. Expected name:kind:threshold, for example unit:hard:1.0"
                ));
            }
            if !matches!(kind, "hard" | "soft" | "mixed") {
                return Err(format!(
                    "Invalid gate kind '{kind}' in --gate '{gate}'. Expected hard, soft, or mixed."
                ));
            }
            let threshold = threshold.parse::<f64>().map_err(|e| {
                format!("Invalid threshold in --gate '{gate}'. Expected a number: {e}")
            })?;
            Ok(serde_json::json!({
                "name": name,
                "kind": kind,
                "threshold": threshold,
            }))
        })
        .collect()
}

fn parse_rejected_edits(edits: &[String]) -> Result<Vec<serde_json::Value>, String> {
    edits
        .iter()
        .map(|edit| {
            let mut parts = edit.splitn(3, ':');
            let artifact_id = parts.next().unwrap_or_default().trim();
            let reason = parts.next().unwrap_or_default().trim();
            let patch_preview = parts.next().unwrap_or_default().trim();
            if artifact_id.is_empty() || reason.is_empty() || patch_preview.is_empty() {
                return Err(format!(
                    "Invalid --rejected-edit '{edit}'. Expected artifact_id:reason:patch_preview"
                ));
            }
            Ok(serde_json::json!({
                "artifactId": artifact_id,
                "reason": reason,
                "patchPreview": patch_preview,
            }))
        })
        .collect()
}
