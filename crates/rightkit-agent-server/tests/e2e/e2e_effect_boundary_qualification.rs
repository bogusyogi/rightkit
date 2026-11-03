//! Real-daemon effect admission across a transforming PreToolUse hook.
//!
//! EFF-002 black-box slice: the approval request and eventual execution must
//! use the hook-transformed final payload, and the mutation must wait for the
//! approval decision. Engine unit coverage exercises digest calculation; this
//! journey proves the public daemon path carries the effective call end to end.

use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use scripted_provider::Step;
use serde_json::{json, Value};
#[test]
fn transformed_write_is_approved_and_executed_as_the_effective_payload() {
    let hook_dir = tempfile::tempdir().expect("hook temp dir");
    let hook = hook_dir.path().join("transform.js");
    std::fs::write(
        &hook,
        "const a=JSON.parse(process.env.RIGHTKIT_HOOK_ARGS_JSON);a.path='effective.txt';process.stdout.write(JSON.stringify({schemaVersion:1,action:'transform',arguments:a}));",
    )
    .expect("write transform hook");
    let hook_stderr = hook_dir.path().join("hook.stderr");
    let command = format!(
        "node \"{}\" 2> \"{}\"",
        hook.to_string_lossy().replace('\\', "/"),
        hook_stderr.to_string_lossy().replace('\\', "/")
    );

    let rig = Rig::start_with_env(
        vec![
            Step::tool(
                "eff002-write",
                "write_file",
                json!({"path":"proposed.txt", "content":"approved bytes"}),
            ),
            Step::text("effective write completed"),
        ],
        &[("RIGHTKIT_HOOK_PRE_TOOL_USE", &command)],
    );
    let session = rig.session("eff002", json!({"permissionMode":"ask"}));
    assert_eq!(rig.send("eff002", "write the requested file").0, 200);

    let requested = rig.wait_event("eff002", "toolApprovalRequested");
    let call = requested
        .get("call")
        .or_else(|| requested.get("data").and_then(|data| data.get("call")))
        .expect("approval call");
    assert_eq!(call["id"], "eff002-write");
    let approved_args: Value =
        serde_json::from_str(call["argumentsJson"].as_str().expect("approval arguments"))
            .expect("normalized approval payload");
    assert_eq!(
        approved_args["path"],
        "effective.txt",
        "hook stderr: {}; events: {:?}",
        std::fs::read_to_string(&hook_stderr).unwrap_or_default(),
        rig.history("eff002")
    );
    assert_eq!(approved_args["content"], "approved bytes");
    let root = std::path::Path::new(session["workspaceRoot"].as_str().unwrap());
    assert!(
        std::fs::read_to_string(root.join("effective.txt")).is_err(),
        "effective mutation must not land before approval"
    );
    assert!(
        !rig.kinds("eff002")
            .iter()
            .any(|kind| kind == "toolCallStarted"),
        "effect dispatch must wait until approval"
    );

    let (status, body) = rig.post(
        "/sessions/eff002/tool-approval",
        json!({"toolCallId":"eff002-write", "approved":true}),
    );
    assert_eq!(status, 200, "{body}");
    rig.wait_assistant("eff002", "effective write completed");

    assert_eq!(
        std::fs::read_to_string(root.join("effective.txt")).unwrap(),
        "approved bytes"
    );
    assert!(
        std::fs::read_to_string(root.join("proposed.txt")).is_err(),
        "unapproved original payload must not execute"
    );

    let events = rig.history("eff002");
    let approval = events
        .iter()
        .position(|event| kind_of(event) == "toolApprovalRequested")
        .unwrap();
    let resolved = events
        .iter()
        .position(|event| kind_of(event) == "toolApprovalResolved")
        .unwrap();
    let started = events
        .iter()
        .position(|event| {
            kind_of(event) == "toolCallStarted" && event.to_string().contains("eff002-write")
        })
        .unwrap();
    let result = events
        .iter()
        .position(|event| {
            kind_of(event) == "toolResultAdded" && event.to_string().contains("eff002-write")
        })
        .unwrap();
    assert!(
        approval < resolved && resolved < started && started < result,
        "effect lifecycle order: {events:?}"
    );
}
