//! PTY-002 durable lease state: restart recovery, crash-safe writes, fencing
//! after reload, execution identity and `Running | Exited | Unknown`.
use rightkit_qa_driver::lease::{
    ExecutionIdentity, ExecutionState, FileLeaseStore, InputError, InputFailure, InputLease,
    InputOutcome, InputStatus, LeaseError, LeaseRecord, LeaseStore, MemoryLeaseStore, StoreError,
    UnacknowledgedInputDecision,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "rkc-lease-{tag}-{}-{}",
        std::process::id(),
        ExecutionIdentity::generate()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn file_store(dir: &std::path::Path) -> Arc<FileLeaseStore> {
    Arc::new(FileLeaseStore::new(dir.join("lease.state")))
}

fn ok(_: ()) -> Result<(), InputFailure> {
    Ok(())
}

/// Persist, drop, reload: epoch monotonic, identity durable, Running becomes
/// Unknown, and input unacknowledged at the "crash" comes back Unknown.
#[test]
fn restart_recovery_keeps_epoch_and_surfaces_unknown_input() {
    let dir = temp_dir("restart");
    let store = file_store(&dir);
    let (identity, crash_epoch) = {
        let lease = InputLease::open("pty:1", store.clone()).unwrap();
        assert!(lease.is_durable());
        assert_eq!(lease.execution_state(), ExecutionState::Running);
        let g = lease.acquire().unwrap();
        assert!(lease.submit(g.epoch, 0, || ok(())).is_ok());
        assert!(lease.submit(g.epoch, 1, || ok(())).is_ok());
        // Seq 2: the executor may have delivered, then the host "crashes".
        let r = lease.submit(g.epoch, 2, || -> Result<(), _> {
            Err(InputFailure::Ambiguous("host died mid-write".into()))
        });
        assert!(matches!(
            r,
            Err(InputError::Ambiguous {
                unacknowledged: (2, 3),
                ..
            })
        ));
        (lease.execution_identity(), g.epoch)
        // dropped without release: simulated crash
    };

    let lease = InputLease::open("pty:1", store.clone()).unwrap();
    let snap = lease.snapshot();
    assert!(
        snap.epoch > crash_epoch,
        "epoch never goes backwards and pre-crash holders are fenced"
    );
    assert!(!snap.attached, "the pre-crash holder did not survive");
    assert_eq!(
        lease.execution_identity(),
        identity,
        "execution identity is durable"
    );
    assert_eq!(
        lease.execution_state(),
        ExecutionState::Unknown,
        "restart never fabricates Running"
    );
    assert_eq!(lease.unknown_input(), Some((2, 3)));
    assert_eq!(lease.input_status(1), InputStatus::Acknowledged);
    assert_eq!(lease.input_status(2), InputStatus::Unknown);
    assert_eq!(lease.input_status(3), InputStatus::Unsent);
    assert_eq!(store.load().unwrap().unwrap().unknown_input, Some((2, 3)));

    // Unknown input blocks a plain acquire and new input until resolved.
    assert_eq!(
        lease.acquire(),
        Err(LeaseError::UnacknowledgedInput { from: 2, to: 3 })
    );

    // A second restart before resolution keeps it Unknown and the epoch rising.
    drop(lease);
    let lease = InputLease::open("pty:1", store.clone()).unwrap();
    assert!(lease.snapshot().epoch > snap.epoch);
    assert_eq!(lease.unknown_input(), Some((2, 3)));

    // Explicit resolution (CodeRight acknowledge_input) at the current epoch.
    let epoch = lease.snapshot().epoch;
    assert_eq!(
        lease.acknowledge_input(epoch, 3),
        Err(LeaseError::UnsentSequence {
            sequence: 3,
            next: 3
        })
    );
    let after = lease.acknowledge_input(epoch, 2).unwrap();
    assert_eq!(after.unacknowledged_input, None);
    assert_eq!(lease.unknown_input(), None);
    assert_eq!(
        lease.acknowledge_input(epoch, 0),
        Err(LeaseError::NonMonotonicAck {
            sequence: 0,
            acked: 3
        })
    );
    assert!(
        lease.acknowledge_input(epoch, 2).is_ok(),
        "re-ack of the last sequence is idempotent"
    );

    // Execution state must be resolved explicitly before new input.
    let g = lease.acquire().unwrap();
    assert_eq!(
        lease.submit(g.epoch, 3, || ok(())).unwrap_err(),
        InputError::Lease(LeaseError::ExecutionNotRunning {
            state: ExecutionState::Unknown
        })
    );
    lease.set_execution_state(ExecutionState::Running).unwrap();
    assert!(matches!(
        lease.submit(g.epoch, 3, || ok(())),
        Ok(InputOutcome::Applied { .. })
    ));

    // Exited is terminal and survives restart as Exited.
    lease.set_execution_state(ExecutionState::Exited).unwrap();
    drop(lease);
    let lease = InputLease::open("pty:1", store).unwrap();
    assert_eq!(lease.execution_state(), ExecutionState::Exited);
    assert!(matches!(
        lease.set_execution_state(ExecutionState::Running),
        Err(LeaseError::InvalidExecutionTransition { .. })
    ));
    let _ = std::fs::remove_dir_all(dir);
}

/// `ReconcileAsLost` after restart rolls the cursor back so the input is resent
/// under the same, now unambiguous, sequence.
#[test]
fn restart_unknown_input_reconciled_as_lost() {
    let store = Arc::new(MemoryLeaseStore::new());
    {
        let lease = InputLease::open("w", store.clone()).unwrap();
        let g = lease.acquire().unwrap();
        let _ = lease.submit(g.epoch, 0, || -> Result<(), _> {
            Err(InputFailure::Ambiguous("crash".into()))
        });
    }
    let lease = InputLease::open("w", store.clone()).unwrap();
    assert_eq!(lease.unknown_input(), Some((0, 1)));
    let g = lease.acquire_reconciling(UnacknowledgedInputDecision::ReconcileAsLost);
    assert_eq!(g.next_input_sequence, 0);
    assert_eq!(lease.unknown_input(), None);
    let rec = store.record().unwrap();
    assert_eq!(
        (
            rec.next_input_sequence,
            rec.acked_input_sequence,
            rec.unknown_input
        ),
        (0, 0, None)
    );
    assert_eq!(rec.holder_epoch, Some(g.epoch));
}

/// A stale holder from before the restart can neither inject, ack, nor release.
#[test]
fn stale_holder_is_fenced_after_reload() {
    let dir = temp_dir("fence");
    let store = file_store(&dir);
    let old = {
        let lease = InputLease::open("pty:2", store.clone()).unwrap();
        let g = lease.acquire().unwrap();
        lease.submit(g.epoch, 0, || ok(())).unwrap();
        g
    };
    let lease = InputLease::open("pty:2", store.clone()).unwrap();
    let ran = AtomicUsize::new(0);
    let run = || {
        ran.fetch_add(1, Ordering::SeqCst);
        Ok::<(), InputFailure>(())
    };
    // Detached after recovery: nobody holds it.
    assert_eq!(
        lease.submit(old.epoch, 1, run).unwrap_err(),
        InputError::Lease(LeaseError::NotAttached)
    );
    assert!(matches!(
        lease.acknowledge_input(old.epoch, 0),
        Err(LeaseError::StaleEpoch { .. })
    ));
    let fresh = lease.acquire().unwrap();
    assert!(
        fresh.epoch > old.epoch + 1,
        "recovery fence plus the new grant"
    );
    assert!(matches!(
        lease.submit(old.epoch, 1, run),
        Err(InputError::Lease(LeaseError::StaleEpoch { .. }))
    ));
    assert!(matches!(
        lease.release(old.epoch),
        Err(LeaseError::StaleEpoch { .. })
    ));
    assert_eq!(ran.load(Ordering::SeqCst), 0, "stale holder never executed");
    // Execution state came back Unknown; resolve, then the new holder works.
    lease.set_execution_state(ExecutionState::Running).unwrap();
    lease.submit(fresh.epoch, 1, run).unwrap();
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    // The duplicate of pre-crash acknowledged input is deduplicated, not re-run.
    assert!(matches!(
        lease.submit(fresh.epoch, 0, run),
        Ok(InputOutcome::Duplicate { .. })
    ));
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// A torn temp file (crash before rename) never corrupts the committed state;
/// a torn committed file fails closed instead of resetting to epoch 0.
#[test]
fn torn_write_never_corrupts_state() {
    let dir = temp_dir("torn");
    let store = file_store(&dir);
    let committed = {
        let lease = InputLease::open("pty:3", store.clone()).unwrap();
        let g = lease.acquire().unwrap();
        lease.submit(g.epoch, 0, || ok(())).unwrap();
        lease.record()
    };
    // Crash mid-save: a partial temp file, never renamed.
    let full = std::fs::read_to_string(store.path()).unwrap();
    std::fs::write(store.temp_path(), &full[..full.len() / 3]).unwrap();
    assert_eq!(
        store.load().unwrap().unwrap(),
        committed,
        "temp file is ignored"
    );
    let lease = InputLease::open("pty:3", store.clone()).unwrap();
    assert_eq!(lease.snapshot().epoch, committed.epoch + 1);
    assert_eq!(lease.snapshot().acked_input_sequence, 1);
    // The next save overwrites the torn temp file and commits atomically.
    lease.acquire().unwrap();
    assert!(
        !store.temp_path().exists(),
        "temp file was renamed into place"
    );
    drop(lease);

    // Garbage or truncation in the committed file itself: fail closed.
    let good = std::fs::read_to_string(store.path()).unwrap();
    for bad in [&good[..good.len() - 5], "garbage\n", ""] {
        std::fs::write(store.path(), bad).unwrap();
        assert!(
            matches!(store.load(), Err(StoreError::Corrupt(_))),
            "{bad:?}"
        );
        assert!(matches!(
            InputLease::open("pty:3", store.clone()),
            Err(LeaseError::Store { .. })
        ));
    }
    let tampered = good.replace("epoch=", "epoch=9");
    std::fs::write(store.path(), tampered).unwrap();
    assert!(
        matches!(store.load(), Err(StoreError::Corrupt(_))),
        "checksum catches edits"
    );
    std::fs::write(store.path(), &good).unwrap();
    assert!(InputLease::open("pty:3", store.clone()).is_ok());
    // A record for another lease is refused.
    assert!(matches!(
        InputLease::open("other", store),
        Err(LeaseError::Store { .. })
    ));
    let _ = std::fs::remove_dir_all(dir);
}

/// Store that can be made to refuse saves.
#[derive(Default)]
struct FlakyStore {
    inner: MemoryLeaseStore,
    fail: AtomicBool,
}
impl LeaseStore for FlakyStore {
    fn load(&self) -> Result<Option<LeaseRecord>, StoreError> {
        self.inner.load()
    }
    fn save(&self, r: &LeaseRecord) -> Result<(), StoreError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(StoreError::Io("disk full".into()));
        }
        self.inner.save(r)
    }
}

