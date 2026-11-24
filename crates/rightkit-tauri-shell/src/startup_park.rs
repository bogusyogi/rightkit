//! Park the hidden main window's renderer once its frontend surfaces are ready. Moved from
//! host `app_run.rs` (`HUB_STARTUP_SUSPEND_ARMED`, `HUB_FRONTEND_READY_SURFACES`,
//! `HUB_FRONTEND_READY_REQUIRED`, `try_suspend_startup_hub`, and the surface-name match in
//! `hub_frontend_ready`).
//!
//! The window is hidden at startup and its rendering is suspended, but only after every required
//! surface has reported ready (suspending earlier would stall the frontend mid-mount). Two events
//! can come last in either order: the host arming the park, and the final surface report. Whichever
//! comes last takes the park.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// One per window. `const fn new` so it can be a `static`.
pub struct StartupPark {
    armed: AtomicBool,
    ready: AtomicU64,
    required: u64,
}

impl StartupPark {
    /// `required` is the bit mask of surfaces that must all be ready.
    pub const fn new(required: u64) -> Self {
        Self {
            armed: AtomicBool::new(false),
            ready: AtomicU64::new(0),
            required,
        }
    }

    /// The host hid the window at startup and wants it parked once ready.
    pub fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    /// A surface reported ready (`bit` is its mask bit).
    pub fn mark_ready(&self, bit: u64) {
        self.ready.fetch_or(bit, Ordering::SeqCst);
    }

    /// True exactly once: every required surface is ready and the park was armed. The caller then
    /// checks the window is still hidden and suspends it.
    pub fn take_if_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst) & self.required == self.required
            && self.armed.swap(false, Ordering::SeqCst)
    }
}
