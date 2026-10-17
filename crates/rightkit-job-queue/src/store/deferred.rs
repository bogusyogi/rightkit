//! Write-behind for a store whose database is briefly unavailable.
//!
//! Three kinds of write can be accepted now and stored later, when the database refuses them
//! with a storage error (`db` or `io`: busy, I/O error, disk full):
//!
//! - an **admission** ([`JobStore::admit`]): a new job. The caller gets its job id at once
//!   ([`Admitted::Held`]) and the row is inserted under that id when storage recovers. Until then
//!   there is no row, so no runner can see the job: `get` says `job_not_found` and
//!   [`JobStore::provisional_jobs`] lists it;
//! - an **extension write** ([`JobStore::set_ext_later`]): a checkpoint kept in extension
//!   columns. A later held write for the same job is merged into the earlier one (newer values
//!   win), so only the latest checkpoint is stored;
//! - a **delete** ([`JobStore::delete_later`]): the job stays visible until the delete lands.
//!
//! Held writes are retried in the order they were accepted, after the next successful store
//! write and on [`JobStore::flush_deferred`]; there is no timer. A write that can never apply
//! (the job is gone, a newer attempt superseded the fence, a hook refused it) is dropped and
//! reported by the flush that found out.
//!
//! Unlike a held terminal write (see `store/settle.rs`), these are **memory only**: nothing is
//! journalled, so a crash loses them. That is deliberate. A checkpoint is superseded by the next
//! one, a delete is repeated by the app's next cleanup, and an admission that never reached the
//! store was never acknowledged as a stored job. [`JobStore::shutdown_detailed`] flushes them one
//! last time and lists what is still held. The set is bounded by [`MAX_DEFERRED_OPS`]; past it a
//! write is not held and its error is returned.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{
    is_storage_error, now, DeleteOptions, DeleteReport, JobStore, NewJob, Settlement,
    SubmitOptions, Submitted,
};
use crate::config::Ext;
use crate::error::{JobError, JobResult};

/// Most deferred writes held at once. A write that fails while the set is full is not held: its
/// error is returned.
pub const MAX_DEFERRED_OPS: usize = 256;

/// Retries of one entry that run immediately; later ones wait [`SLOW_RETRY`] between tries.
const FAST_RETRIES: u32 = 3;
const SLOW_RETRY: Duration = Duration::from_secs(1);

fn lk<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// What a held write is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeferredKind {
    /// A new job ([`JobStore::admit`]).
    Admission,
    /// Extension column values ([`JobStore::set_ext_later`]).
    Ext,
    /// A delete ([`JobStore::delete_later`]).
    Delete,
}

/// A job accepted while storage was unavailable. It has a job id but no row yet.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ProvisionalJob {
    pub job_id: String,
    pub request_id: Option<String>,
    pub kind: String,
    pub request: Value,
    /// When it was accepted.
    pub accepted_at: String,
}

/// Result of [`JobStore::admit`].
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Admitted {
    /// The job is stored, exactly as [`JobStore::submit_with`] returns it.
    Stored(Submitted),
    /// Storage refused the insert. The job is held in memory under this id and inserted when
    /// storage recovers. It is not runnable until then.
    Held(ProvisionalJob),
}

/// Result of [`JobStore::delete_later`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DeleteLater {
    /// The job is deleted.
    Deleted(DeleteReport),
    /// Storage refused the delete. It is held and retried; the job stays visible meanwhile.
    Deferred,
}

/// A held write, as listed by [`JobStore::pending_deferred`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeferredOp {
    pub kind: DeferredKind,
    pub job_id: String,
    /// Failed attempts so far.
    pub attempts: u32,
    /// The last storage error (empty before the first retry).
    pub last_error: String,
}

/// What a flush did.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct DeferredFlush {
    /// Held writes stored by this flush.
    pub applied: usize,
    /// Job ids of admissions stored by this flush.
    pub admitted: Vec<String>,
    /// Held writes given up because they can never apply, with the reason.
    pub dropped: Vec<(String, JobError)>,
    /// Held writes still pending after the flush.
    pub pending: usize,
}