/// An epoch is never granted, and input never executes, unless the change is
/// durable first.
#[test]
fn store_failure_grants_nothing_and_executes_nothing() {
    let store = Arc::new(FlakyStore::default());
    let lease = InputLease::open("p", store.clone()).unwrap();
    let g = lease.acquire().unwrap();
    store.fail.store(true, Ordering::SeqCst);
    assert!(matches!(lease.acquire(), Err(LeaseError::Store { .. })));
    assert!(matches!(
        lease.try_acquire_reconciling(UnacknowledgedInputDecision::ReconcileAsLost),
        Err(LeaseError::Store { .. })
    ));
    assert_eq!(
        lease.snapshot().epoch,
        g.epoch,
        "a refused save changes nothing"
    );
    let ran = AtomicUsize::new(0);
    let r = lease.submit(g.epoch, 0, || {
        ran.fetch_add(1, Ordering::SeqCst);
        Ok::<(), InputFailure>(())
    });
    assert!(matches!(r, Err(InputError::NotDelivered { .. })));
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert_eq!(lease.snapshot().next_input_sequence, 0, "sequence released");
    store.fail.store(false, Ordering::SeqCst);
    lease.submit(g.epoch, 0, || ok(())).unwrap();
    assert_eq!(store.inner.record().unwrap().acked_input_sequence, 1);
}

