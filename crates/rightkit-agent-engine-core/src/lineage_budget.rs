//! Atomic budget shared by every session in one delegation lineage.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const MICRO_USD_PER_USD: f64 = 1_000_000.0;

#[derive(Debug)]
pub struct LineageBudget {
    ceiling_microusd: Option<u64>,
    spent_microusd: AtomicU64,
    remaining_microusd: AtomicU64,
    /// Woken whenever a reservation is reconciled or dropped, so a lineage
    /// member that found the balance held by a sibling can try again.
    released: tokio::sync::Notify,
}

impl LineageBudget {
    pub fn from_usd(ceiling_usd: Option<f64>, spent_usd: f64) -> Arc<Self> {
        let ceiling_microusd = ceiling_usd.and_then(usd_to_microusd);
        let spent_microusd = usd_to_microusd(spent_usd).unwrap_or(0);
        let remaining_microusd = ceiling_microusd
            .map(|ceiling| ceiling.saturating_sub(spent_microusd))
            .unwrap_or(0);
        Arc::new(Self {
            ceiling_microusd,
            spent_microusd: AtomicU64::new(spent_microusd),
            remaining_microusd: AtomicU64::new(remaining_microusd),
            released: tokio::sync::Notify::new(),
        })
    }

    pub fn unlimited() -> Arc<Self> {
        Self::from_usd(None, 0.0)
    }

    pub fn ceiling_microusd(&self) -> Option<u64> {
        self.ceiling_microusd
    }

    pub fn spent_microusd(&self) -> u64 {
        self.spent_microusd.load(Ordering::Acquire)
    }

    pub fn remaining_microusd(&self) -> Option<u64> {
        self.ceiling_microusd
            .map(|_| self.remaining_microusd.load(Ordering::Acquire))
    }

    /// Reserve the entire remaining metered balance. Provider calls use this
    /// conservative form because no supported provider offers an enforceable
    /// request-level dollar cap; holding less could let concurrent siblings
    /// collectively cross the lineage ceiling.
    pub fn reserve_remaining(self: &Arc<Self>) -> Result<LineageReservation, String> {
        let Some(_) = self.ceiling_microusd else {
            return Ok(LineageReservation::unmetered());
        };
        let available = self.remaining_microusd.swap(0, Ordering::AcqRel);
        if available == 0 {
            return Err("lineage budget exhausted".to_string());
        }
        Ok(LineageReservation {
            budget: Some(self.clone()),
            reserved_microusd: available,
            partial: false,
            reconciled: false,
        })
    }

    /// Like [`Self::reserve_remaining`], but when the balance is only held by
    /// a sibling's in-flight call (a parent and its background sub-agents
    /// share one lineage), wait for that reservation to be released instead
    /// of failing. Fails only when the ceiling is really spent. The ceiling
    /// stays exact: lineage members' metered calls take turns instead of
    /// running against a shared, unbounded balance (2026-11-07 regression:
    /// every concurrent background child failed "lineage budget exhausted").
    pub async fn reserve_remaining_wait(self: &Arc<Self>) -> Result<LineageReservation, String> {
        let Some(ceiling) = self.ceiling_microusd else {
            return Ok(LineageReservation::unmetered());
        };
        loop {
            // Register for the wakeup before checking, so a release between
            // the check and the wait is not lost.
            let notified = self.released.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.spent_microusd() >= ceiling {
                return Err("lineage budget exhausted".to_string());
            }
            match self.reserve_remaining() {
                Ok(reservation) => return Ok(reservation),
                Err(_) => notified.await,
            }
        }
    }

