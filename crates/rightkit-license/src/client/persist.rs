//! Persistence of the licence envelope: outcomes, readback, reconcile.
//!
//! ## The write
//! A record write (activate, refresh, lease, import, and the deactivation TOMBSTONE) is the
//! credentials rotation pair, never a bare `set`: `begin_rotation` (the new envelope is written
//! create-only as the marker), then `commit_rotation_of` (primary set to exactly that envelope,
//! marker removed). A crash between the two leaves either the old primary or the new one,
//! never a mix, and a marker that recovery can see. There is no delete anywhere on this path, so
//! recovery can never resurrect a deactivated licence: deactivation is a write of a tombstone.
//!
//! ## Fencing (v3.2 option A)
//! The whole sequence runs on a fenced worker that owns the licence operation lock
//! ([`run_fenced`]) and, after the credentials calls return, waits until the credentials
//! rotation fence reports that no abandoned child call of either item is still running
//! ([`settle`]). The lock is released only then. A caller whose deadline passed gets
//! [`Written::TimedOut`]; the worker finishes (and reconciles) on its own.
//!
//! ## Outcomes
//! Every write ends as one of [`PersistOutcome`]. A credentials call can fail AFTER it wrote
//! (the primary is set, the marker cleanup fails): that is `CleanupPending`, never a rollback. A
//! backend timeout is `Indeterminate`. Anything not clean is resolved by reading back under the
//! same lock ([`resolve`]) before the worker returns; what cannot be resolved stays visible as
//! a marker or as the in-process unresolved flag, and authorization fails closed on either.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rightkit_credentials::{Error as CredError, Secret, Store};

use super::envelope::{parse_secret, Envelope, RecordState};
use super::lock::{run_fenced, Fenced, OpGuard};
use super::order::{self, Level};
use super::view::LicenseError;

/// How often the worker re-checks the credentials fence for a still-running child call.
const SETTLE_POLL: Duration = Duration::from_millis(50);

/// What a stored item holds.
pub(crate) enum Loaded {
    Absent,
    Present(Box<Envelope>),
    /// The item is there but is not a record this crate understands.
    Corrupt,
}

/// A read of the licence item together with its fence.
pub(crate) struct Snapshot {
    pub loaded: Loaded,
    /// A rotation marker exists: a write began and was not resolved.
    pub marker: bool,
    /// A credentials call (possibly an abandoned one) still holds the rotation fence.
    pub held: bool,
}

impl Snapshot {
    /// Authorization must refuse: a write may be in flight or half done.
    pub(crate) fn unresolved(&self) -> bool {
        self.marker || self.held
    }
}

/// How one save ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PersistOutcome {
    /// The primary holds the new envelope and the marker is gone.
    Committed,
    /// The primary holds the new envelope; removing the marker failed. Not a rollback.
    CleanupPending,
    /// Nothing was written (a clean refusal, or the readback proved our begin never landed).
    NotCommitted(Refusal),
    /// A backend call timed out or failed in a way that does not say whether it wrote, and the
    /// readback could not settle it.
    Indeterminate,
}

/// Why nothing was committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// A marker already existed: an earlier write is unresolved.
    MarkerPresent,
    /// The credential store refused before writing (guard, unavailable, in progress, ...).
    Store,
    /// Our marker never landed and nothing is in flight.
    Lost,
}

/// What a reconcile found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reconciled {
    /// No marker: nothing to do.
    Clean,
    /// A marker was resolved (rolled forward, or discarded where rolling forward could resurrect
    /// a tombstone).
    Resolved,
    /// Still unresolved (a child call is running, or the store failed).
    Pending,
}

/// The result of handing a write to the fenced worker.
pub(crate) enum Written<R> {
    /// The worker finished inside the caller's wait; the caller holds the lock again.
    Done(R, OpGuard),
    /// The caller's wait passed. The worker keeps the lock until it has finished; the write is
    /// unresolved from the caller's side.
    TimedOut,
    /// The worker could not run.
    Failed,
}