#[derive(Clone)]
#[allow(clippy::large_enum_variant)] // Admit carries the full NewJob; the queue is short-lived and private
enum Op {
    Admit {
        job_id: String,
        new: NewJob,
        ext: Ext,
        opts: SubmitOptions,
        accepted_at: String,
    },
    Ext {
        job_id: String,
        fence: Option<i64>,
        ext: Ext,
    },
    Delete {
        job_id: String,
        opts: DeleteOptions,
    },
}

impl Op {
    fn job_id(&self) -> &str {
        match self {
            Op::Admit { job_id, .. } | Op::Ext { job_id, .. } | Op::Delete { job_id, .. } => job_id,
        }
    }
    fn kind(&self) -> DeferredKind {
        match self {
            Op::Admit { .. } => DeferredKind::Admission,
            Op::Ext { .. } => DeferredKind::Ext,
            Op::Delete { .. } => DeferredKind::Delete,
        }
    }
}

struct Held {
    seq: u64,
    op: Op,
    attempts: u32,
    last_error: String,
    last_try: Instant,
}

impl Held {
    fn due(&self, force: bool) -> bool {
        force || self.attempts < FAST_RETRIES || self.last_try.elapsed() >= SLOW_RETRY
    }
}

#[derive(Default)]
pub(super) struct DeferredState {
    ops: Mutex<Vec<Held>>,
    /// `ops.len()`, readable without the lock: the check every write makes.
    len: AtomicUsize,
    next: AtomicU64,
    /// Serialises flushes and every edit of a held write.
    flush: Mutex<()>,
}

impl DeferredState {
    fn sync_len(&self, ops: &[Held]) {
        self.len.store(ops.len(), Ordering::Relaxed);
    }
}

impl JobStore {
    /// [`JobStore::submit_with`] that holds the job when storage refuses the insert.
    ///
    /// The job id is chosen here, so the caller can answer its own caller at once. On a storage
    /// error (`db` or `io`) the job is held in memory and inserted under that id when storage
    /// recovers (after the next successful write, or [`JobStore::flush_deferred`]); the options
    /// (recovery class, single-flight, prune) are applied then. Any other error is returned, as
    /// is a storage error when [`MAX_DEFERRED_OPS`] writes are already held. A repeated
    /// `request_id` of a held job returns the held job again.
    ///
    /// A held job is not in the store: `get` reports `job_not_found`, and no runner sees it.
    /// Run it once it is stored ([`DeferredFlush::admitted`], or [`JobStore::get`] succeeding).
    /// [`JobStore::cancel_provisional`] withdraws one that has not landed.
    pub fn admit(&self, n: &NewJob, ext: &Ext, opts: &SubmitOptions) -> JobResult<Admitted> {
        if let Some(rid) = &n.request_id {
            if let Some(held) = self.provisional_by_request(rid) {
                return Ok(Admitted::Held(held));
            }
        }
        let job_id = uuid::Uuid::new_v4().to_string();
        match self.submit_with_id(n, ext, opts, Some(&job_id)) {
            Ok(done) => Ok(Admitted::Stored(done)),
            Err(e) if is_storage_error(&e) => {
                let accepted_at = now();
                let op = Op::Admit {
                    job_id: job_id.clone(),
                    new: n.clone(),
                    ext: ext.clone(),
                    opts: opts.clone(),
                    accepted_at: accepted_at.clone(),
                };
                if !self.hold(op, &e) {
                    return Err(e);
                }
                Ok(Admitted::Held(ProvisionalJob {
                    job_id,
                    request_id: n.request_id.clone(),
                    kind: n.kind.clone(),
                    request: n.request.clone(),
                    accepted_at,
                }))
            }
            Err(e) => Err(e),
        }
    }

