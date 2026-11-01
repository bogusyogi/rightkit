//! OPS-004 API route/method/schema qualification driver.
//!
//! Canon (`docs/canon/daemon-master-canon-atoms.md`) OPS-004: "Exercise every
//! API route using its declared method/schema/contract; path-only GET
//! reachability does not count."
//!
//! The daemon already publishes a canonical live route inventory at
//! `GET /api/contract` / `GET /api/openapi.json`
//! (`engine/bins/coderight/src/api_contract.rs`). This file does NOT
//! hand-maintain a second inventory. Instead:
//!
//! 1. A committed coverage manifest
//!    (`engine/bins/coderight/tests/ops004_route_coverage.toml`) declares,
//!    for every `(method, path)` pair, its auth requirement, whether the
//!    daemon publishes a request/response schema for it, the expected
//!    success status, and a `verified` flag.
//! 2. The **drift test** (`manifest_matches_live_route_inventory`, always
//!    run) fails the build if:
//!    - a live route has no manifest entry (uncovered route slipped in), or
//!    - a manifest entry's route no longer exists live (stale coverage).
//!    This is the load-bearing property this file exists to guarantee.
//! 3. The **executor** (`every_route_responds_to_its_declared_method`,
//!    the full exhaustive acceptance run) actually drives every manifest
//!    route with its declared HTTP method, OpenAPI-derived request schema and
//!    authentication carrier, then validates declared response schemas.
//!    A `405 Method Not Allowed` is a wiring/method-mismatch defect, never
//!    acceptable "the route exists" evidence, and is treated as a hard
//!    failure everywhere in this file.
//!
//! The manifest is generated, not hand-typed: see
//! `bootstrap_generate_route_coverage_manifest` (`#[ignore]`d — run it
//! explicitly with `--include-ignored` after adding routes, review the diff,
//! and commit the regenerated file).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(15);
const MANIFEST_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/ops004_route_coverage.toml"
);

// ---------------------------------------------------------------------
// Minimal, self-contained real-daemon harness (mirrors daemon_lifecycle.rs;
// kept local so this file has no cross-test-binary dependency).
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct StartupContract {
    #[serde(rename = "Port")]
    port: u16,
    #[serde(rename = "Token")]
    token: String,
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
            #[cfg(windows)]
            {
                let system_root = std::env::var_os("SystemRoot")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
                let _ =
                    rightkit_child_process::command_std(system_root.join("System32/taskkill.exe"))
                        .args(["/PID", &self.child.id().to_string(), "/T", "/F"])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
            }
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn executable() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_coderight")
        .map(PathBuf::from)
        .expect("Cargo must provide CARGO_BIN_EXE_coderight for this integration test")
}

fn isolated_command(home: &Path) -> Command {
    let mut command = rightkit_child_process::command_std(executable());
    command
        .arg("serve")
        .env("RIGHTKIT_CONTEXT", "off")
        .arg("--port")
        .arg("0");
    command
        .envs([crate::common::suite_env(home)])
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("RIGHTKIT_DISABLE_OS_KEYCHAIN", "1")
        .env("RIGHTKIT_OPS004_SECRET", "ops004-fixture-secret")
        // Machine-probe determinism: the daemon's `PATH` is reduced to the
        // fixture driver directory so host-installed tools (ollama, vllm,
        // nvidia-smi, ...) cannot turn `/local-models/*` probes into slow
        // real-machine discovery runs, while the seeded `claude` driver
        // binary remains discoverable through the production lookup path.
        // Absolute: binary_lookup::find_on_path keeps only absolute candidates (governed
        // launch requires an absolute program path), so a relative PATH entry hides the fixture.
        .env("PATH", home.join("driverbin"))
        .current_dir(home)
        .env_remove("RIGHTKIT_DAEMON_URL")
        .env_remove("RIGHTKIT_DAEMON_TOKEN")
        .env_remove("RIGHTKIT_SERVE_FORCE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

fn start_daemon(home: &Path) -> TestDaemon {
    let mut child = isolated_command(home)
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
            crate::support::daemon_process::stop_child(&mut child);
            panic!("real daemon failed to publish startup contract ({error}); status={status:?}");
        }
        Err(error) => {
            crate::support::daemon_process::stop_child(&mut child);
            panic!("timed out waiting for real daemon startup contract: {error}");
        }
    };

    let daemon = TestDaemon { child, startup };
    wait_for_health(&daemon);
    daemon
}

fn client() -> rightkit_http::Client {
    // Route contract, not latency: 371 routes share a loaded host.
    let timeout = Duration::from_secs(20);
    rightkit_http::Client::new(rightkit_http::ClientConfig {
        request_timeout: timeout,
        response_timeout: Some(timeout),
        retry: rightkit_http::RetryPolicy::none(),
        address_policy: crate::support::daemon_process::loopback_policy(),
        ..rightkit_http::ClientConfig::default()
    })
}

fn send(
    request: &rightkit_http::Request,
) -> Result<rightkit_http::Response, rightkit_http::HttpError> {
    client().send_any(request)
}

