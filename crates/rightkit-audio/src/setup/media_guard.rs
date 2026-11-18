//! Session-owned media lifecycle guard used by setup capture.
//!
//! This module deliberately has no app or platform dependencies. The host supplies its
//! media-control callbacks.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Clone, Copy)]
pub struct MediaLifecycle {
    begin: fn(&str),
    restore: fn(&str, &str),
}

impl MediaLifecycle {
    pub const fn new(begin: fn(&str), restore: fn(&str, &str)) -> Self {
        Self { begin, restore }
    }
}

/// Cloneable handle; the worker thread holds one clone. Media is restored by
/// an explicit `restore` or, failing that, when the LAST clone drops. Each
/// clone used to restore on its own drop, so the worker's clone (dropped when
/// it was joined) always won with "capture_guard_drop" and every normal stop
/// or cancel was logged as a leak.
#[derive(Clone)]
pub struct MediaPauseGuard {
    inner: Arc<GuardInner>,
}

struct GuardInner {
    session_id: String,
    restored: AtomicBool,
    lifecycle: MediaLifecycle,
}

impl GuardInner {
    fn restore(&self, reason: &str) {
        if !self.restored.swap(true, Ordering::AcqRel) {
            (self.lifecycle.restore)(&self.session_id, reason);
        }
    }
}

impl MediaPauseGuard {
    pub fn begin(session_id: &str, lifecycle: MediaLifecycle) -> Self {
        (lifecycle.begin)(session_id);
        Self {
            inner: Arc::new(GuardInner {
                session_id: session_id.to_string(),
                restored: AtomicBool::new(false),
                lifecycle,
            }),
        }
    }

    pub fn restore(&self, reason: &str) {
        self.inner.restore(reason);
    }

    pub fn restore_for_session(&self, session_id: &str, reason: &str) -> bool {
        if self.inner.session_id != session_id {
            return false;
        }
        self.restore(reason);
        true
    }
}

impl Drop for GuardInner {
    fn drop(&mut self) {
        self.restore("capture_guard_drop");
    }
}