#[derive(Clone)]
pub(crate) struct RecordStore {
    store: Store,
    account: String,
    wait: Duration,
    unresolved: Arc<AtomicBool>,
}

impl RecordStore {
    pub(crate) fn new(store: Store, account: String, wait: Duration) -> Self {
        Self {
            store,
            account,
            wait,
            unresolved: Arc::new(AtomicBool::new(false)),
        }
    }

    /// An earlier write in this process did not settle.
    pub(crate) fn flagged_unresolved(&self) -> bool {
        self.unresolved.load(Ordering::SeqCst)
    }

    pub(crate) fn flag_unresolved(&self, value: bool) {
        self.unresolved.store(value, Ordering::SeqCst);
    }

    /// Read the item and its fence. The caller holds the operation lock.
    pub(crate) fn read(&self) -> Result<Snapshot, LicenseError> {
        // The fence is read first: with it idle and no marker, the primary below is stable.
        let fence = self
            .store
            .rotation_status(&self.account)
            .map_err(|_| LicenseError::Store)?;
        let loaded = match self.store.get(&self.account) {
            Ok(None) => Loaded::Absent,
            Ok(Some(secret)) if secret.is_empty() => Loaded::Absent,
            Ok(Some(secret)) => match parse_secret(&secret) {
                Ok(envelope) => Loaded::Present(Box::new(envelope)),
                Err(_) => Loaded::Corrupt,
            },
            Err(_) => return Err(LicenseError::Store),
        };
        Ok(Snapshot {
            loaded,
            marker: fence.pending,
            held: fence.held,
        })
    }

    /// Write `envelope` on a fenced worker that owns `guard` for the whole sequence.
    pub(crate) fn write(&self, guard: OpGuard, envelope: &Envelope) -> Written<PersistOutcome> {
        let secret = envelope.encode();
        let revision = envelope.revision.clone().unwrap_or_default();
        let (store, account) = (self.store.clone(), self.account.clone());
        let fenced = run_fenced(guard, self.wait, move || {
            let outcome = persist(&store, &account, &secret, &revision);
            settle(&store, &account);
            outcome
        });
        match fenced {
            Fenced::Done(outcome, guard) => {
                match outcome {
                    PersistOutcome::Committed => self.flag_unresolved(false),
                    PersistOutcome::CleanupPending | PersistOutcome::Indeterminate => {
                        self.flag_unresolved(true)
                    }
                    PersistOutcome::NotCommitted(_) => {}
                }
                Written::Done(outcome, guard)
            }
            Fenced::TimedOut => {
                self.flag_unresolved(true);
                Written::TimedOut
            }
            Fenced::Failed => {
                self.flag_unresolved(true);
                Written::Failed
            }
        }
    }

    /// Resolve a leftover marker by readback, on a fenced worker.
    pub(crate) fn reconcile(&self, guard: OpGuard, wait: Duration) -> Written<Reconciled> {
        let (store, account) = (self.store.clone(), self.account.clone());
        let fenced = run_fenced(guard, wait, move || {
            let result = reconcile_job(&store, &account);
            settle(&store, &account);
            result
        });
        match fenced {
            Fenced::Done(result, guard) => {
                if result != Reconciled::Pending {
                    self.flag_unresolved(false);
                }
                Written::Done(result, guard)
            }
            Fenced::TimedOut => Written::TimedOut,
            Fenced::Failed => Written::Failed,
        }
    }

    /// Run any other credentials mutation (a legacy item delete) on a fenced worker.
    pub(crate) fn fenced<R: Send + 'static>(
        &self,
        guard: OpGuard,
        job: impl FnOnce() -> R + Send + 'static,
    ) -> Written<R> {
        match run_fenced(guard, self.wait, job) {
            Fenced::Done(result, guard) => Written::Done(result, guard),
            Fenced::TimedOut => Written::TimedOut,
            Fenced::Failed => Written::Failed,
        }
    }
}

/// Wait until no credentials call (abandoned or not) of the licence item or its marker is
/// still running. This is what keeps the operation lock held for a timed-out child worker.
/// Returns `false` only when the fence cannot be probed at all.
pub(crate) fn settle(store: &Store, account: &str) -> bool {
    loop {
        match store.rotation_status(account) {
            Ok(status) if !status.held => return true,
            Ok(_) => std::thread::sleep(SETTLE_POLL),
            Err(_) => return false,
        }
    }
}

