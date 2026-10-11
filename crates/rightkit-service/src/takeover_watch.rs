//! Wake-up channel of the takeover monitor.
//!
//! The monitor thread of a host used to sleep 25 ms and read `takeover.json` on every pass, which
//! kept it waking 7 to 11 times a second while nothing happened. It now blocks on a [`Bell`] with a
//! one-second timeout:
//!
//! * [`Wake::Stop`]: the host released (clean shutdown, drop, or a completed handoff). The monitor
//!   leaves at once instead of finishing a sleep.
//! * [`Wake::Poke`]: this process just wrote a takeover request for this service ([`poke`], called
//!   by `write_takeover` and by a host that requests a takeover). The monitor looks immediately.
//! * [`Wake::Timeout`]: a request written by another process has no in-process signal (it is a
//!   file), so the monitor stats `takeover.json` once per [`TAKEOVER_CHECK_INTERVAL`].
//!
//! Everything here is `std` mutex and condition variable only: the same code runs unchanged on
//! macOS, Linux and Windows. No lock is held while the monitor judges a request or calls the
//! handler.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// How often a host looks for a takeover request written by another process. A second process
/// therefore waits up to about this long for the incumbent to notice its request.
pub(crate) const TAKEOVER_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// Why [`Bell::wait`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wake {
    /// The host is releasing: leave.
    Stop,
    /// A takeover request was written by this process: look now.
    Poke,
    /// The interval passed: check the file.
    Timeout,
}

#[derive(Default)]
struct State {
    stop: bool,
    poked: bool,
}

/// The monitor's wait point. One per hosted service.
pub(crate) struct Bell {
    state: Mutex<State>,
    cv: Condvar,
    /// Returns from [`Bell::wait`], for the quiet-window test.
    #[cfg(test)]
    wakes: AtomicU64,
    /// Full reads of `takeover.json` by the monitor, for the stat-only test.
    #[cfg(test)]
    reads: AtomicU64,
    /// Passes of the monitor loop that finished their file check, for the in-process wake tests.
    #[cfg(test)]
    passes: AtomicU64,
    /// The monitor function has returned.
    #[cfg(test)]
    exited: AtomicBool,
}

impl Bell {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            cv: Condvar::new(),
            #[cfg(test)]
            wakes: AtomicU64::new(0),
            #[cfg(test)]
            reads: AtomicU64::new(0),
            #[cfg(test)]
            passes: AtomicU64::new(0),
            #[cfg(test)]
            exited: AtomicBool::new(false),
        }
    }

    /// Block for at most `timeout`, or until [`Bell::stop`] or [`Bell::poke`]. Stop wins over a
    /// poke; a poke is consumed by the call that reports it, a stop is permanent.
    pub(crate) fn wait(&self, timeout: Duration) -> Wake {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (mut guard, _) = self
            .cv
            .wait_timeout_while(guard, timeout, |s| !s.stop && !s.poked)
            .unwrap_or_else(|e| e.into_inner());
        #[cfg(test)]
        self.wakes.fetch_add(1, Ordering::SeqCst);
        if guard.stop {
            Wake::Stop
        } else if guard.poked {
            guard.poked = false;
            Wake::Poke
        } else {
            Wake::Timeout
        }
    }

    /// End the monitor's waits for good. Idempotent.
    pub(crate) fn stop(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).stop = true;
        self.cv.notify_all();
    }

    /// Ask the monitor to look at `takeover.json` now.
    fn poke_now(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).poked = true;
        self.cv.notify_all();
    }

    #[cfg(test)]
    pub(crate) fn wakes(&self) -> u64 {
        self.wakes.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn note_read(&self) {
        self.reads.fetch_add(1, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn note_pass(&self) {
        self.passes.fetch_add(1, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn passes(&self) -> u64 {
        self.passes.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn mark_exited(&self) {
        self.exited.store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }
}

/// Every live monitor in this process, by the `takeover.json` it watches. Weak, so a released
/// host's bell is simply dropped.
static WATCHERS: Mutex<Vec<(PathBuf, Weak<Bell>)>> = Mutex::new(Vec::new());

/// Make `bell` reachable from [`poke`] for `path` (the `takeover.json` of its service).
pub(crate) fn register(path: PathBuf, bell: &Arc<Bell>) {
    let mut watchers = WATCHERS.lock().unwrap_or_else(|e| e.into_inner());
    watchers.retain(|(_, w)| w.strong_count() > 0);
    watchers.push((path, Arc::downgrade(bell)));
}

/// This process just wrote the takeover request at `path`: wake the monitor that watches it, so a
/// request raised in-process does not wait for the next file check. A request written by another
/// process cannot call this; the monitor finds it on its interval.
pub(crate) fn poke(path: &Path) {
    let bells: Vec<Arc<Bell>> = {
        let mut watchers = WATCHERS.lock().unwrap_or_else(|e| e.into_inner());
        watchers.retain(|(_, w)| w.strong_count() > 0);
        watchers
            .iter()
            .filter(|(p, _)| p.as_path() == path)
            .filter_map(|(_, w)| w.upgrade())
            .collect()
    };
    for bell in bells {
        bell.poke_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_ends_a_long_wait_at_once() {
        let bell = Arc::new(Bell::new());
        let b = bell.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let t = std::thread::spawn(move || {
            let reason = b.wait(Duration::from_secs(600));
            let _ = tx.send(reason);
        });
        bell.stop();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(10)),
            Ok(Wake::Stop),
            "stop must end a 10-minute wait"
        );
        t.join().unwrap();
        // Permanent: every later wait reports it without blocking.
        assert_eq!(bell.wait(Duration::from_secs(600)), Wake::Stop);
    }

    #[test]
    fn a_poke_is_reported_once_and_stop_wins_over_it() {
        let bell = Bell::new();
        bell.poke_now();
        assert_eq!(bell.wait(Duration::from_secs(600)), Wake::Poke);
        assert_eq!(bell.wait(Duration::from_millis(1)), Wake::Timeout);
        bell.poke_now();
        bell.stop();
        assert_eq!(bell.wait(Duration::from_secs(600)), Wake::Stop);
    }

    #[test]
    fn a_poke_reaches_only_the_bell_that_watches_that_path() {
        let mine = Arc::new(Bell::new());
        let other = Arc::new(Bell::new());
        let path = PathBuf::from("/rk-takeover-watch-test/mine/takeover.json");
        register(path.clone(), &mine);
        register(
            PathBuf::from("/rk-takeover-watch-test/other/takeover.json"),
            &other,
        );
        {
            let gone = Arc::new(Bell::new());
            register(path.clone(), &gone);
        }
        poke(&path);
        assert_eq!(mine.wait(Duration::from_secs(600)), Wake::Poke);
        assert_eq!(
            other.wait(Duration::from_millis(1)),
            Wake::Timeout,
            "a request for another service must not wake this monitor"
        );
        // The dropped bell was pruned by the poke that followed it. Only this test's own path is
        // checked: bells of other tests come and go concurrently.
        let watchers = WATCHERS.lock().unwrap_or_else(|e| e.into_inner());
        assert!(watchers
            .iter()
            .filter(|(p, _)| *p == path)
            .all(|(_, w)| w.strong_count() > 0));
    }
}
