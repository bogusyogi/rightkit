//! Shared real-process daemon spawn/control helpers for qualification
//! drivers (EVT-005, EVT-006). Adapted from the `executable()` /
//! `isolated_command()` / `start_daemon()` pattern in
//! `tests/daemon_lifecycle.rs`, extended to accept extra environment
//! variables (the fault-injection env vars) and to support runs that are
//! expected to crash rather than serve.

use serde::Deserialize;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Deserialize)]
pub struct StartupContract {
    #[serde(rename = "Port")]
    pub port: u16,
    #[serde(rename = "Token")]
    pub token: String,
    #[serde(rename = "DatabasePath")]
    pub database_path: String,
    #[serde(rename = "Pid")]
    pub pid: u32,
    #[serde(rename = "BootEpochMs")]
    pub boot_epoch_ms: u128,
    #[serde(rename = "ProtocolVersion")]
    pub protocol_version: u16,
}

pub struct TestDaemon {
    pub child: Child,
    pub startup: StartupContract,
}

impl TestDaemon {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.startup.port)
    }

    #[allow(dead_code)]
    pub fn is_running(&mut self) -> bool {
        self.child
            .try_wait()
            .expect("probe daemon process")
            .is_none()
    }

    /// Reap fixture owner & its Windows descendants before TempDir cleanup.
    pub fn stop(&mut self) {
        stop_child(&mut self.child);
    }
}

/// Stop & reap a fixture process tree before its temporary storage drops.
pub fn stop_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        // Killing only CodeRight leaves Chrome holding its profile open.
        // Restrict tree termination to this fixture's still-live child.
        #[cfg(windows)]
        {
            let system_root = std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            let _ = rightkit_child_process::command_std(
                system_root.join("System32").join("taskkill.exe"),
            )
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        }
        let _ = child.kill();
    }
    let _ = child.wait();
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Write `<app root>/config.toml` (the app root of the suite rooted at `home`) that registers a single
/// keyless OpenAI-compatible provider route pointed at `base_url`, and makes
/// it the default model. This is the real production config surface
/// (`rightkit_app_settings::agent_config::ProviderRouteConfig`, loaded by
/// `main_runtime_config::resolve_engine_config` at daemon boot) for pointing
/// the daemon's engine at a local/test provider — no source change needed.
/// Must be called before `start_daemon`, since the route is only resolved at
/// boot.
pub fn write_local_provider_route_config(home: &Path, base_url: &str, model: &str) {
    let dir = crate::common::app_root(&home);
    std::fs::create_dir_all(&dir).expect("create the CodeRight app root");
    let toml = format!(
        "[model]\ndefault-model = \"{model}\"\n\n\
         [provider-routes.qual-fixture]\n\
         display-name = \"Qualification fixture\"\n\
         base-url = \"{base_url}\"\n\
         model = \"{model}\"\n\
         api-key-required = false\n"
    );
    std::fs::write(dir.join("config.toml"), toml).expect("write provider-route config.toml");
}

include!("host_env.rs");

pub fn executable() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_coderight")
        .map(PathBuf::from)
        .expect("Cargo must provide CARGO_BIN_EXE_coderight for this integration test")
}

pub fn isolated_command(home: &Path, extra_env: &[(&str, &str)]) -> Command {
    let mut command = rightkit_child_process::command_std(executable());
    // Explicit direct context mode: qualification daemons must never reach
    // Membrane discovery/provisioning/activation/repair/residency or the
    // installed root, all of which live behind the Membrane startup branch.
    command
        .arg("serve")
        // Provider discovery confines its version probe to process cwd.
        // Keep qualification probes in fixture home, never source checkout.
        .current_dir(home)
        .env("RIGHTKIT_CONTEXT", "off")
        .arg("--port")
        .arg("0");
    command
        .envs([crate::common::suite_env(home)])
        .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
        .env("RIGHTKIT_DISABLE_DEFAULT_MCP", "1")
        // Isolated qualification: this daemon has no canonical Membrane
        // binding, which canon permits for a deliberately isolated test.
        .env("RIGHTKIT_QUAL_ISOLATED_MEMBRANE", "1")
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("RIGHTKIT_DISABLE_OS_KEYCHAIN", "1")
        // Arm the qualification fault injectors for this spawned daemon only;
        // without this marker the production binaries ignore the point vars.
        .env("RIGHTKIT_QUAL_FAULTS_ARMED", "1")
        .env_remove("RIGHTKIT_DAEMON_URL")
        .env_remove("RIGHTKIT_DAEMON_TOKEN")
        .env_remove("RIGHTKIT_SERVE_FORCE")
        .env_remove("RIGHTKIT_QUAL_EVT005_FAULT")
        .env_remove("RIGHTKIT_QUAL_EVT006_FAULT")
        .stdin(Stdio::null())
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        // Keep daemon warnings (sandbox refusals etc.) for diagnosis.
        .stderr(
            std::fs::File::create(std::env::temp_dir().join(format!(
                    "coderight-e2e-daemon-{}.log",
                    home.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default()
                )))
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null()),
        );
    restore_host_env(&mut command);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command
}

