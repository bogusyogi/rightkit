//! A store in the process's memory: nothing on disk can approve, and a restart forgets every
//! request and grant.

use std::sync::{Mutex, MutexGuard};

use crate::error::{Refusal, Result};
use crate::model::{Change, Record, State};
use crate::store::Store;

/// How much a [`MemoryStore`] keeps. Every access sweeps first: a pending request past its
/// expiry and an approval whose grant ended are dropped, a closed one (denied, failed,
/// expired) stays visible for `closed_visible_ms`, and past each cap the oldest go first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retention {
    /// Most pending records kept.
    pub max_pending: usize,
    /// Most approved or closed records kept.
    pub max_decided: usize,
    /// Most used records kept, so a used id is refused as used rather than unknown.
    pub max_done: usize,
    /// How long a closed record stays readable after its last move.
    pub closed_visible_ms: u64,
}

/// Records held in memory behind one lock. `const`-constructible, so it can be a `static`.
pub struct MemoryStore {
    records: Mutex<Vec<Record>>,
    retention: Retention,
}

impl MemoryStore {
    pub const fn new(retention: Retention) -> MemoryStore {
        MemoryStore {
            records: Mutex::new(Vec::new()),
            retention,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Record>> {
        self.records.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn swept(&self, now_ms: u64) -> MutexGuard<'_, Vec<Record>> {
        let mut records = self.lock();
        sweep(&mut records, &self.retention, now_ms);
        records
    }

    /// Every record still kept at `now_ms`, oldest first.
    pub fn records(&self, now_ms: u64) -> Vec<Record> {
        self.swept(now_ms).clone()
    }

    /// Drops every record `remove` selects; returns how many. Only ever reduces trust.
    pub fn remove_where(&self, remove: impl Fn(&Record) -> bool) -> usize {
        let mut records = self.lock();
        let before = records.len();
        records.retain(|r| !remove(r));
        before - records.len()
    }

    /// Changes the grant-carrying record `id` in place while it is approved (for example an
    /// expiry the decider chose after approving). `false` when there is no such record.
    pub fn update_approved(&self, id: &str, update: impl FnOnce(&mut Record)) -> bool {
        let mut records = self.lock();
        match records
            .iter_mut()
            .find(|r| r.request.id == id && r.state == State::Approved)
        {
            Some(record) => {
                update(record);
                true
            }
            None => false,
        }
    }
}

fn sweep(records: &mut Vec<Record>, keep: &Retention, now_ms: u64) {
    records.retain(|r| match r.state {
        State::Pending | State::Approved => r.live(now_ms),
        State::Done => true,
        _ => now_ms.saturating_sub(r.decided_ms) <= keep.closed_visible_ms,
    });
    let cap = |records: &mut Vec<Record>, select: fn(State) -> bool, max: usize| {
        let mut over = records.iter().filter(|r| select(r.state)).count();
        records.retain(|r| {
            if over > max && select(r.state) {
                over -= 1;
                false
            } else {
                true
            }
        });
    };
    cap(records, |s| s == State::Pending, keep.max_pending);
    cap(
        records,
        |s| !matches!(s, State::Pending | State::Done),
        keep.max_decided,
    );
    cap(records, |s| s == State::Done, keep.max_done);
}

impl Store for MemoryStore {
    fn insert(&self, record: &Record) -> Result<()> {
        let mut records = self.lock();
        if records.iter().any(|r| r.request.id == record.request.id) {
            return Err(Refusal::new(
                "approval_invalid",
                "that approval id is taken",
            ));
        }
        records.push(record.clone());
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Option<Record>> {
        let now = now_ms();
        Ok(self.swept(now).iter().find(|r| r.request.id == id).cloned())
    }

    fn pending(&self, now_ms: u64) -> Result<Vec<Record>> {
        Ok(self
            .swept(now_ms)
            .iter()
            .filter(|r| r.state == State::Pending)
            .cloned()
            .collect())
    }

    fn approved(&self, now_ms: u64) -> Result<Vec<Record>> {
        Ok(self
            .swept(now_ms)
            .iter()
            .filter(|r| r.state == State::Approved)
            .cloned()
            .collect())
    }

    fn transition(&self, id: &str, from: State, change: &Change) -> Result<bool> {
        let mut records = self.lock();
        match records
            .iter_mut()
            .find(|r| r.request.id == id && r.state == from)
        {
            Some(record) => {
                change.apply(record);
                // A moved record goes to the end, so caps drop the oldest moves first.
                if let Some(at) = records.iter().position(|r| r.request.id == id) {
                    let moved = records.remove(at);
                    records.push(moved);
                }
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn touch(&self, id: &str, at_ms: u64) -> Result<()> {
        if let Some(record) = self.lock().iter_mut().find(|r| r.request.id == id) {
            record.last_used_ms = at_ms;
        }
        Ok(())
    }
}

/// Unix milliseconds now; 0 when the clock is before 1970.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
