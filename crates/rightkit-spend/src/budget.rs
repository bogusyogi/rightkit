//! Caller and process budget scopes: a ceiling on the spend counted against one key, checked
//! after the allowance caps and never loosening them.
//!
//! A budget counts the reservations tagged with its kind and key: an open one at its estimate, a
//! committed one at its actual, a released or expired one at nothing (the caps' accounting).
//! The limit is the total the key may hold, so a caller that only knows what it has left passes
//! that as the limit together with a key whose earlier spend it has not already subtracted.

use super::*;

/// Which budget scope a key belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetKind {
    /// A budget a caller hands with one request ([`ReserveOptions::caller_budget`]); the key
    /// is usually the job or request family whose spend it bounds.
    Caller,
    /// A budget for a whole run of this process ([`Ledger::set_process_budget`]); the key names
    /// the run, so every job reserved under it shares the limit.
    Process,
}

impl BudgetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BudgetKind::Caller => "caller",
            BudgetKind::Process => "process",
        }
    }
}

/// A budget: a key and the most micro-units reservations tagged with it may hold in total.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Budget {
    key: String,
    limit_micro: i64,
}

impl Budget {
    /// A budget for `key` (1 to 128 characters, no control characters) with a non-negative
    /// limit.
    pub fn new(key: impl Into<String>, limit_micro: i64) -> Result<Self> {
        let key = key.into().trim().to_owned();
        if key.is_empty() || key.chars().count() > 128 || key.chars().any(char::is_control) {
            return Err(SpendError::BadInput(
                "budget key must be 1 to 128 characters with no control characters".into(),
            ));
        }
        if limit_micro < 0 {
            return Err(SpendError::BadInput("budget limit is negative".into()));
        }
        Ok(Self { key, limit_micro })
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn limit_micro(&self) -> i64 {
        self.limit_micro
    }
}

/// What the ledger holds against one budget key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetSpend {
    pub kind: BudgetKind,
    pub key: String,
    /// Estimates of open reservations tagged with the key.
    pub reserved_micro: i64,
    /// Actuals of committed reservations tagged with the key.
    pub committed_micro: i64,
}

impl BudgetSpend {
    /// `reserved + committed`: what a budget counts.
    pub fn counted_micro(&self) -> i64 {
        self.reserved_micro.saturating_add(self.committed_micro)
    }
}

fn spend_of(conn: &Connection, kind: BudgetKind, key: &str) -> Result<BudgetSpend> {
    let (reserved, committed): (i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(CASE WHEN r.state IN ('reserved', 'in_flight') \
                              THEN r.estimated_micro ELSE 0 END), 0), \
                COALESCE(SUM(CASE WHEN r.state = 'committed' \
                              THEN r.committed_micro ELSE 0 END), 0) \
         FROM spend_budget_tags t JOIN spend_reservations r ON r.seq = t.seq \
         WHERE t.kind = ?1 AND t.key = ?2",
        params![kind.as_str(), key],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(BudgetSpend {
        kind,
        key: key.to_owned(),
        reserved_micro: reserved,
        committed_micro: committed,
    })
}

/// Refuse when `used + needed` is not within the budget. Written so an overflow refuses.
fn check(conn: &Connection, kind: BudgetKind, budget: &Budget, needed_micro: i64) -> Result<()> {
    let used = spend_of(conn, kind, budget.key())?.counted_micro();
    let fits = used
        .checked_add(needed_micro)
        .is_some_and(|total| total <= budget.limit_micro());
    if fits {
        Ok(())
    } else {
        Err(SpendError::BudgetExceeded {
            kind: kind.as_str(),
            key: budget.key().to_owned(),
            needed_micro,
            remaining_micro: budget.limit_micro().saturating_sub(used).max(0),
        })
    }
}

/// Check the request's caller budgets and the process budget for a new reservation, then tag
/// the reservation with their keys. Nothing is tagged unless every budget admits it. A key that
/// is both a caller and the process budget is tagged under each kind.
pub(crate) fn apply_budgets(
    conn: &Connection,
    ctx: &Ctx,
    options: &ReserveOptions,
    res: &Reservation,
) -> Result<()> {
    let mut scopes: Vec<(BudgetKind, &Budget)> = options
        .budget
        .iter()
        .map(|b| (BudgetKind::Caller, b))
        .collect();
    if let Some(process) = &ctx.process {
        scopes.push((BudgetKind::Process, process));
    }
    for (kind, budget) in &scopes {
        check(conn, *kind, budget, res.estimate_micro)?;
    }
    let seq = seq_of(&res.reservation_id)?;
    for (kind, budget) in &scopes {
        conn.execute(
            "INSERT OR REPLACE INTO spend_budget_tags (seq, kind, key) VALUES (?1, ?2, ?3)",
            params![seq, kind.as_str(), budget.key()],
        )?;
    }
    Ok(())
}

impl Ledger {
    /// What the ledger holds against a budget key: open estimates and committed actuals of the
    /// reservations tagged with it. Zero for a key never used.
    pub fn budget_spend(&self, kind: BudgetKind, key: &str) -> Result<BudgetSpend> {
        non_empty("key", key)?;
        spend_of(&self.conn, kind, key)
    }

    /// Advisory early check with no reservation: would `estimate_micro` fit `budget` given what
    /// its key already holds?
    pub fn check_budget(
        &self,
        kind: BudgetKind,
        budget: &Budget,
        estimate_micro: i64,
    ) -> Result<()> {
        if estimate_micro < 0 {
            return Err(SpendError::BadInput("estimate_micro is negative".into()));
        }
        check(&self.conn, kind, budget, estimate_micro)
    }
}
