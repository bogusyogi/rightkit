//! Mutation-ordered writes without holding host state locks during disk I/O.
//!
//! Take tickets while holding the host state lock, release that lock, then call
//! [`OrderedWriteQueue::write_in_turn`]. Dropping an unused ticket cancels its turn;
//! errors & unwinding also release subsequent writers. Tickets belong to their queue.
//!
//! Never hold host state locks while waiting. Write callbacks must not synchronously
//! submit another write to this queue. Ordering is process-local; no file lock is taken.
//! Refused-load protection is opt-in: call [`OrderedWriteQueue::refuse_persist`] after
//! an unsafe load & check [`OrderedWriteQueue::persist_refused`] inside each write.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

#[derive(Debug)]
struct Order {
    next: u64,
    turn: u64,
    cancelled: BTreeSet<u64>,
}

/// A queue-bound write turn. Dropping it releases or cancels its turn automatically.
#[must_use = "dropping a ticket cancels its write"]
#[derive(Debug)]
pub struct WriteTicket<'a> {
    queue: &'a OrderedWriteQueue,
    sequence: u64,
}

impl Drop for WriteTicket<'_> {
    fn drop(&mut self) {
        let mut order = self.queue.order.lock().unwrap_or_else(|e| e.into_inner());
        if order.turn == self.sequence {
            order.turn += 1;
            loop {
                let turn = order.turn;
                if !order.cancelled.remove(&turn) {
                    break;
                }
                order.turn += 1;
            }
            self.queue.turn_cv.notify_all();
        } else {
            order.cancelled.insert(self.sequence);
        }
    }
}

/// Serializes write callbacks in ticket order, including error & unwind paths.
#[derive(Debug)]
pub struct OrderedWriteQueue {
    order: Mutex<Order>,
    turn_cv: Condvar,
    persist_refused: AtomicBool,
}

impl OrderedWriteQueue {
    pub const fn new() -> Self {
        Self {
            order: Mutex::new(Order {
                next: 0,
                turn: 0,
                cancelled: BTreeSet::new(),
            }),
            turn_cv: Condvar::new(),
            persist_refused: AtomicBool::new(false),
        }
    }

    /// Reserve a turn while the host state lock still establishes mutation order.
    ///
    /// Panics if all representable ticket numbers have been issued.
    pub fn take_ticket(&self) -> WriteTicket<'_> {
        let mut order = self.order.lock().unwrap_or_else(|e| e.into_inner());
        let sequence = order.next;
        order.next = sequence.checked_add(1).expect("write ticket space exhausted");
        WriteTicket {
            queue: self,
            sequence,
        }
    }

    /// Wait for earlier tickets, then invoke `write` without holding queue locks.
    /// The turn releases when `write` returns or unwinds.
    ///
    /// Panics if `ticket` came from another queue. Never wait under host state locks
    /// or submit another synchronous write from this callback.
    pub fn write_in_turn<E>(
        &self,
        ticket: WriteTicket<'_>,
        write: impl FnOnce() -> Result<(), E>,
    ) -> Result<(), E> {
        assert!(std::ptr::eq(self, ticket.queue), "ticket belongs to another queue");
        {
            let mut order = self.order.lock().unwrap_or_else(|e| e.into_inner());
            while order.turn != ticket.sequence {
                order = self.turn_cv.wait(order).unwrap_or_else(|e| e.into_inner());
            }
        }
        let result = write();
        drop(ticket);
        result
    }

    /// Permanently mark an unsafe load; hosts must refuse subsequent persistence.
    pub fn refuse_persist(&self) {
        self.persist_refused.store(true, Ordering::SeqCst);
    }

    /// Whether persistence has been refused following an unsafe load.
    pub fn persist_refused(&self) -> bool {
        self.persist_refused.load(Ordering::SeqCst)
    }
}

impl Default for OrderedWriteQueue {
    fn default() -> Self {
        Self::new()
    }
}
