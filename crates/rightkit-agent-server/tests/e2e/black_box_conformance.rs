//! Executable black-box machine conformance suite (S:P8 / V02 / M06).
//!
//! The three `engine/fixtures/machine-contract/*.jsonl` goldens
//! (`error-envelope.jsonl`, `session-idempotency.jsonl`,
//! `acp-negotiation.jsonl`) existed with zero consumers before this file:
//! nothing loaded or executed them. This is the first real Cargo target that
//! does -- it spawns the actual `coderight` binary (daemon + ACP stdio
//! bridge, the same processes every other black-box client talks to) and
//! drives each golden's documented contract against it.
//!
//! This Rust driver also orchestrates TS/Python/shell clients when explicitly
//! selected. A SQLite trigger drives real durable-admission failure here;
//! `evt005_admission_restart` drives unknown outcome after process failure,
//! and `acp_prompt_timeout_conformance` drives the real provider timeout.

use serde::Deserialize;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const FIXTURES_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/machine-contract"
);

#[derive(Debug, Deserialize)]
struct StartupContract {
    #[serde(rename = "Port")]
    port: u16,
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DatabasePath")]
    database_path: String,
}

struct TestDaemon {
    child: Child,
    startup: StartupContract,
}

impl TestDaemon {
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.startup.port)
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
        .expect("Cargo must provide CARGO_BIN_EXE_coderight")
}

fn start_daemon(home: &Path) -> TestDaemon {
    let mut child = rightkit_child_process::command_std(executable())
        .arg("serve")
        .env("RIGHTKIT_CONTEXT", "off")
        .arg("--port")
        .arg("0")
        .envs([crate::common::suite_env(home)])
        // Protocol-only qualification uses an isolated home, like support/daemon_process.rs.
        .env("RIGHTKIT_QUAL_ISOLATED_MEMBRANE", "1")
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("RIGHTKIT_DISABLE_OS_KEYCHAIN", "1")
        .env_remove("RIGHTKIT_DAEMON_URL")
        .env_remove("RIGHTKIT_DAEMON_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn isolated daemon");
    let stdout = child.stdout.take().expect("startup stdout");
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout)
            .read_line(&mut line)
            .map_err(|error| error.to_string())
            .and_then(|count| {
                if count == 0 {
                    Err("daemon exited before startup contract".to_string())
                } else {
                    serde_json::from_str(line.trim()).map_err(|error| error.to_string())
                }
            });
        let _ = sender.send(result);
    });
    let startup = receiver
        .recv_timeout(STARTUP_TIMEOUT)
        .expect("startup contract timeout")
        .expect("valid startup contract");
    let daemon = TestDaemon { child, startup };

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if crate::support::daemon_process::send(
            &rightkit_http::Request::get(format!("{}/health", daemon.url()))
                .bearer(&daemon.startup.token),
        )
        .map(|response| crate::support::daemon_process::is_success(response.status))
        .unwrap_or(false)
        {
            return daemon;
        }
        assert!(Instant::now() < deadline, "daemon health timeout");
        thread::sleep(Duration::from_millis(50));
    }
}

fn load_golden(name: &str) -> Vec<Value> {
    let path = Path::new(FIXTURES_DIR).join(name);
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read golden {}: {error}", path.display()));
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("parse golden line `{line}`: {error}"))
        })
        .collect()
}

/// Every real error this suite observes must conform to the
/// `coderight.error.v1` envelope shape `error-envelope.jsonl` declares:
/// stable field set, typed correctly, `remediation` present (string or
/// null).
fn assert_conforms_to_error_envelope_schema(error: &Value) {
    assert_eq!(
        error["schemaVersion"], "coderight.error.v1",
        "error envelope schema: {error}"
    );
    assert!(
        error["code"].is_string(),
        "error.code must be a string: {error}"
    );
    assert!(
        error["detail"].is_string(),
        "error.detail must be a string: {error}"
    );
    assert!(
        error["correlationId"].is_string(),
        "error.correlationId must be a string: {error}"
    );
    assert!(
        error["retryable"].is_boolean(),
        "error.retryable must be a bool: {error}"
    );
    assert!(
        error["effectState"].is_string(),
        "error.effectState must be a string: {error}"
    );
    assert!(
        error.get("remediation").is_some(),
        "error.remediation must be present (string or null): {error}"
    );
}

