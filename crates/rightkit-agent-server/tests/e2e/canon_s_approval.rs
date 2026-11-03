//! S:B4 / S:B5 approval review and correlation through a real daemon session.

use crate::support::e2e_api::*;
use crate::support::scripted_provider::Step;
use serde_json::json;

#[test]
fn approval_request_id_is_bound_to_call_and_wrong_id_does_not_consume_it() {
    let rig = Rig::start(vec![
        Step::tool(
            "s-b5-shell",
            "shell",
            json!({"command":"echo approval-e2e"}),
        ),
        Step::text("approval completed"),
    ]);
    rig.session("s-b5", json!({"permissionMode":"ask"}));
    assert_eq!(rig.send("s-b5", "run the requested shell command").0, 200);

    let requested = rig.wait_event("s-b5", "toolApprovalRequested");
    assert!(requested.to_string().contains("s-b5-shell"), "{requested}");
    let request_id =
        rightkit_agent_policy::denial_tracking::correlation_token("s-b5", "s-b5-shell");
    assert!(!request_id.is_empty());

    let (status, _) = rig.post(
        "/sessions/s-b5/tool-approval",
        json!({"toolCallId":"s-b5-shell", "approved":true, "requestId":"wrong-request"}),
    );
    assert_eq!(
        status, 404,
        "wrong request identity must be rejected without resolving approval"
    );

    let (status, result) = rig.post(
        "/sessions/s-b5/tool-approval",
        json!({"toolCallId":"s-b5-shell", "approved":true, "requestId":request_id}),
    );
    assert_eq!(status, 200, "matching approval request: {result}");
    assert_eq!(result["resolved"], true);
    rig.wait_assistant("s-b5", "approval completed");
}
