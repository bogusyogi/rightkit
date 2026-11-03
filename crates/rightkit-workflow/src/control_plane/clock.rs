//! Deterministic clock abstraction for the workflow control plane.
//!
//! The outer workflow DAG must be reproducible from its journal, so every
//! timestamp that lands in a journal event, projection, or reducer outcome
//! comes from a [`Clock`]. Production callers obtain a [`SystemClock`] at
//! the daemon boundary; tests use a [`TestClock`] seeded to an explicit
//! instant.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// Milliseconds since the Unix epoch (1970-01-01T00:00:00Z).
pub type UnixMs = i64;

/// Abstract clock interface. The reducer must treat this as the *only*
/// source of time.
pub trait Clock: Send + Sync {
    /// Current time as Unix milliseconds.
    fn now_unix_ms(&self) -> UnixMs;
}

/// Wall-clock implementation. Wraps `std::time::SystemTime` behind an atomic
/// so the daemon can swap in a frozen clock for fault-injection tests
/// without re-wiring the supervisor.
#[derive(Debug, Default, Clone)]
pub struct SystemClock {
    cached: Arc<AtomicI64>,
}

impl SystemClock {
    /// Build a fresh system clock.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> UnixMs {
        // Cache the value once per millisecond to keep test seams cheap.
        let now = self.cached.load(Ordering::Relaxed);
        let measured = system_time_ms();
        if now < measured {
            self.cached.store(measured, Ordering::Relaxed);
            measured
        } else {
            now
        }
    }
}

/// Test-only deterministic clock. Advance with [`TestClock::advance_ms`].
/// Multiple instances with the same seed observe the same monotonic
/// sequence; this is the property-test seam for the reducer.
#[derive(Debug, Clone)]
pub struct TestClock {
    now: UnixMs,
}

impl TestClock {
    /// Seed a test clock at `now_unix_ms`.
    pub fn new(now_unix_ms: UnixMs) -> Self {
        Self { now: now_unix_ms }
    }

    /// Move the clock forward by `delta_ms` (negative values rewind).
    pub fn advance_ms(&mut self, delta_ms: UnixMs) {
        self.now = self.now.saturating_add(delta_ms);
    }

    /// Pin the clock to an exact instant.
    pub fn set(&mut self, now_unix_ms: UnixMs) {
        self.now = now_unix_ms;
    }

    /// Read the current instant without consuming it.
    pub fn now_unix_ms(&self) -> UnixMs {
        self.now
    }
}

impl Clock for TestClock {
    fn now_unix_ms(&self) -> UnixMs {
        self.now
    }
}

fn system_time_ms() -> UnixMs {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(_) => 0,
    }
}
