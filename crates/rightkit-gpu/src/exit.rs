//! Leaving: no GPU work may be in flight when the process ends (issue #620). Adapted from Ember `crates/gpu`.
//!
//! The devices are statics that are never dropped, and the threads that render (preview workers,
//! export, denoise, the device warm-up) are not the one that ends the process. Exiting runs the
//! driver's own teardown while those threads keep running: a render that is inside the driver at
//! that moment crashes there (seen as a SIGSEGV in the NVIDIA Vulkan driver), where neither the
//! panic hook nor `catch_unwind` can help.
//!
//! So every entry into a device goes through [`enter`], and the host ends the process with
//! [`begin_shutdown`] (nothing new enters) and [`wait_idle`] (what is in flight
//! finishes, up to a deadline).

use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

struct State {
    closed: bool,
    /// [`Work`] in flight, on any thread.
    busy: usize,
}

pub struct GpuGate {
    state: Mutex<State>,
    idle: Condvar,
}

/// GPU work in flight (until dropped).
pub struct Work<'a>(&'a GpuGate);

static GATE: GpuGate = GpuGate::new();

/// Start GPU work, or `None` when the process is ending: the caller then does without the GPU.
/// Work may nest (a render that creates the device).
pub fn enter() -> Option<Work<'static>> {
    GATE.enter()
}

pub fn begin_shutdown() {
    GATE.close();
}

pub fn shutting_down() -> bool {
    GATE.lock().closed
}

pub fn wait_idle(timeout: Duration) -> bool {
    GATE.wait_idle(timeout)
}

impl Default for GpuGate {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuGate {
    pub const fn new() -> GpuGate {
        GpuGate {
            state: Mutex::new(State {
                closed: false,
                busy: 0,
            }),
            idle: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn enter(&self) -> Option<Work<'_>> {
        let mut s = self.lock();
        if s.closed {
            return None;
        }
        s.busy = s.busy.saturating_add(1);
        Some(Work(self))
    }

    /// Nothing enters from now on.
    pub fn close(&self) {
        self.lock().closed = true;
    }

    /// Wait up to `timeout` until no work is in flight; `false` when some still is.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let t0 = Instant::now();
        let mut s = self.lock();
        while s.busy > 0 {
            let Some(left) = timeout.checked_sub(t0.elapsed()).filter(|d| !d.is_zero()) else {
                return false;
            };
            s = self
                .idle
                .wait_timeout(s, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }
}

impl Drop for Work<'_> {
    fn drop(&mut self) {
        let mut s = self.0.lock();
        s.busy = s.busy.saturating_sub(1);
        if s.busy == 0 {
            drop(s);
            self.0.idle.notify_all();
        }
    }
}
