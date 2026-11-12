async fn daemon_logs_until_terminal(base_url: &str, token: &str, session_id: &str) -> i32 {
    use futures_util::StreamExt as _;
    let client = match rightkit_http::http_client::daemon_client(base_url) {
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

    if !rightkit_http::http_client::is_success(resp.status) {
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
            eprintln!("No running daemon found. Start one with `{app} serve`.", app = crate::host_identity::app());
            return 1;
        }
    };

    let encoded_session_id = url_encode_component(session_id);
    let result = match rightkit_http::http_client::daemon_client(&base_url) {
        Ok(client) => {
            rightkit_http::http_client::send_any(
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
        Ok(resp) if rightkit_http::http_client::is_success(resp.status) => {
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

async fn daemon_handoff(session_id: &str) -> i32 {
    match fetch_daemon_handoff(session_id).await {
        Ok(payload) => {
            print_handoff_payload(&payload);
            0
        }
        Err(message) => {
            eprintln!("{message}");
            1
        }
    }
}

async fn daemon_app_open(session_id: &str) -> i32 {
    match fetch_daemon_handoff(session_id).await {
        Ok(_) => {}
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    }

    let url = session_deeplink_url(session_id);
    match open_deeplink_url(&url) {
        Ok(status) if status.success() => {
            println!("Opening RightKit: {url}");
            0
        }
        Ok(status) => {
            eprintln!("Failed to open RightKit link (exit {status}). Open manually: {url}");
            eprintln!("{}", render_app_open_handoff_hint(session_id));
            1
        }
        Err(error) => {
            eprintln!("Failed to open RightKit link: {error}. Open manually: {url}");
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
    format!("For machine handoff JSON, run `{app} app handoff` with session ID: {session_id}", app = crate::host_identity::app())
}

async fn fetch_daemon_handoff(session_id: &str) -> Result<serde_json::Value, String> {
    let (base_url, token) = match crate::cli_presence::resolve_daemon_or_autostart().await {
        Some(p) => p,
        None => {
            return Err(format!("No running daemon found. Start one with `{app} serve`.", app = crate::host_identity::app()));
        }
    };

    let encoded_session_id = url_encode_component(session_id);
    let result = match rightkit_http::http_client::daemon_client(&base_url) {
        Ok(client) => {
            rightkit_http::http_client::send_any(
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
        Ok(resp) if rightkit_http::http_client::is_success(resp.status) => Ok(resp
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
        percent_encode_path_segment(session_id), app = crate::host_identity::app()
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

fn open_deeplink_url(url: &str) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(target_os = "windows")]
    {
        return rightkit_child_process::command_std("rundll32.exe")
            .arg("url.dll,FileProtocolHandler")
            .arg(url)
            .status();
    }

    #[cfg(target_os = "macos")]
    {
        return rightkit_child_process::command_std("open")
            .arg(url)
            .status();
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return rightkit_child_process::command_std("xdg-open")
            .arg(url)
            .status();
    }

    #[allow(unreachable_code)]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "opening app links is unsupported on this platform",
        ))
    }
}
