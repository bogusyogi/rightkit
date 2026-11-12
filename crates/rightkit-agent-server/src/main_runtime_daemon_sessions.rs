fn same_cli_path(left: &str, right: &str) -> bool {
    fn normalize(raw: &str) -> String {
        let trimmed = raw.trim().trim_end_matches(['\\', '/']);
        std::fs::canonicalize(trimmed)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| trimmed.to_string())
            .replace('\\', "/")
            .to_ascii_lowercase()
    }
    !left.trim().is_empty() && normalize(left) == normalize(right)
}

async fn daemon_session(command: SessionCommand) -> i32 {
    match command {
        SessionCommand::List {
            status,
            tag,
            json: _json,
        } => daemon_session_list(status, tag).await,
        SessionCommand::Search { query, limit } => daemon_session_search(&query, limit).await,
        SessionCommand::Delete { session_id } => daemon_session_delete(&session_id).await,
        SessionCommand::Archive { session_id } => {
            daemon_session_set_status(&session_id, "archived").await
        }
        SessionCommand::Restore { session_id } => {
            daemon_session_set_status(&session_id, "dormant").await
        }
        SessionCommand::Set {
            session_id,
            title,
            model,
            tags,
            clear_tags,
        } => daemon_session_set(&session_id, title, model, tags, clear_tags).await,
        SessionCommand::Export { session_id, json } => daemon_export(&session_id, json).await,
    }
}

async fn daemon_session_list(status: Option<String>, tag: Option<String>) -> i32 {
    // DL-014: `{app} ps`/`{app} sessions` reach the daemon through
    // the shared `LifecycleAuthority` (`Start { Background }`) rather than
    // calling the autostart coordinator directly; see
    // `daemon::lifecycle_backend::resolve_daemon_endpoint_via_authority`.
    // Both the autostart/readiness wait and the request are bounded: a daemon
    // that accepts the connection but never answers must fail loudly, not
    // leave `{app} ps` hanging forever.
    let resolved = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        crate::daemon::lifecycle_backend::resolve_daemon_endpoint_via_authority(),
    )
    .await;
    let (base_url, token) = match resolved {
        Ok(Some(p)) => p,
        Ok(None) => {
            eprintln!(
                "No running daemon found. Start one with `{app} serve`.",
                app = crate::host_identity::app()
            );
            return 1;
        }
        Err(_) => {
            eprintln!("Timed out after 60s waiting for the daemon to become ready.");
            return 1;
        }
    };

    let mut url = format!("{base_url}/sessions");
    let mut params = Vec::new();
    if let Some(status) = status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        params.push(("status", status.to_string()));
    }
    if let Some(tag) = tag.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        params.push(("tag", tag.to_string()));
    }
    if !params.is_empty() {
        let query = params
            .iter()
            .map(|(key, value)| format!("{key}={}", url_encode_component(value)))
            .collect::<Vec<_>>()
            .join("&");
        url.push('?');
        url.push_str(&query);
    }
    let listed = match rightkit_agent_daemon::http_client::daemon_client_with(
        &url,
        std::time::Duration::from_secs(30),
    ) {
        Ok(client) => {
            rightkit_agent_daemon::http_client::send_any(
                &client,
                &rightkit_http::Request::get(url.clone()).bearer(&token),
            )
            .await
        }
        Err(e) => Err(e),
    };
    match listed {
        Ok(resp) if rightkit_agent_daemon::http_client::is_success(resp.status) => {
            println!("{}", resp.text());
            0
        }
        Ok(resp) => {
            eprintln!("Daemon returned {}", resp.status);
            1
        }
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            1
        }
    }
}

async fn daemon_session_search(query: &str, limit: usize) -> i32 {
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
    let query = query.trim();
    if query.is_empty() {
        eprintln!("Search query cannot be empty.");
        return 1;
    }

    let url = format!(
        "{base_url}/sessions/search?q={}&limit={}",
        url_encode_component(query),
        limit.max(1)
    );
    match rightkit_agent_daemon::http_client::daemon_get(&url, &token).await {
        Ok(resp) if rightkit_agent_daemon::http_client::is_success(resp.status) => {
            println!("{}", resp.text());
            0
        }
        Ok(resp) => {
            eprintln!("Daemon returned {} for session search", resp.status);
            1
        }
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            1
        }
    }
}

async fn daemon_session_delete(session_id: &str) -> i32 {
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

    let encoded_session_id = url_encode_component(session_id);
    match rightkit_agent_daemon::http_client::daemon_call(
        rightkit_http::Method::Delete,
        &format!("{base_url}/sessions/{encoded_session_id}"),
        &token,
        None,
    )
    .await
    {
        Ok(resp) if rightkit_agent_daemon::http_client::is_success(resp.status) => {
            println!("Session {session_id} deleted.");
            0
        }
        Ok(resp) if resp.status == 404 => {
            eprintln!("Session not found: {session_id}");
            1
        }
        Ok(resp) => {
            eprintln!("Daemon returned {} for session delete", resp.status);
            1
        }
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            1
        }
    }
}

async fn daemon_session_set_status(session_id: &str, status: &str) -> i32 {
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

    let encoded_session_id = url_encode_component(session_id);
    let body = serde_json::json!({ "status": status });
    match rightkit_agent_daemon::http_client::daemon_call(
        rightkit_http::Method::Patch,
        &format!("{base_url}/sessions/{encoded_session_id}"),
        &token,
        Some(&body),
    )
    .await
    {
        Ok(resp) if rightkit_agent_daemon::http_client::is_success(resp.status) => {
            println!("Session {session_id} status set to {status}.");
            0
        }
        Ok(resp) if resp.status == 404 => {
            eprintln!("Session not found: {session_id}");
            1
        }
        Ok(resp) => {
            eprintln!("Daemon returned {} for session status update", resp.status);
            1
        }
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            1
        }
    }
}

async fn daemon_session_set(
    session_id: &str,
    title: Option<String>,
    model: Option<String>,
    tags: Vec<String>,
    clear_tags: bool,
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

    let mut body = serde_json::Map::new();
    if let Some(title) = title
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        body.insert("title".to_string(), serde_json::Value::String(title));
    }
    if let Some(model) = model
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        body.insert("model".to_string(), serde_json::Value::String(model));
    }
    if clear_tags && !tags.is_empty() {
        eprintln!("Use either --tag or --clear-tags, not both.");
        return 1;
    }
    if clear_tags {
        body.insert("tags".to_string(), serde_json::json!([]));
    } else if !tags.is_empty() {
        let tags: Vec<String> = tags
            .into_iter()
            .map(|tag| tag.trim().to_string())
            .filter(|tag| !tag.is_empty())
            .collect();
        body.insert("tags".to_string(), serde_json::json!(tags));
    }
    if body.is_empty() {
        eprintln!("Nothing to update. Provide --title, --model, --tag, or --clear-tags.");
        return 1;
    }

    let encoded_session_id = url_encode_component(session_id);
    match rightkit_agent_daemon::http_client::daemon_call(
        rightkit_http::Method::Patch,
        &format!("{base_url}/sessions/{encoded_session_id}"),
        &token,
        Some(&serde_json::Value::Object(body)),
    )
    .await
    {
        Ok(resp) if rightkit_agent_daemon::http_client::is_success(resp.status) => {
            println!("Session {session_id} updated.");
            0
        }
        Ok(resp) if resp.status == 404 => {
            eprintln!("Session not found: {session_id}");
            1
        }
        Ok(resp) => {
            eprintln!("Daemon returned {} for session update", resp.status);
            1
        }
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            1
        }
    }
}

fn url_encode_component(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
