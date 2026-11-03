//! The pending-release queue seam and its drain (rightkit#1036).
//!
//! A removal made offline, or interrupted, must still free the server seat later. The app owns the
//! durable queue (it is in OS secure storage because legacy rows carry an activation token) and
//! lends it to the client through [`PendingReleaseQueue`]; the client owns the rule for when a row
//! is done and the serialisation of the queue's read-modify-write.
//!
//! The rule (HeardRight HR-PRO-010): a release the server answers with 2xx is acknowledged, 404 and
//! 410 say the lease or activation is already gone and settle it too, and anything else (another
//! status, no answer) keeps the row queued for the next drain. A lease row carries the device hash
//! and the exact lease id, so a delayed retry cannot revoke a later reactivation; a legacy row
//! carries the activation token. A row is never dropped on a failed attempt.
//!
//! Every call into the queue is made under one lock per client, so an implementation may load,
//! change and store its row set without a lock of its own. A queue implementation must not call
//! back into the client and must not use the network. A panic in a queue call is a failed call.
//! The release itself runs outside that lock, so a drain does not hold up an enqueue; a drain that
//! overlaps a removal may send the same release twice, which the server answers idempotently (a
//! second answer settles with 404 or 410), and an acknowledgement of a row that is already gone
//! must succeed.

use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};

use super::ops::Core;
use super::types::PendingLicenseRelease;
use super::view::LicenseError;
use super::wire::settles;
use super::LicenseClient;

/// The app's name for one queued row. Opaque to the client, which only hands it back to
/// [`PendingReleaseQueue::acknowledge`]. Carries no secret.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PendingReleaseId(String);

impl PendingReleaseId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PendingReleaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PendingReleaseId").field(&self.0).finish()
    }
}

/// One queued release and the id the app gave its row.
pub struct PendingReleaseEntry {
    pub id: PendingReleaseId,
    pub release: PendingLicenseRelease,
}

/// The app's durable queue of releases that still have to reach the server. Failure is `Err(())`:
/// the client records no reason and keeps the state it had.
pub trait PendingReleaseQueue {
    /// Persist `release` and return the id of its row. Called before the local tombstone is
    /// written. A failure stops the removal, and the active record stays.
    fn enqueue(&self, release: &PendingLicenseRelease) -> Result<PendingReleaseId, ()>;

    /// Every row that is still queued, including rows an older build wrote.
    fn pending(&self) -> Result<Vec<PendingReleaseEntry>, ()>;

    /// Delete the row `id`. Succeeds when the row is already gone.
    fn acknowledge(&self, id: &PendingReleaseId) -> Result<(), ()>;
}

/// What one [`LicenseClient::drain_pending_releases`] did. Counts only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrainReport {
    /// Rows the queue listed.
    pub attempted: u32,
    /// Rows the server settled (2xx, 404 or 410) and the queue deleted.
    pub settled: u32,
    /// Rows left queued: no answer, or a status that does not settle.
    pub kept: u32,
    /// Rows the server settled but the queue could not delete; they settle again on the next drain.
    pub acknowledge_failed: u32,
}

impl Core {
    /// One queue call, under the client's queue lock, with a panic turned into a failure.
    pub(crate) fn queue_call<R>(&self, call: impl FnOnce() -> Result<R, ()>) -> Result<R, ()> {
        let _serialised = self.queue_lock();
        catch_unwind(AssertUnwindSafe(call)).map_err(|_| ())?
    }

    /// Send one release and say whether the server settled it.
    pub(crate) fn send_release(&self, release: &PendingLicenseRelease) -> bool {
        settles(match release {
            PendingLicenseRelease::Lease {
                lease_id,
                device_id_hash,
            } => self.transport.lease_remove_status(device_id_hash, lease_id),
            PendingLicenseRelease::Activation(token) => {
                self.transport.deactivate_status(token.expose())
            }
        })
    }

    fn drain(&self, queue: &dyn PendingReleaseQueue) -> Result<DrainReport, LicenseError> {
        let entries = self
            .queue_call(|| queue.pending())
            .map_err(|()| LicenseError::Store)?;
        let mut report = DrainReport::default();
        for PendingReleaseEntry { id, release } in entries {
            report.attempted = report.attempted.saturating_add(1);
            if !self.send_release(&release) {
                report.kept = report.kept.saturating_add(1);
                continue;
            }
            match self.queue_call(|| queue.acknowledge(&id)) {
                Ok(()) => report.settled = report.settled.saturating_add(1),
                Err(()) => report.acknowledge_failed = report.acknowledge_failed.saturating_add(1),
            }
        }
        Ok(report)
    }
}

impl LicenseClient {
    /// Send every row of `queue` to the server and delete the ones it settles (2xx, 404, 410; see
    /// the module docs). Call it on launch, on focus and when the app returns to the foreground.
    /// It never touches the stored licence, so it does not wait for the licence operation lock, and
    /// it never fails because one row did: a row the server did not settle stays queued and is
    /// counted in [`DrainReport::kept`]. `Err` only when the queue could not be listed
    /// ([`LicenseError::Store`]) or the client is disabled ([`LicenseError::Unconfigured`]).
    pub fn drain_pending_releases(
        &self,
        queue: &dyn PendingReleaseQueue,
    ) -> Result<DrainReport, LicenseError> {
        self.configured()?.drain(queue)
    }

    /// [`deactivate_with_pending_release`](Self::deactivate_with_pending_release) over a
    /// [`PendingReleaseQueue`]: the row is persisted before the local tombstone, and when the
    /// immediate release the removal makes is settled (2xx, 404 or 410) the row it was handed is
    /// acknowledged at once, so a removal that reached the server leaves no row behind. When the
    /// release is not settled the row stays for [`drain_pending_releases`](Self::drain_pending_releases).
    /// When the tombstone is definitely not written (a store refusal), the row is deleted again,
    /// because the licence is still active and a later drain must not free its seat; a write whose
    /// outcome is not known keeps the row.
    pub fn deactivate_with_queue(
        &self,
        queue: &dyn PendingReleaseQueue,
    ) -> Result<super::LicenseView, LicenseError> {
        let core = self.configured()?;
        core.signalled(|| core.deactivate_with_queue(queue))
    }
}
