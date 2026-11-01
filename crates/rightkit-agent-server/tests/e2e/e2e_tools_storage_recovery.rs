//! Storage journeys through a REAL `coderight serve` daemon that is hard-killed (no shutdown
//! hooks) and restarted on the same database: sessions, durable event history, model-visible
//! conversation and file effects must all come back, and the session must keep working - both
//! after a settled turn and after a kill in the middle of a running tool call.
//! Maps to canon EVT-001/003/004 (durable truth); EVT-005/006 are qualified by the dedicated
//! `evt005_*` / `evt006_*` crash drivers. Retires the `coderight-storage` session / events /
//! migrations / tool-invocation unit tests and the `private_fs` atomic-write ones.

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use scripted_provider::Step;
use serde_json::{json, Value};

fn model_saw(req: &Value) -> String {
    req["messages"].to_string()
}

#[test]
fn killed_daemon_restarts_with_history_transcript_and_file_effects_intact() {
    let mut rig = Rig::start(vec![
        Step::tool(
            "c1",
            "write_file",
            json!({"path":"durable.txt","content":"persist-me\n"}),
        ),
        Step::text("wrote durable"),
        Step::text("second answer"),
    ]);
    let created = rig.session("s1", json!({"permissionMode":"bypass"}));
    let exec_root = std::path::PathBuf::from(created["workspaceRoot"].as_str().unwrap());
    let (status, _) = rig.send("s1", "please write durable.txt");
    assert_eq!(status, 200);
    rig.wait_assistant("s1", "wrote durable");

    assert_eq!(
        std::fs::read_to_string(exec_root.join("durable.txt")).unwrap(),
        "persist-me\n"
    );
    let before = rig.kinds("s1");
    for needed in [
        "userMessageSubmitted",
        "toolCallStarted",
        "toolResultAdded",
        "sessionTurnFinished",
    ] {
        assert!(
            before.iter().any(|k| k == needed),
            "{needed} missing from {before:?}"
        );
    }

    rig.restart();

    // Durable history is byte-for-byte the same prefix after a hard kill.
    let after = rig.kinds("s1");
    assert_eq!(
        &after[..before.len()],
        &before[..],
        "history must survive the kill unchanged"
    );
    let (status, transcript) = rig.get("/sessions/s1/transcript");
    assert_eq!(status, 200);
    let transcript = transcript.to_string();
    assert!(
        transcript.contains("please write durable.txt"),
        "{transcript}"
    );
    assert!(transcript.contains("wrote durable"), "{transcript}");
    assert_eq!(
        std::fs::read_to_string(exec_root.join("durable.txt")).unwrap(),
        "persist-me\n"
    );

    // The session keeps working and the model is shown the pre-crash conversation.
    let (status, _) = rig.send("s1", "and now say something else");
    assert_eq!(status, 200);
    rig.wait_assistant("s1", "second answer");
    let reqs = rig.provider.requests();
    let saw = model_saw(reqs.last().unwrap());
    assert!(saw.contains("please write durable.txt"), "{saw}");
    assert!(
        saw.contains("Created '"),
        "tool result must be replayed to the model: {saw}"
    );
    assert!(saw.contains("and now say something else"), "{saw}");
}

#[test]
fn kill_during_a_running_tool_call_recovers_a_usable_session() {
    let mut rig = Rig::start(vec![
        Step::tool("c1", "shell", json!({"command":"sleep 25"})),
        Step::text("recovered answer"),
        Step::text("recovered answer"),
    ]);
    rig.session("s2", json!({"permissionMode":"bypass"}));
    let (status, _) = rig.send("s2", "run something slow");
    assert_eq!(status, 200);
    rig.wait_event("s2", "toolCallStarted");
    // Let the child actually start before the daemon is killed under it.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let before = rig.kinds("s2");
    assert!(before.iter().any(|k| k == "toolCallStarted"), "{before:?}");
    assert!(
        !before.iter().any(|k| k == "toolResultAdded"),
        "the slow tool must still be running when the daemon dies: {before:?}"
    );

    rig.restart();

    let (status, session) = rig.get("/sessions/s2/transcript");
    assert_eq!(status, 200, "{session}");
    let after = rig.kinds("s2");
    assert_eq!(
        &after[..before.len()],
        &before[..],
        "pre-crash history must be preserved"
    );
    assert!(
        after.iter().any(|k| k == "userMessageSubmitted"),
        "{after:?}"
    );

    let (status, _) = rig.send("s2", "are you there");
    assert_eq!(status, 200);
    rig.wait_assistant("s2", "recovered answer");
}
