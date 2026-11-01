//! Atomic budget shared by every session in one delegation lineage.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const MICRO_USD_PER_USD: f64 = 1_000_000.0;

#[derive(Debug)]
pub struct LineageBudget {
    ceiling_microusd: Option<u64>,
    spent_microusd: AtomicU64,
    remaining_microusd: AtomicU64,
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
            reconciled: false,
        })
    }

    fn add_spent(&self, amount: u64) {
        let _ = self
            .spent_microusd
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(current.saturating_add(amount))
            });
    }

    fn release(&self, amount: u64) {
        let ceiling = self.ceiling_microusd.unwrap_or(0);
        let _ =
            self.remaining_microusd
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    Some(current.saturating_add(amount).min(ceiling))
                });
    }
}

pub struct LineageReservation {
    budget: Option<Arc<LineageBudget>>,
    reserved_microusd: u64,
    reconciled: bool,
}

impl LineageReservation {
    fn unmetered() -> Self {
        Self {
            budget: None,
            reserved_microusd: 0,
            reconciled: false,
        }
    }

    pub fn reconcile_microusd(mut self, actual_microusd: u64) {
        if let Some(budget) = &self.budget {
            // A reservation is the hard lineage ceiling. Providers may report
            // an estimate above the held amount; never let that overrun turn
            // into unbounded spent accounting or a negative refund.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_reservation_is_exclusive_and_reconciliation_releases_remainder() {
        let budget = LineageBudget::from_usd(Some(1.0), 0.0);
        let first = budget.reserve_remaining().unwrap();
        assert!(budget.reserve_remaining().is_err());

        first.reconcile_microusd(250_000);
        assert_eq!(budget.spent_microusd(), 250_000);
        assert_eq!(budget.remaining_microusd(), Some(750_000));
        assert!(budget.reserve_remaining().is_ok());
    }

    #[test]
    fn dropped_and_unlimited_reservations_preserve_semantics() {
        let budget = LineageBudget::from_usd(Some(1.0), 400_000.0 / MICRO_USD_PER_USD);
        drop(budget.reserve_remaining().unwrap());
        assert_eq!(budget.remaining_microusd(), Some(600_000));

        let unlimited = LineageBudget::unlimited();
        unlimited
            .reserve_remaining()
            .unwrap()
            .reconcile_microusd(u64::MAX);
        assert_eq!(unlimited.remaining_microusd(), None);
        assert_eq!(unlimited.spent_microusd(), 0);
    }

    #[test]
    fn restored_spend_is_enforced_after_restart() {
        let restored = LineageBudget::from_usd(Some(0.5), 0.5);
        assert!(restored.reserve_remaining().is_err());
    }
}
