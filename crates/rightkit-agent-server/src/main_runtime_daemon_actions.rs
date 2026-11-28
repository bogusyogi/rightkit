async fn daemon_logs_until_terminal(base_url: &str, token: &str, session_id: &str) -> i32 {
    use futures_util::StreamExt as _;
    let client = match rightkit_agent_daemon::http_client::daemon_client(base_url) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Stream error: {e}");
            return 1;
        }
    };
    let encoded_session_id = url_encode_component(session_id);
    let url = format!("{base_url}/sessions/{encoded_session_id}/events?after=0");
    let resp = match client
        .stream_any(&rightkit_http::Request::get(url).bearer(token))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Stream error: {e}");
            return 1;
        }
    };

    if !rightkit_agent_daemon::http_client::is_success(resp.status) {
        return 1;
    }

    let mut body = resp.into_stream();
    let mut buffer = String::new();
    while let Some(Ok(bytes)) = body.next().await {
        let text = String::from_utf8_lossy(&bytes);
        print!("{text}");
        let _ = std::io::stdout().flush();
        buffer.push_str(&text);

        while let Some(newline) = buffer.find('\n') {
            let line = buffer[..newline].trim_end_matches('\r').to_string();
            buffer.drain(..=newline);
            if let Some(code) = terminal_sse_exit_code(&line) {
                return code;
            }
        }
    }
    1
}

async fn daemon_kill(session_id: &str) -> i32 {
    let (base_url, token) = match crate::cli_presence::resolve_daemon_or_autostart().await {
        Some(p) => p,
        None => {
            return host_unreachable_exit(false);
        }
    };

    let encoded_session_id = url_encode_component(session_id);
    let result = match rightkit_agent_daemon::http_client::daemon_client(&base_url) {
        Ok(client) => {
            rightkit_agent_daemon::http_client::send_any(
                &client,
                &rightkit_http::Request::post(format!(
                    "{base_url}/sessions/{encoded_session_id}/kill"
                ))
                .bearer(&token),
            )
            .await
        }
        Err(e) => Err(e),
    };
    match result {
        Ok(resp) if rightkit_agent_daemon::http_client::is_success(resp.status) => {
            println!("Session {session_id} killed.");
            0
        }
        Ok(resp) => {
            eprintln!("Daemon returned {} for kill", resp.status);
            1
        }
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            1
        }
    }
}

/// `{app} queue SESSION --message TEXT [--resume]`: put one message on the
/// session's steering queue (`POST /sessions/{id}/steer` with `action: enqueue`).
/// The daemon delivers it after the running turn; the call returns at once.
async fn daemon_queue(session_id: &str, message: &str, resume: bool) -> i32 {
    if message.trim().is_empty() {
        eprintln!(
            "`{app} queue` needs a non-empty --message",
            app = crate::host_identity::app()
        );
        return 2;
    }
    let (base_url, token) = match crate::cli_presence::resolve_daemon_or_autostart().await {
        Some(p) => p,
        None => {
            return host_unreachable_exit(false);
        }
    };
    let encoded_session_id = url_encode_component(session_id);
    if resume {
        match rightkit_agent_daemon::http_client::daemon_call(
            rightkit_http::Method::Post,
            &format!("{base_url}/sessions/{encoded_session_id}/resume"),
            &token,
            None,
        )
        .await
        {
            Ok(resp) if rightkit_agent_daemon::http_client::is_success(resp.status) => {}
            Ok(resp) => {
                eprintln!(
                    "Daemon returned {} resuming session {session_id}",
                    resp.status
                );
                return 1;
            }
            Err(e) => {
                eprintln!("Failed to contact daemon: {e}");
                return 1;
            }
        }
    }
    let body = serde_json::json!({ "action": "enqueue", "text": message });
    match rightkit_agent_daemon::http_client::daemon_call(
        rightkit_http::Method::Post,
        &format!("{base_url}/sessions/{encoded_session_id}/steer"),
        &token,
        Some(&body),
    )
    .await
    {
        Ok(resp) if rightkit_agent_daemon::http_client::is_success(resp.status) => {
            let value = resp
                .json::<serde_json::Value>()
                .unwrap_or(serde_json::Value::Null);
            match value
                .pointer("/queued/id")
                .and_then(serde_json::Value::as_str)
            {
                Some(id) => println!("Queued message {id} for session {session_id}."),
                None => println!("Queued a message for session {session_id}."),
            }
            0
        }
        Ok(resp) => {
            eprintln!("{}", queue_refusal(resp.status, session_id));
            1
        }
        Err(e) => {
            eprintln!("Failed to contact daemon: {e}");
            1
        }
    }
}

