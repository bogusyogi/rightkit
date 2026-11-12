//! Bounded, inspectable mid-run steering queue.
//!
//! This module deliberately owns only queue semantics.  The daemon remains
//! responsible for authorization, transcript persistence, & delivery through
//! its existing engine channels.
//!
//! FM-124: edit, reorder & send-now controls on queued messages. The daemon
//! route (`steer_session` in `src/api_sessions/messages.rs`)
//! dispatches `action` in {enqueue, list, edit, reorder, remove, sendNow} to
//! `edit`/`reorder`/`pending`+`mark_dispatched` below; every mutator here
//! rejects an already-dispatched item so a promoted (send-now'd) message can
//! no longer be edited, reordered, or removed out from under the in-flight
//! turn.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

const MAX_PENDING: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct QueuedSteer {
    pub id: String,
    pub text: String,
    pub dispatched: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueError {
    Full,
    NotFound,
    AlreadyDispatched,
    InvalidPosition,
}

#[derive(Debug, Default)]
pub struct SteeringQueue {
    next_id: u64,
    items: VecDeque<QueuedSteer>,
}

impl SteeringQueue {
    pub fn enqueue(&mut self, text: String) -> Result<QueuedSteer, QueueError> {
        if self.items.len() >= MAX_PENDING {
            return Err(QueueError::Full);
        }
        let item = QueuedSteer {
            id: format!("steer-{}", self.next_id),
            text,
            dispatched: false,
        };
        self.next_id = self.next_id.wrapping_add(1);
        self.items.push_back(item.clone());
        Ok(item)
    }

    pub fn list(&self) -> Vec<QueuedSteer> {
        self.items.iter().cloned().collect()
    }

    pub fn edit(&mut self, id: &str, text: String) -> Result<QueuedSteer, QueueError> {
        let item = self
            .items
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or(QueueError::NotFound)?;
        if item.dispatched {
            return Err(QueueError::AlreadyDispatched);
        }
        item.text = text;
        Ok(item.clone())
    }

    pub fn remove(&mut self, id: &str) -> Result<QueuedSteer, QueueError> {
        let index = self
            .items
            .iter()
            .position(|item| item.id == id)
            .ok_or(QueueError::NotFound)?;
        if self.items[index].dispatched {
            return Err(QueueError::AlreadyDispatched);
        }
        let item = self.items.remove(index).expect("position was found");
        Ok(item)
    }

    pub fn reorder(&mut self, id: &str, position: usize) -> Result<(), QueueError> {
        if position >= self.items.len() {
            return Err(QueueError::InvalidPosition);
        }
        let index = self
            .items
            .iter()
            .position(|item| item.id == id)
            .ok_or(QueueError::NotFound)?;
        let item = self.items.remove(index).expect("position was found");
        if item.dispatched {
            self.items.insert(index, item);
            return Err(QueueError::AlreadyDispatched);
        }
        self.items.insert(position, item);
        Ok(())
    }

    /// Read a queued item without changing its delivery state. The daemon
    /// uses this before a bounded channel send so a full/closed consumer does
    /// not consume an item that never reached the engine.
    pub fn pending(&self, id: &str) -> Result<QueuedSteer, QueueError> {
        let item = self
            .items
            .iter()
            .find(|item| item.id == id)
            .ok_or(QueueError::NotFound)?;
        if item.dispatched {
            return Err(QueueError::AlreadyDispatched);
        }
        Ok(item.clone())
    }

    pub fn mark_dispatched(&mut self, id: &str) -> Result<QueuedSteer, QueueError> {
        let item = self
            .items
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or(QueueError::NotFound)?;
        if item.dispatched {
            return Err(QueueError::AlreadyDispatched);
        }
        item.dispatched = true;
        Ok(item.clone())
    }
}

fn queues() -> &'static Mutex<HashMap<String, SteeringQueue>> {
    static QUEUES: OnceLock<Mutex<HashMap<String, SteeringQueue>>> = OnceLock::new();
    QUEUES.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn with_session<T>(session_id: &str, f: impl FnOnce(&mut SteeringQueue) -> T) -> T {
    let mut all = queues()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(all.entry(session_id.to_owned()).or_default())
}

pub fn retire_session(session_id: &str) {
    queues()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id);
}
