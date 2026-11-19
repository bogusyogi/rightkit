//! A bounded "reveal again once the splash is gone" retry (platform-free).
//!
//! A host that keeps the pill hidden while a splash window is in front needs one place that waits
//! for the splash to leave and then reveals the pill. [`RevealRetry::schedule`] is that place: at
//! most one waiting thread exists at a time (a second call while one waits does nothing), it polls
//! every `delay`, runs `while_splash` each time the splash is still there, and runs `reveal` once
//! on the polling thread when it is gone. The flag is cleared before `reveal` runs, so `reveal` may
//! call [`RevealRetry::schedule`] again (the splash can come back). A failed thread spawn clears the
//! flag and is returned, so a later call can try again.
//!
//! The crate owns only the single-flight rule and the thread. What counts as a splash, the delay,
//! the thread name, what is hidden while waiting and what the reveal does are the host's.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

struct Release(&'static AtomicBool);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The single-flight latch of one retry loop. Keep it in a `static`.
pub struct RevealRetry {
    armed: AtomicBool,
}

impl Default for RevealRetry {
    fn default() -> Self {
        Self::new()
    }
}

impl RevealRetry {
    pub const fn new() -> RevealRetry {
        RevealRetry {
            armed: AtomicBool::new(false),
        }
    }

    /// Whether a retry thread is waiting right now.
    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::SeqCst)
    }

    /// Start the waiting thread unless one already waits. `Ok(true)` when started, `Ok(false)` when
    /// one was already armed, `Err` when the OS refused the thread (the latch is released).
    ///
    /// The thread sleeps `delay`, then: while `splash_present()` it calls `while_splash()` and
    /// sleeps again; once it is false it releases the latch and calls `reveal()`.
    pub fn schedule(
        &'static self,
        thread_name: &str,
        delay: Duration,
        mut splash_present: impl FnMut() -> bool + Send + 'static,
        mut while_splash: impl FnMut() + Send + 'static,
        reveal: impl FnOnce() + Send + 'static,
    ) -> Result<bool, std::io::Error> {
        if self.armed.swap(true, Ordering::SeqCst) {
            return Ok(false);
        }
        let spawned = std::thread::Builder::new()
            .name(thread_name.to_string())
            .spawn(move || {
                // Released on every exit, including a panic in a host closure.
                let release = Release(&self.armed);
                loop {
                    std::thread::sleep(delay);
                    if splash_present() {
                        while_splash();
                        continue;
                    }
                    break;
                }
                drop(release);
                reveal();
            });
        match spawned {
            Ok(_) => Ok(true),
            Err(error) => {
                self.armed.store(false, Ordering::SeqCst);
                Err(error)
            }
        }
    }
}
