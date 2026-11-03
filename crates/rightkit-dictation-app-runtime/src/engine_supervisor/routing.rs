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
