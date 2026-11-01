//! Black-box provider, credential & driver contracts (PL-001..PL-005).

use crate::support::e2e_api::Rig;
use serde_json::json;

#[test]
fn pl001_subscription_status_is_typed_and_never_returns_credential_bytes() {
    let rig = Rig::start(vec![]);
    let (status, body) = rig.get("/provider-auth/openai");
    assert_eq!(status, 200, "{body}");
    assert!(
        body["subscription_auth_state"].is_string(),
        "typed auth state: {body}"
    );
    assert!(
        body["subscription_eligibility"].is_string(),
        "eligibility state is explicit: {body}"
    );
    let serialized = body.to_string().to_ascii_lowercase();
    assert!(
        !serialized.contains("access_token"),
        "credential bytes are never projected: {body}"
    );
    assert!(
        !serialized.contains("refresh_token"),
        "credential bytes are never projected: {body}"
    );
}

#[test]
fn pl002_inference_uses_one_broker_resolution_without_exposing_secret() {
    let rig = Rig::start(vec![crate::support::scripted_provider::Step::text(
        "broker-ok",
    )]);
    rig.session("broker", json!({}));
    let (status, _) = rig.send("broker", "say broker-ok");
    assert_eq!(status, 200);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    rig.wait("assistant broker-ok", 60, || {
        let transcript = rig.get("/sessions/broker/transcript").1;
        let found = transcript
            .as_array()
            .and_then(|rows| {
                rows.iter().find(|row| {
                    row["$kind"] == "assistant" && row.to_string().contains("broker-ok")
                })
            })
            .cloned();
        assert!(
            std::time::Instant::now() < deadline || found.is_some(),
            "broker turn stalled; provider_calls={}; events={:?}; transcript={transcript}",
            rig.provider.request_count(),
            rig.history("broker")
        );
        found
    });
    let transcript = rig.get("/sessions/broker/transcript").1.to_string();
    for marker in [
        "OPENAI_API_KEY",
        "RIGHTKIT_API_KEY",
        "access_token",
        "refresh_token",
    ] {
        assert!(
            !transcript.contains(marker),
            "credential material must not enter transcript"
        );
    }
    assert_eq!(
        rig.provider.request_count(),
        1,
        "one resolved route reaches provider"
    );
}

#[test]
fn pl003_provider_registry_projects_availability_and_separate_subscription_eligibility() {
    let rig = Rig::start(vec![]);
    let (status, body) = rig.get("/provider-auth/openai");
    assert_eq!(status, 200, "provider registry status: {body}");
    assert_eq!(body["provider_id"], "openai");
    assert!(
        body["configured"].is_boolean(),
        "provider availability/configuration: {body}"
    );
    assert!(
        body["subscription_eligibility"].is_string(),
        "eligibility stays separate from API availability: {body}"
    );
    assert!(
        body["available_model_ids"].as_array().is_some(),
        "model availability metadata: {body}"
    );
}