/// An error that says nothing was written.
fn refused_cleanly(error: &CredError) -> bool {
    matches!(
        error,
        CredError::Guarded { .. }
            | CredError::Unavailable(_)
            | CredError::InteractionRequired
            | CredError::Denied
            | CredError::Invalid { .. }
            | CredError::InProgress { .. }
    )
}

fn revision_of(secret: Option<&Secret>) -> Option<String> {
    secret
        .and_then(|secret| parse_secret(secret).ok())
        .and_then(|envelope| envelope.revision)
}

/// begin, commit, and a readback for anything that is not clean. Runs on the fenced worker.
fn persist(store: &Store, account: &str, new: &Secret, revision: &str) -> PersistOutcome {
    let _level = order::enter(Level::Rotation);
    match store.begin_rotation(account, new) {
        Ok(true) => {}
        Ok(false) => return PersistOutcome::NotCommitted(Refusal::MarkerPresent),
        Err(error) if refused_cleanly(&error) => {
            return PersistOutcome::NotCommitted(Refusal::Store)
        }
        Err(_) => return resolve(store, account, new, revision),
    }
    match store.commit_rotation_of(account, new) {
        Ok(_) => PersistOutcome::Committed,
        Err(_) => resolve(store, account, new, revision),
    }
}

/// Settle what a failed or timed-out call left, by reading back.
fn resolve(store: &Store, account: &str, new: &Secret, revision: &str) -> PersistOutcome {
    resolve_step(store, account, new, revision, true)
}

fn resolve_step(
    store: &Store,
    account: &str,
    new: &Secret,
    revision: &str,
    may_retry: bool,
) -> PersistOutcome {
    // A timed-out credentials call keeps running on its own thread; nothing it left is final
    // until it has returned.
    if !settle(store, account) {
        return PersistOutcome::Indeterminate;
    }
    let (Ok(primary), Ok(marker)) = (store.get(account), store.pending_rotation(account)) else {
        return PersistOutcome::Indeterminate;
    };
    let primary_is_new = revision_of(primary.as_ref()).as_deref() == Some(revision);
    let marker_is_new = revision_of(marker.as_ref()).as_deref() == Some(revision);
    match (primary_is_new, marker_is_new) {
        (true, false) => {
            if marker.is_some() {
                // Our value is the primary, but a marker of another write sits beside it.
                PersistOutcome::CleanupPending
            } else {
                PersistOutcome::Committed
            }
        }
        (true, true) => match store.commit_rotation_of(account, new) {
            Ok(_) => PersistOutcome::Committed,
            Err(_) => PersistOutcome::CleanupPending,
        },
        (false, true) => match store.commit_rotation_of(account, new) {
            Ok(_) => PersistOutcome::Committed,
            // The call may still be running: only a readback after it ends can say more.
            Err(CredError::Timeout { .. }) => PersistOutcome::Indeterminate,
            // It failed: read back once more (it may have written before failing).
            Err(_) if may_retry => resolve_step(store, account, new, revision, false),
            // The primary write keeps failing and nothing is running: take the begun value
            // back, so the previous verified record stands and the caller sees a store error.
            Err(_) => match store.abort_rotation_of(account, new) {
                Ok(_) => PersistOutcome::NotCommitted(Refusal::Store),
                Err(_) => PersistOutcome::Indeterminate,
            },
        },
        (false, false) if marker.is_some() => PersistOutcome::Indeterminate,
        (false, false) => PersistOutcome::NotCommitted(Refusal::Lost),
    }
}