/// What a refused `queue` call means, in words.
fn queue_refusal(status: u16, session_id: &str) -> String {
    match status {
        404 => format!(
            "Session {session_id} is not live in the daemon; pass --resume to wake a dormant session first."
        ),
        429 => format!("Session {session_id} already holds the most queued messages it accepts."),
        status => format!("Daemon returned {status} queuing a message for session {session_id}"),
    }
}

async fn daemon_handoff(session_id: &str) -> i32 {
    match fetch_daemon_handoff(session_id).await {
        Ok(payload) => {
            print_handoff_payload(&payload);
            0
        }
        Err(message) => daemon_error_exit(false, &message),
    }
}

async fn daemon_app_open(session_id: &str) -> i32 {
    match fetch_daemon_handoff(session_id).await {
        Ok(_) => {}
        Err(message) => return daemon_error_exit(false, &message),
    }

    let url = session_deeplink_url(session_id);
    match crate::open_url::open_url(&url) {
        Ok(status) if status.success() => {
            println!("Opening {}: {url}", rightkit_agent_config::app_name());
            0
        }
        Ok(status) => {
            eprintln!(
                "Failed to open {} link (exit {status}). Open manually: {url}",
                rightkit_agent_config::app_name()
            );
            eprintln!("{}", render_app_open_handoff_hint(session_id));
            1
        }
        Err(error) => {
            eprintln!(
                "Failed to open {} link: {error}. Open manually: {url}",
                rightkit_agent_config::app_name()
            );
            eprintln!("{}", render_app_open_handoff_hint(session_id));
            1
        }
    }
}

fn print_handoff_payload(payload: &serde_json::Value) {
    println!("{}", render_handoff_payload(payload));
}

fn render_handoff_payload(payload: &serde_json::Value) -> String {
    serde_json::to_string_pretty(payload).unwrap_or_default()
}

fn render_app_open_handoff_hint(session_id: &str) -> String {
    format!(
        "For machine handoff JSON, run `{app} app handoff` with session ID: {session_id}",
        app = crate::host_identity::app()
    )
}

async fn fetch_daemon_handoff(session_id: &str) -> Result<serde_json::Value, String> {
    let (base_url, token) = match crate::cli_presence::resolve_daemon_or_autostart().await {
        Some(p) => p,
        None => {
            return Err(no_daemon_message());
        }
    };

    let encoded_session_id = url_encode_component(session_id);
    let result = match rightkit_agent_daemon::http_client::daemon_client(&base_url) {
        Ok(client) => {
            rightkit_agent_daemon::http_client::send_any(
                &client,
                &rightkit_http::Request::get(format!(
                    "{base_url}/sessions/{encoded_session_id}/handoff"
                ))
                .bearer(&token),
            )
            .await
        }
        Err(e) => Err(e),
    };
    match result {
        Ok(resp) if rightkit_agent_daemon::http_client::is_success(resp.status) => Ok(resp
            .json::<serde_json::Value>()
            .unwrap_or(serde_json::Value::Null)),
        Ok(resp) if resp.status == 404 => Err(format!("Session not found: {session_id}")),
        Ok(resp) => Err(format!("Daemon returned {} for handoff", resp.status)),
        Err(e) => Err(format!("Failed to contact daemon: {e}")),
    }
}

fn session_deeplink_url(session_id: &str) -> String {
    format!(
        "{app}://sessions/{}",
        percent_encode_path_segment(session_id),
        app = crate::host_identity::app()
    )
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
