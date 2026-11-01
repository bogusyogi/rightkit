//! EVT-006 qualification: real-process crash-and-recovery driver for the
//! `after_effect_before_settlement` fault point in
//! `engine/crates/engine/src/engine_tool_dispatch_single.rs`.
//!
//! This drives a REAL non-idempotent mutating tool (`write_file`) through
//! the real native model loop — a real child `coderight serve` process, a
//! real independently observable OpenAI-compatible provider fixture that
//! requests the tool call, and a real workspace directory the effect lands
//! in on disk. `RIGHTKIT_QUAL_EVT006_FAULT=after_effect_before_settlement`
//! kills the process (exit 87) after the file write lands but before
//! `ToolResultAdded` is durably recorded. On restart, production recovery
//! (`reconcile_interrupted_effects` in
//! `engine/bins/coderight/src/api_tool_invocations/replay.rs`, invoked from
//! `resume_session_with_ws` in `engine/bins/coderight/src/api_state_impl_05.rs`)
//! must close the gap with a synthetic `interrupted_unknown` settlement,
//! never re-run the mutation, and be idempotent under repeated recovery.

use crate::support::daemon_process;
use crate::support::mock_provider;

use daemon_process::{post_json, start_daemon, wait_for_exit, write_local_provider_route_config};
use mock_provider::MockProvider;
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;

const EVT006_FAULT_ENV: &str = "RIGHTKIT_QUAL_EVT006_FAULT";
const TOOL_CALL_ID: &str = "call_evt006_1";
const EFFECT_FILE_NAME: &str = "evt006-effect.txt";
const EFFECT_MARKER: &str = "evt006-effect-marker";
// Must name an embedded-catalog slug: the CRA-10 capacity gate resolves the
// request model against loaded model metadata before dispatch. The fixture
// ignores the request body; the slug only feeds CodeRight's admission path.
const QUAL_MODEL: &str = "deepseek/deepseek-v4-pro";

fn open_storage(db_path: &str) -> rightkit_agent_engine_storage::Storage {
    rightkit_agent_engine_storage::Storage::open(db_path)
        .expect("independently reopen the durable db")
}

