//! The ordered worker a host's sends, retries and transfer changes wait in.
//!
//! A send's route decision can probe the LAN twin for seconds, so the host's own loop (which also
//! writes state and the heartbeat) never runs it. Jobs wait here for one worker, in the order they
//! came. Bounded: when the queue is full a job is refused and handed back. The commands that
//! change a transfer or a trust (cancel, dismiss, trust, forget) go through the same worker, so a
//! cancel issued after a send finds the send's transfer, never before it; a full queue or a gone
//! worker runs such an ordered job right on the caller's thread rather than drop it.
//!
//! A panicking job does not end the worker: later jobs would never run. The host's `panicked` hook
//! is told, with the peer of the job when it was a route job, so it can write the line the panic
//! cut off.

use std::sync::mpsc::{self, SyncSender, TrySendError};

/// What the host hooks into the worker.
pub struct QueueHooks<P> {
    /// Called on the worker before each job.
    pub begin: fn(),
    /// Called on the worker after a job panicked, with the peer of a route job (`None` for an
    /// ordered job, which owes no line).
    pub panicked: fn(Option<P>),
    /// Called when the worker thread could not start: its name and the error text. Every later
    /// route job is then refused [`Refused::Disconnected`].
    pub spawn_failed: fn(&str, &str),
}

struct Job<P> {
    /// `Some(peer)` for a route job, `None` for an ordered job.
    peer: Option<P>,
    run: Box<dyn FnOnce() + Send>,
}

/// Why a route job was not queued. The job's peer comes back with it, so the caller can write the
/// line the job owed.
pub enum Refused<P> {
    /// The queue holds its limit of waiting jobs.
    Full(Option<P>),
    /// The worker is gone.
    Disconnected(Option<P>),
}

/// One worker thread and its bounded queue.
pub struct JobQueue<P: Send + 'static> {
    sender: SyncSender<Job<P>>,
}

impl<P: Send + 'static> JobQueue<P> {
    /// Starts the worker under `name` with room for `capacity` waiting jobs.
    pub fn start(name: &str, capacity: usize, hooks: QueueHooks<P>) -> JobQueue<P> {
        let (sender, receiver) = mpsc::sync_channel::<Job<P>>(capacity);
        let QueueHooks {
            begin,
            panicked,
            spawn_failed,
        } = hooks;
        let started = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                for job in receiver {
                    let Job { peer, run } = job;
                    begin();
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).is_err() {
                        panicked(peer);
                    }
                }
            });
        if let Err(error) = started {
            // The receiver went with the failed spawn: every later job finds the worker gone.
            spawn_failed(name, &error.to_string());
        }
        JobQueue { sender }
    }

    /// Queue a route job (a send or retry) for `peer`. Refused, with the peer handed back, when the
    /// queue is full or the worker is gone.
    pub fn route(&self, peer: P, job: impl FnOnce() + Send + 'static) -> Result<(), Refused<P>> {
        let job = Job {
            peer: Some(peer),
            run: Box::new(job),
        };
        match self.sender.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(job)) => Err(Refused::Full(job.peer)),
            Err(TrySendError::Disconnected(job)) => Err(Refused::Disconnected(job.peer)),
        }
    }

    /// Queue an ordered job (cancel, dismiss, trust change) behind the jobs before it. A full queue
    /// or a gone worker runs it right here instead (out of order, but never dropped).
    pub fn ordered(&self, job: impl FnOnce() + Send + 'static) {
        let job = Job {
            peer: None,
            run: Box::new(job),
        };
        match self.sender.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(job) | TrySendError::Disconnected(job)) => (job.run)(),
        }
    }
}
