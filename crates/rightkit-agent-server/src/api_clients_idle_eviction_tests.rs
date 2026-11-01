// RA-003: real snapshot producer + production sweep, driven from live
// `DaemonState` rather than the synthetic fixtures `daemon/residency.rs` and
// `daemon/idle_eviction.rs` use for the pure policy functions.
use super::*;
use crate::daemon::clients::{SessionId as SupervisorSessionId, WorkerId, WorkerOwner};
use crate::daemon::residency::WorkerLifecycle;

fn worker_for(session_id: &str) -> WorkerOwner {
    WorkerOwner {
        worker_id: WorkerId(session_id.to_string()),
        client_id: None,
        session_id: SupervisorSessionId(session_id.to_string()),
    }
}

#[test]
fn parse_iso_to_millis_round_trips_now_iso_format() {
    let stamp = "2024-01-01T00:00:00Z";
    assert_eq!(parse_iso_to_millis(stamp), Some(1_704_067_200_000));
    assert_eq!(parse_iso_to_millis("garbage"), None);
    assert_eq!(parse_iso_to_millis(""), None);
}

#[test]
fn parse_iso_to_millis_honours_offsets_and_rejects_impossible_dates() {
    // 01:00 at +01:00 is midnight UTC; a fraction does not change the whole second.
    assert_eq!(
        parse_iso_to_millis("2024-01-01T01:00:00+01:00"),
        Some(1_704_067_200_000)
    );
    assert_eq!(
        parse_iso_to_millis("2024-01-01T00:00:00.750Z"),
        Some(1_704_067_200_000)
    );
    // Leap day exists in 2024 only.
    assert_eq!(
        parse_iso_to_millis("2024-02-29T00:00:00Z"),
        Some(1_709_164_800_000)
    );
    assert_eq!(parse_iso_to_millis("2023-02-29T00:00:00Z"), None);
    // Before the epoch is not a usable activity time.
    assert_eq!(parse_iso_to_millis("1969-12-31T23:59:59Z"), None);
}

#[tokio::test]
async fn snapshot_of_missing_root_session_is_protected_not_ready() {
    let state = DaemonState::new("");
    let worker = worker_for("does-not-exist");
    let snapshot = build_worker_residency_snapshot(&state, &worker);
    assert_eq!(snapshot.lifecycle, WorkerLifecycle::Failed);
    assert!(snapshot.sessions.is_empty());
    assert!(!can_evict_worker_for_test(&snapshot));
}

// Local re-export so this test file does not need `daemon::residency::can_evict_worker`
// imported twice under two names in `super::*`.
fn can_evict_worker_for_test(snapshot: &WorkerResidencySnapshot) -> bool {
    crate::daemon::residency::can_evict_worker(
        snapshot,
        IdleEvictionMinutes::minutes(5),
        now_millis(),
    )
}

#[tokio::test]
async fn real_attached_session_is_never_evicted() {
    let state = DaemonState::new("");
    let _session = state.create_session("root-attached", None, "", "root", None);
    supervisor_attach_session("client-1", "root-attached");
    if let Ok(mut supervisor) = client_supervisor().lock() {
        supervisor
            .register_client(ClientId("client-1".into()), ConnectionId("conn-1".into()))
            .unwrap();
        let _ = supervisor.attach_session(
            &ClientId("client-1".into()),
            SupervisorSessionId("root-attached".into()),
        );
        supervisor
            .claim_worker(
                None,
                SupervisorSessionId("root-attached".into()),
                WorkerId("root-attached".into()),
            )
            .unwrap();
    }

    let worker = worker_for("root-attached");
    let snapshot = build_worker_residency_snapshot(&state, &worker);
    assert_eq!(snapshot.sessions.len(), 1);
    assert_eq!(snapshot.sessions[0].attached_clients, 1);
    assert!(!can_evict_worker_for_test(&snapshot));

    let report = run_daemon_idle_eviction_sweep(&state, IdleEvictionMinutes::minutes(5));
    assert_eq!(report.evicted_workers, 0);
    assert!(
        lock_recover(&state.sessions, "sessions").contains_key("root-attached"),
        "an attached session must survive a sweep"
    );
}

#[tokio::test]
async fn genuinely_idle_session_is_evicted() {
    let state = DaemonState::new("");
    let session = state.create_session("root-idle", None, "", "root", None);
    // Force the persisted-format timestamp far enough into the past to clear
    // any idle threshold under test.
    *lock_recover(&session.updated_at, "test.updated_at") = "2000-01-01T00:00:00Z".to_string();

    if let Ok(mut supervisor) = client_supervisor().lock() {
        supervisor
            .claim_worker(
                None,
                SupervisorSessionId("root-idle".into()),
                WorkerId("root-idle".into()),
            )
            .unwrap();
    }

    let report = run_daemon_idle_eviction_sweep(&state, IdleEvictionMinutes::minutes(5));
    assert_eq!(report.evicted_workers, 1, "{report:?}");
    assert!(
        !lock_recover(&state.sessions, "sessions").contains_key("root-idle"),
        "a genuinely idle, unattached session must be evicted"
    );
}

#[tokio::test]
async fn restart_replay_yields_identical_decision() {
    let state = DaemonState::new("");
    let session = state.create_session("root-replay", None, "", "root", None);
    *lock_recover(&session.updated_at, "test.updated_at") = "2000-01-01T00:00:00Z".to_string();
    let worker = worker_for("root-replay");

    let now_ms = now_millis();
    let first = build_worker_residency_snapshot(&state, &worker);
    let second = build_worker_residency_snapshot(&state, &worker);
    assert_eq!(
        first, second,
        "rebuilding the snapshot must be deterministic"
    );
    assert_eq!(
        can_evict_worker_for_test(&first),
        crate::daemon::residency::can_evict_worker(
            &second,
            IdleEvictionMinutes::minutes(5),
            now_ms
        ),
        "the eviction decision must be identical across a replayed snapshot build"
    );
}
