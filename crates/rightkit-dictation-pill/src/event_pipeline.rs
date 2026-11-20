//! The pill event pipeline a host declares as a `static`: events enter from any thread, wait in a
//! generation-stamped queue ([`crate::dispatch_queue`]), and are applied one at a time on the
//! thread the host names (a UI thread, say), after exactly one scheduled drain.
//!
//! What it decides: an audio-rate `Voice` level replaces a still-pending `Voice` instead of growing
//! the queue; a `Status` is shown for a host-chosen time and then followed by `Idle`, but only if no
//! newer event arrived meanwhile (the delayed `Idle` carries the status's generation); a full queue
//! drops its oldest event and says so; a dispatch that panics is counted, never fatal.
//!
//! What the host supplies ([`PipelineHost`]): how to run a task on its applying thread, how to apply
//! one queued event, the diagnostic sink, and how long a `Status` stays. Diagnostic lines carry the
//! closed event names of [`crate::visibility_policy::event_name`], counts and booleans only.

use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use rightkit_dictation_core::pill::PillEvent;

use crate::dispatch_queue::{self, DispatchQueue, QueuedEvent};
use crate::visibility_policy::event_name;

/// A task for the applying thread.
pub type Task = Box<dyn FnOnce() + Send + 'static>;

/// What the host supplies to a [`PillEvents`] pipeline.
#[derive(Clone, Copy)]
pub struct PipelineHost {
    /// Runs `task` on the thread that applies events. `None`: that thread does not exist yet (the
    /// queue is then emptied with nothing applied, as the host has nothing to apply it to).
    /// `Some(Err(()))`: scheduling failed (the drain is marked unscheduled so the next event
    /// schedules again). The scheduler's error text is deliberately not carried.
    pub run_on_main: fn(Task) -> Option<Result<(), ()>>,
    /// Applies one queued event; runs on the applying thread, never under the queue lock.
    pub apply: fn(QueuedEvent<PillEvent>),
    /// The host's diagnostic sink: `(event name, detail)`.
    pub log: fn(&str, &str),
    /// How long a `Status` event stays before `Idle` follows it (unless a newer event arrived).
    pub temporary_event_duration: Duration,
}

type Queue = Mutex<DispatchQueue<PillEvent>>;

fn new_queue() -> Queue {
    Mutex::new(DispatchQueue::default())
}

/// The pipeline. Declare one `static` per host.
pub struct PillEvents {
    queue: LazyLock<Queue, fn() -> Queue>,
    host: PipelineHost,
}

impl PillEvents {
    pub const fn new(host: PipelineHost) -> Self {
        Self {
            queue: LazyLock::new(new_queue),
            host,
        }
    }

    /// Show `event`: a `Status` is temporary, everything else is queued as it comes.
    pub fn show_event(&'static self, event: PillEvent) {
        if matches!(event, PillEvent::Status { .. }) {
            self.show_temporary_event(event);
            return;
        }
        self.dispatch_new_event(event);
    }

    /// Queue `event` and schedule the drain when none is pending. Returns the event's generation.
    pub fn dispatch_new_event(&'static self, event: PillEvent) -> u64 {
        let name = event_name(&event);
        let should_log = !matches!(event, PillEvent::Voice { .. });
        let queued = {
            let mut queue = dispatch_queue::lock(&self.queue);
            if matches!(event, PillEvent::Voice { .. }) {
                // The microphone level arrives at audio rate: a level still waiting for the
                // applying thread is replaced by the newer one, so the queue never fills with
                // amplitudes.
                queue
                    .enqueue_replacing_last(event, |last, _| matches!(last, PillEvent::Voice { .. }))
            } else {
                queue.enqueue(event)
            }
        };
        if queued.dropped {
            (self.host.log)("pill_dispatch_queue_full", "dropped=oldest");
        }
        if should_log {
            (self.host.log)(
                "pill_dispatch_enqueued",
                &format!(
                    "generation={} event={} schedule_drain={}",
                    queued.generation, name, queued.schedule_drain
                ),
            );
        }
        if queued.schedule_drain {
            self.schedule_drain();
        }
        queued.generation
    }

    /// Schedule one drain on the applying thread.
    pub fn schedule_drain(&'static self) {
        let apply = self.host.apply;
        let log = self.host.log;
        let queue: &'static Queue = &self.queue;
        let task: Task = Box::new(move || {
            // A dispatch that panics is caught by the queue (it would otherwise leave the drain
            // marked as scheduled for good); report it by count.
            let report = dispatch_queue::drain(queue, apply);
            if report.panicked > 0 {
                log("pill_dispatch_panic", &format!("count={}", report.panicked));
            }
        });
        match (self.host.run_on_main)(task) {
            None => {
                dispatch_queue::drain(&self.queue, |_| {});
            }
            Some(Ok(())) => {}
            // The scheduler's error text is not logged: the outcome is the whole report.
            Some(Err(())) => {
                dispatch_queue::lock(&self.queue).mark_drain_unscheduled();
                (self.host.log)("pill_event_schedule_error", "outcome=failed");
            }
        }
    }

    /// Show a transient result, then restore resting state unless a newer pill event arrived
    /// meanwhile. This prevents command results from remaining as an active waveform while
    /// preserving recording/error events that begin during the timeout.
    pub fn show_temporary_event(&'static self, event: PillEvent) {
        let generation = self.dispatch_new_event(event);
        let duration = self.host.temporary_event_duration;
        std::thread::spawn(move || {
            std::thread::sleep(duration);
            self.enqueue_idle_if_current(generation);
        });
    }

    /// Queue `Idle` only while `generation` is still the latest.
    pub fn enqueue_idle_if_current(&'static self, generation: u64) {
        let queued =
            dispatch_queue::lock(&self.queue).enqueue_if_current(generation, PillEvent::Idle);
        if queued.is_some_and(|queued| queued.schedule_drain) {
            self.schedule_drain();
        }
    }
}