    /// Reserve `estimate_microusd` (capped at the balance) so lineage members' calls overlap
    /// while the balance covers them. Waits like [`Self::reserve_remaining_wait`] when siblings
    /// hold the balance; fails only when the ceiling is really spent. The reservation is
    /// reconciled to the real cost, which may exceed the estimate.
    pub async fn reserve_estimate_wait(
        self: &Arc<Self>,
        estimate_microusd: u64,
    ) -> Result<LineageReservation, String> {
        let Some(ceiling) = self.ceiling_microusd else {
            return Ok(LineageReservation::unmetered());
        };
        let estimate = estimate_microusd.max(1);
        loop {
            let notified = self.released.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.spent_microusd() >= ceiling {
                return Err("lineage budget exhausted".to_string());
            }
            if let Some((held, whole)) = self.take_up_to(estimate) {
                return Ok(LineageReservation {
                    budget: Some(self.clone()),
                    reserved_microusd: held,
                    partial: !whole,
                    reconciled: false,
                });
            }
            notified.await;
        }
    }

    /// Take up to `estimate` from the remaining balance in one atomic step. Returns what was
    /// taken and whether it was the whole balance, or `None` when nothing is left.
    fn take_up_to(&self, estimate: u64) -> Option<(u64, bool)> {
        let mut current = self.remaining_microusd.load(Ordering::Acquire);
        loop {
            if current == 0 {
                return None;
            }
            let held = current.min(estimate);
            match self.remaining_microusd.compare_exchange_weak(
                current,
                current - held,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some((held, held == current)),
                Err(seen) => current = seen,
            }
        }
    }

    fn add_spent(&self, amount: u64) {
        update_atomic(&self.spent_microusd, |current| {
            current.saturating_add(amount)
        });
    }

    fn take_overrun(&self, amount: u64) {
        update_atomic(&self.remaining_microusd, |current| {
            current.saturating_sub(amount)
        });
        self.released.notify_waiters();
    }

    fn release(&self, amount: u64) {
        let ceiling = self.ceiling_microusd.unwrap_or(0);
        update_atomic(&self.remaining_microusd, |current| {
            current.saturating_add(amount).min(ceiling)
        });
        self.released.notify_waiters();
    }
}

/// Replace `cell` with `next(current)` atomically, retrying when another thread changed it first.
/// `next` always yields a value, so the update itself cannot be refused.
fn update_atomic(cell: &AtomicU64, next: impl Fn(u64) -> u64) {
    let mut current = cell.load(Ordering::Acquire);
    loop {
        match cell.compare_exchange_weak(
            current,
            next(current),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(seen) => current = seen,
        }
    }
}

pub struct LineageReservation {
    budget: Option<Arc<LineageBudget>>,
    reserved_microusd: u64,
    /// True when only an estimate was held (the balance was larger): a call that costs more
    /// than the estimate is charged in full instead of being clamped to what was held.
    partial: bool,
    reconciled: bool,
}

impl LineageReservation {
    fn unmetered() -> Self {
        Self {
            budget: None,
            reserved_microusd: 0,
            partial: false,
            reconciled: false,
        }
    }

    pub fn reconcile_microusd(mut self, actual_microusd: u64) {
        if let Some(budget) = &self.budget {
            // A reservation is the hard lineage ceiling. Providers may report
            // an estimate above the held amount; never let that overrun turn
            // into unbounded spent accounting or a negative refund.
            if self.partial && actual_microusd > self.reserved_microusd {
                // Only an estimate was held: the real cost is charged in full.
                budget.add_spent(actual_microusd);
                budget.take_overrun(actual_microusd - self.reserved_microusd);
                self.reconciled = true;
                return;
            }
            let charged = actual_microusd.min(self.reserved_microusd);
            budget.add_spent(charged);
            budget.release(self.reserved_microusd - charged);
        }
        self.reconciled = true;
    }
}

impl Drop for LineageReservation {
    fn drop(&mut self) {
        if self.reconciled {
            return;
        }
        if let Some(budget) = &self.budget {
            budget.release(self.reserved_microusd);
        }
    }
}

pub fn usd_to_microusd(value: f64) -> Option<u64> {
    (value.is_finite() && value > 0.0).then(|| {
        (value * MICRO_USD_PER_USD)
            .round()
            .clamp(1.0, u64::MAX as f64) as u64
    })
}
