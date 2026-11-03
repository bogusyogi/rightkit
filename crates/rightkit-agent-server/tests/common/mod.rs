//! Shared real-process + real-provider machinery for the EVT-004 and OPS-005
//! qualification drivers.
//!
//! Both tests need the same three things: a real `coderight serve` process
//! talking to a real file-backed SQLite database, a deterministic local
//! OpenAI-compatible SSE provider standing in for a hosted model, and a way
//! to independently reopen the daemon's own SQLite file to check durable
//! truth without trusting the daemon's in-process view of itself. This
//! module is the one event/provider framework both drivers build on — do not
//! duplicate it.
#![allow(dead_code)]

use rightkit_agent_engine_protocol::ProtocolEvent;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

// ── Real daemon process ─────────────────────────────────────────────────────

pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(15);
pub const TURN_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Deserialize)]
struct StartupContract {
    #[serde(rename = "Port")]
    port: u16,
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DatabasePath")]
    database_path: String,
    #[serde(rename = "Pid")]
    pid: u32,
    #[serde(rename = "BootEpochMs")]
    boot_epoch_ms: u128,
    #[serde(rename = "ProtocolVersion")]
    protocol_version: u16,
}

pub struct TestDaemon {
    child: Child,
    startup: StartupContract,
}

impl TestDaemon {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.startup.port)
    }

    pub fn token(&self) -> &str {
        &self.startup.token
    }

    pub fn database_path(&self) -> PathBuf {
        PathBuf::from(&self.startup.database_path)
    }

    pub fn port(&self) -> u16 {
        self.startup.port
    }

    pub fn is_running(&mut self) -> bool {
        self.child
            .try_wait()
            .expect("probe daemon process")
            .is_none()
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn executable() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_coderight")
        .map(PathBuf::from)
        .expect("Cargo must provide CARGO_BIN_EXE_coderight for this integration test")
}

/// Cargo's scratch dir for this test crate; an allowed place for an isolated suite root besides
/// the system temp dir.
fn cargo_tmp() -> &'static Path {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
}

/// Set `RIGHTKIT_SUITE_ROOT` on a command that spawns a CodeRight binary, failing the test when
/// `suite_root` is not a fresh directory inside a temp dir. Returns the CodeRight app root
/// (`<suite_root>/apps/CodeRight`). EVERY spawn of a CodeRight binary in a test goes through
/// this: a `test-mode` binary with no suite root would fall back to the real per-user data folder.
pub fn isolate_state(command: &mut Command, suite_root: &Path) -> PathBuf {
    rightkit_app_settings::agent_config::test_isolation::apply_to_command(
        command,
        suite_root,
        &[cargo_tmp()],
    )
}

/// `(RIGHTKIT_SUITE_ROOT, suite_root)` for builder chains: `.envs([common::suite_env(home)])`.
/// Fails the test when `suite_root` is not inside a temp dir, so no spawn can use the real
/// per-user data folder.
pub fn suite_env(suite_root: impl AsRef<Path>) -> (&'static str, PathBuf) {
    let suite_root = suite_root.as_ref();
    rightkit_app_settings::agent_config::test_isolation::assert_isolated(
        suite_root,
        &[cargo_tmp()],
    );
    (
        rightkit_app_settings::agent_config::test_isolation::SUITE_ROOT_ENV,
        suite_root.to_path_buf(),
    )
}

/// Panic unless `command` already carries an isolated `RIGHTKIT_SUITE_ROOT`.
pub fn assert_state_isolated(command: &Command) {
    rightkit_app_settings::agent_config::test_isolation::assert_command_isolated(
        command,
        &[cargo_tmp()],
    )
}

/// The CodeRight app root a spawned binary uses for `suite_root` (`<suite_root>/apps/CodeRight`),
/// resolved by the product's own rightkit-daemon code. Config, `daemon.port`, databases and
/// journals all live directly inside it.
pub fn app_root(suite_root: &Path) -> PathBuf {
    rightkit_app_settings::agent_config::test_isolation::app_root_in(suite_root)
}

/// Write the user-global config the daemon reads at boot
/// (`<app root>/config.toml`), pointing the default model at
/// a local OpenAI-compatible route with no API key required. This is the
/// only route into `resolve_engine_config` (see
/// `engine/bins/coderight/src/main_runtime_config.rs`) that lets a real
/// daemon process point at an in-test provider double instead of a hosted
/// provider requiring a real credential.
pub fn write_provider_config(home: &Path, base_url: &str, model_id: &str) {
    let dir = app_root(home);
    std::fs::create_dir_all(&dir).expect("create the CodeRight app root");
    let toml = format!(
        "[model]\n\
         default-model = \"{model_id}\"\n\
         \n\
         [provider-routes.qa-mock]\n\
         display-name = \"QA Mock Provider\"\n\
         base-url = \"{base_url}\"\n\
         model = \"{model_id}\"\n\
         api-key-required = false\n"
    );
    std::fs::write(dir.join("config.toml"), toml).expect("write config.toml");
}

