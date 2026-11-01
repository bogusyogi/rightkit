// Per-generation coordination around the RightKit supervisor: the BYOK delivery gate, the
// generation-fenced event router (exactly-once completion routing) and exit classification from
// typed state. Pure logic, no process handling, so each rule is unit-tested here.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use rightkit_dictation_core::engine::EngineFrame;
use rightkit_job_queue::Terminal;
use serde_json::Value;

use super::protocol::frame_to_engine_event;
use super::wire::frame_from_value;
use crate::engine_process::EngineEvent;

/// What the caller of [`KeyGate::claim`] must do for a generation.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Claim {
    /// This caller delivers the keys, then calls [`KeyGate::settle`].
    Deliver,
    /// Delivery for this generation settled successfully (or the generation is stale).
    Done,
    /// Delivery for this generation failed or is uncertain. The gate stays closed for it: every
    /// dependent caller gets this error, and the same generation is never delivered to again
    /// (a replay after `delivery_unknown` could double-apply). Only a NEW generation delivers.
    Failed(String),
}

#[derive(Default)]
struct GateState {
    /// Highest generation whose delivery settled, and how it ended.
    settled_generation: u64,
    outcome: Option<Result<(), String>>,
    inflight: Option<u64>,
}

/// Gates all key-dependent work (start, calls, L3) until the BYOK delivery for the current engine
/// generation has settled. Each new generation is claimed afresh, so the keys are read and sent
/// again for every `Ready` generation; one caller delivers while the others wait.
#[derive(Default)]
pub(crate) struct KeyGate {
    state: Mutex<GateState>,
    settled: Condvar,
}

/// Longest a dependent caller waits for another caller's delivery (delivery itself is bounded by
/// the call timeout, so this only stops a stuck run).
const GATE_WAIT: Duration = Duration::from_secs(15);