    /// Jobs accepted by [`JobStore::admit`] that are not stored yet, oldest first.
    pub fn provisional_jobs(&self) -> Vec<ProvisionalJob> {
        lk(&self.deferred.ops)
            .iter()
            .filter_map(|h| match &h.op {
                Op::Admit {
                    job_id,
                    new,
                    accepted_at,
                    ..
                } => Some(ProvisionalJob {
                    job_id: job_id.clone(),
                    request_id: new.request_id.clone(),
                    kind: new.kind.clone(),
                    request: new.request.clone(),
                    accepted_at: accepted_at.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    fn provisional_by_request(&self, request_id: &str) -> Option<ProvisionalJob> {
        self.provisional_jobs()
            .into_iter()
            .find(|p| p.request_id.as_deref() == Some(request_id))
    }

    /// Withdraw a job that [`JobStore::admit`] held and storage has not taken yet. `true` when it
    /// was held (it will never be stored); `false` when it is not held, because it was stored
    /// already or never existed: use [`JobStore::cancel`] or [`JobStore::delete`] then.
    pub fn cancel_provisional(&self, job_id: &str) -> bool {
        let _guard = lk(&self.deferred.flush);
        let mut ops = lk(&self.deferred.ops);
        let before = ops.len();
        ops.retain(|h| !(matches!(h.op, Op::Admit { .. }) && h.op.job_id() == job_id));
        self.deferred.sync_len(&ops);
        ops.len() != before
    }

    /// [`JobStore::set_ext`] / [`JobStore::set_ext_fenced`] (`fence` is the attempt generation,
    /// `None` for unfenced) that holds the write when storage refuses it. `Written` carries the
    /// stored row; `Deferred` says the values are held and will be stored later, merged with any
    /// later held values for the job. While values are held for a job, a new write for it is
    /// merged into them rather than stored ahead of them. Errors other than storage errors are
    /// returned.
    pub fn set_ext_later(
        &self,
        job_id: &str,
        fence: Option<i64>,
        ext: &Ext,
    ) -> JobResult<Settlement> {
        let guard = lk(&self.deferred.flush);
        if self.merge_held_ext(job_id, fence, ext) {
            let _ = self.flush_locked(&guard, true);
            drop(guard);
            return Ok(if self.has_held(job_id, DeferredKind::Ext) {
                Settlement::Deferred
            } else {
                Settlement::Written(self.record(job_id).ok())
            });
        }
        drop(guard);
        match self.set_ext_in(job_id, fence, ext) {
            Ok(rec) => Ok(Settlement::Written(Some(rec))),
            Err(e) if is_storage_error(&e) => {
                let op = Op::Ext {
                    job_id: job_id.to_string(),
                    fence,
                    ext: ext.clone(),
                };
                if self.hold(op, &e) {
                    Ok(Settlement::Deferred)
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Merge `ext` into the held extension write of `job_id`, if there is one. The caller holds
    /// the flush lock.
    fn merge_held_ext(&self, job_id: &str, fence: Option<i64>, ext: &Ext) -> bool {
        let mut ops = lk(&self.deferred.ops);
        for h in ops.iter_mut() {
            if let Op::Ext {
                job_id: id,
                fence: held_fence,
                ext: held,
            } = &mut h.op
            {
                if id.as_str() == job_id {
                    for (k, v) in ext {
                        held.insert(k.clone(), v.clone());
                    }
                    if fence.is_some() {
                        *held_fence = fence;
                    }
                    return true;
                }
            }
        }
        false
    }

    /// [`JobStore::delete_with`] that holds the delete when storage refuses it. The job stays
    /// visible until the delete lands; a repeated call while one is held is `Deferred` again.
    /// Errors other than storage errors are returned.
    pub fn delete_later(&self, job_id: &str, opts: &DeleteOptions) -> JobResult<DeleteLater> {
        if self.has_held(job_id, DeferredKind::Delete) {
            return Ok(DeleteLater::Deferred);
        }
        match self.delete_with(job_id, opts) {
            Ok(report) => Ok(DeleteLater::Deleted(report)),
            Err(e) if is_storage_error(&e) => {
                let op = Op::Delete {
                    job_id: job_id.to_string(),
                    opts: *opts,
                };
                if self.hold(op, &e) {
                    Ok(DeleteLater::Deferred)
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }

    fn has_held(&self, job_id: &str, kind: DeferredKind) -> bool {
        lk(&self.deferred.ops)
            .iter()
            .any(|h| h.op.kind() == kind && h.op.job_id() == job_id)
    }

    /// Hold `op` after storage refused it with `error`. `false` when the set is full.
    fn hold(&self, op: Op, error: &JobError) -> bool {
        let _guard = lk(&self.deferred.flush);
        let mut ops = lk(&self.deferred.ops);
        if ops.len() >= MAX_DEFERRED_OPS {
            return false;
        }
        ops.push(Held {
            seq: self.deferred.next.fetch_add(1, Ordering::Relaxed),
            op,
            attempts: 0,
            last_error: error.message.clone(),
            last_try: Instant::now(),
        });
        self.deferred.sync_len(&ops);
        true
    }

    /// The writes held for a later retry, oldest first. Empty when nothing is held.
    pub fn pending_deferred(&self) -> Vec<DeferredOp> {
        lk(&self.deferred.ops)
            .iter()
            .map(|h| DeferredOp {
                kind: h.op.kind(),
                job_id: h.op.job_id().to_string(),
                attempts: h.attempts,
                last_error: h.last_error.clone(),
            })
            .collect()
    }

    /// Try every held write now, in the order it was accepted, however recently it was tried.
    /// Stops at the first storage error (the rest would meet the same one). Safe to call at any
    /// time and from any thread.
    pub fn flush_deferred(&self) -> DeferredFlush {
        let guard = lk(&self.deferred.flush);
        self.flush_locked(&guard, true)
    }

    /// Run after a store write completed: retry the held writes that are due. Cheap when nothing
    /// is held. Never called with the connection locked.
    pub(super) fn after_write_deferred(&self) {
        if self.deferred.len.load(Ordering::Relaxed) == 0 {
            return;
        }
        let guard = match self.deferred.flush.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        let _ = self.flush_locked(&guard, false);
    }

    fn flush_locked(&self, _flush: &MutexGuard<'_, ()>, force: bool) -> DeferredFlush {
        let mut report = DeferredFlush::default();
        let work: Vec<(u64, Op)> = lk(&self.deferred.ops)
            .iter()
            .filter(|h| h.due(force))
            .map(|h| (h.seq, h.op.clone()))
            .collect();
        for (seq, op) in work {
            match self.apply_deferred(&op) {
                Ok(admitted) => {
                    self.remove_held(seq);
                    report.applied += 1;
                    report.admitted.extend(admitted);
                }
                Err(e) if is_storage_error(&e) => {
                    let mut ops = lk(&self.deferred.ops);
                    if let Some(h) = ops.iter_mut().find(|h| h.seq == seq) {
                        h.attempts += 1;
                        h.last_try = Instant::now();
                        h.last_error = e.message.clone();
                    }
                    break;
                }
                Err(e) => {
                    self.remove_held(seq);
                    report.dropped.push((op.job_id().to_string(), e));
                }
            }
        }
        report.pending = lk(&self.deferred.ops).len();
        report
    }

    fn remove_held(&self, seq: u64) {
        let mut ops = lk(&self.deferred.ops);
        ops.retain(|h| h.seq != seq);
        self.deferred.sync_len(&ops);
    }

    /// Store one held write. `Ok(Some(id))` for a stored admission.
    fn apply_deferred(&self, op: &Op) -> JobResult<Option<String>> {
        match op {
            Op::Admit {
                job_id,
                new,
                ext,
                opts,
                ..
            } => {
                let done = self.submit_with_id(new, ext, opts, Some(job_id))?;
                Ok(Some(done.record.job.job_id))
            }
            Op::Ext { job_id, fence, ext } => {
                self.set_ext_in(job_id, *fence, ext)?;
                Ok(None)
            }
            Op::Delete { job_id, opts } => match self.delete_with(job_id, opts) {
                Ok(_) => Ok(None),
                // Gone already (pruned, or an earlier try got further than it reported).
                Err(e) if e.code == "job_not_found" => Ok(None),
                Err(e) => Err(e),
            },
        }
    }
}
