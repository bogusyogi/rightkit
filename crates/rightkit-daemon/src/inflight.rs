//! The short requests a service is serving now, so a host can tell "the process lives" (its
//! heartbeat) from "the process answers" (Pulse #351, #426).
//!
//! A host registers each short request with [`Inflight::begin`] for as long as the returned guard
//! lives, and its heartbeat or health call reads [`Inflight::oldest`]: a short request still
//! unanswered after the host's stall threshold marks the service stalled. A long request (a scan,
//! a link over ssh) is not registered: it may legitimately take minutes.
//!
//! Each entry also holds the serving thread (name and id) and the stack at the point the request
//! was taken (std cannot unwind another thread later), which one log line can report when a stall
//! is found ([`Inflight::oldest_detail`]). Frames are not resolved to names until reported.
//!
//! ```ignore
//! static QUICK: rightkit_daemon::inflight::Inflight = rightkit_daemon::inflight::Inflight::new();
//! let _guard = QUICK.begin("doctor");
//! ```

use std::backtrace::Backtrace;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

struct Entry {
    id: u64,
    started: Instant,
    label: String,
    thread: String,
    stack: Arc<Backtrace>,
}

/// A set of requests in flight. Put one in a `static` (its constructor is `const`).
pub struct Inflight {
    entries: Mutex<Vec<Entry>>,
    seq: AtomicU64,
}

impl Inflight {
    pub const fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            seq: AtomicU64::new(0),
        }
    }

    /// Registers one request, named `label` (a method name), until the guard drops.
    pub fn begin(&'static self, label: &str) -> InflightGuard {
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        let current = std::thread::current();
        let thread = format!(
            "{} {:?}",
            current.name().unwrap_or("<unnamed>"),
            current.id()
        );
        // Taken before the lock.
        let stack = Arc::new(Backtrace::force_capture());
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Entry {
                id,
                started: Instant::now(),
                label: label.to_string(),
                thread,
                stack,
            });
        InflightGuard { owner: self, id }
    }

    /// The oldest request still in flight: its age in milliseconds and its label.
    pub fn oldest(&self) -> Option<(u64, String)> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .min_by_key(|entry| entry.started)
            .map(|entry| {
                (
                    u64::try_from(entry.started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    entry.label.clone(),
                )
            })
    }

    /// The serving thread and the dispatch stack of the oldest request still in flight.
    pub fn oldest_detail(&self) -> Option<(String, Arc<Backtrace>)> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .min_by_key(|entry| entry.started)
            .map(|entry| (entry.thread.clone(), Arc::clone(&entry.stack)))
    }
}

impl Default for Inflight {
    fn default() -> Self {
        Self::new()
    }
}

/// Keeps one request registered while it lives.
pub struct InflightGuard {
    owner: &'static Inflight,
    id: u64,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.owner
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|entry| entry.id != self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static SET: Inflight = Inflight::new();

    #[test]
    fn oldest_is_the_first_begun_and_guards_unregister() {
        assert!(SET.oldest().is_none());
        let first = SET.begin("first");
        let second = SET.begin("second");
        assert_eq!(
            SET.oldest().map(|(_, label)| label).as_deref(),
            Some("first")
        );
        assert!(SET.oldest_detail().is_some());
        drop(first);
        assert_eq!(
            SET.oldest().map(|(_, label)| label).as_deref(),
            Some("second")
        );
        drop(second);
        assert!(SET.oldest().is_none());
    }
}
