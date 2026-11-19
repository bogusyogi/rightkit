//! Retained stop-pin bookkeeping: pure, no COM, generic over the element type so
//! the tests drive it with a plain value instead of an `El`.
//!
//! Owns the policy that the worker used to scatter through `take_pruned`:
//!
//! * a pin is usable only if its token names THIS handle and THIS generation
//!   AND the pin is still in the map (`PinStore::usable`);
//! * TTL victims are removed at request time or on the idle tick (`take_expired`),
//!   never because of capacity;
//! * capacity eviction happens only when a NEW pin is inserted (`insert_new`),
//!   and never evicts the pin being inserted or any pin a request is using;
//! * nothing here drops an element that came out of the map: callers hand
//!   victims to a supervised release window (also on a clean close), or leak
//!   them. Leaks are counted process-wide in `LeakLedger`: at abandonment time
//!   for a generation that was abandoned (`count_abandoned`, from the
//!   supervisor's own count of the pins that generation owned), at the moment of
//!   the leak otherwise. A `PinStore` dropped while it still holds elements
//!   (unwinding) leaks and counts them instead of releasing a COM proxy on an
//!   unsupervised thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::supervisor::{make_token, token_generation, token_handle, MAX_SEQ};

/// Process-wide count of COM references deliberately leaked (never released),
/// and the switch that stops new pins once too many are held. The count never
/// decreases: a forgotten reference cannot be recovered, so the only way back
/// under the cap is a process restart.
pub(crate) struct LeakLedger {
    count: AtomicUsize,
    suspension_reported: AtomicBool,
}

/// The ledger the real worker uses. Tests build their own.
pub(crate) static GLOBAL_LEDGER: LeakLedger = LeakLedger::new();

impl LeakLedger {
    pub(crate) const fn new() -> Self {
        Self {
            count: AtomicUsize::new(0),
            suspension_reported: AtomicBool::new(false),
        }
    }

    /// Count `count` references that were forgotten at the moment their worker
    /// generation was abandoned (the supervisor knows how many it owned). The
    /// thread, if it ever returns, forgets them without counting them again.
    pub(crate) fn count_abandoned(&self, count: usize) {
        self.count.fetch_add(count, Ordering::AcqRel);
    }

    /// Leak `items` (never run their `Drop`) and count them.
    pub(crate) fn leak<T>(&self, items: Vec<T>) {
        self.count.fetch_add(items.len(), Ordering::AcqRel);
        std::mem::forget(items);
    }

    /// Leak one value and count it.
    pub(crate) fn leak_one<T>(&self, item: T) {
        self.count.fetch_add(1, Ordering::AcqRel);
        std::mem::forget(item);
    }

    pub(crate) fn leaked(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    /// True when `cap` or more references are held leaked: new pins are then
    /// refused. `cap == 0` is treated as 1.
    pub(crate) fn suspended(&self, cap: usize) -> bool {
        self.leaked() >= cap.max(1)
    }

    /// True once, for the first caller after construction: the event sink is
    /// told the suspension started, not on every refused capture.
    pub(crate) fn report_suspension_once(&self) -> bool {
        !self.suspension_reported.swap(true, Ordering::AcqRel)
    }

    /// Allow the next suspension to be reported again. Called when a new handle
    /// is created and whenever a capture finds the ledger under its cap, so a
    /// later suspension episode (or a handle with a different cap) is not
    /// silent. The count itself never decreases: a leaked reference is forgotten
    /// for good.
    pub(crate) fn rearm_suspension_report(&self) {
        self.suspension_reported.store(false, Ordering::Release);
    }
}

/// Does `token` name this handle and this generation? (Not whether the pin
/// exists: see `PinStore::usable`.)
pub(crate) fn token_belongs(token: u64, handle: u64, generation: u64) -> bool {
    token_handle(token) == handle && token_generation(token) == generation
}

/// Expiry instant of a pin captured at `captured_at`; `None` when the sum is
/// unrepresentable (the pin then never expires by TTL; capacity still bounds it).
fn expires_at(captured_at: Instant, ttl: Duration) -> Option<Instant> {
    captured_at.checked_add(ttl)
}

pub(crate) struct Pinned<V> {
    pub(crate) element: V,
    pub(crate) captured_at: Instant,
}

pub(crate) struct PinStore<V> {
    handle: u64,
    generation: u64,
    next_seq: u64,
    map: HashMap<u64, Pinned<V>>,
    /// Capacity victims waiting for a supervised release window.
    evicted: Vec<Pinned<V>>,
    ledger: &'static LeakLedger,
}

impl<V> PinStore<V> {
    pub(crate) fn new(handle: u64, generation: u64, ledger: &'static LeakLedger) -> Self {
        Self {
            handle,
            generation,
            next_seq: 1,
            map: HashMap::new(),
            evicted: Vec::new(),
            ledger,
        }
    }

