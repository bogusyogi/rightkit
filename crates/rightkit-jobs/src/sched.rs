//! Scheduling policy shared by every engine: interactive work preempts batch
//! work at step boundaries, a host handoff pauses work at the next boundary, and
//! stalls (no progress), not total duration, bound a run.
//!
//! Modelled on HeardRight's speech service: file jobs run one window per turn
//! and pause while dictation is active; a displaced host fails in-flight jobs
//! with a retryable handoff error at the next window boundary.

use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::store::Job;

/// Who is waiting on the job. Interactive work (a person is dictating) always
/// runs; batch work (a file transcription) yields at its step boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    Interactive,
    Batch,
}

impl Priority {
    /// `request.priority == "interactive"` selects interactive; anything else is batch.
    pub fn of(job: &Job) -> Priority {
        Self::from_request(&job.request)
    }
    pub fn from_request(request: &Value) -> Priority {
        if request["priority"] == "interactive" {
            Priority::Interactive
        } else {
            Priority::Batch
        }
    }
}

/// Process-wide coordination point. Share one `Gate` between the `Runner` and
/// the service handler so interactive jobs, batch jobs and handoff see each other.
#[derive(Default)]
pub struct Gate {
    interactive: AtomicUsize,
    handoff: AtomicBool,
    lock: Mutex<()>,
    cv: Condvar,
}

impl Gate {
    pub fn new() -> Arc<Gate> {
        Arc::new(Gate::default())
    }
    /// Mark interactive work active until the guard drops.
    pub fn begin_interactive(self: &Arc<Self>) -> InteractiveGuard {
        self.interactive.fetch_add(1, Ordering::SeqCst);
        InteractiveGuard(self.clone())
    }
    pub fn interactive_active(&self) -> usize {
        self.interactive.load(Ordering::SeqCst)
    }
    /// Block up to `timeout` for interactive work to finish (or any notify).
    pub(crate) fn wait_idle(&self, timeout: Duration) {
        let g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if self.interactive.load(Ordering::SeqCst) > 0 {
            let _ = self.cv.wait_timeout(g, timeout);
        }
    }
    /// Ask running work to stop at its next boundary with a retryable
    /// `host_handoff` error. Called when a higher-ranked host takes over.
    pub fn request_handoff(&self) {
        self.handoff.store(true, Ordering::SeqCst);
        self.cv.notify_all();
    }
    pub fn clear_handoff(&self) {
        self.handoff.store(false, Ordering::SeqCst);
    }
    pub fn handoff_requested(&self) -> bool {
        self.handoff.load(Ordering::SeqCst)
    }
}

pub struct InteractiveGuard(Arc<Gate>);
impl Drop for InteractiveGuard {
    fn drop(&mut self) {
        self.0.interactive.fetch_sub(1, Ordering::SeqCst);
        let _g = self.0.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.0.cv.notify_all();
    }
}