fn isolated_command(home: &Path, db: Option<&Path>) -> Command {
    let mut command = rightkit_child_process::command_std(executable());
    // Explicit direct context mode: qualification daemons must never touch
    // Membrane discovery, provisioning, activation, repair, residency, or the
    // installed root — all of which sit behind the Membrane-mode startup
    // branch. `RIGHTKIT_QUAL_ISOLATED_MEMBRANE` alone is insufficient: it
    // only exempts the turn-time binding gate, not startup binding discovery.
    command
        .arg("serve")
        .env("RIGHTKIT_CONTEXT", "off")
        .arg("--port")
        .arg("0");
    if let Some(db) = db {
        command.arg("--db").arg(db);
    }
    isolate_state(&mut command, home);
    command
        // Isolated qualification: this daemon has no canonical Membrane
        // binding, which canon permits for a deliberately isolated test.
        .env("RIGHTKIT_QUAL_ISOLATED_MEMBRANE", "1")
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("RIGHTKIT_DISABLE_OS_KEYCHAIN", "1")
        .env_remove("RIGHTKIT_DAEMON_URL")
        .env_remove("RIGHTKIT_DAEMON_TOKEN")
        .env_remove("RIGHTKIT_SERVE_FORCE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Start a real `coderight serve` process rooted at `home`, having already
/// written a provider config pointing at `provider_base_url` /
/// `provider_model`. Panics with the daemon's stderr on failure so a real
/// provider-resolution defect (wrong config shape, wrong field name, wrong
/// section) is diagnosable instead of silently hanging.
pub fn start_daemon_with_provider(
    home: &Path,
    db: Option<&Path>,
    provider_base_url: &str,
    provider_model: &str,
) -> TestDaemon {
    write_provider_config(home, provider_base_url, provider_model);
    let mut child = isolated_command(home, db)
        .spawn()
        .expect("spawn real coderight daemon");
    let stdout = child.stdout.take().expect("daemon startup stdout pipe");
    let mut stderr = child.stderr.take().expect("daemon stderr pipe");
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
            let mut captured = String::new();
            let _ = stderr.read_to_string(&mut captured);
            let status = child.try_wait().ok().flatten();
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "real daemon failed to publish startup contract ({error}); status={status:?}\nstderr:\n{captured}"
            );
        }
        Err(error) => {
            let mut captured = String::new();
            let _ = stderr.read_to_string(&mut captured);
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "timed out waiting for real daemon startup contract: {error}\nstderr:\n{captured}"
            );
        }
    };
    assert_eq!(startup.protocol_version, 1);
    assert!(startup.port > 0);
    assert!(!startup.token.is_empty());
    assert!(startup.pid > 0);
    assert!(startup.boot_epoch_ms > 0);

    // Drain stderr in the background so the daemon never blocks on a full
    // pipe, teeing into `home/daemon-stderr.log` so a later failure can still
    // inspect what the daemon actually reported (engine run errors surface
    // only through tracing, never through durable events).
    let stderr_log = home.join("daemon-stderr.log");
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&stderr_log)
            .ok();
        let mut line = String::new();
        while let Ok(count) = reader.read_line(&mut line) {
            if count == 0 {
                break;
            }
            if let Some(file) = log.as_mut() {
                let _ = file.write_all(line.as_bytes());
            }
            line.clear();
        }
    });

    let daemon = TestDaemon { child, startup };
    wait_for_health(&daemon);
    daemon
}

