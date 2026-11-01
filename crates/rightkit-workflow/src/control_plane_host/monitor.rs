//! In-process producer queue for daemon-internal workflow monitor events.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Mutex;

/// One internally observed signal a `monitor`/`internal_event` trigger may
/// admit. `topic` matches `WorkflowTriggerRow::dedupe_key`'s topic segment
/// (the convention already used by `find_trigger_by_dedupe_key`); callers
/// inside the daemon (schedulers, storage watchers, completion hooks)
/// publish one of these instead of performing a loopback HTTP call.
#[derive(Debug, Clone)]
pub struct InternalMonitorEvent {
    /// Trigger id the event is addressed to. The producer never scans for
    /// matching triggers by topic string alone — the caller who observed
    /// the internal condition already knows which trigger it maps to,
    /// exactly as the external HTTP route requires today.
    pub trigger_id: String,
    /// Logical event type recorded on the trigger-event row (e.g.
    /// `"workflow.completed"`, `"storage.threshold"`).
    pub event_type: String,
    /// Caller-assigned external id; combined with `event_type` and the
    /// content hash of `data` to derive a stable dedupe key so redelivery
    /// of the same internal condition is a no-op.
    pub external_id: String,
    /// Structured event payload normalized the same way the manual/cron
    /// producers normalize theirs.
    pub data: serde_json::Value,
}

/// In-process queue an internal producer publishes onto; `MonitorWorker`
/// drains it on `tick()`. This is the direct-fire leg: no HTTP request is
/// constructed or parsed anywhere in this path.
pub struct MonitorEventQueue {
    tx: Sender<InternalMonitorEvent>,
    rx: Mutex<Receiver<InternalMonitorEvent>>,
}

impl MonitorEventQueue {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            tx,
            rx: Mutex::new(rx),
        }
    }

    /// Handle for producers (schedulers, watchers, completion hooks) to
    /// publish onto. Clonable so multiple internal sources can share one
    /// queue.
    pub fn sender(&self) -> Sender<InternalMonitorEvent> {
        self.tx.clone()
    }

    pub fn drain(&self) -> Vec<InternalMonitorEvent> {
        let rx = self.rx.lock().expect("monitor queue lock");
        rx.try_iter().collect()
    }
}

impl Default for MonitorEventQueue {
    fn default() -> Self {
        Self::new()
    }
}
