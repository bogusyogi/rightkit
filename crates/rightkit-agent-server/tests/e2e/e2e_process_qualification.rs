//! Real ServiceHost process-history qualification across a hard daemon restart.
//!
//! Existing `e2e_api_fs_process` owns same-epoch detach/reattach and stale-lease
//! coverage. This suite adds restart reconciliation using the same real daemon,
//! API, terminal shell, and workspace-backed process snapshot store.

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use serde_json::json;

fn process_query(root: &str) -> String {
    let encoded = root
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect::<String>();
    format!("/execution/processes?workspaceRoot={encoded}")
}

#[test]
fn service_process_snapshot_survives_hard_daemon_restart_as_unavailable_history() {
    let mut rig = Rig::start(vec![]);
    rig.session("restart-process", json!({}));

    let (status, created) = rig.post(
        "/execution/processes",
        json!({"sessionId": "restart-process", "persistence": "service"}),
    );
    assert_eq!(status, 200, "{created}");
    let initial = created["process"].clone();
    let id = initial["id"].as_str().expect("process id").to_owned();
    let first_epoch = initial["leaseEpoch"].as_u64().expect("lease epoch");

    // Finish the real shell before terminating its daemon, so this test leaves
    // no unmanaged child process behind while still exercising durable history.
    let (status, written) = rig.post(
        &format!("/execution/processes/{id}/stdin"),
        json!({
            "input": "exit",
            "appendNewline": true,
            "leaseEpoch": first_epoch,
            "inputSequence": initial["nextInputSequence"],
        }),
    );
    assert_eq!(status, 200, "{written}");
    rig.wait("terminal shell exit", 20, || {
        let (status, row) = rig.get(&format!("/execution/processes/{id}"));
        (status == 200 && row["process"]["status"] == "exited").then_some(())
    });

    let workspace_root = rig.ws().to_string_lossy().to_string();
    rig.restart();

    // The restarted real ServiceHost reads the workspace snapshot. Since the
    // OS process handle belonged to the killed daemon, controls must be fenced.
    let (status, recovered) = rig.get(&process_query(&workspace_root));
    assert_eq!(status, 200, "{recovered}");
    let row = recovered["processes"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == id))
        .cloned()
        .expect("historical process row survives restart");
    assert_eq!(row["id"], id);
    assert_eq!(row["persistence"], "service");
    assert_eq!(row["live"], false);
    assert_eq!(row["status"], "unknown");
    assert_eq!(row["controlMode"], "historicalUnavailable");
    assert!(row["controlUnavailableReason"].as_str().is_some());
    assert_eq!(row["leaseEpoch"], first_epoch);

    let (status, detail) = rig.get(&format!(
        "/execution/processes/{id}?workspaceRoot={}",
        workspace_root
            .replace('\\', "/")
            .replace(':', "%3A")
            .replace('/', "%2F")
    ));
    assert_eq!(status, 200, "{detail}");
    assert_eq!(detail["process"]["controlMode"], "historicalUnavailable");
    let (status, denied) = rig.post(
        &format!(
            "/execution/processes/{id}/kill?workspaceRoot={}",
            workspace_root
                .replace('\\', "/")
                .replace(':', "%3A")
                .replace('/', "%2F")
        ),
        json!({}),
    );
    assert_eq!(status, 409, "{denied}");
    assert!(
        denied["error"]
            .as_str()
            .is_some_and(|error| error.contains("cannot be killed")),
        "{denied}"
    );
}
