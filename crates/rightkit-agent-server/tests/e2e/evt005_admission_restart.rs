//! EVT-005 qualification: real-process crash-and-restart drivers for the
//! `after_admission_before_dispatch` and `after_dispatch_fence` fault points
//! in `engine/bins/coderight/src/api_sessions/messages.rs`.
//!
//! These drivers exercise the real `coderight serve` binary as a child
//! process against a persistent on-disk data root reused across restarts,
//! and an independently observable local HTTP provider fixture (never
//! CodeRight's own self-reported count). The daemon is pointed at the
//! fixture via the real production provider-route config surface
//! (`<app root>/config.toml`, `rightkit_app_settings::agent_config::ProviderRouteConfig`) —
//! no source change needed. Durable state is inspected after each restart by
//! independently reopening the SQLite file with
//! `rightkit_agent_engine_storage::Storage::open` — never through the (possibly
//! crashed) daemon's own handle.

use crate::support::daemon_process;
use crate::support::mock_provider;

use daemon_process::{
    post_ignoring_transport_failure, post_json, start_daemon, wait_for_exit,
    write_local_provider_route_config, TestDaemon,
};
use mock_provider::MockProvider;
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;

const EVT005_FAULT_ENV: &str = "RIGHTKIT_QUAL_EVT005_FAULT";
// Must name an embedded-catalog slug: the CRA-10 capacity gate resolves the
// request model against loaded model metadata before dispatch. The fixture
// ignores the request body; the slug only feeds CodeRight's admission path.
const QUAL_MODEL: &str = "deepseek/deepseek-v4-pro";

/// Create a session pinned to the fixture-backed model, with an explicit
/// non-default title so the daemon's background auto-title generator (which
/// also calls the configured provider) never fires and confounds the
/// dispatch count this driver observes.
fn provision_session(daemon: &TestDaemon, workspace: &std::path::Path, session_id: &str) {
    let (status, body) = post_json(
        &daemon.url(),
        "/sessions",
        &daemon.startup.token,
        &json!({
            "id": session_id,
            "workspaceRoot": workspace.to_string_lossy(),
            "model": QUAL_MODEL,
            "title": "EVT-005 fixture session",
        }),
    );
    assert_eq!(status, 200, "create fixture-routed session: {body}");
}

fn open_storage(db_path: &str) -> rightkit_agent_engine_storage::Storage {
    rightkit_agent_engine_storage::Storage::open(db_path)
        .expect("independently reopen the durable db")
}

fn count_user_message_submitted(
    storage: &rightkit_agent_engine_storage::Storage,
    session_id: &str,
    nonce: &str,
) -> usize {
    storage
        .all_events(session_id)
        .expect("read durable events for session")
        .into_iter()
        .filter(|row| row.kind == "userMessageSubmitted" && row.payload_json.contains(nonce))
        .count()
}

/// Case A: crash at `after_admission_before_dispatch`. On restart, an
/// identical retry must adopt the already-admitted turn rather than
/// duplicate it, and dispatch to the provider exactly once overall.
#[test]
fn evt005_case_a_admission_before_dispatch_survives_restart_without_duplicate_dispatch() {
    let home = TempDir::new().expect("temp data root");
    let workspace = TempDir::new().expect("temp workspace");
    let provider = MockProvider::start();
    write_local_provider_route_config(home.path(), &provider.base_url(), QUAL_MODEL);
    let nonce = "evt005-case-a-nonce";
    let text = "evt005 case A stable body";
    let session_id = "evt005-case-a";

    // --- First run: fault right after admission is durably committed. ---
    let mut daemon = start_daemon(
        home.path(),
        &[(EVT005_FAULT_ENV, "after_admission_before_dispatch")],
    );
    provision_session(&daemon, workspace.path(), session_id);

    post_ignoring_transport_failure(
        &daemon.url(),
        &format!("/sessions/{session_id}/messages"),
        &daemon.startup.token,
        &json!({"text": text, "nonce": nonce}),
    );
    let status = wait_for_exit(&mut daemon.child, Duration::from_secs(10));
    assert_eq!(
        status.code(),
        Some(86),
        "daemon must exit with the EVT-005 fault code at after_admission_before_dispatch"
    );

    let db_path = daemon.startup.database_path.clone();
    {
        let storage = open_storage(&db_path);
        let count = count_user_message_submitted(&storage, session_id, nonce);
        assert_eq!(
            count, 1,
            "exactly one durable UserMessageSubmitted must exist for the nonce after the crash"
        );
    }
    assert_eq!(
        provider.dispatches(),
        0,
        "the fault fires before dispatch — the provider must never have been called yet"
    );

    // --- Restart on the SAME data root, WITHOUT the fault var. ---
    let daemon2 = start_daemon(home.path(), &[]);
    let (status, body) = post_json(
        &daemon2.url(),
        &format!("/sessions/{session_id}/messages"),
        &daemon2.startup.token,
        &json!({"text": text, "nonce": nonce}),
    );
    assert_eq!(
        status, 200,
        "identical retry after restart must be accepted: {body}"
    );

    // Give the async dispatch a moment to actually reach the provider fixture.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while provider.dispatches() == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    {
        let storage = open_storage(&db_path);
        let count = count_user_message_submitted(&storage, session_id, nonce);
        assert_eq!(
            count, 1,
            "the retry after restart must NOT allocate a second durable UserMessageSubmitted"
        );
    }
    assert_eq!(
        provider.dispatches(),
        1,
        "exactly one provider dispatch must have occurred, observed independently by the fixture"
    );
}