#[test]
fn evt006_effect_lands_and_recovers_as_interrupted_unknown_without_duplicate_mutation() {
    let home = TempDir::new().expect("temp data root");
    let workspace = TempDir::new().expect("temp workspace");
    let arguments_json = json!({
        "path": EFFECT_FILE_NAME,
        "content": EFFECT_MARKER,
    })
    .to_string();
    let provider =
        MockProvider::start_with_one_tool_call(TOOL_CALL_ID, "write_file", &arguments_json);
    write_local_provider_route_config(home.path(), &provider.base_url(), QUAL_MODEL);

    // --- First run: the real native model loop calls write_file for real,
    //     the fault fires right after the effect lands and before
    //     ToolResultAdded is durably recorded. ---
    let mut daemon = start_daemon(
        home.path(),
        &[(EVT006_FAULT_ENV, "after_effect_before_settlement")],
    );

    let (session_id, execution_workspace_root) = create_bypass_session(&daemon, workspace.path());

    // The message need not complete a round trip — the process is expected
    // to die mid-turn once the tool call is dispatched.
    daemon_process_send_best_effort(&daemon, &session_id);

    let status = wait_for_exit(&mut daemon.child, Duration::from_secs(15));
    assert_eq!(
        status.code(),
        Some(87),
        "daemon must exit with the EVT-006 fault code at after_effect_before_settlement"
    );

    let db_path = daemon.startup.database_path.clone();
    // The daemon's default execution backend resolves each session to its
    // own worktree under the requested workspace root (returned as
    // `workspaceRoot` from session creation) — that resolved path, not the
    // raw temp dir, is where the tool's effect actually lands.
    let effect_path = std::path::PathBuf::from(&execution_workspace_root).join(EFFECT_FILE_NAME);
    assert!(
        effect_path.exists(),
        "the real write_file effect must have landed on disk before the crash"
    );
    let content_after_crash =
        std::fs::read_to_string(&effect_path).expect("read the landed effect");
    assert_eq!(content_after_crash, EFFECT_MARKER);

    {
        let storage = open_storage(&db_path);
        let events = storage
            .all_events(&session_id)
            .expect("read durable events for session");
        assert!(
            events
                .iter()
                .any(|row| row.kind == "toolCallStarted" && row.payload_json.contains(TOOL_CALL_ID)),
            "the pre-effect origin must be durably recorded before the crash"
        );
        assert!(
            !events
                .iter()
                .any(|row| row.kind == "toolResultAdded" && row.payload_json.contains(TOOL_CALL_ID)),
            "no terminal ToolResultAdded may exist yet — that is exactly the gap EVT-006 tests"
        );
    }

    // --- Restart on the SAME data root, WITHOUT the fault var. A GET on the
    //     transcript rehydrates straight from storage without resuming the
    //     session; resuming the dormant session (which is what actually runs
    //     `reconcile_interrupted_effects`) happens on the next real access —
    //     posting a message. That is a harmless, distinct benign turn (the
    //     fixture's second-and-later responses are plain text, never another
    //     tool call), so it cannot re-trigger the mutation. ---
    let daemon2 = start_daemon(home.path(), &[]);
    let (status, body) = post_json(
        &daemon2.url(),
        &format!("/sessions/{session_id}/messages"),
        &daemon2.startup.token,
        &json!({"text": "recovery ping 1"}),
    );
    assert_eq!(
        status, 200,
        "resuming the dormant session must reconcile it: {body}"
    );
    // Inspect what the independent provider actually receives after recovery.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let requests = provider.requests();
        let sees_unknown = requests.iter().skip(1).any(|request| {
            request["messages"].as_array().is_some_and(|messages| {
                messages.iter().any(|message| {
                    message["role"] == "tool"
                        && message["tool_call_id"] == TOOL_CALL_ID
                        && message["content"].as_str().is_some_and(|content| {
                            content.contains("interrupted") || content.contains("unknown")
                        })
                })
            })
        });
        if sees_unknown {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "provider did not receive explicit unknown result for recovered tool call: {requests:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    // The effect must still be present, unchanged — recovery must not have
    // re-run the mutation.
    let content_after_recovery =
        std::fs::read_to_string(&effect_path).expect("effect file must still exist after recovery");
    assert_eq!(
        content_after_recovery, EFFECT_MARKER,
        "recovery must not duplicate or alter the already-landed mutation"
    );

    let (settlement_count_1, invocation_state_1, invocation_events_1) = {
        let storage = open_storage(&db_path);
        let events = storage
            .all_events(&session_id)
            .expect("read durable events for session");
        let settlements = events
            .iter()
            .filter(|row| row.kind == "toolResultAdded" && row.payload_json.contains(TOOL_CALL_ID))
            .count();
        let invocation = storage
            .get_tool_invocation(TOOL_CALL_ID)
            .expect("query tool invocation ledger")
            .expect("tool invocation must exist in the ledger");
        let invocation_events = storage
            .list_tool_invocation_events(TOOL_CALL_ID)
            .expect("query tool invocation events")
            .into_iter()
            .filter(|row| row.kind == "effectSettlementInterruptedUnknown")
            .count();
        (settlements, invocation.lifecycle_state, invocation_events)
    };
    assert_eq!(
        settlement_count_1, 1,
        "exactly one synthetic durable settlement must exist for the interrupted tool call"
    );
    assert_eq!(
        invocation_state_1, "interrupted_unknown",
        "the tool invocation lifecycle must be interrupted_unknown after recovery"
    );
    assert_eq!(
        invocation_events_1, 1,
        "exactly one effectSettlementInterruptedUnknown ledger event must exist"
    );

    {
        let storage = open_storage(&db_path);
        let events = storage
            .all_events(&session_id)
            .expect("read durable events");
        let unknown_result = events
            .iter()
            .find(|row| row.kind == "toolResultAdded" && row.payload_json.contains(TOOL_CALL_ID));
        let unknown_result = unknown_result.expect("the synthetic result must exist");
        assert!(
            unknown_result.payload_json.contains("interrupted")
                || unknown_result.payload_json.contains("unknown"),
            "the provider-visible synthetic result must explicitly say the outcome is unknown: {}",
            unknown_result.payload_json
        );
    }

    // --- Run recovery a second time: restart again (so the session goes
    //     dormant again) and resume it once more. Reconciliation must be
    //     idempotent — no second settlement, no second mutation. ---
    drop(daemon2);
    let daemon3 = start_daemon(home.path(), &[]);
    let (status, body) = post_json(
        &daemon3.url(),
        &format!("/sessions/{session_id}/messages"),
        &daemon3.startup.token,
        &json!({"text": "recovery ping 2"}),
    );
    assert_eq!(
        status, 200,
        "second resume must reconcile idempotently: {body}"
    );
    std::thread::sleep(Duration::from_millis(500));

    let content_after_second_recovery =
        std::fs::read_to_string(&effect_path).expect("effect file must still exist");
    assert_eq!(
        content_after_second_recovery, EFFECT_MARKER,
        "a second recovery pass must not re-run or duplicate the mutation"
    );

    let (settlement_count_2, invocation_events_2) = {
        let storage = open_storage(&db_path);
        let events = storage
            .all_events(&session_id)
            .expect("read durable events");
        let settlements = events
            .iter()
            .filter(|row| row.kind == "toolResultAdded" && row.payload_json.contains(TOOL_CALL_ID))
            .count();
        let invocation_events = storage
            .list_tool_invocation_events(TOOL_CALL_ID)
            .expect("query tool invocation events")
            .into_iter()
            .filter(|row| row.kind == "effectSettlementInterruptedUnknown")
            .count();
        (settlements, invocation_events)
    };
    assert_eq!(
        settlement_count_2, 1,
        "recovery must be idempotent: still exactly one durable settlement"
    );
    assert_eq!(
        invocation_events_2, 1,
        "recovery must be idempotent: still exactly one settlement ledger event"
    );
}

/// Returns `(session_id, resolved_execution_workspace_root)`.
fn create_bypass_session(
    daemon: &daemon_process::TestDaemon,
    workspace: &std::path::Path,
) -> (String, String) {
    let session_id = "evt006-effect".to_string();
    let (status, body) = post_json(
        &daemon.url(),
        "/sessions",
        &daemon.startup.token,
        &json!({
            "id": session_id,
            "workspaceRoot": workspace.to_string_lossy(),
            "model": QUAL_MODEL,
            "title": "EVT-006 fixture session",
            // Bypass so the write_file tool call auto-executes instead of
            // pausing for interactive approval — this driver exercises the
            // crash/recovery seam, not the approval UI.
            "permissionMode": "bypass",
        }),
    );
    assert_eq!(
        status, 200,
        "create bypass-mode fixture-routed session: {body}"
    );
    let resolved_workspace_root = body["workspaceRoot"]
        .as_str()
        .expect("session response carries its resolved workspaceRoot")
        .to_string();
    (session_id, resolved_workspace_root)
}

/// Send the user turn that triggers the model loop's tool call. The process
/// is expected to crash mid-turn, so any transport-level error here is
/// expected and ignored.
fn daemon_process_send_best_effort(daemon: &daemon_process::TestDaemon, session_id: &str) {
    daemon_process::post_ignoring_transport_failure(
        &daemon.url(),
        &format!("/sessions/{session_id}/messages"),
        &daemon.startup.token,
        &json!({"text": "please write the marker file"}),
    );
}
