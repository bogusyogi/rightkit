//! Event-driven waits for resident loops: a condvar-backed `Wake` that a file-system event, a
//! stop request or a drain can signal, so a loop blocks until there is work and wakes at most once
//! per long backstop otherwise.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Longest a healthy loop sleeps with nothing to do (the backstop behind every event wake).
pub(crate) const BACKSTOP: Duration = Duration::from_secs(30);
/// Cadence of a loop whose file watch could not start: it falls back to bounded polling.
pub(crate) const DEGRADED_POLL: Duration = Duration::from_secs(1);

#[derive(Default)]
struct WakeState {
    pending: bool,
    closed: bool,
}

/// A level-triggered wake: `notify` makes the next `wait` return at once, `close` makes every
/// `wait` return at once from then on.
#[derive(Clone, Default)]
pub(crate) struct Wake(Arc<(Mutex<WakeState>, Condvar)>);

impl Wake {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WakeState> {
        self.0
             .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn notify(&self) {
        self.lock().pending = true;
        self.0 .1.notify_all();
    }

    pub(crate) fn close(&self) {
        self.lock().closed = true;
        self.0 .1.notify_all();
    }

    /// False once every holder but the caller's list entry has dropped it.
    pub(crate) fn is_shared(&self) -> bool {
        Arc::strong_count(&self.0) > 1
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Blocks until notified, closed, or `timeout`; returns false only on timeout. A notify that
    /// arrived before the call is not lost.
    pub(crate) fn wait(&self, timeout: Duration) -> bool {
        let guard = self.lock();
        let (mut state, result) = self
            .0
             .1
            .wait_timeout_while(guard, timeout, |state| !state.pending && !state.closed)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let woken = !result.timed_out();
        state.pending = false;
        woken
    }
}

/// A started directory watch. `is_live` turns false once its backend ends or its forwarder exits;
/// the owning loop then polls at the degraded cadence and starts a new watch on its next pass.
pub(crate) struct DirWatch {
    live: Arc<AtomicBool>,
}

impl DirWatch {
    pub(crate) fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }
}

/// Watches `dir` and signals `wake` for every event on a file whose name `keep` accepts. The
/// forwarder thread exits within one backstop after `wake` is closed. Returns None when the
/// watch could not start (missing directory, backend failure); callers then poll.
pub(crate) fn watch_dir_into(
    dir: &Path,
    keep: impl Fn(&str) -> bool + Send + Sync + 'static,
    wake: &Wake,
) -> Option<DirWatch> {
    use rightkit_file_watch::{TypedOptions, TypedWatcher};
    let options = TypedOptions::new()
        .queue_capacity(NonZeroUsize::new(64).expect("non-zero"))
        .exclude_with(move |path: &Path| {
            !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(&keep)
        });
    let dir: PathBuf = dir.to_path_buf();
    let Ok(watcher) = TypedWatcher::watch_with(&dir, options) else {
        return None;
    };
    let wake = wake.clone();
    let live = Arc::new(AtomicBool::new(true));
    let thread_live = Arc::clone(&live);
    std::thread::Builder::new()
        .name("membrane-file-wake".into())
        .spawn(move || {
            while !wake.is_closed() {
                match watcher.recv_timeout(BACKSTOP) {
                    Ok(_) => {
                        while watcher.try_recv().is_ok() {}
                        wake.notify();
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        // The backend ended: mark the watch dead before waking the loop, so the
                        // loop sees it dead and polls at the degraded cadence, not the backstop.
                        thread_live.store(false, Ordering::Release);
                        wake.notify();
                        return;
                    }
                }
            }
            thread_live.store(false, Ordering::Release);
        })
        .ok()
        .map(|_| DirWatch { live })
}