impl KeyGate {
    pub(crate) fn claim(&self, generation: u64) -> Claim {
        let deadline = Instant::now() + GATE_WAIT;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if state.settled_generation >= generation {
                return match (&state.outcome, state.settled_generation == generation) {
                    (Some(Err(reason)), true) => Claim::Failed(reason.clone()),
                    _ => Claim::Done,
                };
            }
            if state.inflight != Some(generation) {
                state.inflight = Some(generation);
                return Claim::Deliver;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Claim::Done;
            }
            state = self
                .settled
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Delivery for `generation` ended with `outcome`: record it and release the waiters.
    pub(crate) fn settle(&self, generation: u64, outcome: Result<(), String>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if generation >= state.settled_generation {
            state.settled_generation = generation;
            state.outcome = Some(outcome);
        }
        if state.inflight == Some(generation) {
            state.inflight = None;
        }
        drop(state);
        self.settled.notify_all();
    }

    /// Wait (bounded) while another caller is delivering for `generation`; never delivers.
    pub(crate) fn wait_if_inflight(&self, generation: u64) {
        let deadline = Instant::now() + GATE_WAIT;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.inflight == Some(generation) && state.settled_generation < generation {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            state = self
                .settled
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// Where an engine event went.
#[derive(Debug)]
// An engine event is built once and moved to one consumer; boxing it would only add an allocation
// per event.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Routed {
    /// From another engine process: dropped.
    Stale,
    /// Consumed by the caller waiting on this request id (never also forwarded).
    Completed,
    /// Forwarded to the shell event consumer.
    Event(EngineEvent),
    /// Not a frame the shell consumes.
    Ignored,
}

/// Fences id-less engine events on the engine generation (`instance`, from hello) and routes each
/// completion exactly once: to the waiter registered for its request id, else to the shell.
#[derive(Default)]
pub(crate) struct EventRouter {
    instance: Mutex<String>,
    waiters: Mutex<HashMap<String, Sender<EngineFrame>>>,
}

impl EventRouter {
    pub(crate) fn set_instance(&self, instance: &str) {
        *self.instance.lock().unwrap_or_else(|e| e.into_inner()) = instance.to_string();
    }

    pub(crate) fn register(&self, request_id: &str, tx: Sender<EngineFrame>) {
        self.waiters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id.to_string(), tx);
    }

    pub(crate) fn forget(&self, request_id: &str) {
        self.waiters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(request_id);
    }

    /// `current_instance` re-reads the supervisor's hello once when an event names an instance the
    /// router has not seen yet (an event can outrun the lifecycle bridge's `Ready`).
    pub(crate) fn route(
        &self,
        envelope: &Value,
        current_instance: impl FnOnce() -> Option<String>,
    ) -> Routed {
        let payload = envelope.get("event");
        let Some(generation) = payload
            .and_then(|p| p.get("generation"))
            .and_then(Value::as_str)
        else {
            return Routed::Stale;
        };
        let known = self
            .instance
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if known != generation {
            match current_instance() {
                Some(now) if now == generation => self.set_instance(&now),
                _ => return Routed::Stale,
            }
        }
        let Some(frame) = payload
            .and_then(|p| p.get("frame"))
            .and_then(|e| frame_from_value(e).ok())
        else {
            return Routed::Ignored;
        };
        let waiter = self
            .waiters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&frame.request_id);
        if let Some(waiter) = waiter {
            let _ = waiter.send(frame);
            return Routed::Completed;
        }
        match frame_to_engine_event(&frame) {
            Some(event) => Routed::Event(event),
            None => Routed::Ignored,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ExitKind {
    /// Already reported, an intentional stop, or not this generation's exit.
    Ignore,
    /// The generation never reached `Ready`.
    SpawnFailure,
    /// A generation that was `Ready` ended.
    Crash,
}

/// Classify a lifecycle exit from typed state only: generations and the supervisor's `terminal`
/// (read at the event's generation), never message text.
pub(crate) fn classify_exit(
    event_generation: u64,
    last_reported_generation: u64,
    last_ready_generation: u64,
    shutdown_generation: u64,
    terminal_at_event: Option<&Terminal>,
) -> ExitKind {
    if event_generation <= last_reported_generation
        || event_generation <= shutdown_generation
        || matches!(terminal_at_event, Some(Terminal::Stopped))
    {
        ExitKind::Ignore
    } else if event_generation == last_ready_generation {
        ExitKind::Crash
    } else {
        ExitKind::SpawnFailure
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_dictation_core::engine::{EnginePayload, EngineSchemaName, StopKind};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::channel;
    use std::sync::Arc;

    // ---- (1) a fresh key per generation, dependent work gated -------------------------------

    #[test]
    fn each_new_generation_delivers_once_and_a_settled_one_is_done() {
        let gate = KeyGate::default();
        assert_eq!(gate.claim(1), Claim::Deliver);
        gate.settle(1, Ok(()));
        assert_eq!(gate.claim(1), Claim::Done);
        assert_eq!(
            gate.claim(2),
            Claim::Deliver,
            "a new generation re-reads and re-sends"
        );
        gate.settle(2, Ok(()));
        assert_eq!(gate.claim(2), Claim::Done);
        assert_eq!(
            gate.claim(1),
            Claim::Done,
            "a stale generation never delivers"
        );
    }

    #[test]
    fn a_failed_or_uncertain_delivery_keeps_the_generation_closed_and_is_never_replayed() {
        let gate = KeyGate::default();
        assert_eq!(gate.claim(7), Claim::Deliver);
        gate.settle(
            7,
            Err("provider key delivery failed: delivery_unknown".into()),
        );
        // Every dependent caller sees the error, again and again; none becomes the deliverer.
        for _ in 0..3 {
            assert_eq!(
                gate.claim(7),
                Claim::Failed("provider key delivery failed: delivery_unknown".into())
            );
        }
        // Only a new generation delivers again, and it can succeed.
        assert_eq!(gate.claim(8), Claim::Deliver);
        gate.settle(8, Ok(()));
        assert_eq!(gate.claim(8), Claim::Done);
        // A late settle for an older generation cannot reopen or overwrite the newer outcome.
        gate.settle(7, Err("late".into()));
        assert_eq!(gate.claim(8), Claim::Done);
    }

    #[test]
    fn dependent_work_waits_until_delivery_settles_and_delivery_runs_once() {
        let gate = Arc::new(KeyGate::default());
        let deliveries = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(AtomicUsize::new(0));
        assert_eq!(gate.claim(5), Claim::Deliver);
        deliveries.fetch_add(1, Ordering::SeqCst);
        let waiters: Vec<_> = (0..4)
            .map(|_| {
                let (gate, deliveries, released) =
                    (gate.clone(), deliveries.clone(), released.clone());
                std::thread::spawn(move || {
                    if gate.claim(5) == Claim::Deliver {
                        deliveries.fetch_add(1, Ordering::SeqCst);
                    }
                    released.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            released.load(Ordering::SeqCst),
            0,
            "dependents must wait for delivery"
        );
        gate.settle(5, Ok(()));
        for waiter in waiters {
            waiter.join().unwrap();
        }
        assert_eq!(released.load(Ordering::SeqCst), 4);
        assert_eq!(deliveries.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn wait_if_inflight_blocks_sends_only_while_that_generation_delivers() {
        let gate = Arc::new(KeyGate::default());
        gate.wait_if_inflight(1); // nothing in flight: returns at once
        assert_eq!(gate.claim(1), Claim::Deliver);
        let released = Arc::new(AtomicUsize::new(0));
        let waiter = {
            let (gate, released) = (gate.clone(), released.clone());
            std::thread::spawn(move || {
                gate.wait_if_inflight(1);
                released.fetch_add(1, Ordering::SeqCst);
            })
        };
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(released.load(Ordering::SeqCst), 0);
        gate.settle(1, Ok(()));
        waiter.join().unwrap();
        assert_eq!(released.load(Ordering::SeqCst), 1);
    }

    // ---- (2) accepted, then an id-less event: exactly once and generation-fenced -------------

    fn event_envelope(generation: &str, request_id: &str) -> Value {
        let frame = EngineFrame::base(
            EngineSchemaName::TranscribingStarted,
            request_id,
            Some("s1".into()),
            "t",
            Some(EnginePayload::TranscribingStarted {
                stop_kind: Some(StopKind::Cancel),
                stop_target: None,
            }),
            None,
        );
        serde_json::json!({
            "type": "event",
            "session_id": "s1",
            "event": {
                "generation": generation,
                "frame": serde_json::to_value(frame).unwrap(),
            },
        })
    }

    #[test]
    fn a_completion_is_forwarded_exactly_once_when_nobody_waits() {
        let router = EventRouter::default();
        router.set_instance("gen-a");
        let envelope = event_envelope("gen-a", "stop:1");
        assert!(matches!(router.route(&envelope, || None), Routed::Event(_)));
    }

    #[test]
    fn a_waiter_consumes_its_completion_and_the_shell_never_sees_it() {
        let router = EventRouter::default();
        router.set_instance("gen-a");
        let (tx, rx) = channel();
        router.register("cancel:9", tx);
        let envelope = event_envelope("gen-a", "cancel:9");
        assert!(matches!(
            router.route(&envelope, || None),
            Routed::Completed
        ));
        assert_eq!(rx.try_recv().unwrap().request_id, "cancel:9");
        // The waiter is gone: a duplicate is an ordinary event, and never reaches the waiter twice.
        assert!(matches!(router.route(&envelope, || None), Routed::Event(_)));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn events_from_another_generation_are_dropped_not_routed() {
        let router = EventRouter::default();
        router.set_instance("gen-b");
        let (tx, rx) = channel();
        router.register("stop:2", tx);
        let stale = event_envelope("gen-a", "stop:2");
        assert!(matches!(
            router.route(&stale, || Some("gen-b".into())),
            Routed::Stale
        ));
        assert!(
            rx.try_recv().is_err(),
            "a stale event must not complete a current waiter"
        );
        // No generation at all is also stale (the engine always names one).
        let mut anonymous = event_envelope("gen-b", "stop:2");
        anonymous["event"]
            .as_object_mut()
            .unwrap()
            .remove("generation");
        assert!(matches!(router.route(&anonymous, || None), Routed::Stale));
    }

    #[test]
    fn a_completion_from_the_previous_generation_is_rejected_after_a_restart() {
        let router = EventRouter::default();
        router.set_instance("gen-a");
        let old = event_envelope("gen-a", "stop:4");
        assert!(matches!(router.route(&old, || None), Routed::Event(_)));
        // The engine restarted: the supervisor now reports gen-b. A late gen-a completion is stale
        // and never reaches the shell, even though it was valid a moment ago.
        router.set_instance("gen-b");
        assert!(matches!(
            router.route(&old, || Some("gen-b".into())),
            Routed::Stale
        ));
    }

    #[test]
    fn an_event_that_outruns_ready_adopts_the_supervisors_current_instance() {
        let router = EventRouter::default();
        let envelope = event_envelope("gen-new", "stop:3");
        assert!(matches!(
            router.route(&envelope, || Some("gen-new".into())),
            Routed::Event(_)
        ));
        // Now known: no re-read needed.
        assert!(matches!(router.route(&envelope, || None), Routed::Event(_)));
    }

    // ---- (4) exits are classified from typed state -------------------------------------------

    #[test]
    fn exit_kind_comes_from_generations_and_the_typed_terminal() {
        let stopped = Terminal::Stopped;
        let failed = Terminal::Failed { reason: "x".into() };
        // A generation that was Ready and ended is a crash; one that never was is a spawn failure.
        assert_eq!(classify_exit(3, 2, 3, 0, None), ExitKind::Crash);
        assert_eq!(
            classify_exit(4, 3, 3, 0, Some(&failed)),
            ExitKind::SpawnFailure
        );
        // Already reported, or at/below the generation an intentional shutdown stopped.
        assert_eq!(classify_exit(3, 3, 3, 0, None), ExitKind::Ignore);
        assert_eq!(classify_exit(3, 2, 3, 3, None), ExitKind::Ignore);
        // The typed terminal Stopped is an intentional stop, whatever the message says.
        assert_eq!(classify_exit(5, 4, 5, 0, Some(&stopped)), ExitKind::Ignore);
    }
}
