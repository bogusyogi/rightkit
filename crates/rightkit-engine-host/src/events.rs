//! The engine's event fan-out: every event a service publishes goes to its live subscribers and
//! to a bounded [`EventLog`] that a `*.events.poll` command reads.
//!
//! Engine events do not belong to one request (a refresh can outlive the call that started it, and
//! an event frame must precede its own request's response), so no request-scoped event is
//! forwarded; the broadcaster is the one place they all land. The names of the commands that poll
//! it and the sink type the services publish through stay with the engine.

use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use rightkit_daemon::event_log::{EventLog, PollResult};
use serde_json::Value;
use std::time::Duration;

/// Events a subscriber may have waiting; beyond that its newest events are dropped, never the
/// engine's threads held up.
pub const SUBSCRIBER_QUEUE: usize = 1024;

/// One published event: its topic (the page event name) and its JSON payload.
#[derive(Clone, Debug)]
pub struct TopicEvent {
    pub topic: String,
    pub payload: Value,
}

/// The function a service calls to publish: topic and payload.
pub type PublishSink = Arc<dyn Fn(&str, Value) + Send + Sync>;

/// Fans events out to its subscribers and records each in a bounded poll log.
#[derive(Clone, Default)]
pub struct EventBroadcaster {
    subscribers: Arc<Mutex<Vec<SyncSender<TopicEvent>>>>,
    log: Arc<EventLog>,
}

impl EventBroadcaster {
    pub fn new() -> EventBroadcaster {
        EventBroadcaster::default()
    }

    /// A new subscription: every event published from now on.
    pub fn subscribe(&self) -> Receiver<TopicEvent> {
        let (sender, receiver) = sync_channel(SUBSCRIBER_QUEUE);
        self.subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(sender);
        receiver
    }

    /// Ends every waiting poll at once (the engine is shutting down).
    pub fn close(&self) {
        self.log.close();
    }

    /// Reads the poll log: events after `after` on `topics` (empty: every topic), waiting up to
    /// `wait` for one.
    pub fn poll(&self, after: u64, topics: &[String], wait: Duration) -> PollResult {
        self.log.poll(after, topics, wait)
    }

    /// Records `payload` as the event `topic` and hands it to every live subscriber.
    pub fn publish(&self, topic: &str, payload: Value) {
        self.log.push(topic, &payload);
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        if subscribers.is_empty() {
            return;
        }
        let event = TopicEvent {
            topic: topic.to_string(),
            payload,
        };
        subscribers.retain(|sender| match sender.try_send(event.clone()) {
            Ok(()) | Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    /// The sink the services publish through; an engine wraps it in its own event handle.
    pub fn sink(&self) -> PublishSink {
        let broadcaster = self.clone();
        Arc::new(move |name: &str, payload: Value| broadcaster.publish(name, payload))
    }
}
