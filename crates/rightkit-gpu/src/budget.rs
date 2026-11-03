//! Device memory shared by concurrent GPU jobs (upstream issue #1764). Adapted from Ember `crates/gpu`.
//!
//! Each job asks for room for what it is expected to hold on the device and waits while the
//! renders in flight leave too little; a render always gets in when nothing else is in flight,
//! however large it is (the device's own limits still apply). wgpu reports no memory size, so the
//! budget starts from the largest buffer the device allows and adapts to the device it meets:
//! it is halved when a render runs out of device memory and grows back a step with each render
//! that succeeds (the additive-increase / multiplicative-decrease rule of TCP congestion control).
//! A 2 GB card settles near what it holds instead of failing every render past its memory.
//! Renders get in in the order they asked (a fair semaphore): a large preview is not passed over
//! for ever by a stream of thumbnails that each fit.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

/// The budget never shrinks below this: one render always fits anyway, so this only bounds how
/// far a run of failures can push it.
const FLOOR: u64 = 256 << 20;

/// Device memory a render is expected to hold: the source, two working copies and the output,
/// as float RGBA, at the source's size.
pub fn footprint(width: usize, height: usize) -> u64 {
    (width as u64)
        .saturating_mul(height as u64)
        .saturating_mul(16 * 4)
}

/// Where the budget starts: 1.5 × the largest buffer the device allows (about the device's
/// memory on small cards), at least 1 GiB.
pub fn initial(max_buffer: u64) -> u64 {
    (max_buffer / 2).saturating_mul(3).max(1 << 30)
}

struct State {
    /// Bytes held by renders in flight.
    used: u64,
    /// Renders in flight.
    running: u32,
    /// A retry holds the device exclusively until its permit drops.
    exclusive: bool,
    /// Bytes renders may hold together now.
    limit: u64,
    /// What the budget grows back to.
    ceiling: u64,
    /// Tickets of the renders waiting, in the order they asked.
    queue: VecDeque<u64>,
    next_ticket: u64,
}

pub struct MemoryBudget {
    state: Mutex<State>,
    room: Condvar,
}

/// Room on the device for one render, released when dropped.
pub struct Permit<'a> {
    budget: &'a MemoryBudget,
    bytes: u64,
    alone: bool,
}

impl MemoryBudget {
    pub fn new(ceiling: u64) -> MemoryBudget {
        MemoryBudget {
            state: Mutex::new(State {
                used: 0,
                running: 0,
                exclusive: false,
                limit: ceiling,
                ceiling,
                queue: VecDeque::new(),
                next_ticket: 0,
            }),
            room: Condvar::new(),
        }
    }

    /// Wait for room for `bytes`; `alone` holds the device exclusively (the retry of a
    /// render that ran out of memory). `None` when `stop` turns true while waiting (the process is
    /// ending): the caller renders on the CPU.
    pub fn admit(&self, bytes: u64, alone: bool, stop: impl Fn() -> bool) -> Option<Permit<'_>> {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let ticket = st.next_ticket;
        st.next_ticket = st.next_ticket.wrapping_add(1);
        st.queue.push_back(ticket);
        while st.queue.front() != Some(&ticket)
            || st.exclusive
            || (st.running > 0 && (alone || st.used.saturating_add(bytes) > st.limit))
        {
            if stop() {
                st.queue.retain(|t| *t != ticket);
                // the render behind may be the next to fit
                self.room.notify_all();
                return None;
            }
            st = self
                .room
                .wait_timeout(st, Duration::from_millis(100))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        st.queue.pop_front();
        // the next in line may fit beside this one
        self.room.notify_all();
        st.used = st.used.saturating_add(bytes);
        st.running += 1;
        st.exclusive = alone;
        Some(Permit {
            budget: self,
            bytes,
            alone,
        })
    }

    /// A render ran out of device memory: halve the budget (not below what one render needs).
    pub fn out_of_memory(&self) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.limit = (st.limit / 2).max(FLOOR);
    }

    /// A render succeeded: grow the budget back by a sixteenth of its ceiling.
    pub fn succeeded(&self) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.limit = st.limit.saturating_add(st.ceiling / 16).min(st.ceiling);
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut st = self
            .budget
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        st.used = st.used.saturating_sub(self.bytes);
        st.running = st.running.saturating_sub(1);
        if self.alone {
            st.exclusive = false;
        }
        self.budget.room.notify_all();
    }
}
