//! FM runtime/API journeys for consolidation, hook operations, and trigger auth.

use crate::support::e2e_api::Rig;
use crate::support::e2e_orch::{Env, Provider, Reply};
use crate::support::scripted_provider::Step;
use serde_json::json;

#[test]
fn memory_consolidation_status_is_a_typed_live_projection() {
    let rig = Rig::start(vec![Step::text("ready")]);
    let (status, body) = rig.get("/memory/consolidation-status");
    assert_eq!(status, 200, "consolidation status route: {body}");
    for field in ["lastRunOk", "totalRuns", "totalMemoriesWritten"] {
        assert!(body.get(field).is_some(), "missing {field}: {body}");
    }
}

#[test]
fn invalid_external_trigger_signature_is_unauthorized_and_audited() {
    let env = Env::start(
        Provider::start(|_, _| Reply::text("must not run")),
        &[(
            "RIGHTKIT_FM151_TEST_SECRET",
            "fixture-secret-at-least-16-chars",
        )],
        |_, _| {},
    );
    let (status, trigger) = env.post(
        "/triggers",
        json!({
            "id":"fm151-e2e",
            "provider":"generic",
            "workspaceRoot":env.workspace.to_string_lossy(),
            "prompt":"never admitted without a valid signature",
            "secretEnv":"RIGHTKIT_FM151_TEST_SECRET"
        }),
    );
    assert_eq!(status, 201, "trigger creation: {trigger}");

    let response = crate::support::daemon_process::send(
        &rightkit_http::Request::post(format!("{}/triggers/fm151-e2e/deliver/generic", env.url()))
            .header("x-coderight-delivery-id", "fm151-delivery-1")
            .header("x-coderight-event", "push")
            .header("content-type", "application/json")
            .body(r#"{"action":"test"}"#),
    )
    .expect("unauthenticated trigger delivery");
    assert_eq!(
        response.status, 401,
        "invalid signature must be typed UNAUTHORIZED"
    );
    let (status, audit) = env.get("/triggers/audit");
    assert_eq!(status, 200, "delivery audit: {audit}");
    assert!(
        audit["deliveries"].as_array().unwrap().iter().any(|row| {
            row["deliveryId"] == "fm151-delivery-1" && row["reason"] == "invalid_signature"
        }),
        "rejected delivery must leave durable audit evidence: {audit}"
    );
    assert_eq!(
        env.provider.count(),
        0,
        "rejected delivery must not reach model"
    );
}
