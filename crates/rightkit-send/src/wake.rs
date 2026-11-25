//! A wake-up that is not lost when nobody waits yet.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// `notify` sets it, `wait` takes it, or sleeps until it or the timeout. A host's loops wait on one
/// instead of polling, and a notification that arrives before the loop waits is not lost.
pub struct Wake {
    fired: Mutex<bool>,
    wake: Condvar,
}

impl Wake {
    pub const fn new() -> Wake {
        Wake {
            fired: Mutex::new(false),
            wake: Condvar::new(),
        }
    }

    /// Set the wake-up and end any wait now.
    pub fn notify(&self) {
        *self.fired.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.wake.notify_all();
    }

    /// Return at once if a wake-up is set (taking it), else sleep until one is set or `timeout`
    /// passes. Either way the wake-up is cleared on return.
    pub fn wait(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut fired = self.fired.lock().unwrap_or_else(|e| e.into_inner());
        while !*fired {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            fired = self
                .wake
                .wait_timeout(fired, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        *fired = false;
    }
}

impl Default for Wake {
    fn default() -> Self {
        Wake::new()
    }
}
