//! FM approval and policy journeys through the real daemon session runtime.

#[path = "e2e_engine_session_control.rs"]
mod session_control_journeys;

#[path = "e2e_engine_policy.rs"]
mod policy_journeys;

use crate::support::e2e_api::Rig;
use crate::support::scripted_provider::Step;
use serde_json::{json, Value};

#[test]
fn auto_safe_classifier_allows_file_mutation_but_gates_shell_and_keeps_cache_safe() {
    let rig = Rig::start(vec![
        Step::tool(
            "write1",
            "write_file",
            json!({"path":"auto-safe.txt","content":"approved"}),
        ),
        Step::text("file done"),
        Step::tool("shell1", "shell", json!({"command":"echo should-wait"})),
    ]);
    // Auto-safe auto-edits apply only in trusted workspaces (policy::trust).
    let (status, trust) = rig.patch(
        "/workspace/trust",
        json!({
            "workspaceRoot":rig.ws().to_string_lossy(), "trust":"trusted", "approvedBy":"e2e"
        }),
    );
    assert_eq!(status, 200, "trust fixture workspace: {trust}");
    let session = rig.session("fm029", json!({"permissionMode":"auto-safe"}));
    let workspace = std::path::PathBuf::from(session["workspaceRoot"].as_str().unwrap());
    assert_eq!(rig.send("fm029", "write a safe file").0, 200);
    rig.wait_assistant("fm029", "file done");
    assert_eq!(
        std::fs::read_to_string(workspace.join("auto-safe.txt")).unwrap(),
        "approved"
    );
    assert_eq!(rig.send("fm029", "run shell command").0, 200);
    rig.wait_event("fm029", "toolApprovalRequested");
    let events = rig.history("fm029");
    let approval = events
        .iter()
        .find(|event| kind_of(event) == "toolApprovalRequested");
    assert!(
        approval.is_some(),
        "auto-safe must still gate shell: {events:?}"
    );
    assert_eq!(call_name(approval.unwrap()), "shell");
    let seen = rig.provider.requests();
    assert!(
        seen.len() >= 2,
        "classifier/model calls must reach provider: {seen:?}"
    );
    assert!(
        seen.iter().all(|request| request["messages"].is_array()),
        "provider cache input must remain a valid message sequence"
    );
}

#[test]
fn approval_review_emits_verdict_with_correlation_identity() {
    let rig = Rig::start(vec![
        Step::tool(
            "review1",
            "write_file",
            json!({"path":"reviewed.txt","content":"blocked"}),
        ),
        Step::text("review complete"),
    ]);
    rig.session("fm031", json!({}));
    assert_eq!(rig.send("fm031", "write reviewed file").0, 200);
    let events = rig.wait("approval review verdict", 20, || {
        rig.history("fm031").into_iter().find(|event| {
            matches!(
                kind_of(event),
                "toolApprovalReviewed" | "approvalReviewCompleted"
            )
        })
    });
    let data = events.get("data").unwrap_or(&events);
    assert!(
        data.get("verdict").is_some(),
        "review verdict missing: {events}"
    );
    assert!(
        data.get("correlationId").and_then(Value::as_str).is_some(),
        "review event must carry correlation identity: {events}"
    );
}

fn kind_of(event: &Value) -> &str {
    event["$kind"]
        .as_str()
        .or_else(|| event["kind"].as_str())
        .or_else(|| event["type"].as_str())
        .unwrap_or("")
}

fn call_name(event: &Value) -> &str {
    event["call"]["name"]
        .as_str()
        .or_else(|| event["data"]["call"]["name"].as_str())
        .unwrap_or("")
}