#[test]
fn pl004_custom_provider_registration_keeps_extension_identity_and_auth_declaration() {
    let plugins = tempfile::tempdir().expect("isolated provider extension catalog");
    let extension = plugins.path().join("fixture-provider");
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(extension.join("plugin.py"), "").unwrap();
    std::fs::write(
        extension.join("plugin.json"),
        json!({
            "id":"fixture-provider", "name":"Fixture provider", "version":"1.0.0",
            "description":"Provider registration fixture", "entry":"plugin.py",
            "provides_tools":[], "provides_commands":[], "enabled":true
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        extension.join("provider.json"),
        json!({
            "schema_version":1, "id":"fixture-provider", "display_name":"Fixture provider",
            "version":"1.0.0", "protocol":"open_ai_compatible",
            "endpoint":{"base_url":"http://127.0.0.1:1/v1",
                "origin":{"kind":"extension","extension_id":"fixture-provider"}},
            "auth":{"kind":"api_key","env_vars":["FIXTURE_PROVIDER_KEY"]},
            "secrets":[{"name":"FIXTURE_PROVIDER_KEY","required":true,"purpose":"inference"}],
            "models":[{"id":"fixture-model","slug":"fixture-provider/fixture-model",
                "display_name":"Fixture model","context_window_tokens":8192,
                "streaming":true,"tools":true,"tier":"cheap"}],
            "health_probe":{"kind":"none","timeout_ms":1000}
        })
        .to_string(),
    )
    .unwrap();
    let plugin_dir = plugins.path().to_string_lossy().into_owned();
    let rig = Rig::start_with_env(vec![], &[("RIGHTKIT_PLUGIN_DIR", plugin_dir.as_str())]);
    let (status, body) = rig.get("/provider-extensions");
    assert_eq!(status, 200, "custom provider extension registry: {body}");
    let entries = body
        .as_array()
        .or_else(|| body["providers"].as_array())
        .expect("extension provider rows");
    let registered = entries
        .iter()
        .find(|row| row["id"] == "fixture-provider")
        .expect("real enabled provider extension must be registered at boot");
    assert_eq!(registered["authResolver"], "api_key");
    assert_eq!(registered["authDeclaration"]["kind"], "api_key");
    assert_eq!(
        registered["authDeclaration"]["env_vars"],
        json!(["FIXTURE_PROVIDER_KEY"])
    );
    assert_eq!(registered["endpointOrigin"]["kind"], "extension");
    assert_eq!(
        registered["endpointOrigin"]["extension_id"],
        "fixture-provider"
    );
    assert_eq!(
        registered["health"], "unchecked",
        "registration cannot fabricate probe success"
    );
    assert_eq!(
        registered["models"][0]["slug"],
        "fixture-provider/fixture-model"
    );
    for row in entries {
        assert!(
            row["source"] == "extension",
            "extension remains distinct from built-ins: {row}"
        );
        assert!(
            row["authResolver"].is_string() || row["authModes"].is_array(),
            "auth declaration: {row}"
        );
        assert!(
            row["health"].is_string() || row["healthStatus"].is_string(),
            "health state: {row}"
        );
    }
}

#[test]
fn pl005_foreign_driver_session_supports_start_prompt_resume_interrupt_and_stop() {
    let mut rig = Rig::start(vec![]);
    rig.daemon.stop();
    // PL-005 requires executable/profile attestation plus a session-bound lease.
    // Reuse OPS-004's real persistent stream-json process, isolated from host CLIs.
    crate::ops004_route_contract::seed_driver_fixture(rig.home.path());
    // AppContainer scrubs ambient daemon flags. This non-secret workspace
    // marker selects the fixture without changing production launch policy.
    std::fs::write(rig.ws().join(".pl005-gateway-fixture"), b"gateway")
        .expect("select isolated gateway fixture");
    let driver_path = rig
        .home
        .path()
        .join("driverbin")
        .to_string_lossy()
        .into_owned();
    rig.daemon = crate::support::daemon_process::start_daemon(
        rig.home.path(),
        &[
            ("RIGHTKIT_DISABLE_VENDOR_CLIS", "0"),
            ("PATH", driver_path.as_str()),
        ],
    );
    rig.session("foreign", json!({"driverKind":"claude"}));
    let (status, lease) = rig.post(
        "/sessions/foreign/driver-capabilities/leases",
        json!({
            "driverKind":"claude", "binaryVersion":"ops004-driver-fixture 1.0.0",
            "schemaHash":"ops004", "tools":["read_file"],
        }),
    );
    assert_eq!(status, 200, "issue admitted driver lease: {lease}");
    let binding_id = lease["bindingId"].as_str().expect("binding id");
    let capability_token = lease["capabilityToken"].as_str().expect("capability lease");
    let post = |path: &str, body: serde_json::Value| {
        let response = crate::support::daemon_process::send(
            &rightkit_http::Request::post(format!("{}{path}", rig.url()))
                .bearer(rig.token())
                .header("x-coderight-capability-token", capability_token)
                .json(&body)
                .expect("JSON request body"),
        )
        .expect("driver lifecycle request");
        let status = response.status;
        let body = response
            .json::<serde_json::Value>()
            .unwrap_or(serde_json::Value::Null);
        (status, body)
    };
    let (status, start) = post(
        "/sessions/foreign/driver/start",
        json!({"bindingId":binding_id}),
    );
    assert_eq!(status, 200, "start foreign root: {start}");
    assert_eq!(start["bindingId"], binding_id);
    // PL-005: system/init establishes identity; MCP readiness follows the
    // first governed prompt (driver_status_after_event), never an init echo.
    let initialized = rig.wait("foreign driver init", 20, || {
        let (status, state) = rig.get("/sessions/foreign/driver/status");
        assert_eq!(status, 200, "foreign driver status: {state}");
        assert!(
            !state["drivers"].as_array().is_some_and(|drivers| drivers
                .iter()
                .any(|row| row["bindingId"] == binding_id
                    && row["gatewayState"]["state"] == "failed")),
            "foreign driver failed before init: {state}; events={:?}",
            rig.history("foreign")
        );
        state["drivers"]
            .as_array()
            .and_then(|drivers| {
                drivers.iter().find(|row| {
                    row["bindingId"] == binding_id && row["vendorSessionId"].is_string()
                })
            })
            .cloned()
    });
    let vendor_session_id = initialized["vendorSessionId"]
        .as_str()
        .expect("observed vendor session");
    let (status, prompt) = post(
        "/sessions/foreign/driver/prompt",
        json!({"text":"continue","bindingId":binding_id,"vendorSessionId":vendor_session_id}),
    );
    assert_eq!(
        status,
        200,
        "prompt/steer: {prompt}; driver status={:?}; events={:?}",
        rig.get("/sessions/foreign/driver/status").1,
        rig.history("foreign"),
    );
    assert_eq!(prompt["accepted"], true);
    rig.wait("foreign driver MCP readiness", 20, || {
        let (status, state) = rig.get("/sessions/foreign/driver/status");
        assert_eq!(status, 200, "foreign driver status: {state}");
        assert!(
            !state["drivers"].as_array().is_some_and(|drivers| drivers
                .iter()
                .any(|row| row["bindingId"] == binding_id
                    && row["gatewayState"]["state"] == "failed")),
            "foreign driver failed before MCP readiness: {state}; events={:?}",
            rig.history("foreign")
        );
        state["drivers"]
            .as_array()
            .and_then(|drivers| {
                drivers.iter().find(|row| {
                    row["bindingId"] == binding_id && row["gatewayState"]["state"] == "ready"
                })
            })
            .cloned()
    });
    rig.wait_assistant("foreign", "gateway-read-ok");
    let (status, resume) = post(
        "/sessions/foreign/driver/resume",
        json!({"bindingId":binding_id,"vendorSessionId":vendor_session_id}),
    );
    assert_eq!(status, 200, "resume: {resume}");
    assert_eq!(resume["resumed"], true);
    let (status, interrupted) = post(
        "/sessions/foreign/driver/interrupt",
        json!({"bindingId":binding_id,"vendorSessionId":vendor_session_id}),
    );
    assert_eq!(status, 200, "interrupt: {interrupted}");
    assert_eq!(interrupted["interrupted"], true);
    let (status, stopped) = post(
        "/sessions/foreign/driver/stop",
        json!({"bindingId":binding_id,"vendorSessionId":vendor_session_id}),
    );
    assert_eq!(status, 200, "stop: {stopped}");
    // The shipped stop response acknowledges `stopped`; prove persisted terminal state too.
    assert_eq!(stopped["stopped"], true);
    let storage = rightkit_agent_engine_storage::Storage::open(&rig.daemon.startup.database_path)
        .expect("driver store");
    let binding = storage
        .get_driver_binding(binding_id)
        .expect("read stopped binding")
        .expect("persistent binding");
    assert_eq!(binding.status, "stopped");
}
