//! End-to-end integration tests that spawn the real `coderight serve` binary and exercise
//! the startup contract + HTTP API over a live socket. These are the tests the spec calls
//! out as the highest-value additions of the rewrite (they catch integration breaks that
//! unit tests miss).

use std::io::{BufReader, Read, Write};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

/// A spawned daemon, killed on drop.
struct Daemon {
    child: Child,
    _stdout: std::process::ChildStdout,
    port: u16,
    token: String,
    db: String,
    home: std::path::PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.db);
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Spawn `coderight serve --port 0` and parse its startup line.
fn spawn_daemon() -> Daemon {
    let exe = env!("CARGO_BIN_EXE_coderight");
    let engine_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("resolve engine root");
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let db_path = e2e_temp_root().join(format!("coderight-e2e-{}-{unique}.db", std::process::id()));
    let db_arg = db_path.to_string_lossy().to_string();
    let home = e2e_temp_dir("home");
    let mut child = rightkit_child_process::command_std(exe)
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .env("RIGHTKIT_CONTEXT", "off")
        .args(["serve", "--port", "0", "--db", &db_arg])
        .current_dir(&engine_root)
        // Integration tests exercise the licensed (Pro) product: a `test-mode` daemon starts with
        // the compiled-in Pro feature set (no environment input), so Pro-gated routes are reachable.
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .envs([crate::common::suite_env(&home)])
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("RIGHTKIT_DISABLE_OS_KEYCHAIN", "1")
        // These protocol tests use isolated homes, not a production Membrane binding.
        .env("RIGHTKIT_QUAL_ISOLATED_MEMBRANE", "1")
        .env_remove("RIGHTKIT_DAEMON_URL")
        .env_remove("RIGHTKIT_DAEMON_TOKEN")
        .env_remove("RIGHTKIT_SERVE_FORCE")
        .env_remove("RIGHTKIT_QUAL_EVT005_FAULT")
        .env_remove("RIGHTKIT_QUAL_EVT006_FAULT")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn coderight serve");

    let stdout = child.stdout.take().expect("stdout piped");
    let mut reader = BufReader::new(stdout);
    let mut frame = Vec::new();
    // Parse the startup contract: {"Port":N,"Token":"...","DatabasePath":"..."}
    let v: serde_json::Value =
        rightkit_sidecar::read_json_line(&mut reader, &mut frame, rightkit_sidecar::UNCAPPED)
            .unwrap_or_else(|error| panic!("startup line was not valid JSON: {error}"))
            .expect("startup line before EOF");
    let stdout = reader.into_inner();
    let port = v["Port"].as_u64().expect("Port present") as u16;
    let token = v["Token"].as_str().expect("Token present").to_string();
    let db = v["DatabasePath"]
        .as_str()
        .expect("DatabasePath present")
        .to_string();

    let mut daemon = Daemon {
        child,
        _stdout: stdout,
        port,
        token,
        db,
        home,
    };
    let ready_url = format!("http://127.0.0.1:{}/health", daemon.port);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = daemon.child.try_wait().expect("poll spawned daemon") {
            panic!("daemon exited before becoming HTTP-ready: {status}");
        }
        match crate::support::daemon_process::send(&rightkit_http::Request::get(&ready_url)) {
            Ok(response) if response.status == 200 => break,
            _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => panic!("daemon did not become HTTP-ready on {ready_url} within 30s"),
        }
    }
    daemon
}

fn e2e_temp_root() -> std::path::PathBuf {
    let root = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("e2e-tmp");
    std::fs::create_dir_all(&root).expect("create e2e temp root");
    root
}