#[test]
fn error_envelope_golden_entries_are_self_conformant() {
    // S:P8: the golden is authoritative content, but its own rows must obey
    // the schema they document. This is a real consumer -- it is loaded and
    // asserted, not merely referenced in prose.
    let entries = load_golden("error-envelope.jsonl");
    assert_eq!(entries.len(), 2, "error-envelope.jsonl row count");
    for entry in &entries {
        assert_conforms_to_error_envelope_schema(entry);
    }
    let codes: Vec<&str> = entries
        .iter()
        .map(|entry| entry["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, vec!["session_not_found", "event_persistence_failed"]);
}

#[test]
fn durable_event_failure_drives_real_error_envelope_golden() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let daemon = start_daemon(home.path());
    let session_id = "s-p8-storage-fault";
    let response = crate::support::daemon_process::send(
        &rightkit_http::Request::post(format!("{}/sessions", daemon.url()))
            .bearer(&daemon.startup.token)
            .json(&serde_json::json!({"id": session_id, "workspaceRoot": workspace.path()}))
            .expect("JSON request body"),
    )
    .unwrap();
    assert!(crate::support::daemon_process::is_success(response.status));
    // Inject a real SQLite write failure in this disposable database only.
    // Session creation succeeds, but durable admission cannot commit.
    let conn = rightkit_db::rusqlite::Connection::open(&daemon.startup.database_path).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER qualification_reject_admission BEFORE INSERT ON events \
         WHEN NEW.kind = 'userMessageSubmitted' \
         BEGIN SELECT RAISE(ABORT, 'qualification durable admission failure'); END;",
    )
    .unwrap();
    let response = crate::support::daemon_process::send(
        &rightkit_http::Request::post(format!("{}/sessions/{session_id}/messages", daemon.url()))
            .bearer(&daemon.startup.token)
            .json(&serde_json::json!({"text": "must not be accepted", "nonce": "storage-fault"}))
            .expect("JSON request body"),
    )
    .unwrap();
    assert_eq!(response.status, 503);
    let body: Value = response.json().unwrap();
    let golden = load_golden("error-envelope.jsonl")
        .into_iter()
        .find(|row| row["code"] == "event_persistence_failed")
        .unwrap();
    assert_conforms_to_error_envelope_schema(&body);
    for field in [
        "schemaVersion",
        "code",
        "retryable",
        "effectState",
        "remediation",
    ] {
        assert_eq!(body[field], golden[field], "golden {field}: {body}");
    }
    assert!(body["detail"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND kind = 'userMessageSubmitted'",
            [session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "failed admission must not become durable");
}

/// Drive idempotency against the real daemon, including the required `detail`
/// field on pending-outcome errors. S:P8 closure additionally requires its
/// cross-language and fault-case qualification.
#[test]
fn session_idempotency_golden_drives_real_message_submission() {
    let home = tempfile::tempdir().expect("isolated daemon home");
    let workspace = tempfile::tempdir().expect("isolated workspace");
    let daemon = start_daemon(home.path());

    let session_id = "s-p8-idempotency";
    let create = crate::support::daemon_process::send(
        &rightkit_http::Request::post(format!("{}/sessions", daemon.url()))
            .bearer(&daemon.startup.token)
            .json(&serde_json::json!({
                "id": session_id,
                "workspaceRoot": workspace.path().to_string_lossy(),
                "title": "S:P8 idempotency conformance",
            }))
            .expect("JSON request body"),
    )
    .expect("POST /sessions");
    assert!(
        crate::support::daemon_process::is_success(create.status),
        "create session: {}",
        create.status
    );

    let entries = load_golden("session-idempotency.jsonl");
    assert_eq!(entries.len(), 4, "session-idempotency.jsonl row count");

    // case "fresh": the golden's schema for a first-time accepted submission.
    let fresh = &entries[0];
    assert_eq!(fresh["case"], "fresh");
    let key = fresh["request"]["idempotencyKey"].as_str().unwrap();
    let response = crate::support::daemon_process::send(
        &rightkit_http::Request::post(format!("{}/sessions/{session_id}/messages", daemon.url()))
            .bearer(&daemon.startup.token)
            .json(&serde_json::json!({ "text": "hello", "nonce": key }))
            .expect("JSON request body"),
    )
    .expect("POST message (fresh)");
    assert!(
        crate::support::daemon_process::is_success(response.status),
        "fresh submission: {}",
        response.status
    );
    let fresh_body: Value = response.json().expect("fresh response JSON");
    assert_eq!(
        fresh_body["schemaVersion"],
        fresh["response"]["schemaVersion"]
    );
    assert_eq!(fresh_body["sessionId"], session_id);
    assert_eq!(fresh_body["requestId"], key);
    assert_eq!(fresh_body["accepted"], true);
    assert_eq!(fresh_body["effectState"], "committed");

    // case "identical-retry": golden expects a replay of the original
    // response for the same key + same request content.
    let retry = &entries[1];
    assert_eq!(retry["case"], "identical-retry");
    let retry_response = crate::support::daemon_process::send(
        &rightkit_http::Request::post(format!("{}/sessions/{session_id}/messages", daemon.url()))
            .bearer(&daemon.startup.token)
            .json(&serde_json::json!({ "text": "hello", "nonce": key }))
            .expect("JSON request body"),
    )
    .expect("POST message (identical retry)");
    assert!(
        crate::support::daemon_process::is_success(retry_response.status),
        "identical retry must replay, not fail: {}",
        retry_response.status
    );
    let retry_body: Value = retry_response.json().expect("retry response JSON");
    assert_eq!(
        retry_body, fresh_body,
        "identical-retry must replay the original response verbatim"
    );

    // case "conflicting-reuse": golden expects idempotency_key_conflict for
    // the same key with a different request digest (different text).
    let conflict = &entries[2];
    assert_eq!(conflict["case"], "conflicting-reuse");
    let conflict_response = crate::support::daemon_process::send(
        &rightkit_http::Request::post(format!("{}/sessions/{session_id}/messages", daemon.url()))
            .bearer(&daemon.startup.token)
            .json(&serde_json::json!({ "text": "a different message", "nonce": key }))
            .expect("JSON request body"),
    )
    .expect("POST message (conflicting reuse)");
    assert_eq!(
        conflict_response.status, 409,
        "conflicting reuse must be rejected"
    );
    let conflict_body: Value = conflict_response.json().expect("conflict response JSON");
    assert_conforms_to_error_envelope_schema(&conflict_body);
    assert_eq!(
        conflict_body["code"], conflict["error"]["code"],
        "conflicting-reuse error code must match the golden"
    );
    assert_eq!(
        conflict_body["effectState"],
        conflict["error"]["effectState"]
    );
    assert_eq!(conflict_body["retryable"], conflict["error"]["retryable"]);

    // Actual unknown-outcome fault coverage lives in evt005_admission_restart.
    // Keep this normal-request driver checking the shared fixture shape too.
    let unknown = &entries[3];
    assert_eq!(unknown["case"], "unknown-outcome");
    assert_conforms_to_error_envelope_schema(&unknown["error"]);
}

fn start_acp_server(daemon: &TestDaemon, home: &Path) -> Child {
    rightkit_child_process::command_std(executable())
        .arg("acp-server")
        .arg("--port")
        .arg(daemon.startup.port.to_string())
        .arg("--token")
        .arg(&daemon.startup.token)
        .envs([crate::common::suite_env(home)])
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn coderight acp-server")
}

fn send_jsonrpc_line(child: &mut Child, request: &Value) -> Value {
    let stdin = child.stdin.as_mut().expect("acp-server stdin");
    writeln!(stdin, "{request}").expect("write jsonrpc request");
    stdin.flush().expect("flush jsonrpc request");
    let stdout = child.stdout.as_mut().expect("acp-server stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        line.clear();
        let read = reader.read_line(&mut line).expect("read jsonrpc response");
        assert!(read > 0, "acp-server closed stdout before responding");
        if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "acp-server response timeout: {line}"
        );
    }
}

#[test]
fn acp_negotiation_golden_drives_real_stdio_initialize() {
    let home = tempfile::tempdir().expect("isolated daemon home");
    let daemon = start_daemon(home.path());
    let acp_home = tempfile::tempdir().expect("isolated acp-server home");
    let mut acp = start_acp_server(&daemon, acp_home.path());

    let entries = load_golden("acp-negotiation.jsonl");
    assert_eq!(entries.len(), 3, "acp-negotiation.jsonl row count");

    // case "compatible-v1"
    let compatible = &entries[0];
    assert_eq!(compatible["case"], "compatible-v1");
    let response = send_jsonrpc_line(&mut acp, &compatible["request"]);
    assert_eq!(
        response["result"]["protocolVersion"],
        compatible["expect"]["protocolVersion"]
    );

    // case "incompatible-version"
    let incompatible = &entries[1];
    assert_eq!(incompatible["case"], "incompatible-version");
    let response = send_jsonrpc_line(&mut acp, &incompatible["request"]);
    assert_eq!(response["error"]["code"], incompatible["error"]["code"]);
    assert_eq!(
        response["error"]["data"]["supportedProtocolVersions"],
        incompatible["error"]["supportedProtocolVersions"]
    );

    // acp_prompt_timeout_conformance drives this against a real stalled provider.
    let timeout_case = &entries[2];
    assert_eq!(timeout_case["case"], "prompt-timeout");
    assert!(timeout_case["error"]["data"]["retryable"].is_boolean());

    let _ = acp.kill();
    let _ = acp.wait();
}

/// "Session lifecycle matrix" (S:P8 closure requirement, reconciled-spec.md
/// P8 row): the create/resume/fork/kill matrix documented for CLI session
/// verbs (H7, `docs/research/canon-provenance/reconciled-spec.md:206`).
/// Driven black-box against the real daemon HTTP surface.
#[test]
fn session_lifecycle_matrix_create_resume_fork_kill() {
    let home = tempfile::tempdir().expect("isolated daemon home");
    let workspace = tempfile::tempdir().expect("isolated workspace");
    let daemon = start_daemon(home.path());
    let workspace_root = workspace.path().to_string_lossy().into_owned();

    let root_id = "s-p8-lifecycle-root";
    let create = crate::support::daemon_process::send(
        &rightkit_http::Request::post(format!("{}/sessions", daemon.url()))
            .bearer(&daemon.startup.token)
            .json(&serde_json::json!({
                "id": root_id,
                "workspaceRoot": workspace_root,
                "title": "S:P8 lifecycle root",
            }))
            .expect("JSON request body"),
    )
    .expect("create root session");
    assert!(
        crate::support::daemon_process::is_success(create.status),
        "create: {}",
        create.status
    );

    let kill_root = crate::support::daemon_process::send(
        &rightkit_http::Request::post(format!("{}/sessions/{root_id}/kill", daemon.url()))
            .bearer(&daemon.startup.token),
    )
    .expect("kill root session");
    assert!(
        crate::support::daemon_process::is_success(kill_root.status),
        "kill: {}",
        kill_root.status
    );

    let resume = crate::support::daemon_process::send(
        &rightkit_http::Request::post(format!("{}/sessions/{root_id}/resume", daemon.url()))
            .bearer(&daemon.startup.token),
    )
    .expect("resume root session");
    assert!(
        crate::support::daemon_process::is_success(resume.status),
        "resume: {}",
        resume.status
    );

    let fork_id = "s-p8-lifecycle-fork";
    let fork = crate::support::daemon_process::send(
        &rightkit_http::Request::post(format!("{}/sessions/{root_id}/fork", daemon.url()))
            .bearer(&daemon.startup.token)
            .json(&serde_json::json!({ "id": fork_id, "title": "S:P8 lifecycle fork" }))
            .expect("JSON request body"),
    )
    .expect("fork root session");
    assert!(
        crate::support::daemon_process::is_success(fork.status),
        "fork: {}",
        fork.status
    );

    let kill_fork = crate::support::daemon_process::send(
        &rightkit_http::Request::post(format!("{}/sessions/{fork_id}/kill", daemon.url()))
            .bearer(&daemon.startup.token),
    )
    .expect("kill fork session");
    assert!(
        crate::support::daemon_process::is_success(kill_fork.status),
        "kill fork: {}",
        kill_fork.status
    );

    let kill_root_again = crate::support::daemon_process::send(
        &rightkit_http::Request::post(format!("{}/sessions/{root_id}/kill", daemon.url()))
            .bearer(&daemon.startup.token),
    )
    .expect("kill root session (final)");
    assert!(
        crate::support::daemon_process::is_success(kill_root_again.status),
        "final kill: {}",
        kill_root_again.status
    );

    let list = crate::support::daemon_process::send(
        &rightkit_http::Request::get(format!("{}/sessions?includeSubagents=true", daemon.url()))
            .bearer(&daemon.startup.token),
    )
    .expect("list sessions");
    assert!(
        crate::support::daemon_process::is_success(list.status),
        "list: {}",
        list.status
    );
    let rows: Vec<Value> = list.json().expect("list sessions JSON");
    for row in &rows {
        if row["id"] == root_id || row["id"] == fork_id {
            assert_ne!(
                row["status"], "running",
                "killed session must not still report running: {row}"
            );
        }
    }
}

/// S:P8 cross-language closure: `docs/research/canon-provenance/reconciled-spec.md:307`
/// requires the black-box suite to run "across Rust/TS/Python/shell clients".
/// This is the orchestrating entry for the three non-Rust drivers under
/// `engine/conformance/{ts,py,sh}`, which independently spawn the built
/// `coderight` binary and drive the same three goldens this file drives.
///
/// `#[ignore]`d because it shells out to `node`/`python3`/`bash`, which are
/// not guaranteed present in every environment that runs `cargo test`; it
/// requires every interpreter when explicitly selected. Run
/// explicitly with:
///   cargo test -p coderight --test e2e black_box_conformance:: \
///     cross_language_conformance_drivers_pass -- --ignored --nocapture
#[test]
#[ignore = "shells out to node/python3/bash; run explicitly, see doc comment"]
fn cross_language_conformance_drivers_pass() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let conformance_dir = manifest_dir.join("../../conformance");
    let bin = executable();

    let drivers: [(&str, Command); 3] = [
        ("node", {
            let mut command = rightkit_child_process::command_std("node");
            command.arg(conformance_dir.join("ts/run.mjs"));
            command
        }),
        ("python3", {
            let mut command = rightkit_child_process::command_std(python_interpreter());
            command.arg(conformance_dir.join("py/run.py"));
            command
        }),
        ("bash", {
            let mut command = rightkit_child_process::command_std(bash_interpreter());
            command.arg(
                conformance_dir
                    .join("sh/run.sh")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
            command
        }),
    ];

    let mut ran = 0usize;
    for (name, mut command) in drivers {
        assert!(
            interpreter_available(name),
            "required conformance interpreter unavailable: {name}"
        );
        command.env("RIGHTKIT_BIN", &bin);
        println!(
            "{name} conformance interpreter: {:?}",
            command.get_program()
        );
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("failed to spawn {name} conformance driver: {error}"));
        assert!(
            output.status.success(),
            "{name} conformance driver failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        println!(
            "{name} conformance driver output:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        ran += 1;
    }

    assert_eq!(ran, 3, "all cross-language drivers must execute");
}

fn python_interpreter() -> &'static str {
    if rightkit_child_process::command_std("python3")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        "python3"
    } else {
        "python"
    }
}

fn interpreter_available(name: &str) -> bool {
    match name {
        "bash" => rightkit_child_process::command_std(bash_interpreter())
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success()),
        "python3" => rightkit_child_process::command_std(python_interpreter())
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false),
        other => rightkit_child_process::command_std(other)
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false),
    }
}

fn bash_interpreter() -> std::ffi::OsString {
    if let Some(path) = std::env::var_os("RIGHTKIT_CONFORMANCE_BASH") {
        return path;
    }
    #[cfg(windows)]
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        let git_bash = PathBuf::from(program_files).join("Git/bin/bash.exe");
        if git_bash.is_file() {
            return git_bash.into_os_string();
        }
    }
    #[cfg(windows)]
    if let Ok(output) = rightkit_child_process::command_std("git")
        .arg("--exec-path")
        .output()
    {
        if output.status.success() {
            let exec_path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
            // Git for Windows places git-core under <install>/mingw64/libexec.
            for ancestor in exec_path.ancestors() {
                let candidate = ancestor.join("bin/bash.exe");
                if candidate.is_file() {
                    return candidate.into_os_string();
                }
            }
        }
    }
    "bash".into()
}
