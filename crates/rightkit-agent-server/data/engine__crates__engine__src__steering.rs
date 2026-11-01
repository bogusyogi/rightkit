//! Bounded, inspectable mid-run steering queue.
//!
//! This module deliberately owns only queue semantics.  The daemon remains
//! responsible for authorization, transcript persistence, & delivery through
//! its existing engine channels.
//!
//! FM-124: edit, reorder & send-now controls on queued messages. The daemon
//! route (`steer_session` in `bins/coderight/src/api_sessions/messages.rs`)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_assigns_sequential_ids_and_is_not_dispatched() {
        let mut q = SteeringQueue::default();
        let a = q.enqueue("first".to_string()).unwrap();
        let b = q.enqueue("second".to_string()).unwrap();
        assert_eq!(a.id, "steer-0");
        assert_eq!(b.id, "steer-1");
        assert!(!a.dispatched);
        assert!(!b.dispatched);
        assert_eq!(q.list(), vec![a, b]);
    }

    #[test]
    fn enqueue_fails_full_at_max_pending_boundary() {
        let mut q = SteeringQueue::default();
        for i in 0..MAX_PENDING {
            q.enqueue(format!("item-{i}")).unwrap();
        }
        assert_eq!(q.list().len(), MAX_PENDING);
        assert_eq!(q.enqueue("overflow".to_string()), Err(QueueError::Full));
        // still full after the rejected attempt, not corrupted
        assert_eq!(q.list().len(), MAX_PENDING);
    }

    #[test]
    fn enqueue_succeeds_one_below_max_pending() {
        let mut q = SteeringQueue::default();
        for i in 0..MAX_PENDING - 1 {
            q.enqueue(format!("item-{i}")).unwrap();
        }
        assert!(q.enqueue("last".to_string()).is_ok());
    }

    #[test]
    fn edit_updates_text_of_pending_item() {
        let mut q = SteeringQueue::default();
        let item = q.enqueue("original".to_string()).unwrap();
        let edited = q.edit(&item.id, "changed".to_string()).unwrap();
        assert_eq!(edited.text, "changed");
        assert_eq!(q.list()[0].text, "changed");
    }

    #[test]
    fn edit_unknown_id_returns_not_found() {
        let mut q = SteeringQueue::default();
        assert_eq!(
            q.edit("steer-999", "x".to_string()),
            Err(QueueError::NotFound)
        );
    }

    #[test]
    fn edit_dispatched_item_is_rejected() {
        let mut q = SteeringQueue::default();
        let item = q.enqueue("text".to_string()).unwrap();
        q.mark_dispatched(&item.id).unwrap();
        assert_eq!(
            q.edit(&item.id, "new".to_string()),
            Err(QueueError::AlreadyDispatched)
        );
    }

    #[test]
    fn remove_deletes_pending_item() {
        let mut q = SteeringQueue::default();
        let item = q.enqueue("gone".to_string()).unwrap();
        let removed = q.remove(&item.id).unwrap();
        assert_eq!(removed.id, item.id);
        assert!(q.list().is_empty());
    }

    #[test]
    fn remove_unknown_id_returns_not_found() {
        let mut q = SteeringQueue::default();
        assert_eq!(q.remove("nope"), Err(QueueError::NotFound));
    }

    #[test]
    fn remove_dispatched_item_is_rejected_and_kept() {
        let mut q = SteeringQueue::default();
        let item = q.enqueue("text".to_string()).unwrap();
        q.mark_dispatched(&item.id).unwrap();
        assert_eq!(q.remove(&item.id), Err(QueueError::AlreadyDispatched));
        assert_eq!(q.list().len(), 1);
    }

    #[test]
    fn reorder_moves_item_to_new_position() {
        let mut q = SteeringQueue::default();
        let a = q.enqueue("a".to_string()).unwrap();
        let _b = q.enqueue("b".to_string()).unwrap();
        let c = q.enqueue("c".to_string()).unwrap();
        q.reorder(&c.id, 0).unwrap();
        let ids: Vec<_> = q.list().into_iter().map(|i| i.id).collect();
        assert_eq!(ids, vec![c.id, a.id, "steer-1".to_string()]);
    }

    #[test]
    fn reorder_out_of_bounds_position_is_rejected() {
        let mut q = SteeringQueue::default();
        let a = q.enqueue("a".to_string()).unwrap();
        assert_eq!(q.reorder(&a.id, 5), Err(QueueError::InvalidPosition));
    }

    #[test]
    fn reorder_unknown_id_returns_not_found() {
        let mut q = SteeringQueue::default();
        q.enqueue("a".to_string()).unwrap();
        assert_eq!(q.reorder("missing", 0), Err(QueueError::NotFound));
    }

    #[test]
    fn reorder_dispatched_item_is_rejected_and_restored_to_original_index() {
        let mut q = SteeringQueue::default();
        let a = q.enqueue("a".to_string()).unwrap();
        let b = q.enqueue("b".to_string()).unwrap();
        q.mark_dispatched(&a.id).unwrap();
        assert_eq!(q.reorder(&a.id, 1), Err(QueueError::AlreadyDispatched));
        // item must be restored at its original index, not lost or moved
        let ids: Vec<_> = q.list().into_iter().map(|i| i.id).collect();
        assert_eq!(ids, vec![a.id, b.id]);
    }

    #[test]
    fn pending_reads_without_consuming_item() {
        let mut q = SteeringQueue::default();
        let item = q.enqueue("text".to_string()).unwrap();
        assert_eq!(q.pending(&item.id).unwrap(), item);
        assert!(!q.list()[0].dispatched);
    }

    #[test]
    fn pending_rejects_dispatched_item() {
        let mut q = SteeringQueue::default();
        let item = q.enqueue("text".to_string()).unwrap();
        q.mark_dispatched(&item.id).unwrap();
        assert_eq!(q.pending(&item.id), Err(QueueError::AlreadyDispatched));
    }

    #[test]
    fn mark_dispatched_transitions_pending_item() {
        let mut q = SteeringQueue::default();
        let item = q.enqueue("text".to_string()).unwrap();
        let dispatched = q.mark_dispatched(&item.id).unwrap();
        assert!(dispatched.dispatched);
    }

    #[test]
    fn mark_dispatched_twice_is_rejected() {
        let mut q = SteeringQueue::default();
        let item = q.enqueue("text".to_string()).unwrap();
        q.mark_dispatched(&item.id).unwrap();
        assert_eq!(
            q.mark_dispatched(&item.id),
            Err(QueueError::AlreadyDispatched)
        );
    }

    #[test]
    fn mark_dispatched_unknown_id_returns_not_found() {
        let mut q = SteeringQueue::default();
        assert_eq!(q.mark_dispatched("nope"), Err(QueueError::NotFound));
    }

    #[test]
    fn with_session_isolates_queues_by_session_id() {
        with_session("session-a", |q| q.enqueue("a-item".to_string()).unwrap());
        with_session("session-b", |q| q.enqueue("b-item".to_string()).unwrap());
        let a_list = with_session("session-a", |q| q.list());
        let b_list = with_session("session-b", |q| q.list());
        assert_eq!(a_list.len(), 1);
        assert_eq!(b_list.len(), 1);
        assert_eq!(a_list[0].text, "a-item");
        assert_eq!(b_list[0].text, "b-item");
        retire_session("session-a");
        retire_session("session-b");
    }

    #[test]
    fn retire_session_clears_its_queue() {
        with_session("session-retire", |q| q.enqueue("x".to_string()).unwrap());
        retire_session("session-retire");
        let list = with_session("session-retire", |q| q.list());
        assert!(list.is_empty());
    }

    #[test]
    fn dispatched_item_cannot_be_edited_or_removed_and_queue_length_stable() {
        let mut q = SteeringQueue::default();
        let a = q.enqueue("a".to_string()).unwrap();
        let b = q.enqueue("b".to_string()).unwrap();
        q.mark_dispatched(&a.id).unwrap();
        assert_eq!(
            q.edit(&a.id, "new".to_string()),
            Err(QueueError::AlreadyDispatched)
        );
        assert_eq!(q.remove(&a.id), Err(QueueError::AlreadyDispatched));
        assert_eq!(q.list().len(), 2);
        assert_eq!(q.list()[0].id, a.id);
        assert_eq!(q.list()[1].id, b.id);
    }

    // FM-124: reorder is stable — moving one item to a new position must not
    // disturb the relative order of the untouched items around it.
    #[test]
    fn reorder_is_stable_for_untouched_items() {
        let mut q = SteeringQueue::default();
        let a = q.enqueue("a".to_string()).unwrap();
        let b = q.enqueue("b".to_string()).unwrap();
        let c = q.enqueue("c".to_string()).unwrap();
        let d = q.enqueue("d".to_string()).unwrap();
        // Move the last item to just after the first. b, c, d keep their
        // relative order to each other around the insertion point.
        q.reorder(&d.id, 1).unwrap();
        let ids: Vec<_> = q.list().into_iter().map(|i| i.id).collect();
        assert_eq!(
            ids,
            vec![a.id.clone(), d.id.clone(), b.id.clone(), c.id.clone()]
        );
        // Moving it back to the end restores the original relative order.
        q.reorder(&d.id, 3).unwrap();
        let ids: Vec<_> = q.list().into_iter().map(|i| i.id).collect();
        assert_eq!(ids, vec![a.id, b.id, c.id, d.id]);
    }

    // FM-124: send-now (mark_dispatched) promotes exactly the targeted
    // message — sibling queued items stay pending and unaffected.
    #[test]
    fn send_now_promotes_exactly_one_message() {
        let mut q = SteeringQueue::default();
        let a = q.enqueue("a".to_string()).unwrap();
        let b = q.enqueue("b".to_string()).unwrap();
        let c = q.enqueue("c".to_string()).unwrap();
        let promoted = q.mark_dispatched(&b.id).unwrap();
        assert!(promoted.dispatched);
        let list = q.list();
        assert_eq!(list.len(), 3, "send-now must not remove or duplicate items");
        for item in &list {
            let should_be_dispatched = item.id == b.id;
            assert_eq!(
                item.dispatched, should_be_dispatched,
                "only the send-now target ({}) may be dispatched, item {} was {}",
                b.id, item.id, item.dispatched
            );
        }
        // Siblings remain fully controllable — they were never touched.
        assert!(q.edit(&a.id, "a-edited".to_string()).is_ok());
        assert!(q.edit(&c.id, "c-edited".to_string()).is_ok());
        // The promoted item itself is now locked.
        assert_eq!(
            q.edit(&b.id, "too-late".to_string()),
            Err(QueueError::AlreadyDispatched)
        );
    }

    #[test]
    fn reorder_then_edit_preserves_intended_position() {
        let mut q = SteeringQueue::default();
        let _a = q.enqueue("a".to_string()).unwrap();
        let b = q.enqueue("b".to_string()).unwrap();
        let c = q.enqueue("c".to_string()).unwrap();
        q.reorder(&c.id, 0).unwrap();
        q.edit(&b.id, "b-edited".to_string()).unwrap();
        let ids: Vec<_> = q.list().into_iter().map(|i| i.text).collect();
        assert_eq!(ids, vec!["c", "a", "b-edited"]);
    }
}