/// In-memory remains the default and keeps its behaviour (Running, no store).
#[test]
fn in_memory_default_is_unchanged() {
    let lease = InputLease::new("mem");
    assert!(!lease.is_durable());
    assert_eq!(lease.execution_state(), ExecutionState::Running);
    let g = lease.acquire().unwrap();
    lease.submit(g.epoch, 0, || ok(())).unwrap();
    assert_eq!(lease.record().holder_epoch, Some(g.epoch));
    assert_eq!(lease.unknown_input(), None);
    // Ambiguous input in a live run is Unacknowledged, not Unknown.
    let _ = lease.submit(g.epoch, 1, || -> Result<(), _> {
        Err(InputFailure::Ambiguous("x".into()))
    });
    assert_eq!(lease.input_status(1), InputStatus::Unacknowledged);
    lease.acknowledge_input(g.epoch, 1).unwrap();
    assert_eq!(lease.input_status(1), InputStatus::Acknowledged);
}

/// A caller-chosen identity is kept; a mismatching one is refused on reload.
#[test]
fn caller_identity_is_bound_to_the_record() {
    let store = Arc::new(MemoryLeaseStore::new());
    let id = ExecutionIdentity::new("job-7").unwrap();
    let lease = InputLease::open_with_identity("j", store.clone(), id.clone()).unwrap();
    assert_eq!(lease.execution_identity(), id);
    drop(lease);
    assert!(InputLease::open_with_identity("j", store.clone(), id.clone()).is_ok());
    let other = ExecutionIdentity::new("job-8").unwrap();
    assert!(matches!(
        InputLease::open_with_identity("j", store, other),
        Err(LeaseError::Store { .. })
    ));
}
