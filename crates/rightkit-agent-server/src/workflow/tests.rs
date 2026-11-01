//! Cross-module integration tests for the workflow harness (WP-03c).
//!
//! These tests exercise the **public** surface of `crate::workflow` — the
//! re-exports declared in [`super::mod`] — together with the durable storage
//! layer (`rightkit_agent_engine_storage`) and the pure workflow domain
//! (`rightkit_workflow::control_plane::prelude`). They do **not** reach into the
//! `supervisor` / `restart` / `dispatcher` / `executor` submodules directly;
//! every claim, release, recovery, and lifecycle outcome is observed through
//! `RunSupervisor` / `RecoveryWorker` so the test suite survives internal
//! refactors as long as the public API stays stable.
//!
//! Run with:
//! ```text
//! cargo test -p coderight --bin coderight workflow::tests
//! ```
//!
//! Acceptance target (WP-03c):
//! 1. `claim_then_release_round_trip` — claim a queued run, release it as
//!    succeeded, observe a single terminal journal event, an absent lease
//!    row, and a terminal `succeeded` projection.
//! 2. `feature_disabled_makes_run_once_return_disabled` — a supervisor built
//!    behind `FeatureGate::from_bool(false)` never dispatches a run; the
//!    outcome is `Disabled` (or `Empty`, whichever the harness exposes).
//! 3. `recovery_replays_journal_to_same_lifecycle_state` — replay a seeded
//!    4-event journal through the pure reducer and through
//!    `RecoveryWorker::rehydrate`; the resulting `LifecycleState`s must be
//!    equal.
//! 4. `run_once_skips_runs_with_unexpired_lease` — once a run is leased,
//!    a subsequent `run_once` must not pick it up.
//!
//! NOTE: test #3 references `RecoveryWorker` / `RecoveryOutcome` from
//! `crate::workflow::restart` (re-exported via `super`). That module is
//! owned by a parallel WP-03 sub-task; until `restart.rs` is in place this
//! file will not compile. Tests #1, #2, and #4 are independent of restart
//! and validate the supervisor-only surface.

#![cfg(test)]

use super::*;
use crate::api::SharedStorage;
// `Storage`, `Clock`, `FeatureGate`, and `Arc` are already in scope from
// mod.rs's private `use` statements and would collide with explicit
// re-imports. The `pub use` re-exports in mod.rs also bring in
// `RunSupervisor`, `ClaimOutcome`, `RunOnceOutcome`, `SupervisorLeaseTtl`,
// `SupervisorError`, `RecoveryWorker`, `RecoveryOutcome`,
// `NodeDispatcher`, `NodeExecutor`, etc. — all reachable through `super::*`.
use rightkit_agent_engine_storage::{
    JournalAppend, ProjectionUpdate, Storage, WorkflowJournalRow, WorkflowJournalWriter,
    WorkflowRepository,
};
// `LifecycleState` / `LifecyclePhase` and the `workflow_id` /
// `workflow_version_id` / `run_id` helpers live in `rightkit_workflow::control_plane::domain`
// but are **not** re-exported by `prelude`.
use rightkit_workflow::control_plane::domain::{
    run_id as domain_run_id, workflow_id as domain_workflow_id,
    workflow_version_id as domain_workflow_version_id, LifecyclePhase, LifecycleState,
};
// Import only the prelude items that `super::*` does not already bring in
// (it does not re-export the prelude wholesale — `Clock` / `FeatureGate`
// reach this module via mod.rs's private `use`, and the rest of the prelude
// is reached by these explicit names). `Revision`, `TestClock`,
// `ReducerOutcome`, and `reduce` are needed for the recovery replay test
// and the typed release failure path (`FailureClass` lives in the prelude
// and is reached transitively through `SupervisorError`, but the test
// surface here only ever passes `None` for the terminal failure).
use rightkit_workflow::control_plane::prelude::{reduce, ReducerOutcome, Revision, TestClock};
use rightkit_workflow::control_plane::reducer::Command;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Seed one `workflow_runs` row directly through the writer connection. The
/// supervisor never seeds its own runs — admission is owned by the trigger /
/// occurrence layer (WP-06) — so the integration test must plant the row
/// itself. A claimed fixture is already running so strict reducer authority
/// permits its terminal completion.
fn seed_queued_run(conn: &rightkit_db::rusqlite::Connection, run_id: &str) {
    conn.execute(
        "INSERT INTO workflow_runs(
            id, workflow_id, workflow_version_id,
            lifecycle_phase, lifecycle_revision,
            initiated_by_kind, input_json, started_at, runs_root_durable
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rightkit_db::rusqlite::params![
            run_id,
            "wfd-test",
            "wfv-test",
            "running",
            0_i64,
            "manual",
            "{}",
            "2026-08-07T00:00:00Z",
            1_i64,
        ],
    )
    .expect("seed workflow_runs row");
}

