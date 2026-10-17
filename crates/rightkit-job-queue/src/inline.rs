//! Cooperative cancellation for requests that have no persisted job row.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::{JobError, JobResult, JOB_EXISTS};

type Entries = Mutex<HashMap<String, Arc<AtomicBool>>>;

/// Cloneable, process-local registry for inline requests. Clones share registrations.
///
/// Keep each returned [`InlineJobGuard`] alive until work finishes. Cancellation sets
/// its `Arc<AtomicBool>`; work must observe that flag cooperatively. Nothing is persisted.
#[derive(Clone, Default)]
pub struct InlineCancelRegistry {
    entries: Arc<Entries>,
}

impl InlineCancelRegistry {
    /// Creates an independent registry. Share it (or clones) across request handlers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `job_id`, rejecting an active duplicate with [`JOB_EXISTS`].
    /// Empty IDs return `bad_input`. Dropping the guard releases the ID, including on unwind.
    pub fn register(&self, job_id: &str) -> JobResult<(InlineJobGuard, Arc<AtomicBool>)> {
        if job_id.is_empty() {
            return Err(JobError::new(
                "bad_input",
                "inline job id must not be empty",
            ));
        }
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.contains_key(job_id) {
            return Err(JobError::new(
                JOB_EXISTS,
                format!("job {job_id} is already running"),
            ));
        }
        let flag = Arc::new(AtomicBool::new(false));
        entries.insert(job_id.to_owned(), Arc::clone(&flag));
        Ok((
            InlineJobGuard {
                entries: Arc::clone(&self.entries),
                job_id: job_id.to_owned(),
                flag: Arc::clone(&flag),
            },
            flag,
        ))
    }

    /// Requests cancellation; false when no request with this ID is registered.
    /// Repeated cancellation returns true until its guard drops.
    pub fn cancel(&self, job_id: &str) -> bool {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(flag) = entries.get(job_id) {
            flag.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    /// Cancels all current registrations; returns their count. Future requests start uncancelled.
    pub fn cancel_all(&self) -> usize {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        for flag in entries.values() {
            flag.store(true, Ordering::SeqCst);
        }
        entries.len()
    }
}

/// Registration lifetime. Drop removes its entry without resetting cancellation flags
/// already held by work. This guard is intentionally not cloneable.
#[must_use = "keep this guard alive until inline work finishes"]
pub struct InlineJobGuard {
    entries: Arc<Entries>,
    job_id: String,
    flag: Arc<AtomicBool>,
}

impl InlineJobGuard {
    /// Registered ID.
    pub fn job_id(&self) -> &str {
        &self.job_id
    }
}

impl Drop for InlineJobGuard {
    fn drop(&mut self) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries
            .get(&self.job_id)
            .is_some_and(|flag| Arc::ptr_eq(flag, &self.flag))
        {
            entries.remove(&self.job_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_empty_unknown_and_reuse() {
        let registry = InlineCancelRegistry::new();
        assert!(registry.register("").is_err());
        assert!(!registry.cancel("missing"));
        let (guard, flag) = registry.register("render-1").unwrap();
        assert_eq!(guard.job_id(), "render-1");
        assert_eq!(
            registry.register("render-1").err().unwrap().code,
            JOB_EXISTS
        );
        assert!(!flag.load(Ordering::SeqCst));
        assert!(registry.cancel("render-1"));
        assert!(registry.cancel("render-1"));
        assert!(flag.load(Ordering::SeqCst));
        drop(guard);
        assert!(!registry.cancel("render-1"));
        let (_guard, fresh) = registry.register("render-1").unwrap();
        assert!(!fresh.load(Ordering::SeqCst));
        assert!(flag.load(Ordering::SeqCst));
    }

    #[test]
    fn clones_share_but_independent_registries_do_not() {
        let registry = InlineCancelRegistry::new();
        let (_guard, flag) = registry.register("transcribe-1").unwrap();
        assert!(!InlineCancelRegistry::new().cancel("transcribe-1"));
        assert!(registry.clone().cancel("transcribe-1"));
        assert!(flag.load(Ordering::SeqCst));
    }

    #[test]
    fn cancel_all_does_not_cancel_future_work() {
        let registry = InlineCancelRegistry::new();
        let (_a, a) = registry.register("a").unwrap();
        let (_b, b) = registry.register("b").unwrap();
        assert_eq!(registry.cancel_all(), 2);
        assert!(a.load(Ordering::SeqCst) && b.load(Ordering::SeqCst));
        let (_c, c) = registry.register("c").unwrap();
        assert!(!c.load(Ordering::SeqCst));
    }

    #[test]
    fn unwind_releases_registration() {
        let registry = InlineCancelRegistry::new();
        let _ = std::panic::catch_unwind(|| {
            let (_guard, _) = registry.register("assemble-1").unwrap();
            panic!("request failed");
        });
        assert!(!registry.cancel("assemble-1"));
        assert!(registry.register("assemble-1").is_ok());
    }
}