fn e2e_temp_dir(label: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = e2e_temp_root().join(format!("{label}-{}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn bare_invocation_enters_interactive_session_and_quit_exits_cleanly() {
    let exe = env!("CARGO_BIN_EXE_coderight");
    let engine_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("resolve engine root");
    let home = e2e_temp_dir("bare-interactive-home");
    let mut child = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .current_dir(engine_root)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn bare coderight");
    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(b"/quit\n")
        .expect("send interactive quit");

    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll bare coderight") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("bare coderight did not exit after /quit within 20s");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let _ = std::fs::remove_dir_all(home);
    assert!(status.success(), "bare interactive exit was {status}");
}

#[test]
fn explicit_json_mode_remains_noninteractive() {
    let exe = env!("CARGO_BIN_EXE_coderight");
    let home = e2e_temp_dir("explicit-json-home");
    let output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args(["caps", "--json"])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(Stdio::piped())
        .output()
        .expect("run explicit JSON CLI mode");
    let _ = std::fs::remove_dir_all(home);

    assert!(
        output.status.success(),
        "explicit JSON mode exited with {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let body = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|_| panic!("caps --json should print JSON, got: {body}"));
    assert!(value["toolCount"].is_u64(), "JSON mode output: {value}");
}

fn get(url: &str) -> (u16, String) {
    // Minimal blocking HTTP GET with a tiny retry.
    http_request("GET", url, None, None)
}

/// Authenticated GET — sends the daemon's launch token.
fn get_auth(url: &str, token: &str) -> (u16, String) {
    http_request("GET", url, None, Some(token))
}

/// Authenticated POST — sends the daemon's launch token.
fn post_json_auth(url: &str, body: &str, token: &str) -> (u16, String) {
    http_request("POST", url, Some(body), Some(token))
}

fn wait_for_child(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            return status;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child did not exit within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn http_request(method: &str, url: &str, body: Option<&str>, token: Option<&str>) -> (u16, String) {
    let timeout = if url.contains("/events") {
        Duration::from_millis(1200)
    } else {
        Duration::from_secs(5)
    };
    // `/events` is a long-lived SSE stream that never closes: the idle limit ends the read
    // once the socket goes quiet; every other call is bounded by the whole-request limit.
    let client = rightkit_http::Client::new(rightkit_http::ClientConfig {
        connect_timeout: Some(Duration::from_secs(1)),
        request_timeout: timeout,
        response_timeout: Some(timeout),
        idle_timeout: Some(timeout),
        retry: rightkit_http::RetryPolicy::none(),
        address_policy: crate::support::daemon_process::loopback_policy(),
        ..rightkit_http::ClientConfig::default()
    });
    let body = body.unwrap_or("");
    let mut last_error = None;

    for attempt in 0..12 {
        let method = match method {
            "GET" => rightkit_http::Method::Get,
            "POST" => rightkit_http::Method::Post,
            "DELETE" => rightkit_http::Method::Delete,
            other => panic!("unsupported method for e2e helper: {other}"),
        };
        let mut request = rightkit_http::Request::new(method, url);
        if let Some(token) = token {
            request = request.bearer(token);
        }
        if !body.is_empty() {
            request = request
                .header("content-type", "application/json")
                .body(body.as_bytes().to_vec());
        }

        let response = match client.stream_any(&request) {
            Ok(response) => response,
            Err(error) if attempt < 11 => {
                last_error = Some(error.to_string());
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(error) => return (0, error.to_string()),
        };
        let status = response.status;
        let mut response = response.into_reader();

        // The /events endpoint is a long-lived SSE stream that never closes, so
        // we collect whatever arrives until the socket goes idle.
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match response.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }
        return (status, String::from_utf8_lossy(&buf).to_string());
    }
    (
        0,
        last_error.unwrap_or_else(|| "request loop exited unexpectedly".to_string()),
    )
}

/// Count discrete protocol events in an SSE response body (one `"$kind"` per
/// event payload).
fn sse_event_count(body: &str) -> usize {
    sse_event_values(body).len()
}

fn sse_event_values(body: &str) -> Vec<serde_json::Value> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        .collect()
}

#[test]
fn auxiliary_daemon_does_not_replace_the_primary_port_file() {
    let daemon = spawn_daemon();
    assert!(
        !crate::common::app_root(&daemon.home)
            .join("daemon.port")
            .exists(),
        "serve --db is auxiliary and must not publish the primary daemon discovery file"
    );
}

#[test]
fn startup_contract_line_is_emitted() {
    let d = spawn_daemon();
    assert!(d.port > 0, "port should be OS-assigned and non-zero");
    assert!(!d.token.is_empty(), "token should be present");
    assert!(!d.db.is_empty(), "database path should be present");
}

#[test]
fn health_endpoint_responds() {
    let d = spawn_daemon();
    let (status, body) = get(&format!("http://127.0.0.1:{}/health", d.port));
    assert_eq!(status, 200);
    assert!(body.contains("ok"));
}

#[test]
fn refactor04_cli_and_daemon_surfaces_are_live_and_secret_safe() {
    let d = spawn_daemon();
    let base = format!("http://127.0.0.1:{}", d.port);

    let (status, provider_status) = get_auth(&format!("{base}/provider-keys/openrouter"), &d.token);
    assert_eq!(status, 200, "provider status body: {provider_status}");
    assert!(!provider_status.to_ascii_lowercase().contains("api_key"));
    assert!(!provider_status.to_ascii_lowercase().contains("apikey"));

    let (status, _) = post_json_auth(
        &format!("{base}/provider-keys/not-a-provider/test"),
        "{}",
        &d.token,
    );
    assert_eq!(
        status, 400,
        "provider test route must reject unknown providers"
    );

    let (status, _) = get_auth(
        &format!("{base}/sessions/missing/browser/workspace"),
        &d.token,
    );
    assert!(matches!(status, 403 | 404));

    for ambiguous in ["install", "uninstall"] {
        let output = rightkit_child_process::command_std(env!("CARGO_BIN_EXE_coderight"))
            .envs([crate::common::suite_env(&d.home)])
            .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
            .arg(ambiguous)
            .output()
            .expect("run ambiguous service verb");
        assert!(
            !output.status.success(),
            "bare {ambiguous} must be rejected"
        );
    }

    let config = rightkit_child_process::command_std(env!("CARGO_BIN_EXE_coderight"))
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .arg("config")
        .env("HOME", &d.home)
        .env("USERPROFILE", &d.home)
        .output()
        .expect("run redacted config view");
    assert!(config.status.success());
    let config: serde_json::Value =
        serde_json::from_slice(&config.stdout).expect("config output JSON");
    assert!(config.is_object(), "config view must remain structured");
}

#[test]
fn session_lifecycle_message_then_events() {
    let d = spawn_daemon();
    let base = format!("http://127.0.0.1:{}", d.port);
    // The daemon seeds a "default" session.
    let (status, body) = post_json_auth(
        &format!("{base}/sessions/default/messages"),
        r#"{"text":"hello world"}"#,
        &d.token,
    );
    assert_eq!(status, 200, "post message body: {body}");
    assert!(body.contains("accepted"));

    // The posted message is recorded in the session transcript.
    let (status, body) = get_auth(&format!("{base}/sessions/default/transcript"), &d.token);
    assert_eq!(status, 200);
    assert!(body.contains("hello world"), "transcript: {body}");

    // The SSE stream carries at least one event for the turn (the echo reply
    // when no provider key is configured).
    let (status, body) = get_auth(&format!("{base}/sessions/default/events?after=0"), &d.token);
    assert_eq!(status, 200);
    assert!(
        sse_event_count(&body) >= 1,
        "expected ≥1 SSE event, body: {body}"
    );
}

#[test]
fn unknown_session_is_404() {
    let d = spawn_daemon();
    let (status, _) = get_auth(
        &format!("http://127.0.0.1:{}/sessions/ghost/events", d.port),
        &d.token,
    );
    assert_eq!(status, 404);
}

#[test]
fn delete_session_removes_it() {
    let d = spawn_daemon();
    let base = format!("http://127.0.0.1:{}", d.port);
    // The seeded "default" session is listed.
    let (status, body) = get_auth(&format!("{base}/sessions"), &d.token);
    assert_eq!(status, 200);
    assert!(body.contains("default"), "default session should be listed");
    // Delete it.
    let (status, _) = http_request(
        "DELETE",
        &format!("{base}/sessions/default"),
        None,
        Some(&d.token),
    );
    assert_eq!(status, 200, "delete should succeed");
    // It is gone from the list.
    let (status, body) = get_auth(&format!("{base}/sessions"), &d.token);
    assert_eq!(status, 200);
    assert!(
        !body.contains("\"default\""),
        "deleted session should not be listed: {body}"
    );
    // Deleting again → 404.
    let (status, _) = http_request(
        "DELETE",
        &format!("{base}/sessions/default"),
        None,
        Some(&d.token),
    );
    assert_eq!(status, 404, "deleting a missing session is 404");
}

#[test]
fn api_requires_token() {
    let d = spawn_daemon();
    let base = format!("http://127.0.0.1:{}", d.port);
    // No token → 401 on a protected route.
    let (status, _) = get(&format!("{base}/sessions/default/transcript"));
    assert_eq!(status, 401, "unauthenticated API call must be rejected");
    // Wrong token → 401.
    let (status, _) = get_auth(
        &format!("{base}/sessions/default/transcript"),
        "not-the-token",
    );
    assert_eq!(status, 401, "wrong token must be rejected");
    // Correct token → 200.
    let (status, _) = get_auth(&format!("{base}/sessions/default/transcript"), &d.token);
    assert_eq!(status, 200, "correct token must be accepted");
    // The SSE stream accepts the token via the access_token query param.
    let (status, _) = get(&format!(
        "{base}/sessions/default/events?after=0&access_token={}",
        d.token
    ));
    assert_eq!(
        status, 200,
        "SSE access_token query param must authenticate"
    );
    // Health stays open (connectivity probe).
    let (status, _) = get(&format!("{base}/health"));
    assert_eq!(status, 200, "health must remain unauthenticated");
}

#[test]
fn sse_catch_up_returns_only_newer_events() {
    let d = spawn_daemon();
    let base = format!("http://127.0.0.1:{}", d.port);
    // Post three messages → three events on the default session.
    for i in 0..3 {
        let (s, _) = post_json_auth(
            &format!("{base}/sessions/default/messages"),
            &format!(r#"{{"text":"m{i}"}}"#),
            &d.token,
        );
        assert_eq!(s, 200);
    }
    // The events endpoint is SSE: replay buffered history (filtered by `after`)
    // then stream live. Catching up after a later seq must yield strictly fewer
    // events than catching up from the start.
    let (status, all) = get_auth(
        &format!("{base}/sessions/default/events?after=0&kind=userMessageSubmitted"),
        &d.token,
    );
    assert_eq!(status, 200);
    let all_events = sse_event_values(&all);
    let all_count = all_events.len();
    assert!(
        all_count >= 1,
        "after=0 should replay buffered events, got {all_count}"
    );

    // Catch up after the first buffered event's seq → it must be dropped.
    let cut = all_events[0]["seq"]
        .as_i64()
        .expect("buffered events carry a seq");
    let (status, newer) = get_auth(
        &format!("{base}/sessions/default/events?after={cut}&kind=userMessageSubmitted"),
        &d.token,
    );
    assert_eq!(status, 200);
    let newer_events = sse_event_values(&newer);
    let newer_count = newer_events.len();
    assert!(
        newer_count < all_count,
        "after={cut} should drop earlier events: newer={newer_count} all={all_count}; all={all_events:?}; newer={newer_events:?}"
    );
    assert!(
        newer_events
            .iter()
            .all(|event| event["seq"].as_i64().unwrap_or_default() > cut),
        "after={cut} returned stale events: {newer}"
    );
}

#[test]
fn cli_attach_forwards_stdin_to_session_messages() {
    let d = spawn_daemon();
    let exe = env!("CARGO_BIN_EXE_coderight");
    let base = format!("http://127.0.0.1:{}", d.port);
    let marker = format!("cli attach e2e {}", std::process::id());

    let mut child = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args(["attach", "default"])
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn coderight attach");

    {
        let stdin = child.stdin.as_mut().expect("attach stdin");
        writeln!(stdin, "{marker}").expect("write attach message");
    }
    drop(child.stdin.take());

    let status = wait_for_child(&mut child, Duration::from_secs(5));
    assert!(status.success(), "attach exited with {status}");

    let (status, body) = get_auth(&format!("{base}/sessions/default/transcript"), &d.token);
    assert_eq!(status, 200);
    assert!(
        body.contains(&marker),
        "attach stdin should appear in transcript, got: {body}"
    );
}

#[test]
fn cli_status_reports_running_daemon() {
    let d = spawn_daemon();
    let exe = env!("CARGO_BIN_EXE_coderight");
    let base = format!("http://127.0.0.1:{}", d.port);

    let output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .arg("status")
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight status");

    assert!(
        output.status.success(),
        "status exited with {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let body = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|_| panic!("status should print JSON, got: {body}"));
    assert_eq!(value["running"], true);
    assert_eq!(value["daemonUrl"], base);
    assert_eq!(value["health"]["service"], "coderight-daemon");
    assert_eq!(value["health"]["port"], d.port);
    assert!(
        value["sessionCount"].as_u64().unwrap_or_default() >= 1,
        "daemon should expose the seeded default session: {value}"
    );
    assert!(
        value["sessions"]
            .as_array()
            .map(|sessions| sessions.iter().any(|s| s["id"] == "default"))
            .unwrap_or(false),
        "default session should be listed: {value}"
    );
}

#[test]
fn cli_remote_status_reports_redacted_telegram_state() {
    let d = spawn_daemon();
    let exe = env!("CARGO_BIN_EXE_coderight");
    let base = format!("http://127.0.0.1:{}", d.port);

    let output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args(["remote", "status"])
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight remote status");

    assert!(
        output.status.success(),
        "remote status exited with {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let body = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|_| panic!("remote status should print JSON, got: {body}"));
    assert_eq!(value["tokenConfigured"], false);
    assert!(value["bindingStorePath"].as_str().is_some(), "{value}");
    assert!(value["runtime"]["activeChatCount"].is_number(), "{value}");
    assert!(
        !body.contains("telegram-token"),
        "remote status must not expose raw tokens: {body}"
    );
}

#[test]
fn cli_remote_setup_saves_config_and_redacts_token() {
    let exe = env!("CARGO_BIN_EXE_coderight");
    let home = e2e_temp_dir("coderight-remote-setup-home");
    let cwd = e2e_temp_dir("coderight-remote-setup-cwd");
    let token = "123456:secret-setup-token";

    let output = rightkit_child_process::command_std(exe)
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args([
            "remote",
            "setup",
            "--telegram-token",
            token,
            "--allowed-user",
            "4242",
            "--enable-web",
        ])
        .current_dir(&cwd)
        .envs([crate::common::suite_env(&home)])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight remote setup");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let config_path = crate::common::app_root(&home).join("config.toml");
    let config = std::fs::read_to_string(&config_path)
        .unwrap_or_else(|err| panic!("read saved config at {config_path:?}: {err}"));
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|_| panic!("remote setup should print JSON, got: {stdout}"));

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&cwd);

    assert!(
        output.status.success(),
        "remote setup exited with {:?}, stderr: {stderr}",
        output.status.code()
    );
    assert_eq!(value["saved"], true, "{value}");
    assert_eq!(value["remote"]["telegramTokenConfigured"], true, "{value}");
    assert_eq!(value["remote"]["telegramAllowedUserCount"], 1, "{value}");
    assert_eq!(value["remote"]["webEnabled"], true, "{value}");
    assert!(
        stdout.contains("coderight remote smoke --timeout-seconds 120"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("secret-setup-token"),
        "remote setup must not expose bot token on stdout: {stdout}"
    );
    assert!(
        !stderr.contains("secret-setup-token"),
        "remote setup must not expose bot token on stderr: {stderr}"
    );
    assert!(config.contains(token), "config should persist the token");
    assert!(
        config.contains("4242"),
        "config should persist allowed user id"
    );
}

#[test]
fn cli_remote_test_reports_redacted_readiness_blocks() {
    let d = spawn_daemon();
    let exe = env!("CARGO_BIN_EXE_coderight");
    let base = format!("http://127.0.0.1:{}", d.port);

    let output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args(["remote", "test"])
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight remote test");

    assert!(
        output.status.success(),
        "remote test exited with {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let body = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|_| panic!("remote test should print JSON, got: {body}"));
    assert_eq!(value["verdict"], "block", "{value}");
    assert_eq!(value["tokenConfigured"], false, "{value}");
    assert_eq!(value["allowedUserCount"], 0, "{value}");
    assert!(
        value["checks"]
            .as_array()
            .map(|checks| checks
                .iter()
                .any(|check| check["id"] == "telegram_token" && check["status"] == "block"))
            .unwrap_or(false),
        "remote test should include blocking token readiness check: {value}"
    );
    assert!(
        !body.contains(&d.token),
        "remote test must not expose daemon token on stdout: {body}"
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains(&d.token),
        "remote test must not expose daemon token on stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_remote_smoke_blocks_cleanly_without_telegram_config() {
    let d = spawn_daemon();
    let exe = env!("CARGO_BIN_EXE_coderight");
    let base = format!("http://127.0.0.1:{}", d.port);

    let mut child = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args([
            "remote",
            "smoke",
            "--timeout-seconds",
            "1",
            "--poll-interval-seconds",
            "1",
        ])
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run coderight remote smoke");

    let status = wait_for_child(&mut child, Duration::from_secs(5));
    let mut stdout = String::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .expect("smoke stdout")
        .read_to_string(&mut stdout)
        .expect("read smoke stdout");
    child
        .stderr
        .take()
        .expect("smoke stderr")
        .read_to_string(&mut stderr)
        .expect("read smoke stderr");

    assert!(
        !status.success(),
        "remote smoke should block without Telegram config; stdout: {stdout}; stderr: {stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|_| panic!("remote smoke should print JSON, got: {stdout}"));
    assert_eq!(value["verdict"], "block", "{value}");
    assert_eq!(value["readinessVerdict"], "block", "{value}");
    assert_eq!(value["telegramTokenConfigured"], false, "{value}");
    assert_eq!(value["activeChatCount"], 0, "{value}");
    assert!(
        stdout.contains("Telegram daemon readiness is not passing yet."),
        "smoke output should explain the readiness block: {stdout}"
    );
    assert!(
        stdout.contains("Telegram BotFather")
            && stdout.contains("coderight remote setup --telegram-token")
            && stdout.contains("coderight remote smoke --timeout-seconds 120"),
        "smoke output should guide token setup and rerun path: {stdout}"
    );
    assert!(
        !stdout.contains(&d.token),
        "remote smoke must not expose daemon token on stdout: {stdout}"
    );
    assert!(
        !stderr.contains(&d.token),
        "remote smoke must not expose daemon token on stderr: {stderr}"
    );
}

#[test]
fn remote_telegram_users_rejects_api_base_override() {
    let d = spawn_daemon();
    let base = format!("http://127.0.0.1:{}", d.port);

    let (status, body) = get_auth(
        &format!("{base}/remote/telegram/users?apiBase=http%3A%2F%2F127.0.0.1%3A9"),
        &d.token,
    );
    assert_eq!(
        status, 400,
        "remote user discovery must reject caller-controlled apiBase overrides, body: {body}"
    );
    assert!(
        !body.contains(&d.token),
        "remote user discovery rejection must not expose daemon token: {body}"
    );
}

#[test]
fn cli_session_list_search_and_delete_use_daemon_contracts() {
    let d = spawn_daemon();
    let exe = env!("CARGO_BIN_EXE_coderight");
    let base = format!("http://127.0.0.1:{}", d.port);
    let marker = format!("cli session search e2e {}", std::process::id());

    let list_output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args(["session", "list"])
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight session list");
    assert!(
        list_output.status.success(),
        "session list exited with {:?}, stderr: {}",
        list_output.status.code(),
        String::from_utf8_lossy(&list_output.stderr)
    );
    let list_body = String::from_utf8_lossy(&list_output.stdout);
    let sessions: serde_json::Value = serde_json::from_str(&list_body)
        .unwrap_or_else(|_| panic!("session list should print JSON, got: {list_body}"));
    assert!(
        sessions
            .as_array()
            .map(|rows| rows.iter().any(|row| row["id"] == "default"))
            .unwrap_or(false),
        "default session should be listed: {sessions}"
    );

    let (status, body) = post_json_auth(
        &format!("{base}/sessions/default/messages"),
        &format!(r#"{{"text":"{marker}"}}"#),
        &d.token,
    );
    assert_eq!(status, 200, "post message body: {body}");

    let search_output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args(["session", "search", &marker, "--limit", "5"])
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight session search");
    assert!(
        search_output.status.success(),
        "session search exited with {:?}, stderr: {}",
        search_output.status.code(),
        String::from_utf8_lossy(&search_output.stderr)
    );
    let search_body = String::from_utf8_lossy(&search_output.stdout);
    let hits: serde_json::Value = serde_json::from_str(&search_body)
        .unwrap_or_else(|_| panic!("session search should print JSON, got: {search_body}"));
    assert!(
        hits.as_array()
            .map(|rows| rows.iter().any(|row| row["sessionId"] == "default"))
            .unwrap_or(false),
        "search should find default session marker: {hits}"
    );

    let set_output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args([
            "session",
            "set",
            "default",
            "--title",
            "CLI renamed session",
            "--model",
            "coder",
            "--tag",
            "parity",
            "--tag",
            "terminal",
        ])
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight session set");
    assert!(
        set_output.status.success(),
        "session set exited with {:?}, stderr: {}",
        set_output.status.code(),
        String::from_utf8_lossy(&set_output.stderr)
    );
    let set_body = String::from_utf8_lossy(&set_output.stdout);
    assert!(set_body.contains("Session default updated."));

    let tagged_output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args(["session", "list", "--tag", "parity"])
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight session list --tag parity");
    assert!(
        tagged_output.status.success(),
        "session tagged list exited with {:?}, stderr: {}",
        tagged_output.status.code(),
        String::from_utf8_lossy(&tagged_output.stderr)
    );
    let tagged_body = String::from_utf8_lossy(&tagged_output.stdout);
    let tagged: serde_json::Value = serde_json::from_str(&tagged_body)
        .unwrap_or_else(|_| panic!("tagged list should print JSON, got: {tagged_body}"));
    assert!(
        tagged
            .as_array()
            .map(|rows| {
                rows.iter().any(|row| {
                    row["id"] == "default"
                        && row["title"] == "CLI renamed session"
                        && row["model"] == "coder"
                        && row["tags"]
                            .as_array()
                            .map(|tags| tags.iter().any(|tag| tag == "terminal"))
                            .unwrap_or(false)
                })
            })
            .unwrap_or(false),
        "tag-filtered list should include updated default session: {tagged}"
    );

    let archive_output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args(["session", "archive", "default"])
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight session archive");
    assert!(
        archive_output.status.success(),
        "session archive exited with {:?}, stderr: {}",
        archive_output.status.code(),
        String::from_utf8_lossy(&archive_output.stderr)
    );
    let archive_body = String::from_utf8_lossy(&archive_output.stdout);
    assert!(archive_body.contains("Session default status set to archived."));

    let archived_output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args(["session", "list", "--status", "archived"])
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight session list --status archived");
    assert!(
        archived_output.status.success(),
        "session archived list exited with {:?}, stderr: {}",
        archived_output.status.code(),
        String::from_utf8_lossy(&archived_output.stderr)
    );
    let archived_body = String::from_utf8_lossy(&archived_output.stdout);
    let archived: serde_json::Value = serde_json::from_str(&archived_body)
        .unwrap_or_else(|_| panic!("archived list should print JSON, got: {archived_body}"));
    assert!(
        archived
            .as_array()
            .map(|rows| rows
                .iter()
                .any(|row| row["id"] == "default" && row["status"] == "archived"))
            .unwrap_or(false),
        "archived list should include default: {archived}"
    );

    let restore_output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args(["session", "restore", "default"])
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight session restore");
    assert!(
        restore_output.status.success(),
        "session restore exited with {:?}, stderr: {}",
        restore_output.status.code(),
        String::from_utf8_lossy(&restore_output.stderr)
    );
    let restore_body = String::from_utf8_lossy(&restore_output.stdout);
    assert!(restore_body.contains("Session default status set to dormant."));

    let dormant_output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args(["session", "list", "--status", "dormant"])
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight session list --status dormant");
    assert!(
        dormant_output.status.success(),
        "session dormant list exited with {:?}, stderr: {}",
        dormant_output.status.code(),
        String::from_utf8_lossy(&dormant_output.stderr)
    );
    let dormant_body = String::from_utf8_lossy(&dormant_output.stdout);
    let dormant: serde_json::Value = serde_json::from_str(&dormant_body)
        .unwrap_or_else(|_| panic!("dormant list should print JSON, got: {dormant_body}"));
    assert!(
        dormant
            .as_array()
            .map(|rows| rows
                .iter()
                .any(|row| row["id"] == "default" && row["status"] == "dormant"))
            .unwrap_or(false),
        "dormant list should include restored default: {dormant}"
    );

    let delete_output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args(["session", "delete", "default"])
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight session delete");
    assert!(
        delete_output.status.success(),
        "session delete exited with {:?}, stderr: {}",
        delete_output.status.code(),
        String::from_utf8_lossy(&delete_output.stderr)
    );
    let delete_body = String::from_utf8_lossy(&delete_output.stdout);
    assert!(delete_body.contains("Session default deleted."));

    let (status, body) = get_auth(&format!("{base}/sessions"), &d.token);
    assert_eq!(status, 200);
    assert!(
        !body.contains("\"default\""),
        "deleted session should not be listed: {body}"
    );
}

#[test]
fn cli_handoff_prints_session_attach_payload() {
    let d = spawn_daemon();
    let exe = env!("CARGO_BIN_EXE_coderight");
    let base = format!("http://127.0.0.1:{}", d.port);

    let output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args(["handoff", "default"])
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight handoff");

    assert!(
        output.status.success(),
        "handoff exited with {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let body = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|_| panic!("handoff should print JSON, got: {body}"));
    assert_eq!(value["kind"], "coderight.session_handoff.v1");
    assert_eq!(value["session"]["id"], "default");
    assert_eq!(value["daemon"]["baseUrl"], base);
    assert_eq!(value["daemon"]["tokenHeader"], "Authorization");
    assert_eq!(value["daemon"]["tokenScheme"], "Bearer");
    assert_eq!(value["daemon"]["accessToken"], d.token);
    assert_eq!(value["endpoints"]["messages"], "/sessions/default/messages");
    assert_eq!(
        value["endpoints"]["eventsHistory"],
        "/sessions/default/events/history?after=0"
    );
    assert_eq!(value["continuity"]["after"], 0);
    assert!(
        value["continuity"]["highWaterMark"].as_i64().unwrap_or(-1) >= 0,
        "handoff must expose non-negative event high-water mark: {value}"
    );
    assert_eq!(
        value["continuity"]["replayEndpoint"],
        value["endpoints"]["eventsHistory"]
    );
    assert_eq!(
        value["continuity"]["streamEndpoint"],
        value["endpoints"]["eventsSse"]
    );
    assert_eq!(value["continuity"]["resyncEvent"], "streamResyncRequired");
    assert_eq!(value["continuity"]["deduplicateBy"], "sessionId+seq");
    assert_eq!(
        value["webUrl"],
        format!("{base}/?pane=session&session=default")
    );
}

#[test]
fn cli_export_prints_session_transcript() {
    let d = spawn_daemon();
    let exe = env!("CARGO_BIN_EXE_coderight");
    let base = format!("http://127.0.0.1:{}", d.port);
    let marker = format!("cli export e2e {}", std::process::id());

    let (status, body) = post_json_auth(
        &format!("{base}/sessions/default/messages"),
        &format!(r#"{{"text":"{marker}"}}"#),
        &d.token,
    );
    assert_eq!(status, 200, "post message body: {body}");

    let json_output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args(["export", "default", "--json"])
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight export --json");
    assert!(
        json_output.status.success(),
        "export --json exited with {:?}, stderr: {}",
        json_output.status.code(),
        String::from_utf8_lossy(&json_output.stderr)
    );
    let body = String::from_utf8_lossy(&json_output.stdout);
    let value: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|_| panic!("export --json should print JSON, got: {body}"));
    assert_eq!(value["sessionId"], "default");
    assert!(
        value["messageCount"].as_u64().unwrap_or_default() >= 1,
        "export should contain at least the posted user message: {value}"
    );
    assert!(
        value["messages"]
            .as_array()
            .map(|messages| messages.iter().any(|m| m["content"] == marker))
            .unwrap_or(false),
        "exported messages should contain posted marker: {value}"
    );

    let markdown_output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&d.home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args(["transcript", "default"])
        .env("RIGHTKIT_DAEMON_URL", &base)
        .env("RIGHTKIT_DAEMON_TOKEN", &d.token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight transcript");
    assert!(
        markdown_output.status.success(),
        "transcript exited with {:?}, stderr: {}",
        markdown_output.status.code(),
        String::from_utf8_lossy(&markdown_output.stderr)
    );
    let markdown = String::from_utf8_lossy(&markdown_output.stdout);
    assert!(
        markdown.contains("**Session export:** default"),
        "{markdown}"
    );
    assert!(markdown.contains("## User"), "{markdown}");
    assert!(markdown.contains(&marker), "{markdown}");
}

#[test]
fn cli_diff_prints_real_git_status_and_patch() {
    let exe = env!("CARGO_BIN_EXE_coderight");
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let repo = e2e_temp_root().join(format!(
        "coderight-cli-diff-{}-{unique}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).expect("create repo dir");

    fn git(repo: &std::path::Path, args: &[&str]) {
        let output = rightkit_child_process::command_std("git")
            .args(args)
            .current_dir(repo)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap_or_else(|e| panic!("run git {args:?}: {e}"));
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    git(&repo, &["init"]);
    std::fs::write(repo.join("file.txt"), "one\n").expect("write initial file");
    git(&repo, &["add", "file.txt"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test User",
            "commit",
            "-m",
            "initial",
        ],
    );
    std::fs::write(repo.join("file.txt"), "one\ntwo\n").expect("write changed file");

    let diff_suite = e2e_temp_dir("cli-diff-suite");
    let output = rightkit_child_process::command_std(exe)
        .envs([crate::common::suite_env(&diff_suite)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .args(["diff", repo.to_str().expect("repo path")])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run coderight diff");

    let _ = std::fs::remove_dir_all(&repo);
    assert!(
        output.status.success(),
        "diff exited with {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let body = String::from_utf8_lossy(&output.stdout);
    assert!(body.contains("**Diff:**"), "{body}");
    assert!(body.contains("Scope: unstaged"), "{body}");
    assert!(body.contains("Status:"), "{body}");
    assert!(body.contains("file.txt"), "{body}");
    assert!(body.contains("+two"), "{body}");
}