fn wait_for_health(daemon: &TestDaemon) {
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    loop {
        if let Ok(response) = send(
            &rightkit_http::Request::get(format!("{}/health", daemon.url()))
                .bearer(&daemon.startup.token),
        ) {
            if crate::support::daemon_process::is_success(response.status) {
                if let Ok(value) = response.json::<Value>() {
                    if value["ok"] == true {
                        return;
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "real daemon did not become healthy at {}",
            daemon.url()
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn fetch_openapi_document(daemon: &TestDaemon) -> Value {
    send(
        &rightkit_http::Request::get(format!("{}/api/openapi.json", daemon.url()))
            .bearer(&daemon.startup.token),
    )
    .expect("fetch /api/openapi.json from the real daemon")
    .json()
    .expect("/api/openapi.json must return valid JSON")
}

const OPS004_SECRET: &str = "ops004-fixture-secret";

fn provision_trigger(daemon: &TestDaemon, home: &Path, id: &str, provider: &str) {
    let body = serde_json::json!({
        "id": id,
        "provider": provider,
        "workspaceRoot": home.to_string_lossy(),
        "prompt": "OPS-004 route contract fixture",
        "mode": "agent",
        "title": format!("OPS-004 {provider} fixture"),
        "secretEnv": "RIGHTKIT_OPS004_SECRET",
        "enabled": true,
    });
    let response = send(
        &rightkit_http::Request::post(format!("{}/triggers", daemon.url()))
            .bearer(&daemon.startup.token)
            .json(&body)
            .expect("JSON request body"),
    )
    .expect("provision OPS-004 trigger fixture");
    assert_eq!(
        response.status, 201,
        "OPS-004 trigger fixture must be created before signed delivery routes are exercised"
    );
}

/// Seed the driver-capability chain against the daemon's real validation:
/// a fixture `claude` executable on the daemon PATH, a content-addressed
/// `cli_harness_attestations` row the readiness gate accepts, a real session,
/// a real `issue_capability_lease`, and a real governed `start_driver` spawn.
/// The lease token + driver-instance secret then satisfy the production auth
/// carriers for every `/driver/*` and `/driver-capabilities/*` route.
fn driver_fixture_executable() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_ops004_driver_fixture")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        // Cargo embeds this path at compilation. Direct combined runs only
        // provide CARGO_BIN_EXE_coderight in their runtime environment.
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_ops004_driver_fixture")))
}

fn sha256_hex(bytes: &[u8]) -> String {
    rightkit_fs::digest::sha256_bytes(bytes)
}

// Keep the seeded driver attestation tied to the same public tool schemas
// that the daemon's capability gateway qualifies (api_drivers::cli_profiles).
fn gateway_tool_schema_hash() -> String {
    let registry = rightkit_agent_tools::coding_tools::ToolRegistry::build_default();
    let special = [
        "memory_recall",
        "delegate_task",
        "delegate_parallel",
        "verify",
    ];
    let blocked = [
        "spawn_subagent",
        "send_message",
        "team_create",
        "team_delete",
        "followup_subagent",
        "wait_for_subagents",
        "interrupt_subagent",
    ];
    let mut names: Vec<String> = registry
        .all()
        .iter()
        .map(|tool| tool.name().to_string())
        .chain(special.iter().map(|name| (*name).to_string()))
        .filter(|name| !blocked.contains(&name.as_str()))
        .collect();
    names.sort();
    names.dedup();
    let schemas: Vec<Value> = names
        .iter()
        .filter_map(|name| {
            let read_only = name == "memory_recall";
            let input_schema = match name.as_str() {
                "memory_recall" => json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":10}},"required":["query"]}),
                "delegate_task" => json!({"type":"object","properties":{"role":{"type":"string"},"task":{"type":"string"},"providerHint":{"type":"string","description":"Optional CLI provider hint (for example codex or claude)."},"modelHint":{"type":"string","description":"Optional model hint for providerHint."},"maxTurns":{"type":"integer","minimum":1},"runtime":{"type":"object","description":"Legacy nested runtime selector; providerHint/modelHint are preferred."},"continuity":{"type":"object"}},"required":["role","task"]}),
                "delegate_parallel" => json!({"type":"object","properties":{"tasks":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","properties":{"role":{"type":"string"},"task":{"type":"string"},"providerHint":{"type":"string","description":"Optional CLI provider hint (for example codex or claude)."},"modelHint":{"type":"string","description":"Optional model hint for providerHint."},"maxTurns":{"type":"integer","minimum":1},"runtime":{"type":"object","description":"Legacy nested runtime selector; providerHint/modelHint are preferred."},"continuity":{"type":"object"}},"required":["role","task"]}}},"required":["tasks"]}),
                "verify" => json!({"type":"object","properties":{"command":{"type":"string"},"timeout_ms":{"type":"integer","minimum":1,"maximum":300000}},"required":["command"],"additionalProperties":false}),
                _ => registry.get(name)?.input_schema(),
            };
            let description = match name.as_str() {
                "memory_recall" => "Recall session-scoped MemRight context with CodeRight provenance.",
                "delegate_task" => "Delegate one bounded task to a CodeRight-managed model/agent.",
                "delegate_parallel" => "Delegate 1-4 independent tasks through CodeRight in parallel.",
                "verify" => "Run a verification command through CodeRight's governed shell and emit typed verification evidence.",
                _ => registry.get(name)?.description(),
            };
            Some(json!({
                "name": name,
                "description": description,
                "inputSchema": input_schema,
                "annotations": {
                    "readOnlyHint": read_only,
                    "destructiveHint": false,
                    "idempotentHint": read_only,
                    "openWorldHint": false,
                }
            }))
        })
        .collect();
    sha256_hex(&serde_json::to_vec(&schemas).expect("serialize gateway schemas"))
}

fn rfc3339_now() -> String {
    // Unix seconds -> "YYYY-MM-DDTHH:MM:SSZ" (the shape the daemon's
    // attestation reader parses).
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_secs();
    rightkit_time::DateTime::from_unix(i64::try_from(secs).expect("seconds fit i64"), 0)
        .expect("offset 0 is always valid")
        .format_rfc3339()
}

/// Runs before the daemon starts: installs the fixture driver under
/// `<home>/driverbin` (the only daemon PATH entry) and writes a
/// self-consistent, content-addressed attestation row into the same SQLite
/// database the daemon will open, so the real readiness gate (`is_ready`)
/// passes for `claude`.
pub(crate) fn seed_driver_fixture(home: &Path) {
    let driverbin = home.join("driverbin");
    std::fs::create_dir_all(&driverbin).expect("create ops004 driverbin");
    let fixture = driver_fixture_executable();
    // `find_binary("claude")` requires the literal name to be a file; on
    // Windows CreateProcess resolves the spawned path by appending `.exe`,
    // so both names must exist and carry identical bytes.
    let target = driverbin.join("claude");
    std::fs::copy(&fixture, &target).expect("install fixture driver as `claude`");
    if cfg!(windows) {
        std::fs::copy(&fixture, driverbin.join("claude.exe"))
            .expect("install fixture driver as `claude.exe`");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&target, perms).unwrap();
    }

    let executable_sha256 = sha256_hex(&std::fs::read(&target).expect("read fixture driver"));
    let version = rightkit_child_process::command_std(&target)
        .arg("--version")
        .current_dir(home)
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .ok()
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "ops004-driver-fixture".to_string());

    // The readiness gate re-derives the row id from the stored inputs; every
    // field below is what the daemon recomputes (or passes through
    // self-consistently) on the lease/start path.
    let inputs = rightkit_agents::driver_host::cli_runtime::AttestationInputs {
        provider_id: rightkit_agents::driver_host::cli_runtime::CliProviderId::new("claude"),
        executable_sha256,
        binary_version: version,
        protocol_version: "stream-json".to_string(),
        // native_profile_hash(default claude profile): sha256("claude/default" + NUL).
        profile_hash: sha256_hex(b"claude/default\0")[..16].to_string(),
        policy_hash: sha256_hex(rightkit_agents::driver_host::RUN_SCOPED_POLICY.as_bytes()),
        tool_schema_hash: gateway_tool_schema_hash(),
        operating_system: std::env::consts::OS.to_string(),
        architecture: std::env::consts::ARCH.to_string(),
        isolation_mode:
            rightkit_agents::driver_host::cli_runtime::HarnessIsolationMode::ExclusiveCodeRightGateway,
    };
    let row = rightkit_agent_engine_storage::CliHarnessAttestationRow {
        id: inputs.address(),
        provider_id: inputs.provider_id.as_str().to_string(),
        executable_sha256: inputs.executable_sha256.clone(),
        binary_version: inputs.binary_version.clone(),
        protocol_version: inputs.protocol_version.clone(),
        profile_hash: inputs.profile_hash.clone(),
        policy_hash: inputs.policy_hash.clone(),
        tool_schema_hash: inputs.tool_schema_hash.clone(),
        operating_system: inputs.operating_system.clone(),
        architecture: inputs.architecture.clone(),
        isolation_mode: inputs.isolation_mode.label().to_string(),
        result: "passed".to_string(),
        evidence_json: "{\"probes\":[]}".to_string(),
        verified_at: rfc3339_now(),
    };
    let db_dir = crate::common::app_root(&home);
    std::fs::create_dir_all(&db_dir).expect("create daemon db dir");
    let storage = rightkit_agent_engine_storage::Storage::open(db_dir.join("coderight.db"))
        .expect("open daemon database for attestation seed");
    storage
        .upsert_cli_harness_attestation(&row)
        .expect("seed cli harness attestation row");
    // PL-005 native CLI launch requires persisted sign-in evidence even when
    // its executable is a qualification fixture (cli_profiles.rs).
    let profile_home = home.join("fixture-claude-profile");
    std::fs::create_dir_all(&profile_home).expect("create isolated vendor profile");
    let now = rfc3339_now();
    storage
        .upsert_cli_account_profile(&rightkit_agent_engine_storage::CliAccountProfileRow {
            id: "claude/default".into(),
            provider_id: "claude".into(),
            label: "default".into(),
            managed_home: profile_home.to_string_lossy().into_owned(),
            auth_mode: "native_cli_account".into(),
            signed_in: true,
            // Matches the seeded native_profile_hash(claude/default + NUL).
            account_fingerprint: None,
            last_verified_at: Some(now.clone()),
            created_at: now.clone(),
            updated_at: now,
        })
        .expect("seed fixture native sign-in profile");
}

const DRIVER_INSTANCE_SECRET: &str =
    "ops004-driver-instance-secret-0123456789abcdef0123456789abcdef99";

fn post_json(daemon: &TestDaemon, path: &str, body: Value) -> (u16, Value) {
    post_json_lease(daemon, path, body, None)
}

fn post_json_lease(
    daemon: &TestDaemon,
    path: &str,
    body: Value,
    capability_token: Option<&str>,
) -> (u16, Value) {
    let mut request = rightkit_http::Request::post(format!("{}{}", daemon.url(), path))
        .bearer(&daemon.startup.token)
        .json(&body)
        .expect("JSON request body");
    if let Some(token) = capability_token {
        request = request.header("x-coderight-capability-token", token);
    }
    let response = send(&request).unwrap_or_else(|error| panic!("POST {path} failed: {error}"));
    let status = response.status;
    let value = response.json::<Value>().unwrap_or(Value::Null);
    (status, value)
}

/// Runs against the live daemon: create a claude-kind session, mint a real
/// capability lease through `issue_capability_lease`, spawn the fixture
/// driver through `start_driver`, then point the lease's process-instance
/// hash at a test-known secret so the capability gateway routes accept the
/// declared headers. Exports the env carriers the executor loop consumes.
fn provision_driver_chain(daemon: &TestDaemon, home: &Path) {
    let (status, session) = post_json(
        daemon,
        "/sessions",
        serde_json::json!({
            "workspaceRoot": home.to_string_lossy(),
            "title": "OPS-004 driver session",
            "driverKind": "claude",
        }),
    );
    assert_eq!(
        status, 200,
        "OPS-004 driver session must be created: {session}"
    );
    let session_id = session["id"]
        .as_str()
        .expect("session response carries id")
        .to_string();

    let (status, lease) = post_json(
        daemon,
        &format!("/sessions/{session_id}/driver-capabilities/leases"),
        serde_json::json!({
            "driverKind": "claude",
            "binaryVersion": "ops004-driver-fixture 1.0.0",
            "schemaHash": "ops004",
            "tools": [],
        }),
    );
    assert_eq!(
        status, 200,
        "real capability lease must issue for the seeded attestation: {lease}"
    );
    let lease_id = lease["leaseId"].as_str().expect("lease id").to_string();
    let binding_id = lease["bindingId"].as_str().expect("binding id").to_string();
    let capability_token = lease["capabilityToken"]
        .as_str()
        .expect("lease capabilityToken")
        .to_string();

    let (status, started) = post_json_lease(
        daemon,
        &format!("/sessions/{session_id}/driver/start"),
        serde_json::json!({ "bindingId": binding_id }),
        Some(&capability_token),
    );
    assert_eq!(
        status, 200,
        "real governed driver spawn must start the fixture driver: {started}"
    );

    // `start_driver` injects its own instance secret into the child env and
    // never returns it; repoint the lease's process-instance hash at the
    // test-known carrier so the gateway routes authorize the declared header.
    let storage = rightkit_agent_engine_storage::Storage::open(
        crate::common::app_root(&home).join("coderight.db"),
    )
    .expect("reopen daemon database for instance-hash fixture");
    let updated = storage
        .set_session_capability_lease_process_instance(
            &lease_id,
            Some(&sha256_hex(DRIVER_INSTANCE_SECRET.as_bytes())),
        )
        .expect("point lease at the test-known instance secret");
    assert!(
        updated,
        "lease {lease_id} must accept the instance-hash update"
    );
    let stored = storage
        .get_session_capability_lease(&lease_id)
        .expect("read back lease row");
    eprintln!(
        "ops004 driver chain: lease={lease_id} binding={binding_id} \
         stored_instance_hash={:?} expected={}",
        stored.and_then(|row| row.process_instance_hash),
        sha256_hex(DRIVER_INSTANCE_SECRET.as_bytes()),
    );
    let status_probe = send(
        &rightkit_http::Request::get(format!(
            "{}/sessions/{session_id}/driver/status",
            daemon.url()
        ))
        .bearer(&daemon.startup.token),
    )
    .expect("driver status probe");
    eprintln!(
        "ops004 driver/status {}: {}",
        status_probe.status,
        status_probe.text()
    );

    // Prove the whole chain before the route sweep begins: bearer + lease
    // token + instance secret must satisfy the production gateway auth.
    let tools_probe = send(
        &rightkit_http::Request::get(format!(
            "{}/sessions/{session_id}/driver-capabilities/tools",
            daemon.url()
        ))
        .bearer(&daemon.startup.token)
        .header("x-coderight-capability-token", &capability_token)
        .header("x-coderight-driver-instance", DRIVER_INSTANCE_SECRET),
    )
    .expect("capability tools probe request");
    let tools_status = tools_probe.status;
    assert_eq!(
        tools_status,
        200,
        "capability gateway must accept the provisioned lease chain: {}",
        tools_probe.text()
    );

    std::env::set_var("RIGHTKIT_OPS004_DRIVER_SESSION_ID", &session_id);
    std::env::set_var("RIGHTKIT_OPS004_CAPABILITY_TOKEN", &capability_token);
    std::env::set_var("RIGHTKIT_OPS004_DRIVER_INSTANCE", DRIVER_INSTANCE_SECRET);
    std::env::set_var("RIGHTKIT_OPS004_DRIVER_BINDING_ID", &binding_id);
    // The fixture emits this session id in its init frame; the daemon's event
    // consumer records it onto the binding before prompt/interrupt need it.
    std::env::set_var(
        "RIGHTKIT_OPS004_DRIVER_VENDOR_SESSION_ID",
        "ops004-vendor-session",
    );
}

/// Authored request bodies for routes whose generated sample cannot satisfy
/// the runtime contract (enum/oneOf/uniqueItems/typed nested payloads), or
/// whose fixture must reference real provisioned state. Returns `None` for
/// routes the schema-driven generator already exercises correctly.
fn authored_request_body(path: &str, home: &Path, daemon_url: &str) -> Option<Value> {
    let binding = std::env::var("RIGHTKIT_OPS004_DRIVER_BINDING_ID").ok();
    let vendor = std::env::var("RIGHTKIT_OPS004_DRIVER_VENDOR_SESSION_ID").ok();
    let recipe = serde_json::json!({
        "id": "ops004-recipe",
        "version": "1.0.0",
        "title": "OPS-004 fixture recipe",
        "description": "Route-contract fixture",
        "prompt": "Implement {{feature}}.",
    });
    match path {
        // WriteExecutionProcessStdinRequest requires lease/sequence plus one
        // input carrier (OPS-004). Its anyOf branches only list required keys;
        // the generic sampler cannot synthesize them without parent properties.
        "/execution/processes/{id}/stdin" => Some(json!({
            "leaseEpoch": 1,
            "inputSequence": 0,
            "input": "ops004-fixture",
        })),
        "/sessions/{id}/driver-capabilities/leases" => Some(serde_json::json!({
            "driverKind": "claude",
            "binaryVersion": "ops004-driver-fixture 1.0.0",
            "schemaHash": "ops004",
        })),
        "/sessions/{id}/driver-capabilities/call" => Some(serde_json::json!({
            "callId": "ops004-call-1",
            "name": "memory_recall",
            "arguments": {"query": "ops004", "limit": 1},
        })),
        "/sessions/{id}/driver/prompt" => binding.map(|binding_id| {
            serde_json::json!({
                "bindingId": binding_id,
                "vendorSessionId": vendor.clone().unwrap_or_default(),
                "text": "OPS-004 route contract fixture",
            })
        }),
        "/sessions/{id}/driver/interrupt" => binding.map(|binding_id| {
            serde_json::json!({
                "bindingId": binding_id,
                "vendorSessionId": vendor.clone().unwrap_or_default(),
            })
        }),
        "/sessions/{id}/driver/start"
        | "/sessions/{id}/driver/resume"
        | "/sessions/{id}/driver/stop" => {
            binding.map(|binding_id| serde_json::json!({ "bindingId": binding_id }))
        }
        "/mcp/servers" => Some(serde_json::json!({
            "scope": "user",
            "name": "ops004-fixture",
            // Probe target: the daemon's own /health answers instantly, so the
            // POST /mcp/servers/{name}/probe exercise is bounded.
            "config": {"type": "http", "url": format!("{daemon_url}/health")},
        })),
        "/runtime/executed-bakeoff" => Some(serde_json::json!({
            "suiteNamespace": "ops004",
            "workspaceRoot": home.to_string_lossy(),
            "candidateRoles": ["coder", "engineer"],
            "baselineRole": "coder",
            "tasks": [{
                "caseNonce": "ops004-t1",
                "prompt": "OPS-004 route contract fixture",
                "expectedOutput": "ok",
            }],
            // The real run is bounded: the handler's internal timeout aborts
            // candidate execution and answers GATEWAY_TIMEOUT.
            "timeoutSeconds": 1,
        })),
        "/recipes/install" => Some(serde_json::json!({
            "recipe": recipe,
            "trusted": true,
        })),
        "/recipes/install-file" => {
            let recipe_path = home.join("ops004-recipe.yaml");
            std::fs::write(
                &recipe_path,
                "id: ops004-file\nversion: 1.0.0\ntitle: OPS-004 file recipe\n\
                 description: Route-contract fixture\nprompt: \"Implement {{feature}}.\"\n",
            )
            .expect("write ops004 recipe file");
            Some(serde_json::json!({
                "path": recipe_path.to_string_lossy(),
                "trusted": true,
            }))
        }
        "/recipes/validate" | "/recipes/render" => Some(serde_json::json!({ "recipe": recipe })),
        _ => None,
    }
}

fn hmac_sha256_hex(secret: &[u8], body: &[u8]) -> String {
    format!(
        "sha256={}",
        rightkit_crypto::hex_encode(&rightkit_crypto::hmac_sha256(secret, body))
    )
}

// ---------------------------------------------------------------------
// Coverage manifest model.
// ---------------------------------------------------------------------

/// One qualification entry for a single `(method, path)` route, derived
/// from the live OpenAPI document served at `/api/openapi.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RouteCoverage {
    method: String,
    path: String,
    /// "daemonBearer" (default document-level auth), "none" (route opts out,
    /// e.g. trigger-delivery HMAC routes), or another declared scheme name.
    auth: String,
    /// True when the OpenAPI document publishes a non-empty request body
    /// schema for this route.
    has_request_schema: bool,
    /// True when requestBody.required is true in the document.
    request_body_required: bool,
    /// True when the OpenAPI document publishes a non-trivial (non-`{}`)
    /// success response schema for this route.
    has_response_schema: bool,
    /// Declared success status code, e.g. "200", "201", "202".
    success_status: String,
    /// True when a documented executor has driven this route with its
    /// declared method + schema and confirmed the declared contract.
    /// Generated entries start `false` ("unverified") unless the route is
    /// trivially self-describing; promoting an entry to `verified = true`
    /// must be backed by an actual passing run of the executor against
    /// that route, never hand-flipped.
    verified: bool,
    /// Free-text note; generated entries record why they are unverified or
    /// what the executor should special-case.
    notes: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CoverageManifest {
    /// Keyed by `"{METHOD} {path}"` for stable, sorted TOML output and O(1)
    /// lookup against the live inventory.
    routes: BTreeMap<String, RouteCoverage>,
}

fn route_key(method: &str, path: &str) -> String {
    format!("{} {}", method.to_ascii_uppercase(), path)
}

fn load_manifest() -> CoverageManifest {
    let text = std::fs::read_to_string(MANIFEST_PATH).unwrap_or_else(|error| {
        panic!(
            "OPS-004 coverage manifest missing at {MANIFEST_PATH}: {error}. \
             Run `cargo test -p coderight --test e2e ops004_route_contract:: \
             bootstrap_generate_route_coverage_manifest -- --ignored --nocapture` \
             and commit the generated file."
        )
    });
    toml::from_str(&text).expect("OPS-004 coverage manifest must parse as TOML")
}

/// Extract the live `(method, path) -> RouteCoverage` inventory from the
/// OpenAPI document, matching exactly what `/api/openapi.json` currently
/// serves. This is the single source of truth OPS-004 qualifies against.
fn live_routes_from_openapi(document: &Value) -> BTreeMap<String, RouteCoverage> {
    let mut routes = BTreeMap::new();
    let paths = document["paths"]
        .as_object()
        .expect("openapi document must publish a paths object");
    for (path, path_item) in paths {
        let methods = path_item
            .as_object()
            .unwrap_or_else(|| panic!("path item for {path} must be an object"));
        for (method, operation) in methods {
            let method_upper = method.to_ascii_uppercase();

            let security = operation.get("security").and_then(Value::as_array);
            let auth = match security {
                Some(schemes) if schemes.is_empty() => "none".to_string(),
                _ => "daemonBearer".to_string(),
            };

            let request_schema = operation
                .get("requestBody")
                .and_then(|body| body["content"]["application/json"]["schema"].as_object());
            let has_request_schema = request_schema
                .map(|schema| !schema.is_empty())
                .unwrap_or(false);
            let request_body_required = operation["requestBody"]["required"] == true;

            let (success_status, response_schema) = operation["responses"]
                .as_object()
                .and_then(|responses| responses.iter().find(|(status, _)| status.starts_with('2')))
                .map(|(status, body)| {
                    (
                        status.clone(),
                        body["content"]["application/json"]["schema"]
                            .as_object()
                            .cloned(),
                    )
                })
                .unwrap_or_else(|| ("200".to_string(), None));
            let has_response_schema = response_schema
                .map(|schema| !schema.is_empty())
                .unwrap_or(false);

            let key = route_key(&method_upper, path);
            routes.insert(
                key,
                RouteCoverage {
                    method: method_upper,
                    path: path.clone(),
                    auth,
                    has_request_schema,
                    request_body_required,
                    has_response_schema,
                    success_status,
                    verified: false,
                    notes: String::new(),
                },
            );
        }
    }
    routes
}

fn operation_for<'a>(document: &'a Value, entry: &RouteCoverage) -> &'a Value {
    let path = document["paths"][&entry.path]
        .as_object()
        .unwrap_or_else(|| panic!("OpenAPI path missing for {} {}", entry.method, entry.path));
    path.get(&entry.method.to_ascii_lowercase())
        .unwrap_or_else(|| {
            panic!(
                "OpenAPI operation missing for {} {}",
                entry.method, entry.path
            )
        })
}

fn request_schema_for(operation: &Value) -> Option<&Value> {
    operation
        .get("requestBody")?
        .get("content")?
        .get("application/json")?
        .get("schema")
}

fn response_schema_for(operation: &Value, status: u16) -> Option<&Value> {
    let status = status.to_string();
    operation
        .get("responses")?
        .get(&status)?
        .get("content")?
        .get("application/json")?
        .get("schema")
}

fn schema_ref<'a>(schema: &'a Value, document: &'a Value) -> Option<&'a Value> {
    let reference = schema.get("$ref")?.as_str()?;
    let name = reference.strip_prefix("#/components/schemas/")?;
    document["components"]["schemas"].get(name)
}

/// Build a deterministic, minimally valid instance from an OpenAPI 3.1 JSON
/// schema. Required fields are populated; optional fields are included only
/// when schema supplies a const/default/example. This keeps route exercises
/// schema-valid without inventing operation-specific side effects.
fn sample_from_schema(
    schema: &Value,
    document: &Value,
    field: Option<&str>,
    home: &Path,
    depth: usize,
) -> Value {
    if depth > 24 {
        return Value::Null;
    }
    if let Some(resolved) = schema_ref(schema, document) {
        return sample_from_schema(resolved, document, field, home, depth + 1);
    }
    if let Some(value) = schema.get("const") {
        return value.clone();
    }
    if let Some(value) = schema.get("default") {
        return value.clone();
    }
    if let Some(value) = schema
        .get("examples")
        .and_then(|v| v.as_array())
        .and_then(|v| v.first())
    {
        return value.clone();
    }
    if let Some(value) = schema
        .get("enum")
        .and_then(|v| v.as_array())
        .and_then(|v| v.first())
    {
        return value.clone();
    }
    if let Some(branches) = schema.get("oneOf").or_else(|| schema.get("anyOf")) {
        if let Some(branch) = branches.as_array().and_then(|v| v.first()) {
            return sample_from_schema(branch, document, field, home, depth + 1);
        }
    }
    if let Some(branches) = schema.get("allOf").and_then(Value::as_array) {
        let mut merged = serde_json::Map::new();
        for branch in branches {
            if let Value::Object(values) =
                sample_from_schema(branch, document, field, home, depth + 1)
            {
                merged.extend(values);
            }
        }
        return Value::Object(merged);
    }

    let schema_type = schema.get("type").and_then(|value| match value {
        Value::String(kind) => Some(kind.as_str()),
        Value::Array(types) => types
            .iter()
            .find_map(Value::as_str)
            .filter(|kind| *kind != "null"),
        _ => None,
    });
    match schema_type.or_else(|| schema.get("properties").map(|_| "object")) {
        Some("object") => {
            let mut result = serde_json::Map::new();
            let required = schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<BTreeSet<_>>();
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                for (name, property) in properties {
                    let include = required.contains(name.as_str())
                        || property.get("const").is_some()
                        || property.get("default").is_some()
                        || property.get("examples").is_some();
                    if include {
                        result.insert(
                            name.clone(),
                            sample_from_schema(property, document, Some(name), home, depth + 1),
                        );
                    }
                }
            }
            Value::Object(result)
        }
        Some("array") => {
            let item = schema
                .get("items")
                .map(|item| sample_from_schema(item, document, field, home, depth + 1));
            let min_items = schema.get("minItems").and_then(Value::as_u64).unwrap_or(0);
            if min_items > 0 {
                Value::Array(
                    (0..min_items)
                        .map(|_| item.clone().unwrap_or(Value::Null))
                        .collect(),
                )
            } else {
                Value::Array(Vec::new())
            }
        }
        Some("boolean") => Value::Bool(false),
        Some("integer") | Some("number") => {
            let minimum = schema.get("minimum").and_then(Value::as_i64).unwrap_or(1);
            if schema_type == Some("integer") {
                Value::Number(minimum.into())
            } else {
                serde_json::json!(minimum as f64)
            }
        }
        Some("string") => {
            let name = field.unwrap_or_default().to_ascii_lowercase();
            let mut value = if schema
                .get("pattern")
                .and_then(Value::as_str)
                .is_some_and(|pattern| pattern.contains("[0-9a-f]{64}"))
            {
                "a".repeat(64)
            } else if schema.get("format").and_then(Value::as_str) == Some("uuid") {
                "00000000-0000-4000-8000-000000000000".to_string()
            } else if name.contains("workspace") || name.ends_with("root") {
                home.to_string_lossy().to_string()
            } else if name.contains("secret") {
                "RIGHTKIT_OPS004_SECRET".to_string()
            } else if name.contains("prompt") {
                "OPS-004 route contract fixture".to_string()
            } else if name == "id" || name.ends_with("id") {
                "ops004-fixture".to_string()
            } else if name.contains("url") || name.contains("endpoint") {
                "http://127.0.0.1".to_string()
            } else {
                "ops004-fixture".to_string()
            };
            if let Some(minimum) = schema.get("minLength").and_then(Value::as_u64) {
                let minimum = minimum as usize;
                if value.len() < minimum {
                    value.extend(std::iter::repeat_n('x', minimum - value.len()));
                }
            }
            Value::String(value)
        }
        _ => Value::Object(serde_json::Map::new()),
    }
}

fn validate_instance(document: &Value, schema: &Value, instance: &Value) -> Result<(), String> {
    let mut root = schema.clone();
    if let Value::Object(root_object) = &mut root {
        root_object.insert("components".to_string(), document["components"].clone());
    }
    let validator = jsonschema::validator_for(&root)
        .map_err(|error| format!("declared schema did not compile: {error}"))?;
    let errors = validator
        .iter_errors(instance)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

// ---------------------------------------------------------------------
// Bootstrap: regenerate the manifest from the live inventory.
// ---------------------------------------------------------------------

/// Regenerates `ops004_route_coverage.toml` from the daemon's live OpenAPI
/// document. Not run by default — the manifest is committed source, and
/// this exists only so a future contributor adding routes can regenerate it
/// (then MUST hand-review the diff: any route whose contract can actually be
/// asserted should be promoted out of `verified = false` by the executor,
/// never by editing this generator).
#[test]
#[ignore = "OPS-004 manifest generator; run explicitly, then review+commit the diff"]
fn bootstrap_generate_route_coverage_manifest() {
    let home = TempDir::new().unwrap();
    let daemon = start_daemon(home.path());
    let document = fetch_openapi_document(&daemon);
    let mut routes = live_routes_from_openapi(&document);

    // Preserve any hand-promoted `verified = true` / notes from the
    // previous manifest generation for routes that still exist, so
    // regenerating does not silently wipe out prior qualification work.
    if let Ok(existing_text) = std::fs::read_to_string(MANIFEST_PATH) {
        if let Ok(existing) = toml::from_str::<CoverageManifest>(&existing_text) {
            for (key, entry) in existing.routes {
                if let Some(current) = routes.get_mut(&key) {
                    if current.method == entry.method && current.path == entry.path {
                        current.verified = entry.verified;
                        current.notes = entry.notes;
                    }
                }
            }
        }
    }

    for entry in routes.values_mut() {
        if entry.notes.is_empty() && !entry.verified {
            entry.notes = "unverified: generated from live OpenAPI inventory; no executed \
                           request/response assertion authored yet"
                .to_string();
        }
    }

    let manifest = CoverageManifest { routes };
    let rendered = toml::to_string_pretty(&manifest).expect("serialize OPS-004 coverage manifest");
    std::fs::write(MANIFEST_PATH, rendered).expect("write OPS-004 coverage manifest");

    let unverified = manifest.routes.values().filter(|r| !r.verified).count();
    println!(
        "OPS-004 manifest regenerated: {} routes, {} unverified",
        manifest.routes.len(),
        unverified
    );
}

// ---------------------------------------------------------------------
// THE DRIFT TEST — must always run, must never be weakened.
// ---------------------------------------------------------------------

/// Fails the build when the live route inventory and the committed
/// coverage manifest disagree in either direction:
/// - a live route with no manifest entry (a route shipped without OPS-004
///   qualification coverage), or
/// - a manifest entry whose route no longer exists live (stale coverage
///   that would otherwise mask real drift).
#[test]
fn manifest_matches_live_route_inventory() {
    let home = TempDir::new().unwrap();
    let daemon = start_daemon(home.path());
    let document = fetch_openapi_document(&daemon);
    let live = live_routes_from_openapi(&document);
    let manifest = load_manifest();

    let live_keys: BTreeSet<&String> = live.keys().collect();
    let manifest_keys: BTreeSet<&String> = manifest.routes.keys().collect();

    let uncovered: Vec<&&String> = live_keys.difference(&manifest_keys).collect();
    let stale: Vec<&&String> = manifest_keys.difference(&live_keys).collect();

    assert!(
        uncovered.is_empty(),
        "OPS-004 drift: {} live route(s) have NO coverage-manifest entry \
         (a route shipped without qualification coverage) — add an entry to \
         {MANIFEST_PATH} for: {:#?}",
        uncovered.len(),
        uncovered
    );
    assert!(
        stale.is_empty(),
        "OPS-004 drift: {} manifest entr(y/ies) reference route(s) that no \
         longer exist in the live inventory (stale coverage masking real \
         drift) — remove from {MANIFEST_PATH}: {:#?}",
        stale.len(),
        stale
    );

    assert_eq!(
        live.len(),
        manifest.routes.len(),
        "OPS-004: live route count and manifest entry count must match exactly"
    );

    let contract_drift = live
        .iter()
        .filter_map(|(key, live_entry)| {
            let manifest_entry = manifest.routes.get(key)?;
            let same_contract = live_entry.method == manifest_entry.method
                && live_entry.path == manifest_entry.path
                && live_entry.auth == manifest_entry.auth
                && live_entry.has_request_schema == manifest_entry.has_request_schema
                && live_entry.request_body_required == manifest_entry.request_body_required
                && live_entry.has_response_schema == manifest_entry.has_response_schema
                && live_entry.success_status == manifest_entry.success_status;
            (!same_contract)
                .then(|| format!("{key}: live={live_entry:?}, manifest={manifest_entry:?}"))
        })
        .collect::<Vec<_>>();
    assert!(
        contract_drift.is_empty(),
        "OPS-004 contract metadata drift (regenerate and review manifest):\n{}",
        contract_drift.join("\n")
    );

    let unverified = manifest.routes.values().filter(|r| !r.verified).count();
    println!(
        "OPS-004 coverage manifest: {} live routes, {} manifest entries, {} unverified",
        live.len(),
        manifest.routes.len(),
        unverified
    );
}

/// Sanity check on the drift-detection logic itself: an inventory missing a
/// route the manifest declares must be caught, and a manifest missing a
/// live route must be caught. Runs against synthetic data only (no daemon).
#[test]
fn drift_detection_catches_uncovered_and_stale_entries() {
    let mut live = BTreeMap::new();
    live.insert(
        route_key("GET", "/health"),
        RouteCoverage {
            method: "GET".into(),
            path: "/health".into(),
            auth: "daemonBearer".into(),
            has_request_schema: false,
            request_body_required: false,
            has_response_schema: false,
            success_status: "200".into(),
            verified: true,
            notes: String::new(),
        },
    );
    live.insert(
        route_key("POST", "/new-route"),
        RouteCoverage {
            method: "POST".into(),
            path: "/new-route".into(),
            auth: "daemonBearer".into(),
            has_request_schema: true,
            request_body_required: true,
            has_response_schema: false,
            success_status: "200".into(),
            verified: false,
            notes: String::new(),
        },
    );

    let mut manifest_routes = BTreeMap::new();
    manifest_routes.insert(
        route_key("GET", "/health"),
        live[&route_key("GET", "/health")].clone(),
    );
    manifest_routes.insert(
        route_key("DELETE", "/removed-route"),
        RouteCoverage {
            method: "DELETE".into(),
            path: "/removed-route".into(),
            auth: "daemonBearer".into(),
            has_request_schema: false,
            request_body_required: false,
            has_response_schema: false,
            success_status: "200".into(),
            verified: true,
            notes: String::new(),
        },
    );

    let live_keys: BTreeSet<&String> = live.keys().collect();
    let manifest_keys: BTreeSet<&String> = manifest_routes.keys().collect();
    let uncovered: Vec<_> = live_keys.difference(&manifest_keys).collect();
    let stale: Vec<_> = manifest_keys.difference(&live_keys).collect();

    assert_eq!(uncovered.len(), 1, "must detect the uncovered new route");
    assert_eq!(stale.len(), 1, "must detect the stale removed-route entry");
}

// ---------------------------------------------------------------------
// THE EXECUTOR — full route method/schema/auth/response acceptance.
// ---------------------------------------------------------------------

fn concrete_path_from_operation(
    template: &str,
    operation: &Value,
    document: &Value,
    home: &Path,
) -> String {
    let parameters = operation.get("parameters").and_then(Value::as_array);
    template
        .split('/')
        .map(|segment| {
            if !(segment.starts_with('{') && segment.ends_with('}')) {
                return segment.to_string();
            }
            let name = &segment[1..segment.len() - 1];
            let schema = parameters
                .into_iter()
                .flatten()
                .find(|parameter| parameter["in"] == "path" && parameter["name"] == name)
                .and_then(|parameter| parameter.get("schema"));
            match schema {
                Some(schema) => {
                    let sample = if name == "id" && template.contains("/driver") {
                        std::env::var("RIGHTKIT_OPS004_DRIVER_SESSION_ID")
                            .map(Value::String)
                            .unwrap_or_else(|_| {
                                sample_from_schema(schema, document, Some(name), home, 0)
                            })
                    } else {
                        sample_from_schema(schema, document, Some(name), home, 0)
                    };
                    match sample {
                        Value::String(value) => value,
                        Value::Number(value) => value.to_string(),
                        Value::Bool(value) => value.to_string(),
                        _ => "ops004-fixture".to_string(),
                    }
                }
                None => "ops004-fixture".to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn add_workspace_query(url: String, home: &Path, path: Option<&str>) -> String {
    let mut parsed = url::Url::parse(&url).expect("OPS-004 fixture URL must parse");
    parsed
        .query_pairs_mut()
        .append_pair("workspaceRoot", &home.to_string_lossy());
    if let Some(path) = path {
        parsed.query_pairs_mut().append_pair("path", path);
    }
    parsed.to_string()
}

/// Drives every manifest route with its declared HTTP method, OpenAPI-derived
/// request body, and declared authentication carrier. Successful responses
/// are validated against their declared response schema. A 405 is always a
/// hard failure; a 415/422 is also a hard failure because it means the
/// declared request contract was not accepted by the live handler.
///
/// A `405` means the route is not actually wired for the method the
/// manifest declares — that is a defect, not passing evidence, and this
/// test treats it as an unconditional failure (never downgraded to a
/// warning, never excluded via an allowlist).
///
/// This is the full OPS-004 exhaustive route-acceptance run. Per dispatch
/// it now runs by default: it completes in under a second against a real
/// daemon and is the only thing that actually satisfies the atom's bar
/// ("exercise every route using its declared method"). It is no longer
/// run — the drift test above is the always-on gate that prevents new
/// routes from shipping without a coverage-manifest entry in the first
/// place.
#[test]
fn every_route_responds_to_its_declared_method() {
    let _process_global_guard = crate::support::process_global_lock();
    let manifest = load_manifest();
    let home = TempDir::new().unwrap();
    seed_driver_fixture(home.path());
    let daemon = start_daemon(home.path());
    provision_trigger(&daemon, home.path(), "ops004-github", "github");
    provision_trigger(&daemon, home.path(), "ops004-generic", "generic");
    provision_driver_chain(&daemon, home.path());
    let document = fetch_openapi_document(&daemon);
    let http = client();
    let mut failures = Vec::new();
    // A route acceptance run that exercised nothing would pass trivially, so
    // the count is asserted and reported rather than left implicit.
    assert!(
        manifest.routes.len() >= 100,
        "route manifest looks empty or truncated ({} entries); \
         an acceptance run over nothing is not evidence",
        manifest.routes.len()
    );
    let mut exercised = 0usize;
    let mut status_counts: std::collections::BTreeMap<u16, usize> =
        std::collections::BTreeMap::new();

    for entry in manifest.routes.values() {
        let operation = operation_for(&document, entry);
        let concrete = if entry.path == "/triggers/{id}/deliver/github" {
            "/triggers/ops004-github/deliver/github".to_string()
        } else if entry.path == "/triggers/{id}/deliver/generic" {
            "/triggers/ops004-generic/deliver/generic".to_string()
        } else {
            concrete_path_from_operation(&entry.path, operation, &document, home.path())
        };
        let mut url = format!("{}{}", daemon.url(), concrete);
        if matches!(
            entry.path.as_str(),
            "/workspace/tree"
                | "/workspace/file"
                | "/workspace/artifacts"
                | "/workspace/recent"
                | "/workspace/watch"
                | "/sandbox/settings"
                | "/local-models/add-provider"
                | "/settings/orchestration"
        ) {
            url = add_workspace_query(
                url,
                home.path(),
                (entry.path == "/workspace/file").then_some("ops004-fixture.txt"),
            );
        }
        if matches!(
            entry.path.as_str(),
            "/run-changes/inventory" | "/run-changes/summary"
        ) {
            // These handlers take `Query<InventoryRequest>` — the query names
            // are the serde (snake_case) wire names, not the camelCase doc
            // conventions used elsewhere.
            let mut parsed = url::Url::parse(&url).expect("OPS-004 fixture URL must parse");
            parsed
                .query_pairs_mut()
                .append_pair("run_id", "ops004-fixture")
                .append_pair("workspace_root", &home.path().to_string_lossy());
            url = parsed.to_string();
        }
        let method = match entry.method.as_str() {
            "GET" => rightkit_http::Method::Get,
            "POST" => rightkit_http::Method::Post,
            "PUT" => rightkit_http::Method::Put,
            "PATCH" => rightkit_http::Method::Patch,
            "DELETE" => rightkit_http::Method::Delete,
            other => {
                failures.push(format!(
                    "{} {}: unsupported method {other}",
                    entry.method, entry.path
                ));
                continue;
            }
        };
        let mut request = rightkit_http::Request::new(method, &url);
        if entry.auth != "none" {
            request = request.bearer(&daemon.startup.token);
        }
        if entry.path == "/sessions/{id}/messages" {
            // A new id resumes/initializes its engine and waits for durable
            // admission before replying. Keep that real path bounded, allowing
            // the same startup budget as start_daemon instead of a 5s probe.
            request = request.timeout(STARTUP_TIMEOUT);
        }
        if entry.path.contains("/driver/") || entry.path.contains("/driver-capabilities/") {
            if let Ok(token) = std::env::var("RIGHTKIT_OPS004_CAPABILITY_TOKEN") {
                request = request.header("x-coderight-capability-token", token);
            }
            if let Ok(instance) = std::env::var("RIGHTKIT_OPS004_DRIVER_INSTANCE") {
                request = request.header("x-coderight-driver-instance", instance);
            }
        }

        let request_body = if matches!(entry.method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE")
            && request_schema_for(operation).is_some()
        {
            let empty_schema = Value::Object(serde_json::Map::new());
            let schema = request_schema_for(operation).unwrap_or(&empty_schema);
            let body = if entry.path == "/triggers/{id}/deliver/github"
                || entry.path == "/triggers/{id}/deliver/generic"
            {
                serde_json::json!({"ops004": true})
            } else if let Some(authored) =
                authored_request_body(&entry.path, home.path(), &daemon.url())
            {
                authored
            } else {
                sample_from_schema(schema, &document, None, home.path(), 0)
            };
            if let Err(error) = validate_instance(&document, schema, &body) {
                failures.push(format!(
                    "{} {}: generated request body does not satisfy OpenAPI schema: {error}; body={body}",
                    entry.method, entry.path
                ));
            }
            Some(serde_json::to_vec(&body).expect("serialize OPS-004 request fixture"))
        } else {
            None
        };
        if let Some(body) = request_body.as_ref() {
            request = request
                .header("content-type", "application/json")
                .body(body.clone());
        }
        if entry.path == "/triggers/{id}/deliver/github"
            || entry.path == "/triggers/{id}/deliver/generic"
        {
            let body = request_body
                .as_ref()
                .expect("signed delivery routes must have a JSON body");
            let (signature, delivery, event) = if entry.path.ends_with("/github") {
                ("X-Hub-Signature-256", "X-GitHub-Delivery", "X-GitHub-Event")
            } else {
                (
                    "X-CodeRight-Signature-256",
                    "X-CodeRight-Delivery-Id",
                    "X-CodeRight-Event",
                )
            };
            request = request
                .header(signature, hmac_sha256_hex(OPS004_SECRET.as_bytes(), body))
                .header(
                    delivery,
                    format!("ops004-{}", entry.method.to_ascii_lowercase()),
                )
                .header(event, "ops004_fixture");
        }

        let response = match http.send_any(&request) {
            Ok(response) => response,
            Err(error) => {
                failures.push(format!(
                    "{} {}: request failed: {error:?}",
                    entry.method, entry.path
                ));
                continue;
            }
        };

        // The critical property: a 405 is never route evidence. Any other
        // status proves the method reached a handler; 415/422 specifically
        // indicate that the declared request schema was not accepted.
        exercised += 1;
        let status = response.status;
        *status_counts.entry(status).or_default() += 1;
        let content_type = response
            .header("content-type")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let body_text = response.text();
        if status == 401 || status == 405 || status == 415 || status == 422 {
            failures.push(format!(
                "{} {}: got HTTP {status} — declared method/schema/auth contract was not accepted; body={body_text:?}",
                entry.method, entry.path,
            ));
        }
        if (200..300).contains(&status) && status.to_string() != entry.success_status {
            failures.push(format!(
                "{} {}: OpenAPI declares success {}, live handler returned {status}",
                entry.method, entry.path, entry.success_status
            ));
        }
        if let Some(schema) = response_schema_for(operation, status) {
            if !body_text.trim().is_empty() {
                match serde_json::from_str::<Value>(&body_text) {
                    Ok(body) => {
                        if let Err(error) = validate_instance(&document, schema, &body) {
                            failures.push(format!(
                                "{} {}: HTTP {status} response violates declared schema: {error}; body={body}",
                                entry.method, entry.path
                            ));
                        }
                    }
                    Err(error) => failures.push(format!(
                        "{} {}: HTTP {status} declared JSON response is not JSON: {error}; content-type={content_type:?}; body={body_text:?}",
                        entry.method, entry.path
                    )),
                }
            } else {
                failures.push(format!(
                    "{} {}: HTTP {status} declared a JSON response schema but returned an empty body",
                    entry.method, entry.path
                ));
            }
        } else if content_type.contains("json") && !body_text.trim().is_empty() {
            if let Err(error) = serde_json::from_str::<Value>(&body_text) {
                failures.push(format!(
                    "{} {}: JSON response could not be parsed: {error}; body={body_text:?}",
                    entry.method, entry.path
                ));
            }
        }
    }

    println!(
        "OPS-004 acceptance: {exercised}/{} routes exercised by declared method; \
         status distribution {status_counts:?}",
        manifest.routes.len()
    );
    assert_eq!(
        exercised,
        manifest.routes.len(),
        "every manifest route must be driven, not skipped; failures:\n{}",
        failures.join("\n")
    );

    assert!(
        failures.is_empty(),
        "OPS-004 route acceptance failures ({}):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