/// Resolve whatever marker is pending.
///
/// - A tombstone marker is rolled forward: completing a deactivation never grants anything.
/// - An active marker is rolled forward unless the primary is a tombstone of another revision:
///   an activation that began after a deactivation and never finished is discarded, so
///   recovery can never resurrect a tombstone.
/// - A marker that is not a readable envelope is discarded and the primary is left alone.
fn reconcile_job(store: &Store, account: &str) -> Reconciled {
    let _level = order::enter(Level::Rotation);
    if !settle(store, account) {
        return Reconciled::Pending;
    }
    let marker = match store.pending_rotation(account) {
        Ok(Some(marker)) => marker,
        Ok(None) => return Reconciled::Clean,
        Err(_) => return Reconciled::Pending,
    };
    let Ok(primary) = store.get(account) else {
        return Reconciled::Pending;
    };
    let finished = match parse_secret(&marker) {
        Err(_) => store.abort_rotation_of(account, &marker),
        Ok(pending) => {
            let primary = primary
                .as_ref()
                .and_then(|secret| parse_secret(secret).ok());
            let supersedes_tombstone = pending.state == RecordState::Active
                && primary
                    .as_ref()
                    .is_some_and(|p| p.is_tombstone() && p.revision != pending.revision);
            if supersedes_tombstone {
                store.abort_rotation_of(account, &marker)
            } else {
                store.commit_rotation_of(account, &marker)
            }
        }
    };
    match finished {
        Ok(_) => match store.pending_rotation(account) {
            Ok(None) => Reconciled::Resolved,
            _ => Reconciled::Pending,
        },
        Err(_) => Reconciled::Pending,
    }
}

/// Map a settled save to the error an operation returns, `Ok` when the new envelope is the
/// stored one.
pub(crate) fn outcome_result(outcome: PersistOutcome) -> Result<(), LicenseError> {
    match outcome {
        PersistOutcome::Committed => Ok(()),
        // The new envelope IS the stored one; the unresolved flag stays up until the marker
        // goes, and authorization refuses meanwhile.
        PersistOutcome::CleanupPending => Ok(()),
        PersistOutcome::NotCommitted(Refusal::MarkerPresent) => Err(LicenseError::Unresolved),
        PersistOutcome::NotCommitted(_) => Err(LicenseError::Store),
        PersistOutcome::Indeterminate => Err(LicenseError::Unresolved),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistence_outcomes_map_to_operation_results() {
        assert_eq!(outcome_result(PersistOutcome::Committed), Ok(()));
        // The new envelope is the stored one; the unresolved flag keeps authorization closed.
        assert_eq!(outcome_result(PersistOutcome::CleanupPending), Ok(()));
        assert_eq!(
            outcome_result(PersistOutcome::NotCommitted(Refusal::MarkerPresent)),
            Err(LicenseError::Unresolved)
        );
        assert_eq!(
            outcome_result(PersistOutcome::NotCommitted(Refusal::Store)),
            Err(LicenseError::Store)
        );
        assert_eq!(
            outcome_result(PersistOutcome::NotCommitted(Refusal::Lost)),
            Err(LicenseError::Store)
        );
        assert_eq!(
            outcome_result(PersistOutcome::Indeterminate),
            Err(LicenseError::Unresolved)
        );
    }

    #[test]
    fn only_clean_refusals_say_nothing_was_written() {
        for clean in [
            CredError::Guarded { guard: "g".into() },
            CredError::Unavailable("x".into()),
            CredError::InteractionRequired,
            CredError::Denied,
            CredError::InProgress { op: "set" },
            CredError::Invalid {
                what: "x",
                reason: "y".into(),
            },
        ] {
            assert!(refused_cleanly(&clean), "{clean:?}");
        }
        for unclear in [
            CredError::Timeout { op: "set", secs: 1 },
            CredError::Backend { op: "set", code: 5 },
            CredError::NotUtf8,
            CredError::Io("x".into()),
        ] {
            assert!(!refused_cleanly(&unclear), "{unclear:?}");
        }
    }

    #[test]
    fn a_snapshot_with_a_marker_or_a_held_fence_is_unresolved() {
        let snap = |marker, held| Snapshot {
            loaded: Loaded::Absent,
            marker,
            held,
        };
        assert!(!snap(false, false).unresolved());
        assert!(snap(true, false).unresolved());
        assert!(snap(false, true).unresolved());
    }
}
