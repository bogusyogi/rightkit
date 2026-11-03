//! The run watchdog — detects stuck / stalled runs by counting progress-free ticks.
//!
//! The cancellation flag is shared by clones so an owner can stop an async
//! worker, including a goal-audit loop, without needing a mutable reference to
//! the worker's watchdog.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Tracks consecutive ticks without progress and flags a stall.
#[derive(Debug, Clone)]
pub struct RunWatchdog {
    stall_threshold_ticks: u32,
    ticks_without_progress: u32,
    cancelled: Arc<AtomicBool>,
}

impl RunWatchdog {
    pub fn new(stall_threshold_ticks: u32) -> Self {
        Self {
            stall_threshold_ticks,
            ticks_without_progress: 0,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Request cancellation of the run observed by this watchdog and all of
    /// its clones. This is cooperative: an in-flight operation must yield for
    /// its owner to observe the request.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested by this watchdog or a clone.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Record a tick during which no progress occurred.
    pub fn tick_without_progress(&mut self) {
        self.ticks_without_progress = self.ticks_without_progress.saturating_add(1);
    }

    /// Reset the stall counter because progress was made.
    pub fn record_progress(&mut self) {
        self.ticks_without_progress = 0;
    }

    /// True when the run has stalled for at least the threshold number of ticks.
    pub fn is_stalled(&self) -> bool {
        self.ticks_without_progress >= self.stall_threshold_ticks
    }

    /// A human-readable nudge to break the stall.
    pub fn nudge_message(&self) -> String {
        format!(
            "Run appears stalled after {} ticks without progress; reassess the plan, \
             unblock dependencies, or escalate.",
            self.ticks_without_progress
        )
    }
}