    #[allow(dead_code)] // asserted by the unit tests
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    #[cfg(test)]
    pub(crate) fn pending_eviction(&self) -> usize {
        self.evicted.len()
    }

    /// Token names this store's handle and generation.
    pub(crate) fn owns(&self, token: u64) -> bool {
        token_belongs(token, self.handle, self.generation)
    }

    /// THE gate for every pin request: the token is ours AND the pin still
    /// exists. A pin that was released, TTL-expired or capacity-evicted is not
    /// usable even when the live focus would match its identity.
    pub(crate) fn usable(&self, token: u64) -> bool {
        self.owns(token) && self.map.contains_key(&token)
    }

    pub(crate) fn get(&self, token: u64) -> Option<&Pinned<V>> {
        if self.owns(token) {
            self.map.get(&token)
        } else {
            None
        }
    }

    /// Next token for this generation, or `None` once its sequence is spent
    /// (the capture is refused rather than a token reused).
    pub(crate) fn mint(&mut self) -> Option<u64> {
        if self.next_seq > MAX_SEQ {
            return None;
        }
        let token = make_token(self.handle, self.generation, self.next_seq)?;
        self.next_seq += 1;
        Some(token)
    }

    /// Insert a NEW pin. If the store is at `max` (floored at 1), the oldest
    /// pins move to the eviction list first, so the new pin is never the victim
    /// and no existing pin is touched by anything except a capture.
    pub(crate) fn insert_new(&mut self, token: u64, element: V, now: Instant, max: usize) {
        let max = max.max(1);
        while self.map.len() >= max {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(token, pin)| (pin.captured_at, **token))
                .map(|(token, _)| *token);
            match oldest.and_then(|token| self.map.remove(&token)) {
                Some(victim) => self.evicted.push(victim),
                None => break,
            }
        }
        self.map.insert(
            token,
            Pinned {
                element,
                captured_at: now,
            },
        );
    }

    pub(crate) fn remove(&mut self, token: u64) -> Option<Pinned<V>> {
        if self.owns(token) {
            self.map.remove(&token)
        } else {
            None
        }
    }

    /// TTL victims only (`now >= captured_at + ttl`), removed from the map and
    /// returned. Capacity plays no part here.
    pub(crate) fn take_expired(&mut self, now: Instant, ttl: Duration) -> Vec<Pinned<V>> {
        let stale: Vec<u64> = self
            .map
            .iter()
            .filter(|(_, pin)| expires_at(pin.captured_at, ttl).is_some_and(|at| now >= at))
            .map(|(token, _)| *token)
            .collect();
        stale
            .into_iter()
            .filter_map(|token| self.map.remove(&token))
            .collect()
    }

    /// Capacity victims collected by `insert_new`.
    pub(crate) fn take_evicted(&mut self) -> Vec<Pinned<V>> {
        std::mem::take(&mut self.evicted)
    }

    /// Earliest instant a pin expires by TTL; `None` when the store is empty or
    /// no pin can expire. The idle worker sleeps until then.
    pub(crate) fn next_expiry(&self, ttl: Duration) -> Option<Instant> {
        self.map
            .values()
            .filter_map(|pin| expires_at(pin.captured_at, ttl))
            .min()
    }

    /// Everything still held, for the exit release window.
    pub(crate) fn drain_all(&mut self) -> Vec<Pinned<V>> {
        let mut all: Vec<Pinned<V>> = self.map.drain().map(|(_, pin)| pin).collect();
        all.append(&mut self.evicted);
        all
    }
}

impl<V> Drop for PinStore<V> {
    /// Reached with elements still held only on unwinding or when an abandoned
    /// thread finally returns: leak them (counted) rather than `Release` a COM
    /// proxy on a thread nothing supervises.
    fn drop(&mut self) {
        let rest = self.drain_all();
        if !rest.is_empty() {
            self.ledger.leak(rest);
        }
    }
}

/// Releases that could not be queued (the worker's queue was full). The worker
/// drains them as soon as it is back in its loop, so a dropped release is
/// retried at once, not at the TTL. Bounded: beyond `cap` the TTL prune stays
/// the fallback.
#[derive(Default)]
pub(crate) struct PendingReleases {
    tokens: Vec<u64>,
}

impl PendingReleases {
    /// False when `token` was already pending or the list is full.
    pub(crate) fn push(&mut self, token: u64, cap: usize) -> bool {
        if self.tokens.contains(&token) || self.tokens.len() >= cap.max(1) {
            return false;
        }
        self.tokens.push(token);
        true
    }

    /// Tokens of this handle and generation; every other entry is stale and is
    /// dropped (its pin went with its worker).
    pub(crate) fn take_for(&mut self, handle: u64, generation: u64) -> Vec<u64> {
        let all = std::mem::take(&mut self.tokens);
        all.into_iter()
            .filter(|token| token_belongs(*token, handle, generation))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.tokens.len()
    }
}