/// Seed one `workflow_versions` row. `claim_run` reads the compiled plan
/// out of this row before returning a `ClaimOutcome`, so any test that
/// exercises the claim path needs it present. The plan JSON is hand-rolled
/// so the test does not pull in `CompiledPlan` / `CapabilitySet` just to
/// build a serialized fixture.
fn seed_version(conn: &rightkit_db::rusqlite::Connection, version_id: &str) {
    let plan_json = r#"{"entry_node":"n1","nodes":[],"effective_capabilities":[]}"#;
    conn.execute(
        "INSERT INTO workflow_versions(
            id, workflow_id, version_number, content_hash,
            definition_json, compiled_plan_json, publication_state,
            created_at, published_at, created_by, notes
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rightkit_db::rusqlite::params![
            version_id,
            "wfd-test",
            1_i64,
            "sha256:deadbeef",
            "{}",
            plan_json,
            "published",
            "2026-08-07T00:00:00Z",
            Some("2026-08-07T00:00:00Z"),
            "system",
            "",
        ],
    )
    .expect("seed workflow_versions row");
}

/// Append `n` journal events for `run_id` through the public
/// `WorkflowJournalWriter`. Each event advances the run's
/// `lifecycle_revision` by one (the writer auto-increments when the
/// projection omits `run_lifecycle_revision`) and leaves the lifecycle
/// phase as whatever the seeded row had — the journal writer only applies
/// caller-supplied phases.
fn append_journal_events(conn: &rightkit_db::rusqlite::Connection, run_id: &str, n: usize) {
    let writer = WorkflowJournalWriter::new(conn);
    let kinds = ["admit", "start", "pause", "resume"];
    for index in 0..n {
        writer
            .append_with_projection(
                &JournalAppend {
                    run_id,
                    event_kind: kinds[index % kinds.len()],
                    node_id: None,
                    payload_json: "{}",
                    created_at: "2026-08-07T00:00:01Z",
                },
                &ProjectionUpdate {
                    run_id,
                    // No phase change: lets the reducer replay produce a
                    // deterministic state that matches the recovery worker.
                    ..Default::default()
                },
            )
            .expect("append journal event");
    }
}

