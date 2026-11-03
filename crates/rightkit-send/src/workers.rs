//! The service's always-on threads: how they wait, and how `Service::stop` ends them.
//!
//! Nothing here polls. The acceptor blocks in `accept`; the liveness thread sleeps on a
//! [`Stopper`] for its whole 20-second period. `Service::stop` ends both and joins them, so the
//! listening socket is closed, and its port free, by the time `stop` returns.
//!
//! **Ending a blocked `accept`.** `Service::stop` connects to the listener from inside the process
//! (`127.0.0.1:<bound port>`, which a listener on `0.0.0.0` accepts). That is the wake chosen
//! because it needs nothing platform-specific and behaves the same on macOS and Windows:
//!
//! * the pending `accept` returns the connection like any peer's, the thread sees the stop flag,
//!   drops the connection and the listener, and ends. No handle is touched from another thread, so
//!   there is nothing to race with the thread's own use of the socket;
//! * the alternatives are not uniform: on BSD-derived systems (macOS) `shutdown` on a listening
//!   socket fails with `ENOTCONN` and `close` from another thread does not interrupt a blocked
//!   `accept`; on Windows `closesocket` from another thread aborts it but leaves the thread holding
//!   a closed handle, and std closes a `TcpListener` only by dropping it on its owner;
//! * loopback traffic to a socket this process owns is outside the Windows firewall's filtering and
//!   outside the macOS Local Network permission, which cover other hosts on the network.
//!
//! If the connect still fails, [`Worker::stop`] retries for a bounded time and then leaves the
//! thread detached (it ends when the next peer arrives) rather than blocking `stop` for good.
//!
//! Per-connection handler threads and the short-lived liveness probes are not workers: they end on
//! their own (30 s socket timeouts, 2 s probe timeout) and are not joined, so `stop` stays prompt.

use crate::lock;
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How long [`Worker::stop`] keeps trying before it gives up on a thread.
const STOP_BOUND: Duration = Duration::from_secs(2);
/// How long [`Worker::stop`] waits for the thread to end between two wake attempts.
const WAKE_RETRY: Duration = Duration::from_millis(100);

/// The service's stop signal for threads that wait with a timeout. A stop ends the wait now.
pub(crate) struct Stopper {
    stopped: Mutex<bool>,
    changed: Condvar,
}

impl Stopper {
    pub(crate) fn new() -> Self {
        Self {
            stopped: Mutex::new(false),
            changed: Condvar::new(),
        }
    }

    /// End every wait, now and later. Idempotent.
    pub(crate) fn stop(&self) {
        *lock(&self.stopped) = true;
        self.changed.notify_all();
    }

    /// Sleep for at most `timeout`, or until stopped. `true` means stopped.
    pub(crate) fn wait(&self, timeout: Duration) -> bool {
        let guard = lock(&self.stopped);
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, timeout, |stopped| !*stopped)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard
    }
}

/// Set when a worker has finished everything it owned.
struct Done {
    finished: Mutex<bool>,
    changed: Condvar,
}

impl Done {
    fn new() -> Self {
        Self {
            finished: Mutex::new(false),
            changed: Condvar::new(),
        }
    }

    fn set(&self) {
        *lock(&self.finished) = true;
        self.changed.notify_all();
    }

    /// Wait for `set`, for at most `timeout`. `true` means finished.
    fn wait(&self, timeout: Duration) -> bool {
        let guard = lock(&self.finished);
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, timeout, |finished| !*finished)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard
    }
}

/// Marks a worker done when it drops, a panic unwinding the thread included.
struct MarkDone(Arc<Done>);

impl Drop for MarkDone {
    fn drop(&mut self) {
        self.0.set();
    }
}

/// One always-on service thread that `Service::stop` can end and join.
pub(crate) struct Worker {
    handle: JoinHandle<()>,
    done: Arc<Done>,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl Worker {
    /// Start `body` on a named thread. `wake` makes a blocked `body` notice the stop flag; `stop`
    /// calls it until the thread has ended. Whatever `body` owns (a listener) is dropped before the
    /// worker counts as ended.
    pub(crate) fn spawn(
        name: &str,
        wake: impl Fn() + Send + Sync + 'static,
        body: impl FnOnce() + Send + 'static,
    ) -> io::Result<Worker> {
        let done = Arc::new(Done::new());
        let mark = MarkDone(done.clone());
        let handle = thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                // Bound first, so it drops last: after `body` and everything `body` owned.
                let _mark = mark;
                body();
            })?;
        Ok(Worker {
            handle,
            done,
            wake: Box::new(wake),
        })
    }

    /// End the thread and join it. `true` means it ended; `false` means it did not end within
    /// [`STOP_BOUND`] (it is left detached), or this is that thread itself (an event callback
    /// running on it), which then ends on its own once it returns to its loop. The caller has
    /// already raised the stop flag the thread checks.
    pub(crate) fn stop(self) -> bool {
        self.stop_within(STOP_BOUND)
    }

    pub(crate) fn stop_within(self, bound: Duration) -> bool {
        if self.handle.thread().id() == thread::current().id() {
            return false;
        }
        let deadline = Instant::now() + bound;
        loop {
            (self.wake)();
            if self.done.wait(WAKE_RETRY) {
                let _ = self.handle.join();
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
        }
    }
}