/// Case B: crash at `after_dispatch_fence` (after
/// `mark_idempotent_request_dispatching` has fenced the request, before any
/// terminal outcome is recorded). On restart, a blind automatic retry of the
/// same nonce must be refused as pending/unknown rather than silently
/// re-dispatched.
#[test]
fn evt005_case_b_dispatch_fence_refuses_blind_replay_after_restart() {
    let home = TempDir::new().expect("temp data root");
    let workspace = TempDir::new().expect("temp workspace");
    let provider = MockProvider::start();
    write_local_provider_route_config(home.path(), &provider.base_url(), QUAL_MODEL);
    let nonce = "evt005-case-b-nonce";
    let text = "evt005 case B stable body";
    let session_id = "evt005-case-b";

    let mut daemon = start_daemon(home.path(), &[(EVT005_FAULT_ENV, "after_dispatch_fence")]);
    provision_session(&daemon, workspace.path(), session_id);

    post_ignoring_transport_failure(
        &daemon.url(),
        &format!("/sessions/{session_id}/messages"),
        &daemon.startup.token,
        &json!({"text": text, "nonce": nonce}),
    );
    let status = wait_for_exit(&mut daemon.child, Duration::from_secs(10));
    assert_eq!(
        status.code(),
        Some(86),
        "daemon must exit with the EVT-005 fault code at after_dispatch_fence"
    );

    let db_path = daemon.startup.database_path.clone();
    assert_eq!(
        provider.dispatches(),
        0,
        "the fence fault fires before any HTTP call reaches the provider fixture"
    );

    // --- Restart on the SAME data root, WITHOUT the fault var. ---
    let daemon2 = start_daemon(home.path(), &[]);
    let (status, body) = post_json(
        &daemon2.url(),
        &format!("/sessions/{session_id}/messages"),
        &daemon2.startup.token,
        &json!({"text": text, "nonce": nonce}),
    );
    assert_eq!(
        status, 409,
        "a blind automatic replay of a dispatch-fenced-but-unresolved nonce must be refused, got: {body}"
    );
    assert_eq!(
        body["code"], "idempotency_outcome_pending",
        "the refusal must surface the pending/unknown machine error, got: {body}"
    );
    // S:P8: consume the shared golden against the actual crash/restart response.
    let golden: serde_json::Value =
        include_str!("../../../../fixtures/machine-contract/session-idempotency.jsonl")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("valid golden"))
            .find(|row| row["case"] == "unknown-outcome")
            .expect("unknown-outcome golden");
    for field in [
        "schemaVersion",
        "code",
        "retryable",
        "effectState",
        "remediation",
    ] {
        assert_eq!(
            body[field], golden["error"][field],
            "golden {field}: {body}"
        );
    }
    for field in ["detail", "correlationId"] {
        assert!(body[field].as_str().is_some_and(|value| !value.is_empty()));
    }
    assert!(
        body.get("remediation").is_some(),
        "full error envelope: {body}"
    );

    // Give any (incorrect) background dispatch a moment to happen, so a
    // regression that dispatches anyway is actually caught.
    std::thread::sleep(Duration::from_millis(500));

    {
        let storage = open_storage(&db_path);
        let count = count_user_message_submitted(&storage, session_id, nonce);
        assert_eq!(
            count, 1,
            "the original admission must remain the only durable UserMessageSubmitted for the nonce"
        );
        let effect_state: String = storage
            .conn()
            .query_row(
                "SELECT effect_state FROM machine_request_idempotency \
             WHERE session_id = ?1 AND operation = 'post_message' AND idempotency_key = ?2",
                rightkit_db::rusqlite::params![session_id, nonce],
                |row| row.get(0),
            )
            .expect("inspect recovered durable request");
        assert_eq!(effect_state, "unknown");
    }
    assert_eq!(
        provider.dispatches(),
        0,
        "the refused replay must never have reached the provider fixture"
    );
}