/// Replay a journal of storage rows through the **pure** reducer and return
/// the resulting `LifecycleState`. Used as the reference implementation
/// against which `RecoveryWorker::rehydrate` must agree (test #3).
fn replay_through_reducer(journal: &[WorkflowJournalRow]) -> LifecycleState {
    let mut state = LifecycleState {
        workflow_id: domain_workflow_id("wfd-test"),
        version_id: domain_workflow_version_id("wfv-test"),
        revision: Revision::ZERO,
        phase: LifecyclePhase::Pending,
    };

    for ev in journal {
        // The journal writer assigns the *post*-event revision to the row,
        // so the command we feed the reducer must claim the *pre*-event
        // revision as `expected_revision`.
        let expected = Revision(ev.revision.saturating_sub(1).max(0) as u64);
        let cmd = Command {
            command_id: format!("cmd-{}", ev.seq),
            kind: ev.event_kind.clone(),
            run_id: domain_run_id(&ev.run_id),
            expected_revision: expected,
            payload_json: ev.payload_json.clone(),
        };
        match reduce(&state, &cmd) {
            ReducerOutcome::Applied { next_state, .. } => state = *next_state,
            // Tests must not produce a journal that the reducer rejects;
            // a rejected event would mean the harness is shipping
            // un-replayable history.
            other => panic!(
                "reducer rejected replay for seq={} kind={}: {:?}",
                ev.seq, ev.event_kind, other
            ),
        }
    }
    state
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// Claim a queued run, release it as succeeded, and observe the supervisor's
/// durable contract: a monotonic lease fence, a terminal projection, an
/// empty lease row, and one terminal journal event with `run_succeeded`.
/// `claim_run` itself does NOT write a journal event — that is the sole
/// responsibility of `release_run`, so the journal length after claim+release
/// is exactly 1, not 2.
#[tokio::test(flavor = "current_thread")]
async fn claim_then_release_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let storage = SharedStorage::new(Storage::open_in_memory()?);
    let storage_guard = storage.lock().await;
    let conn = storage_guard.conn();
    seed_version(conn, "wfv-test");
    seed_queued_run(conn, "wfr_claim");
    drop(storage_guard);

    let clock: Arc<dyn Clock> = Arc::new(TestClock::new(1_000_000));
    let gate = FeatureGate::from_bool(true);
    let supervisor = RunSupervisor::new(storage.clone(), gate, clock);

    // ── claim ────────────────────────────────────────────────────────────
    let claim = supervisor.claim_run("wfr_claim", "test-holder", 30).await?;
    let run_id = claim.run_id.clone();
    assert_eq!(run_id, "wfr_claim");
    assert!(
        claim.lease_fence >= 1,
        "claim must assign a monotonic fence"
    );
    assert_eq!(
        claim.lifecycle_snapshot.phase,
        LifecyclePhase::Running,
        "claim reads the seeded run as Running",
    );

    let repo_guard = storage.lock().await;
    let repo = WorkflowRepository::new(repo_guard.conn());
    let lease = repo
        .find_run_lease(&run_id)?
        .ok_or("lease row must exist immediately after claim_run")?;
    assert_eq!(
        lease.run_id, run_id,
        "lease must be bound to the claimed run"
    );
    assert_eq!(
        lease.lease_fence, claim.lease_fence,
        "lease fence row must match the ClaimOutcome lease_fence",
    );
    assert_eq!(
        lease.lease_holder, "test-holder",
        "lease holder must echo the holder argument",
    );

    drop(repo_guard);

    // ── release (succeeded) ──────────────────────────────────────────────
    supervisor
        .release_run("wfr_claim", "test-holder", None)
        .await?;

    let repo_guard = storage.lock().await;
    let repo = WorkflowRepository::new(repo_guard.conn());
    let journal = repo.replay_journal(&run_id)?;
    assert_eq!(
        journal.len(),
        1,
        "release_run(succeeded) writes exactly one terminal journal event (got {})",
        journal.len(),
    );
    assert_eq!(
        journal[0].event_kind, "run_succeeded",
        "release_run(None) must produce a 'run_succeeded' terminal event",
    );

    assert!(
        repo.find_run_lease(&run_id)?.is_none(),
        "release_run(succeeded) must clear the lease row",
    );

    let run = repo
        .find_run(&run_id)?
        .ok_or("workflow_runs row must remain after release")?;
    assert_eq!(
        run.lifecycle_phase, "succeeded",
        "release_run(succeeded) must mark the run terminal-succeeded",
    );
    assert!(
        run.terminal_at.is_some(),
        "release_run must stamp terminal_at on the run projection",
    );
    assert!(
        run.terminal_failure_class.is_none(),
        "release_run(succeeded) must not record a failure class",
    );

    Ok(())
}

/// A supervisor built behind a disabled feature gate must short-circuit
/// `run_once` before any SQL runs. The outcome is the documented
/// `RunOnceOutcome::Disabled` (or `Empty`, which the harness also treats
/// as a non-action); the journal and lease tables must be untouched.
#[tokio::test(flavor = "current_thread")]
async fn feature_disabled_makes_run_once_return_disabled() -> Result<(), Box<dyn std::error::Error>>
{
    let storage = SharedStorage::new(Storage::open_in_memory()?);
    // Seed even though the gate is off: the test verifies that the gate
    // short-circuits BEFORE any run-state lookup, so the seeded row is
    // expected to remain unclaimed.
    let storage_guard = storage.lock().await;
    let conn = storage_guard.conn();
    seed_version(conn, "wfv-test");
    seed_queued_run(conn, "wfr_gated");
    drop(storage_guard);

    let clock: Arc<dyn Clock> = Arc::new(TestClock::new(0));
    let gate = FeatureGate::from_bool(false);
    let supervisor = RunSupervisor::new(storage.clone(), gate, clock);

    let outcome = supervisor
        .run_once(SupervisorLeaseTtl {
            initial_secs: 30,
            renewal_secs: 15,
        })
        .await?;
    assert!(
        matches!(outcome, RunOnceOutcome::Disabled | RunOnceOutcome::Empty),
        "disabled feature gate must not dispatch; got {:?}",
        outcome,
    );

    // And the journal + lease must remain untouched — the gate's whole
    // purpose is "never move state when disabled".
    let repo_guard = storage.lock().await;
    let repo = WorkflowRepository::new(repo_guard.conn());
    let journal = repo.replay_journal("wfr_gated")?;
    assert!(
        journal.is_empty(),
        "disabled run_once must not append journal events (got {} events)",
        journal.len(),
    );
    assert!(
        repo.find_run_lease("wfr_gated")?.is_none(),
        "disabled run_once must not install a lease row",
    );

    Ok(())
}