fn wait_for_health(daemon: &TestDaemon) {
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    loop {
        if let Some(value) = get_json(&daemon.url(), "/health", daemon.token()) {
            assert_eq!(
                value["ok"], true,
                "health must report a live daemon: {value}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "real daemon did not become healthy at {}",
            daemon.url()
        );
        thread::sleep(Duration::from_millis(50));
    }
}

pub fn get_json(base_url: &str, path: &str, token: &str) -> Option<Value> {
    crate::support::daemon_process::get_json(base_url, path, token)
}

pub fn get_json_or_panic(base_url: &str, path: &str, token: &str) -> Value {
    let response = crate::support::daemon_process::send(
        &rightkit_http::Request::get(format!("{base_url}{path}")).bearer(token),
    )
    .unwrap_or_else(|error| panic!("GET {path} failed: {error}"));
    let status = response.status;
    let body: Value = response
        .json()
        .unwrap_or_else(|error| panic!("GET {path} returned non-JSON body: {error}"));
    assert!(
        crate::support::daemon_process::is_success(status),
        "GET {path} -> {status}: {body}"
    );
    body
}

pub fn post_json(base_url: &str, path: &str, token: &str, body: Value) -> rightkit_http::Response {
    let request = rightkit_http::Request::post(format!("{base_url}{path}"))
        .bearer(token)
        .json(&body)
        .expect("daemon POST body");
    crate::support::daemon_process::send(&request).expect("bounded daemon POST")
}

// ── Session helpers ──────────────────────────────────────────────────────────

/// Create a session and post one user message that will drive a real turn
/// against the mock provider. Returns once the message has been accepted
/// (HTTP 200), not once the turn has finished — callers poll
/// `wait_for_turn_finished`.
pub fn start_turn(daemon: &TestDaemon, session_id: &str, workspace: &Path, text: &str) {
    let create = post_json(
        &daemon.url(),
        "/sessions",
        daemon.token(),
        json!({
            "id": session_id,
            "workspaceRoot": workspace,
            "title": "evt004-ops005 qualification",
        }),
    );
    assert!(
        crate::support::daemon_process::is_success(create.status),
        "session creation failed: {}",
        create.status
    );

    let message = post_json(
        &daemon.url(),
        &format!("/sessions/{session_id}/messages"),
        daemon.token(),
        json!({ "text": text }),
    );
    let status = message.status;
    let body: Value = message.json().unwrap_or(Value::Null);
    assert!(
        crate::support::daemon_process::is_success(status),
        "post message failed: {status}: {body}"
    );
}

/// Poll durable history (never the live conduit) until a `sessionTurnFinished`
/// event for `session_id` appears, returning its `status` and `turnId`.
/// Polling durable history rather than the SSE stream is deliberate: this
/// helper must observe terminal settlement exactly the way a reconnecting,
/// previously-absent client would.
pub fn wait_for_turn_finished(daemon: &TestDaemon, session_id: &str) -> (String, String) {
    let deadline = Instant::now() + TURN_TIMEOUT;
    loop {
        let events = get_json(
            &daemon.url(),
            &format!("/sessions/{session_id}/events/history?kind=sessionTurnFinished"),
            daemon.token(),
        );
        if let Some(Value::Array(rows)) = events {
            if let Some(last) = rows.last() {
                let status = last["status"].as_str().unwrap_or_default().to_string();
                let turn_id = last["turnId"].as_str().unwrap_or_default().to_string();
                if !status.is_empty() {
                    return (status, turn_id);
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "turn for session {session_id} did not reach durable terminal settlement in time"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

// ── Mock OpenAI-compatible SSE provider ─────────────────────────────────────

/// A tiny, deterministic local HTTP server speaking the OpenAI-compatible
/// streaming chat-completions wire shape well enough for
/// `rightkit_llm::agent_providers::OpenAiCompatClient` to consume. It serves the exact
/// same scripted multi-chunk delta stream to every request it accepts, so
/// two independently-run turns against it are provably driven by identical
/// provider output — any difference in the resulting durable truth is a
/// CodeRight defect, not a fixture difference.
pub struct MockProvider {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
    requests_seen: Arc<AtomicUsize>,
    resume_stream: Arc<AtomicBool>,
}

impl MockProvider {
    /// `deltas` are streamed as separate SSE chunks (content deltas), then a
    /// terminal `finish_reason: "stop"` chunk plus `[DONE]` is sent. A small
    /// delay between chunks keeps the stream observably incremental for
    /// tests that want to catch it mid-flight.
    pub fn start(deltas: Vec<String>) -> Self {
        Self::start_internal(deltas, false)
    }

    pub fn start_paused_before_terminal(deltas: Vec<String>) -> Self {
        Self::start_internal(deltas, true)
    }

    pub fn resume_stream(&self) {
        self.resume_stream.store(true, Ordering::SeqCst);
    }

    fn start_internal(deltas: Vec<String>, paused: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock provider");
        listener
            .set_nonblocking(true)
            .expect("mock provider nonblocking accept");
        let port = listener.local_addr().expect("mock provider addr").port();
        let stop = Arc::new(AtomicBool::new(false));
        let requests_seen = Arc::new(AtomicUsize::new(0));
        let resume_stream = Arc::new(AtomicBool::new(!paused));
        let stream_gate = resume_stream.clone();

        let stop_loop = stop.clone();
        let counter = requests_seen.clone();
        let handle = thread::spawn(move || loop {
            if stop_loop.load(Ordering::SeqCst) {
                return;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let deltas = deltas.clone();
                    let gate = stream_gate.clone();
                    thread::spawn(move || serve_one_completion(stream, &deltas, &gate));
                }
                Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return,
            }
        });

        Self {
            port,
            stop,
            handle: Some(handle),
            requests_seen,
            resume_stream,
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    pub fn request_count(&self) -> usize {
        self.requests_seen.load(Ordering::SeqCst)
    }
}

impl Drop for MockProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve_one_completion(mut stream: TcpStream, deltas: &[String], resume: &AtomicBool) {
    // Read (and discard) the request headers + body. We don't need to parse
    // the request beyond finding Content-Length, since this double answers
    // every request identically regardless of prompt content.
    let mut reader = BufReader::new(stream.try_clone().expect("clone mock provider stream"));
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                let trimmed = line.trim_end();
                if trimmed.is_empty() {
                    break;
                }
                if let Some(value) = trimmed
                    .to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(str::trim)
                    .and_then(|value| value.parse::<usize>().ok())
                {
                    content_length = value;
                }
            }
        }
    }
    if content_length > 0 {
        let mut body = vec![0u8; content_length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
    }

    let header = "HTTP/1.1 200 OK\r\n\
        Content-Type: text/event-stream\r\n\
        Cache-Control: no-cache\r\n\
        Connection: close\r\n\
        \r\n";
    if stream.write_all(header.as_bytes()).is_err() {
        return;
    }

    for delta in deltas {
        let chunk = json!({
            "choices": [{
                "delta": { "content": delta },
                "finish_reason": Value::Null,
            }]
        });
        let line = format!("data: {chunk}\n\n");
        if stream.write_all(line.as_bytes()).is_err() {
            return;
        }
        let _ = stream.flush();
        thread::sleep(Duration::from_millis(20));
    }

    let deadline = Instant::now() + Duration::from_secs(15);
    while !resume.load(Ordering::SeqCst) {
        if Instant::now() >= deadline {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let finish = json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": 12,
            "completion_tokens": deltas.len() as u32,
        }
    });
    let _ = stream.write_all(format!("data: {finish}\n\n").as_bytes());
    let _ = stream.flush();
    let _ = stream.write_all(b"data: [DONE]\n\n");
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

// ── Raw SSE subscriber (real disconnect over a real socket) ────────────────

/// A hand-rolled SSE client over a raw `TcpStream`, used where a test needs
/// to *actually* sever the connection (dropping this struct closes the
/// socket immediately) rather than merely stop polling a library client that
/// keeps the connection alive underneath.
///
/// Sends the request as HTTP/1.0 deliberately: hyper never applies chunked
/// transfer-encoding to an HTTP/1.0 response, so the bytes read back are the
/// server's raw `data: ...\n\n` SSE frames with no chunk-length framing to
/// decode — the simplest correct way to read a live, unbounded stream by
/// hand.
pub struct RawSseSession {
    stream: TcpStream,
    buf: Vec<u8>,
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

impl RawSseSession {
    pub fn connect(daemon: &TestDaemon, session_id: &str, after: i64) -> Self {
        let mut stream =
            TcpStream::connect(("127.0.0.1", daemon.port())).expect("connect raw sse socket");
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("set sse socket read timeout");
        let request = format!(
            "GET /sessions/{session_id}/events?after={after} HTTP/1.0\r\n\
             Host: 127.0.0.1\r\n\
             Authorization: Bearer {}\r\n\
             \r\n",
            daemon.token()
        );
        stream
            .write_all(request.as_bytes())
            .expect("send raw sse request");

        let mut raw = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let header_end = find_subslice(&raw, b"\r\n\r\n");
            if let Some(pos) = header_end {
                let body = raw[pos + 4..].to_vec();
                return Self { stream, buf: body };
            }
            assert!(
                Instant::now() < deadline,
                "sse response headers never arrived"
            );
            let mut chunk = [0u8; 512];
            match stream.read(&mut chunk) {
                Ok(0) => panic!("sse connection closed before headers completed"),
                Ok(n) => raw.extend_from_slice(&chunk[..n]),
                Err(ref error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::TimedOut => {}
                Err(error) => panic!("sse header read failed: {error}"),
            }
        }
    }

    /// Read the next decoded SSE `data:` payload as JSON, or `None` if
    /// nothing arrives before `deadline`. Non-data lines (keep-alive
    /// comments) are skipped transparently.
    pub fn next_event(&mut self, deadline: Instant) -> Option<Value> {
        loop {
            if let Some(pos) = find_subslice(&self.buf, b"\n\n") {
                let frame: Vec<u8> = self.buf.drain(..pos + 2).collect();
                for line in String::from_utf8_lossy(&frame).lines() {
                    if let Some(payload) = line.strip_prefix("data:") {
                        if let Ok(value) = serde_json::from_str::<Value>(payload.trim()) {
                            return Some(value);
                        }
                    }
                }
                continue;
            }
            if Instant::now() >= deadline {
                return None;
            }
            let mut chunk = [0u8; 1024];
            match self.stream.read(&mut chunk) {
                Ok(0) => return None,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(ref error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::TimedOut => {}
                Err(_) => return None,
            }
        }
    }
}

// ── Independent durable-truth reopening ─────────────────────────────────────

/// Reopen the daemon's own SQLite file as a brand-new `Storage` handle,
/// independent of the running daemon's in-process handle. This is the only
/// object either qualification driver treats as ground truth.
pub fn reopen_storage(db_path: &Path) -> rightkit_agent_engine_storage::Storage {
    rightkit_agent_engine_storage::Storage::open(db_path)
        .expect("independently reopen daemon SQLite file")
}

/// All durable events for a session, decoded, filtered to `is_durable_fact`
/// truth (deltas and other live-only projections excluded), in seq order.
pub fn durable_events(
    storage: &rightkit_agent_engine_storage::Storage,
    session_id: &str,
) -> Vec<rightkit_agent_engine_storage::EventRow> {
    storage
        .all_events(session_id)
        .expect("read all_events from independently reopened storage")
}

pub fn decode_kind(row: &rightkit_agent_engine_storage::EventRow) -> Option<String> {
    serde_json::from_str::<Value>(&row.payload_json)
        .ok()
        .and_then(|value| value["$kind"].as_str().map(str::to_string))
}

/// Decode every row's payload and keep only canonical durable-fact truth —
/// the same filter production rehydration applies
/// (`ProtocolEvent::is_durable_fact`), reimplemented here only as "decode +
/// call the real public predicate", never as a re-derivation of what counts
/// as durable.
pub fn decode_durable_facts(
    rows: &[rightkit_agent_engine_storage::EventRow],
) -> Vec<ProtocolEvent> {
    rows.iter()
        .filter_map(|row| serde_json::from_str::<ProtocolEvent>(&row.payload_json).ok())
        .filter(|event| event.is_durable_fact())
        .collect()
}

/// The latest durable terminal-turn status, mirroring
/// `rehydrate::rehydrate_terminal_turn`'s "last SessionTurnFinished wins"
/// rule, but implemented independently against durable facts only so this
/// test module never has to reach into the daemon binary's private module.
pub fn terminal_status(events: &[ProtocolEvent]) -> Option<(String, String)> {
    events.iter().rev().find_map(|event| match event {
        ProtocolEvent::SessionTurnFinished {
            status, turn_id, ..
        } => Some((status.clone(), turn_id.clone())),
        _ => None,
    })
}

/// A comparable projection of the human-facing transcript built purely from
/// durable facts: user turns, completed assistant messages, and tool
/// results, in seq order. Two runs with identical transcript signatures
/// carry the same conversation regardless of any intervening
/// disconnect/reconnect.
pub fn transcript_signature(events: &[ProtocolEvent]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| match event {
            ProtocolEvent::UserMessageSubmitted { text, .. } => {
                Some(json!({"role": "user", "text": text}))
            }
            ProtocolEvent::AssistantMessageCompleted { message, .. } => Some(json!({
                "role": "assistant",
                "message": strip_created_at(serde_json::to_value(message).unwrap_or(Value::Null)),
            })),
            ProtocolEvent::ToolResultAdded { result, .. } => Some(json!({
                "role": "tool",
                "result": strip_created_at(serde_json::to_value(result).unwrap_or(Value::Null)),
            })),
            _ => None,
        })
        .collect()
}

/// Two independently-run turns are never expected to complete at the exact
/// same wall-clock instant; the qualification drivers compare conversation
/// *content*, not timestamps, so `createdAt` is normalized out of the
/// comparison rather than compared (and rather than being ignored by luck).
fn strip_created_at(mut value: Value) -> Value {
    if let Value::Object(ref mut map) = value {
        map.insert("createdAt".to_string(), Value::Null);
    }
    value
}