/// Start a real daemon and wait for it to publish its startup contract and
/// become healthy. Used both for "clean" runs and for runs that are expected
/// to serve at least one request successfully before crashing later.
pub fn start_daemon(home: &Path, extra_env: &[(&str, &str)]) -> TestDaemon {
    let mut child = isolated_command(home, extra_env)
        .spawn()
        .expect("spawn real coderight daemon");
    let stdout = child.stdout.take().expect("daemon startup stdout pipe");
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send(match result {
            Ok(0) => Err("daemon exited before its startup contract".to_string()),
            Ok(_) => serde_json::from_str::<StartupContract>(line.trim())
                .map_err(|error| format!("invalid daemon startup contract: {error}")),
            Err(error) => Err(format!("could not read daemon startup contract: {error}")),
        });
    });

    let startup = match receiver.recv_timeout(STARTUP_TIMEOUT) {
        Ok(Ok(startup)) => startup,
        Ok(Err(error)) => {
            let status = child.try_wait().ok().flatten();
            stop_child(&mut child);
            panic!("real daemon failed to publish startup contract ({error}); status={status:?}");
        }
        Err(error) => {
            stop_child(&mut child);
            panic!("timed out waiting for real daemon startup contract: {error}");
        }
    };
    // Own cleanup before validating the startup contract: assertion failures
    // must also kill/reap the child before the caller's TempDir drops.
    let daemon = TestDaemon { child, startup };
    assert_eq!(daemon.startup.protocol_version, 1);
    assert!(daemon.startup.port > 0);
    assert!(!daemon.startup.token.is_empty());
    assert!(daemon.startup.pid > 0);
    assert!(daemon.startup.boot_epoch_ms > 0);
    wait_for_health(&daemon);
    daemon
}

pub fn wait_for_health(daemon: &TestDaemon) -> Value {
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    loop {
        if let Some(value) = get_json(&daemon.url(), "/health", &daemon.startup.token) {
            assert_eq!(
                value["ok"], true,
                "health must report a live daemon: {value}"
            );
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "real daemon did not become healthy at {}",
            daemon.url()
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// Loopback-only address policy for tests that talk to a daemon or fixture on this machine.
pub fn loopback_policy() -> rightkit_http::AddressPolicy {
    rightkit_http::AddressPolicy::default().allow_if(
        vec!["127.0.0.1".into(), "localhost".into(), "::1".into()],
        |ip| ip.is_loopback(),
    )
}

/// Bounded blocking `rightkit-http` client: 10 s per request, no retries, no redirects.
pub fn client() -> rightkit_http::Client {
    rightkit_http::Client::new(rightkit_http::ClientConfig {
        request_timeout: Duration::from_secs(10),
        response_timeout: Some(Duration::from_secs(10)),
        retry: rightkit_http::RetryPolicy::none(),
        address_policy: loopback_policy(),
        ..rightkit_http::ClientConfig::default()
    })
}

/// Send one request on a bounded client; any status is a response.
pub fn send(
    request: &rightkit_http::Request,
) -> Result<rightkit_http::Response, rightkit_http::HttpError> {
    client().send_any(request)
}

/// One bounded daemon call: `method` on `url` with the bearer `token`; `body`, when present,
/// is sent as JSON. Any status is a response.
pub fn call(
    method: rightkit_http::Method,
    url: impl Into<String>,
    token: &str,
    body: Option<&Value>,
) -> Result<rightkit_http::Response, rightkit_http::HttpError> {
    let mut request = rightkit_http::Request::new(method, url).bearer(token);
    if let Some(body) = body {
        request = request.json(body)?;
    }
    send(&request)
}

/// `true` for a 2xx status.
pub fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// Open the daemon's SSE feed at `url` with the bearer `token`; the returned reader yields
/// the raw event stream until the daemon closes it. `None` when the connection fails.
pub fn open_event_stream(url: &str, token: &str) -> Option<Box<dyn std::io::Read + Send>> {
    let client = rightkit_http::Client::new(rightkit_http::ClientConfig {
        response_timeout: Some(Duration::from_secs(30)),
        stream_timeout: None,
        retry: rightkit_http::RetryPolicy::none(),
        address_policy: loopback_policy(),
        ..rightkit_http::ClientConfig::default()
    });
    let request = rightkit_http::Request::get(url)
        .bearer(token)
        .header("accept", "text/event-stream");
    client
        .stream_any(&request)
        .ok()
        .map(rightkit_http::StreamResponse::into_reader)
}

pub fn get_json(base_url: &str, path: &str, token: &str) -> Option<Value> {
    let response =
        send(&rightkit_http::Request::get(format!("{base_url}{path}")).bearer(token)).ok()?;
    if !is_success(response.status) {
        return None;
    }
    response.json().ok()
}

pub fn post_json(base_url: &str, path: &str, token: &str, body: &Value) -> (u16, Value) {
    let request = rightkit_http::Request::post(format!("{base_url}{path}"))
        .bearer(token)
        .json(body)
        .expect("daemon POST body");
    let response = send(&request).expect("bounded daemon POST");
    let value = response.json().unwrap_or(Value::Null);
    (response.status, value)
}

/// Post a request whose handling is expected to crash the daemon process
/// mid-flight (a fault-injection exit). The HTTP round trip itself may
/// error out (connection reset) or may never complete — both are
/// acceptable; the only thing this asserts is nothing about the response.
pub fn post_ignoring_transport_failure(base_url: &str, path: &str, token: &str, body: &Value) {
    if let Ok(request) = rightkit_http::Request::post(format!("{base_url}{path}"))
        .bearer(token)
        .json(body)
    {
        let _ = send(&request);
    }
}

pub fn wait_for_exit(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("probe expected-exit process") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("process did not exit within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(25));
    }
}