/// WP-03 invariant #3: the recovery path is identical to the live path.
/// Both must walk the deterministic reducer over the same journal and end
/// at the same `LifecycleState`. This test seeds a 4-event journal, runs
/// the pure reducer replay (the reference implementation), then asks the
/// public `RecoveryWorker::rehydrate` to produce its own state, and asserts
/// equality.
#[tokio::test(flavor = "current_thread")]
async fn recovery_replays_journal_to_same_lifecycle_state() -> Result<(), Box<dyn std::error::Error>>
{
    let storage = SharedStorage::new(Storage::open_in_memory()?);
    let storage_guard = storage.lock().await;
    let conn = storage_guard.conn();
    let run_id = "wfr_recovery";
    seed_version(conn, "wfv-test");
    seed_queued_run(conn, run_id);

    // Seed exactly four events so the journal length is fixed.
    append_journal_events(conn, run_id, 4);

    let repo = WorkflowRepository::new(conn);
    let journal_rows = repo.replay_journal(run_id)?;
    assert_eq!(journal_rows.len(), 4, "test fixture must seed 4 events");
    drop(storage_guard);

    // Reference: rebuild the state by replaying through the pure reducer.
    let expected_state = replay_through_reducer(&journal_rows);

    let worker = RecoveryWorker::new(
        storage.clone(),
        Arc::new(rightkit_workflow::control_plane::prelude::TestClock::new(
            1_700_000_000_000,
        )),
    );
    let outcome = worker.replay_single(run_id).await?;
    let actual_state = match outcome {
        RecoveryOutcome::Replayed { state } => state,
        other => {
            return Err(format!("expected RecoveryOutcome::Replayed; got {:?}", other).into());
        }
    };

    assert_eq!(
        actual_state, expected_state,
        "RecoveryWorker::rehydrate must agree with the reducer replay (WP-03 invariant #3)",
    );
    assert_eq!(
        actual_state.revision,
        Revision(journal_rows.len() as u64),
        "final revision must equal the number of journal events replayed",
    );

    Ok(())
}

/// Once a supervisor has claimed a run, a subsequent `run_once` on the
/// same supervisor must not re-dispatch the run — the unexpired lease
/// keeps it out of the eligibility filter, and there are no other queued
/// runs to claim. The harness returns `Empty` (or `Disabled`, depending
/// on configuration) and the lease row must remain live across calls.
#[tokio::test(flavor = "current_thread")]
async fn run_once_skips_runs_with_unexpired_lease() -> Result<(), Box<dyn std::error::Error>> {
    let storage = SharedStorage::new(Storage::open_in_memory()?);
    let storage_guard = storage.lock().await;
    let conn = storage_guard.conn();
    seed_version(conn, "wfv-test");
    seed_queued_run(conn, "wfr_lease");
    drop(storage_guard);

    let clock: Arc<dyn Clock> = Arc::new(TestClock::new(0));
    let gate = FeatureGate::from_bool(true);
    let supervisor = RunSupervisor::new(storage.clone(), gate, clock);

    // First `run_once` claims the queued run; that installs a lease whose
    // expiry is in the future relative to the seeded clock.
    let first = supervisor
        .run_once(SupervisorLeaseTtl {
            initial_secs: 30,
            renewal_secs: 15,
        })
        .await?;
    match first {
        RunOnceOutcome::Claimed { run_ids } => {
            assert_eq!(
                run_ids,
                vec!["wfr_lease".to_string()],
                "first run_once must claim the seeded queued run exactly once",
            );
        }
        other => {
            return Err(format!(
                "first run_once should claim the seeded queued run; got {:?}",
                other,
            )
            .into());
        }
    }

    // Second `run_once` must not re-dispatch the same run: the lease is
    // unexpired, the queue has no other eligible runs, so the harness
    // returns an empty outcome instead of double-claiming.
    let second = supervisor
        .run_once(SupervisorLeaseTtl {
            initial_secs: 30,
            renewal_secs: 15,
        })
        .await?;
    assert!(
        matches!(second, RunOnceOutcome::Empty | RunOnceOutcome::Disabled),
        "second run_once must skip the leased run; got {:?}",
        second,
    );

    // And the lease row must still be live — the second call didn't
    // accidentally release the first.
    let repo_guard = storage.lock().await;
    let repo = WorkflowRepository::new(repo_guard.conn());
    let lease = repo
        .find_run_lease("wfr_lease")?
        .ok_or("lease row must survive the second run_once call")?;
    assert_eq!(lease.run_id, "wfr_lease");
    assert!(
        lease.lease_fence >= 1,
        "lease fence must remain positive after the second run_once",
    );

    Ok(())
}
