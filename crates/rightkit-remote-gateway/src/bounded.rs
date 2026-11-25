//! The edge of the gateway protocol, shared by any MCP host that serves the remote tools to a
//! tunnel: every call is bounded in time on its own worker thread, and a reply is measured against
//! the tunnel's message limit.
//!
//! * A call runs on a worker thread and is waited for at most [`Bounds::timeout`]. A stuck call
//!   answers `timed_out`, a panic or a worker that cannot start answers `internal_error`, and a
//!   call that arrives while [`Bounds::max_running`] earlier ones are still stuck answers `busy`.
//! * When `max_running` calls are stuck the process exits non-zero after [`Bounds::exit_delay`],
//!   so the answer to the call that tipped it is written first. A tunnel client that spawns its
//!   stdio child once is started again by hand.
//! * A reply larger than the message limit becomes a `too_large` refusal.
//!
//! Nothing here holds a path, a search, a message or any content.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use serde_json::{Value, json};

use crate::Refusal;

/// The limits and names of one bounded runner.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    /// The longest one call may run before the gateway stops waiting for it.
    pub timeout: Duration,
    /// Calls that timed out keep running. With this many still stuck the process exits; until then a
    /// new call is refused (`busy`).
    pub max_running: usize,
    /// How long the process waits, after the call that made `max_running`, for that call's answer
    /// to be written before it exits.
    pub exit_delay: Duration,
    /// Stack for a call's worker thread, in bytes.
    pub worker_stack: usize,
    /// Name of a call's worker thread.
    pub worker_name: &'static str,
    /// Name of the exit thread.
    pub exit_name: &'static str,
    /// The stderr line's prefix, for example `my-gateway`.
    pub log_prefix: &'static str,
    /// What the stderr line tells the owner to do, for example `start tunnel-client again`.
    pub restart_hint: &'static str,
}

/// How a bounded run ended.
pub enum Bounded<T> {
    Done(T),
    TimedOut,
    /// The worker panicked or could not start.
    Failed,
    /// Too many earlier calls are still running.
    Busy,
}

/// Runs calls with [`Bounds`] and counts the workers that have not finished. Hold one in a `static`.
pub struct Runner {
    bounds: Bounds,
    running: AtomicUsize,
}

struct Slot<'a>(&'a AtomicUsize);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Runner {
    pub const fn new(bounds: Bounds) -> Runner {
        Runner {
            bounds,
            running: AtomicUsize::new(0),
        }
    }

    /// Calls whose worker thread has not finished. Calls are served one at a time, so between calls
    /// this counts the ones that timed out and are still running.
    pub fn running(&'static self) -> usize {
        self.running.load(Ordering::SeqCst)
    }

    /// Runs `work` on its own thread and waits at most `timeout` for it.
    pub fn bounded<T, F>(&'static self, work: F) -> Bounded<T>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        if self.running.load(Ordering::SeqCst) >= self.bounds.max_running {
            return Bounded::Busy;
        }
        self.running.fetch_add(1, Ordering::SeqCst);
        let slot = Slot(&self.running);
        let (sender, receiver) = mpsc::channel();
        let started = std::thread::Builder::new()
            .name(self.bounds.worker_name.into())
            .stack_size(self.bounds.worker_stack)
            .spawn(move || {
                let _slot = slot;
                let _ = sender.send(work());
            });
        if started.is_err() {
            // The closure (and its slot) was dropped with the failed spawn.
            return Bounded::Failed;
        }
        match receiver.recv_timeout(self.bounds.timeout) {
            Ok(value) => Bounded::Done(value),
            Err(RecvTimeoutError::Timeout) => {
                self.exit_if_stuck();
                Bounded::TimedOut
            }
            Err(RecvTimeoutError::Disconnected) => Bounded::Failed,
        }
    }

    /// Ends the process, non-zero and after one line on stderr, once `max_running` workers are
    /// stuck. The exit waits a moment so the answer to the call that tipped it is written first.
    fn exit_if_stuck(&'static self) {
        if self.running.load(Ordering::SeqCst) < self.bounds.max_running {
            return;
        }
        let _ = std::thread::Builder::new()
            .name(self.bounds.exit_name.into())
            .spawn(move || {
                std::thread::sleep(self.bounds.exit_delay);
                // Workers that finished meanwhile are not stuck.
                if self.running.load(Ordering::SeqCst) < self.bounds.max_running {
                    return;
                }
                eprintln!(
                    "{}: {} calls are stuck; exiting; {}",
                    self.bounds.log_prefix, self.bounds.max_running, self.bounds.restart_hint
                );
                std::process::exit(1);
            });
    }
}

/// The refusal for a bounded run that did not finish with an answer. `app` names the product in
/// "the call failed inside `<app>`".
pub fn stopped<T>(run: &Bounded<T>, app: &str) -> Refusal {
    match run {
        Bounded::TimedOut => Refusal::new(
            "timed_out",
            "the call took too long and was stopped; it may still finish in the background",
        ),
        Bounded::Busy => Refusal::new(
            "busy",
            "earlier calls are still running; try again in a moment",
        ),
        Bounded::Done(_) | Bounded::Failed => {
            Refusal::new("internal_error", format!("the call failed inside {app}"))
        }
    }
}

/// A `result` for `tools/call`: the answer (or the refusal, `{"error", "code"}`) as one JSON text
/// block; a refusal sets `isError`. An answer that would not fit in `max_bytes` becomes a
/// `too_large` refusal. Returns the result and the label for a log line (`ok` or the refusal's code).
pub fn reply_value(outcome: Result<Value, Refusal>, max_bytes: usize) -> (Value, &'static str) {
    let (body, failed, label) = match outcome {
        Ok(value) => (value, false, "ok"),
        Err(refusal) => (refusal.to_json(), true, refusal.code),
    };
    let result = json!({
        "content": [{"type": "text", "text": body.to_string()}],
        "isError": failed,
    });
    let fits = serde_json::to_vec(&result).is_ok_and(|bytes| bytes.len() <= max_bytes);
    if fits {
        return (result, label);
    }
    let refusal = Refusal::new(
        "too_large",
        "the answer would not fit in one tunnel message; ask for less (a smaller limit or a later offset)",
    );
    let result = json!({
        "content": [{"type": "text", "text": refusal.to_json().to_string()}],
        "isError": true,
    });
    (result, refusal.code)
}
