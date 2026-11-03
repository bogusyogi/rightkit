//! `rightkit-spend` - a spend reservation, allowance and compare-and-swap ledger on SQLite.
//!
//! The ledger records what a paid call may spend before the call and what it actually cost
//! after. It makes no network call, holds no provider key and reads no price table: the caller
//! computes the estimate and passes the route and price version it used. Qualifying the price
//! (verified, recent) is the caller's check before [`Ledger::reserve`]; price rows belong to
//! `rightkit-llm`'s catalogue.
//!
//! Units: money is an integer count of micro-units of one currency (1 unit = 1 000 000
//! micro-units). An allowance holds no currency; a reservation may carry the currency it was
//! reserved in ([`ReserveOptions::currency`]), stored as given at reserve time and never
//! rewritten, so a later change of the caller's currency does not change what a settled
//! reservation reports. No float is used; every sum is checked and overflow is
//! [`SpendError::Overflow`].
//!
//! Allowances are keyed by an opaque `scope` and an opaque `period` string, so a daily cap
//! (`2026-10-17`), a monthly cap (`2026-10`) and a per-device offline allowance
//! (`device:dev_abc`) are all allowances of the same kind. A call that must pass several caps
//! reserves against each (one request id per allowance) and releases the earlier ones if a
//! later one is refused. A reservation may also carry an opaque `account` key
//! ([`ReserveOptions::account`]); [`Ledger::expire_stale_account`] settles the stale leases of
//! one account only.
//!
//! Reservation life: `reserved` -> `in_flight` (just before the wire request) -> `committed`
//! (actual recorded), or -> `released` (caller gives the estimate back), or -> `expired`
//! (a stale `reserved` lease). An `in_flight` lease whose outcome is unknown is never released:
//! [`Ledger::commit_unknown`] (and an expired in-flight lease) commits it at its ceiling (the
//! estimate), flagged `billed_unknown` and as an overrun, because the request may have been
//! sent.
//!
//! Compare-and-swap: every change to an allowance's totals or limit is an `UPDATE ... WHERE
//! revision = ?` inside one `BEGIN IMMEDIATE` transaction; a lost swap is
//! [`SpendError::RevisionConflict`].
//!
//! Overrun: a commit whose actual exceeds its estimate takes the difference from the allowance
//! and is flagged `over_reservation`; if the allowance cannot cover it the actual is still
//! recorded and `remaining` goes negative. A billed-unknown commit is flagged the same way.
//! While any flagged reservation of an allowance lacks an [`Ledger::acknowledge_overrun`],
//! [`Ledger::reserve`] refuses ([`SpendError::OverrunUnacknowledged`]). Who may acknowledge is
//! the app's decision.
//!
//! Caller transactions: every write has an `_in` variant that runs inside a transaction the
//! caller opened ([`Ledger::begin`]) and neither commits nor rolls it back, so the app's own
//! sidecar rows and the ledger change commit or roll back together. An `_in` call that fails
//! undoes its own partial writes (it runs in a savepoint) and leaves the caller's transaction
//! usable. A caller that holds only a `&Connection` (a store hook, for example) uses the `_on`
//! variants instead; the connection must already be in a transaction, and one in autocommit
//! mode is refused with [`SpendError::NoActiveTransaction`].
//!
//! Strict mode: by default a repeated `release`, `commit` with the same actual or
//! `mark_in_flight` is treated as a replay and returns the stored state. With
//! [`Ledger::with_strict`] they return [`SpendError::InvalidTransition`] instead, for callers
//! that must see a double settle as a bug. Replay of [`Ledger::reserve`] by request id is
//! unaffected: it is the idempotency key, not a transition.
//!
//! Carry-over: a reservation made with [`ReserveOptions::carry_over`] also counts the open
//! (`reserved`, `in_flight`) estimates of earlier periods of the same scope against the new
//! period's cap, so a reservation that outlives midnight keeps counting against the daily cap
//! until it is settled. "Earlier" is text order on a period string of the same length, so
//! periods of one kind must be fixed-width and sort chronologically (`2026-10-17`, `2026-10`).
//!
//! Several caps at once: [`Ledger::reserve_multi`] holds one estimate against a list of [`Cap`]s
//! (daily, monthly, an epoch-reset device allowance, ...) under one request id, all or nothing;
//! [`Ledger::commit_group`], [`Ledger::release_group`] and [`Ledger::mark_in_flight_group`]
//! settle every leg together. [`Ledger::set_device_allowance`] starts a new epoch (a fresh
//! allowance) on every set or clear, so spend before the reset no longer counts.
//!
//! Budget scopes: a [`Budget`] is a ceiling on one caller key ([`ReserveOptions::caller_budget`])
//! or on every reservation made through this ledger value ([`Ledger::set_process_budget`]). It
//! is checked after the caps and can only refuse more ([`SpendError::BudgetExceeded`]).
//!
//! Provider cost and invoices: [`Ledger::record_provider_reported`] and
//! [`Ledger::reconcile_invoice`] store the provider's and the invoice's figures beside the
//! ledger and move no cap. [`Ledger::adjust_actual`] (and `reconcile_invoice` with
//! `adjust_committed`) corrects a committed actual and the allowance total with it.
//!
//! Audit: every allowance create or limit change and every device-allowance set or clear writes
//! a row read back with [`Ledger::cap_audit`]. A stored limit that is not a finite non-negative
//! number reads as 0, so the allowance refuses every non-zero reservation (fail closed).
//!
//! Approved budgets: [`Ledger::create_approved_budget`], [`Ledger::approve_budget`] (owner
//! approval bound to [`approval_bind`], sealed with a [`SealKey`] kept in the OS credential
//! store), [`Ledger::reserve_approved`], idempotent [`Ledger::report_approved_spend`] and an
//! append-only [`Ledger::approved_audit`]; see the `approved` module docs.
//!
//! The ledger lives in the app's SQLite file under `spend_*` tables; its schema version is kept
//! in the table `rightkit_spend_meta`, so the app's own `user_version` is untouched. The daemon
//! single-writer policy is the app's: this crate serialises writers only through SQLite.

#![forbid(unsafe_code)]

use std::time::{SystemTime, UNIX_EPOCH};

use rightkit_db::rusqlite::{
    params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior,
};
use rightkit_db::{migrate, MigrateOptions, Migration, VersionStorage};

pub use rightkit_db::rusqlite;

mod approved;
mod audit;
mod budget;
mod group;
mod reconcile;
mod scope;
pub use approved::{approval_bind, ApprovedAudit, ApprovedBudget, ReportOutcome, SealKey};
pub use audit::CapAudit;
pub use budget::{Budget, BudgetKind, BudgetSpend};
pub use group::{Cap, DeviceAllowance, GroupOutcome};
pub use reconcile::{
    Invoice, InvoiceInput, InvoiceLine, InvoiceReport, MatchedLine, ProviderReported, UnmatchedLine,
};
pub use scope::{Scope, ScopeError};

pub type Result<T> = std::result::Result<T, SpendError>;

/// A ledger failure. The messages carry the amounts so a caller can show them.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SpendError {
    #[error(transparent)]
    Db(#[from] rightkit_db::DbError),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rightkit_db::rusqlite::Error),
    #[error("bad input: {0}")]
    BadInput(String),
    #[error("no allowance for scope {scope:?} and period {period:?}")]
    AllowanceMissing { scope: String, period: String },
    #[error("estimate {estimate_micro} exceeds the remaining allowance {remaining_micro}")]
    AllowanceExceeded {
        estimate_micro: i64,
        remaining_micro: i64,
    },
    #[error("limit {limit_micro} is below the spend already held or committed ({spent_micro})")]
    AllowanceBelowSpend { limit_micro: i64, spent_micro: i64 },
    /// `reservation_id` is the oldest overrun still unacknowledged on the allowance.
    #[error("{count} overrun(s) await acknowledgement before this allowance reserves again (oldest: {reservation_id})")]
    OverrunUnacknowledged { count: i64, reservation_id: String },
    #[error("allowance revision is {actual}, expected {expected}")]
    RevisionConflict { expected: i64, actual: i64 },
    #[error("allowance exists: an expected revision is required to change it")]
    RevisionRequired,
    #[error("request id {request_id:?} was already used with a different scope, period, account, currency, route, price version or estimate")]
    RequestMismatch { request_id: String },
    #[error("no reservation {0:?}")]
    NotFound(String),
    #[error("a {from} reservation cannot become {to}")]
    InvalidTransition {
        from: ReservationState,
        to: ReservationState,
    },
    #[error("amount overflow")]
    Overflow,
    /// A `_on` call was given a connection with no open transaction. Begin one (`BEGIN`, or
    /// `Connection::transaction`) first; the call would otherwise commit on its own.
    #[error("no transaction is open on the connection; begin one before a `_on` call")]
    NoActiveTransaction,
    /// A caller or process budget would be exceeded. `remaining_micro` is what the budget still
    /// allows after the spend already counted against its key (never negative).
    #[error("{kind} budget {key:?} would be exceeded (needed {needed_micro}, remaining {remaining_micro}); nothing was reserved")]
    BudgetExceeded {
        kind: &'static str,
        key: String,
        needed_micro: i64,
        remaining_micro: i64,
    },
    /// One cap of a [`Ledger::reserve_multi`] refused; nothing was reserved on any cap.
    #[error("cap {scope:?}/{period:?} refused: {cause}")]
    CapRefused {
        scope: String,
        period: String,
        #[source]
        cause: Box<SpendError>,
    },
    /// No approved budget with this id.
    #[error("no approved budget {budget_id:?}")]
    BudgetNotFound { budget_id: String },
    /// The approved budget exists with other facts than the call named.
    #[error("approved budget {budget_id:?} already exists with different facts")]
    ApprovedBudgetConflict { budget_id: String },
    /// The budget was not approved yet, so nothing may be reserved against it.
    #[error("approved budget {budget_id:?} is not approved")]
    NotApproved { budget_id: String },
    /// The budget changed since the owner approved it; nothing was approved.
    #[error(
        "approved budget {budget_id:?} is not the one the owner approved; nothing was approved"
    )]
    ApprovalChanged { budget_id: String },
    /// The stored seal does not verify under this key, or the allowance limit was raised above
    /// the approved limit. Nothing was reserved.
    #[error("the seal of approved budget {budget_id:?} does not verify; nothing was done")]
    SealInvalid { budget_id: String },
    /// The seal key could not be obtained. The text never holds key material.
    #[error("seal key unavailable: {0}")]
    SealKeyUnavailable(String),
    /// The report id was already recorded with another budget, reservation or actual.
    #[error(
        "report {report_id:?} was already recorded with a different budget, reservation or actual"
    )]
    ReportConflict { report_id: String },
    /// The invoice reference is already recorded with another provider, period or total.
    #[error("invoice reference {invoice_ref:?} is already recorded with a different provider, period or total")]
    InvoiceConflict { invoice_ref: String },
}

/// Where a reservation is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReservationState {
    Reserved,
    InFlight,
    Committed,
    Released,
    Expired,
}

impl ReservationState {
    pub fn as_str(self) -> &'static str {
        match self {
            ReservationState::Reserved => "reserved",
            ReservationState::InFlight => "in_flight",
            ReservationState::Committed => "committed",
            ReservationState::Released => "released",
            ReservationState::Expired => "expired",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "reserved" => ReservationState::Reserved,
            "in_flight" => ReservationState::InFlight,
            "committed" => ReservationState::Committed,
            "released" => ReservationState::Released,
            "expired" => ReservationState::Expired,
            _ => return None,
        })
    }

    /// An open reservation still holds its estimate in the allowance's `reserved` total.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            ReservationState::Reserved | ReservationState::InFlight
        )
    }
}

impl std::fmt::Display for ReservationState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One allowance: a limit and the totals held against it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allowance {
    pub scope: String,
    pub period: String,
    pub limit_micro: i64,
    /// Estimates of open (`reserved` and `in_flight`) reservations.
    pub reserved_micro: i64,
    /// Actuals of committed reservations.
    pub committed_micro: i64,
    /// Compare-and-swap counter, at least 1, bumped by every change.
    pub revision: i64,
    /// Unix seconds.
    pub updated_at: i64,
}

impl Allowance {
    /// `limit - reserved - committed`. Negative after an overrun the allowance could not cover.
    pub fn remaining_micro(&self) -> i64 {
        self.limit_micro
            .saturating_sub(self.reserved_micro)
            .saturating_sub(self.committed_micro)
    }
}

/// Optional facts stored on a reservation when it is made. Both are opaque, non-empty text when
/// present and are part of the request-id idempotency check.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReserveOptions {
    /// The account the call is made for, so [`Ledger::expire_stale_account`] can settle one
    /// account's leases without touching the rest of the scope.
    pub account: Option<String>,
    /// The currency the estimate is in (for example `USD`), stored on the reservation as given.
    pub currency: Option<String>,
    /// Count open reservations of earlier periods of the scope against this period's cap.
    carry_over: bool,
    /// The caller budget checked and tagged with this reservation.
    budget: Option<Budget>,
}

impl ReserveOptions {
    /// Options with an account key.
    pub fn with_account(account: impl Into<String>) -> Self {
        Self {
            account: Some(account.into()),
            currency: None,
            carry_over: false,
            budget: None,
        }
    }

    /// Count the open reservations of earlier periods of the same scope against this period's
    /// cap (see the crate docs). Not part of the idempotency check: it is a policy of this call,
    /// not a stored fact.
    pub fn carry_over(mut self, carry_over: bool) -> Self {
        self.carry_over = carry_over;
        self
    }

    /// Also hold the reservation against one caller [`Budget`] (see [`Budget::new`]). Calling
    /// this builder method again replaces its option before admission. The budget is checked
    /// after the cap and can only refuse more; its key is stored so later reservations count it.
    /// On request replay, budget presence and normalized key must match; the limit is only
    /// first-admission policy and is not checked again.
    pub fn caller_budget(mut self, budget: Budget) -> Self {
        self.budget = Some(budget);
        self
    }

    /// Set the currency.
    pub fn currency(mut self, currency: impl Into<String>) -> Self {
        self.currency = Some(currency.into());
        self
    }

    fn validate(&self) -> Result<()> {
        if let Some(account) = &self.account {
            non_empty("account", account)?;
        }
        if let Some(currency) = &self.currency {
            non_empty("currency", currency)?;
        }
        Ok(())
    }
}

/// One reservation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reservation {
    /// `res-` plus an 8-digit count; the count is never reused.
    pub reservation_id: String,
    pub request_id: String,
    pub scope: String,
    pub period: String,
    pub route_id: String,
    pub price_version: String,
    pub estimate_micro: i64,
    pub state: ReservationState,
    /// Set exactly when `state` is `Committed`.
    pub committed_micro: Option<i64>,
    /// The actual exceeded the estimate, or the reservation committed at its ceiling because
    /// the outcome was unknown.
    pub over_reservation: bool,
    /// Committed at its ceiling with the real cost unknown ([`Ledger::commit_unknown`] or an
    /// expired in-flight lease). Also flagged `over_reservation`, so it blocks
    /// [`Ledger::reserve`] until [`Ledger::acknowledge_overrun`]. Read with
    /// [`Reservation::billed_unknown`].
    billed_unknown: bool,
    /// The account key given at reserve time, if any. Read with [`Reservation::account`].
    account: Option<String>,
    /// The currency given at reserve time, if any. Never rewritten. Read with
    /// [`Reservation::currency`].
    currency: Option<String>,
    pub created_at: i64,
    pub in_flight_at: Option<i64>,
    pub settled_at: Option<i64>,
    pub reason: Option<String>,
}

impl Reservation {
    /// Committed at its ceiling with the real cost unknown ([`Ledger::commit_unknown`] or an
    /// expired in-flight lease).
    pub fn billed_unknown(&self) -> bool {
        self.billed_unknown
    }

    /// The account key given at reserve time, if any.
    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }

    /// The currency given at reserve time, if any. Never rewritten.
    pub fn currency(&self) -> Option<&str> {
        self.currency.as_deref()
    }
}

/// A reservation with the allowance as it is after the call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub reservation: Reservation,
    pub allowance: Allowance,
    /// True when the call found the work already done (a stored request id on
    /// [`Ledger::reserve`], or, outside strict mode, a repeated transition) and wrote nothing.
    pub replayed: bool,
}

/// What [`Ledger::expire_stale`] did.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ExpireReport {
    /// Stale `reserved` reservations, estimates returned to their allowances.
    pub expired: Vec<Reservation>,
    pub released_micro: i64,
    /// Stale `in_flight` reservations, committed at their ceiling and flagged as overruns.
    pub committed_at_ceiling: Vec<Reservation>,
    pub ceiling_micro: i64,
}

/// Per-route totals in a [`Report`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteTotal {
    pub route_id: String,
    pub count: i64,
    pub estimated_micro: i64,
    /// Actuals of committed reservations on the route.
    pub committed_micro: i64,
}

/// Per-route totals for one state, from [`Ledger::route_totals_by_state`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteStateTotal {
    pub route_id: String,
    pub state: ReservationState,
    pub count: i64,
    pub estimated_micro: i64,
    /// Actuals of committed reservations; zero in every other state.
    pub committed_micro: i64,
}

/// One page of reservations, oldest first, from [`Ledger::reservations`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReservationPage {
    pub reservations: Vec<Reservation>,
    /// Pass as `after` to read the next page; `None` on the last page.
    pub next: Option<String>,
}

/// An allowance's accounting, derived from its reservations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub allowance: Allowance,
    pub remaining_micro: i64,
    pub count_reserved: i64,
    pub count_in_flight: i64,
    pub count_committed: i64,
    pub count_released: i64,
    pub count_expired: i64,
    pub over_reservations: i64,
    /// Reservations committed at their ceiling with the real cost unknown (also counted in
    /// `over_reservations`). Read with [`Report::billed_unknown`].
    billed_unknown: i64,
    pub unacknowledged_overruns: i64,
    /// Estimates of open reservations (`reserved` and `in_flight`).
    pub open_micro: i64,
    /// Committed estimates plus open estimates.
    pub forecast_micro: i64,
    /// Committed actuals.
    pub measured_micro: i64,
    /// `measured - committed estimates`: actual against estimate for the same calls.
    pub variance_micro: i64,
    /// Open estimates equal the allowance's `reserved` and committed actuals equal its
    /// `committed`.
    pub reconciles: bool,
    /// Ordered by route id.
    pub routes: Vec<RouteTotal>,
}

impl Report {
    /// Reservations committed at their ceiling with the real cost unknown (also counted in
    /// [`Report::over_reservations`]).
    pub fn billed_unknown(&self) -> i64 {
        self.billed_unknown
    }
}

const SCHEMA_V1: &str = "
CREATE TABLE spend_allowances (
    scope TEXT NOT NULL,
    period TEXT NOT NULL,
    limit_micro INTEGER NOT NULL CHECK (limit_micro >= 0),
    reserved_micro INTEGER NOT NULL CHECK (reserved_micro >= 0),
    committed_micro INTEGER NOT NULL CHECK (committed_micro >= 0),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (scope, period)
);
CREATE TABLE spend_reservations (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id TEXT NOT NULL UNIQUE,
    scope TEXT NOT NULL,
    period TEXT NOT NULL,
    route_id TEXT NOT NULL,
    price_version TEXT NOT NULL,
    estimated_micro INTEGER NOT NULL CHECK (estimated_micro >= 0),
    state TEXT NOT NULL CHECK (state IN ('reserved', 'in_flight', 'committed', 'released', 'expired')),
    committed_micro INTEGER CHECK (committed_micro IS NULL OR committed_micro >= 0),
    over_reservation INTEGER NOT NULL CHECK (over_reservation IN (0, 1)),
    created_at INTEGER NOT NULL,
    in_flight_at INTEGER,
    settled_at INTEGER,
    reason TEXT,
    CHECK ((state = 'committed') = (committed_micro IS NOT NULL)),
    FOREIGN KEY (scope, period) REFERENCES spend_allowances (scope, period)
);
CREATE INDEX spend_reservations_by_allowance ON spend_reservations (scope, period, state, created_at);
CREATE TABLE spend_overrun_acks (
    seq INTEGER PRIMARY KEY REFERENCES spend_reservations (seq),
    acknowledged_at INTEGER NOT NULL,
    note TEXT
);
";

/// Account key, per-reservation currency and the billed-unknown flag. Existing rows keep a NULL
/// account and currency. The backfill marks only rows that really were committed at their
/// ceiling by an expired in-flight lease: the reason alone is not enough, since a released or
/// otherwise settled row can carry the same reason text.
const SCHEMA_V2: &str = "
ALTER TABLE spend_reservations ADD COLUMN account TEXT;
ALTER TABLE spend_reservations ADD COLUMN currency TEXT;
ALTER TABLE spend_reservations ADD COLUMN billed_unknown INTEGER NOT NULL DEFAULT 0;
UPDATE spend_reservations SET billed_unknown = 1
    WHERE reason = 'lease_expired_in_flight'
      AND state = 'committed'
      AND over_reservation = 1
      AND committed_micro = estimated_micro;
CREATE INDEX spend_reservations_by_account ON spend_reservations (scope, account, state, created_at);
";

/// Cap audit, multi-cap groups, device allowances, budget tags, provider and invoice figures.
/// Purely additive: no existing table or column changes.
const SCHEMA_V3: &str = "
CREATE TABLE spend_cap_audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at INTEGER NOT NULL,
    who TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('allowance', 'device_allowance')),
    scope TEXT NOT NULL,
    period TEXT NOT NULL,
    old_limit_micro INTEGER,
    new_limit_micro INTEGER,
    old_revision INTEGER,
    new_revision INTEGER,
    note TEXT
);
CREATE TABLE spend_groups (
    request_id TEXT PRIMARY KEY,
    route_id TEXT NOT NULL,
    price_version TEXT NOT NULL,
    estimated_micro INTEGER NOT NULL CHECK (estimated_micro >= 0),
    account TEXT,
    currency TEXT,
    caps TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE spend_group_legs (
    request_id TEXT NOT NULL REFERENCES spend_groups (request_id),
    leg INTEGER NOT NULL,
    seq INTEGER NOT NULL UNIQUE REFERENCES spend_reservations (seq),
    PRIMARY KEY (request_id, leg)
);
CREATE TABLE spend_device_allowances (
    scope TEXT NOT NULL,
    device_id TEXT NOT NULL,
    epoch INTEGER NOT NULL CHECK (epoch >= 1),
    limit_micro INTEGER CHECK (limit_micro IS NULL OR limit_micro >= 0),
    set_at INTEGER NOT NULL,
    PRIMARY KEY (scope, device_id)
);
CREATE TABLE spend_budget_tags (
    seq INTEGER NOT NULL REFERENCES spend_reservations (seq),
    kind TEXT NOT NULL CHECK (kind IN ('caller', 'process')),
    key TEXT NOT NULL,
    PRIMARY KEY (seq, kind)
);
CREATE INDEX spend_budget_tags_by_key ON spend_budget_tags (kind, key);
CREATE TABLE spend_adjustments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    seq INTEGER NOT NULL REFERENCES spend_reservations (seq),
    at INTEGER NOT NULL,
    old_micro INTEGER NOT NULL,
    new_micro INTEGER NOT NULL,
    source TEXT NOT NULL,
    note TEXT
);
CREATE TABLE spend_provider_reported (
    seq INTEGER PRIMARY KEY REFERENCES spend_reservations (seq),
    reported_micro INTEGER NOT NULL CHECK (reported_micro >= 0),
    source TEXT NOT NULL,
    reported_at INTEGER NOT NULL
);
CREATE TABLE spend_invoices (
    invoice_ref TEXT PRIMARY KEY,
    ref_generated INTEGER NOT NULL CHECK (ref_generated IN (0, 1)),
    provider TEXT NOT NULL,
    period_from INTEGER NOT NULL,
    period_to INTEGER NOT NULL,
    total_micro INTEGER NOT NULL CHECK (total_micro >= 0),
    recorded_at INTEGER NOT NULL
);
CREATE TABLE spend_invoice_lines (
    seq INTEGER PRIMARY KEY REFERENCES spend_reservations (seq),
    invoice_ref TEXT NOT NULL REFERENCES spend_invoices (invoice_ref),
    invoice_micro INTEGER NOT NULL CHECK (invoice_micro >= 0),
    recorded_at INTEGER NOT NULL
);
";

const MIGRATIONS: &[Migration] = &[
    Migration::sql(1, "spend-ledger", SCHEMA_V1),
    Migration::sql(2, "spend-account-currency-unknown", SCHEMA_V2),
    Migration::sql(3, "spend-caps-groups-budgets-reconcile", SCHEMA_V3),
    Migration::sql(4, "spend-commit-external-id", SCHEMA_V4),
    Migration::sql(5, "spend-approved-budgets", SCHEMA_V5),
];

/// Approved budgets, their append-only audit and the idempotent spend-report log. An approved
/// budget row never changes again; the audit and the report log refuse update and delete.
const SCHEMA_V5: &str = "
CREATE TABLE spend_approved_budgets (
    id TEXT PRIMARY KEY,
    scope TEXT NOT NULL,
    period TEXT NOT NULL,
    currency TEXT NOT NULL,
    limit_micro INTEGER NOT NULL CHECK (limit_micro > 0),
    created_at INTEGER NOT NULL,
    approved_at INTEGER,
    approved_by TEXT,
    seal TEXT,
    UNIQUE (scope, period),
    FOREIGN KEY (scope, period) REFERENCES spend_allowances (scope, period),
    CHECK ((approved_at IS NULL) = (approved_by IS NULL) AND (approved_at IS NULL) = (seal IS NULL))
);
CREATE TRIGGER spend_approved_budgets_frozen BEFORE UPDATE ON spend_approved_budgets
    WHEN OLD.approved_at IS NOT NULL OR NEW.id IS NOT OLD.id OR NEW.scope IS NOT OLD.scope
        OR NEW.period IS NOT OLD.period OR NEW.currency IS NOT OLD.currency
        OR NEW.limit_micro IS NOT OLD.limit_micro OR NEW.created_at IS NOT OLD.created_at
    BEGIN SELECT RAISE(ABORT, 'an approved budget never changes'); END;
CREATE TRIGGER spend_approved_budgets_no_delete BEFORE DELETE ON spend_approved_budgets
    BEGIN SELECT RAISE(ABORT, 'approved budgets are never deleted'); END;
CREATE TABLE spend_approved_reports (
    report_id TEXT PRIMARY KEY,
    budget_id TEXT NOT NULL REFERENCES spend_approved_budgets (id),
    reservation_id TEXT NOT NULL,
    actual_micro INTEGER NOT NULL CHECK (actual_micro >= 0),
    reported_at INTEGER NOT NULL
);
CREATE TRIGGER spend_approved_reports_no_update BEFORE UPDATE ON spend_approved_reports
    BEGIN SELECT RAISE(ABORT, 'spend reports are append-only'); END;
CREATE TRIGGER spend_approved_reports_no_delete BEFORE DELETE ON spend_approved_reports
    BEGIN SELECT RAISE(ABORT, 'spend reports are append-only'); END;
CREATE TABLE spend_approved_audit (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    at INTEGER NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('create', 'approve', 'report')),
    budget_id TEXT NOT NULL,
    actor TEXT NOT NULL,
    detail TEXT NOT NULL
);
CREATE TRIGGER spend_approved_audit_no_update BEFORE UPDATE ON spend_approved_audit
    BEGIN SELECT RAISE(ABORT, 'the approved-budget audit is append-only'); END;
CREATE TRIGGER spend_approved_audit_no_delete BEFORE DELETE ON spend_approved_audit
    BEGIN SELECT RAISE(ABORT, 'the approved-budget audit is append-only'); END;
";

/// The external request id a commit was keyed by ([`Ledger::commit_idempotent`]). Unique per
/// scope among committed rows, so a repeated dispatch actual replays instead of re-settling.
const SCHEMA_V4: &str = "
ALTER TABLE spend_reservations ADD COLUMN commit_external_id TEXT;
CREATE UNIQUE INDEX spend_reservations_commit_external ON spend_reservations (scope, commit_external_id)
    WHERE commit_external_id IS NOT NULL;
";

/// Largest page [`Ledger::reservations`] returns.
const MAX_PAGE: usize = 1000;

const ALLOWANCE_COLUMNS: &str =
    "scope, period, limit_micro, reserved_micro, committed_micro, revision, updated_at";

const RESERVATION_COLUMNS: &str = "seq, request_id, scope, period, route_id, price_version, \
    estimated_micro, state, committed_micro, over_reservation, created_at, in_flight_at, \
    settled_at, reason, account, currency, billed_unknown";

/// The clock reading and mode one ledger call runs under.
struct Ctx {
    now: i64,
    strict: bool,
    /// Refuse `reserve` while an unacknowledged overrun exists on the allowance.
    gate: bool,
    /// Recorded as `who` on cap audit rows.
    actor: String,
    process: Option<Budget>,
}

/// The ledger. It owns one connection; open a second connection for the app's own queries, or
/// use [`Ledger::begin`] and the `_in` methods to write the app's rows in the same transaction.
pub struct Ledger {
    conn: Connection,
    clock: Box<dyn Fn() -> i64 + Send>,
    strict: bool,
    overrun_gate: bool,
    actor: String,
    process_budget: Option<Budget>,
}

impl Ledger {
    /// Create or upgrade the ledger tables in `conn`'s database and use the system clock.
    pub fn attach(conn: Connection) -> Result<Self> {
        Self::attach_with_clock(conn, system_now)
    }

    /// As [`Ledger::attach`] with a clock returning unix seconds (tests, replays).
    pub fn attach_with_clock(
        conn: Connection,
        clock: impl Fn() -> i64 + Send + 'static,
    ) -> Result<Self> {
        let options = MigrateOptions {
            version_storage: VersionStorage::Table {
                table: "rightkit_spend_meta".to_owned(),
                key: "schema".to_owned(),
            },
            backup: false,
            optimize_after: false,
            ..MigrateOptions::default()
        };
        migrate(&conn, None, MIGRATIONS, &options)?;
        Ok(Self {
            conn,
            clock: Box::new(clock),
            strict: false,
            overrun_gate: true,
            actor: "unknown".to_owned(),
            process_budget: None,
        })
    }

    /// Turn the overrun gate on or off. On (the default), [`Ledger::reserve`] refuses while an
    /// allowance has an unacknowledged overrun. Off, overruns are still flagged and counted but
    /// never block a reservation.
    pub fn with_overrun_gate(mut self, gate: bool) -> Self {
        self.overrun_gate = gate;
        self
    }

    /// As [`Ledger::with_overrun_gate`] on an existing ledger.
    pub fn set_overrun_gate(&mut self, gate: bool) {
        self.overrun_gate = gate;
    }

    /// Whether the overrun gate is on.
    pub fn overrun_gate(&self) -> bool {
        self.overrun_gate
    }

    /// Who the cap audit rows name (default `unknown`). The ledger cannot tell who the app's
    /// user is; the app says.
    pub fn with_actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = actor.into();
        self
    }

    /// As [`Ledger::with_actor`] on an existing ledger.
    pub fn set_actor(&mut self, actor: impl Into<String>) {
        self.actor = actor.into();
    }

    /// Set or clear the process budget: every reservation made through this ledger value is
    /// refused beyond it, counting all reservations tagged with the same key (so several jobs of
    /// one run share it). It sits beside the caps and never loosens them.
    pub fn set_process_budget(&mut self, budget: Option<Budget>) {
        self.process_budget = budget;
    }

    /// The process budget, if any.
    pub fn process_budget(&self) -> Option<&Budget> {
        self.process_budget.as_ref()
    }

    /// Turn strict mode on or off (see the crate docs). Off by default.
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// As [`Ledger::with_strict`] on an existing ledger.
    pub fn set_strict(&mut self, strict: bool) {
        self.strict = strict;
    }

    /// Whether strict mode is on.
    pub fn is_strict(&self) -> bool {
        self.strict
    }

    /// The underlying connection (read-only use).
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Begin a write transaction (`BEGIN IMMEDIATE`) on the ledger's connection for the `_in`
    /// methods. Write the app's own rows through the same transaction (it derefs to
    /// [`Connection`]) and call `commit()` once; dropping it rolls everything back, ledger
    /// changes included. Fails if a transaction is already open on the connection.
    pub fn begin(&self) -> Result<Transaction<'_>> {
        Ok(Transaction::new_unchecked(
            &self.conn,
            TransactionBehavior::Immediate,
        )?)
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }

    fn ctx(&self) -> Ctx {
        Ctx {
            now: self.now(),
            strict: self.strict,
            gate: self.overrun_gate,
            actor: self.actor.clone(),
            process: self.process_budget.clone(),
        }
    }

    /// Run `f` in its own `BEGIN IMMEDIATE` transaction and commit it if `f` succeeds.
    fn run<T>(&mut self, f: impl FnOnce(&Connection, &Ctx) -> Result<T>) -> Result<T> {
        let ctx = self.ctx();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&tx, &ctx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Run `f` inside the caller's transaction, in a savepoint so a failure leaves no partial
    /// write behind.
    fn run_in<T>(
        &self,
        tx: &Transaction<'_>,
        f: impl FnOnce(&Connection, &Ctx) -> Result<T>,
    ) -> Result<T> {
        let ctx = self.ctx();
        atomic(tx, |conn| f(conn, &ctx))
    }

    /// Run `f` in a savepoint on a connection the caller has already put in a transaction. A
    /// connection in autocommit mode is refused: the savepoint would open and commit its own
    /// transaction, which the caller did not ask for.
    fn run_on<T>(
        &self,
        conn: &Connection,
        f: impl FnOnce(&Connection, &Ctx) -> Result<T>,
    ) -> Result<T> {
        if conn.is_autocommit() {
            return Err(SpendError::NoActiveTransaction);
        }
        let ctx = self.ctx();
        atomic(conn, |conn| f(conn, &ctx))
    }

    /// Create the allowance for `(scope, period)` at revision 1, or change its limit.
    ///
    /// With an existing row `expected_revision` is required and must match
    /// ([`SpendError::RevisionRequired`], [`SpendError::RevisionConflict`]). With no row it must
    /// be `None`. The limit may not drop below reserved plus committed.
    pub fn set_allowance(
        &mut self,
        scope: &str,
        period: &str,
        limit_micro: i64,
        expected_revision: Option<i64>,
    ) -> Result<Allowance> {
        self.run(|conn, ctx| {
            set_allowance_impl(conn, ctx, scope, period, limit_micro, expected_revision)
        })
    }

    /// [`Ledger::set_allowance`] inside the caller's transaction ([`Ledger::begin`]).
    pub fn set_allowance_in(
        &self,
        tx: &Transaction<'_>,
        scope: &str,
        period: &str,
        limit_micro: i64,
        expected_revision: Option<i64>,
    ) -> Result<Allowance> {
        self.run_in(tx, |conn, ctx| {
            set_allowance_impl(conn, ctx, scope, period, limit_micro, expected_revision)
        })
    }

    /// The allowance for `(scope, period)`, if any.
    pub fn allowance(&self, scope: &str, period: &str) -> Result<Option<Allowance>> {
        get_allowance(&self.conn, scope, period)
    }

    /// One reservation by id (`res-00000001`), if any.
    pub fn reservation(&self, reservation_id: &str) -> Result<Option<Reservation>> {
        find_reservation(&self.conn, reservation_id)
    }

    /// One reservation by its request id, if any.
    pub fn reservation_by_request(&self, request_id: &str) -> Result<Option<Reservation>> {
        find_by_request(&self.conn, request_id)
    }

    /// [`Ledger::allowance`] on the caller's connection. Reads need no open transaction; on a
    /// connection inside the caller's transaction they see its uncommitted rows.
    pub fn allowance_on(
        &self,
        conn: &Connection,
        scope: &str,
        period: &str,
    ) -> Result<Option<Allowance>> {
        get_allowance(conn, scope, period)
    }

    /// [`Ledger::reservation`] on the caller's connection (see [`Ledger::allowance_on`]).
    pub fn reservation_on(
        &self,
        conn: &Connection,
        reservation_id: &str,
    ) -> Result<Option<Reservation>> {
        find_reservation(conn, reservation_id)
    }

    /// [`Ledger::reservation_by_request`] on the caller's connection (see [`Ledger::allowance_on`]).
    pub fn reservation_by_request_on(
        &self,
        conn: &Connection,
        request_id: &str,
    ) -> Result<Option<Reservation>> {
        find_by_request(conn, request_id)
    }

    /// One page of an allowance's reservations, oldest first: every state, or only the open
    /// ones (`reserved`, `in_flight`) when `open_only`. Pass the previous page's `next` as
    /// `after`; `limit` is clamped to 1..=1000. Reads on this ledger's connection.
    pub fn reservations(
        &self,
        scope: &str,
        period: &str,
        open_only: bool,
        after: Option<&str>,
        limit: usize,
    ) -> Result<ReservationPage> {
        self.reservations_on(&self.conn, scope, period, open_only, after, limit)
    }

    /// [`Ledger::reservations`] on the caller's connection (see [`Ledger::allowance_on`]).
    pub fn reservations_on(
        &self,
        conn: &Connection,
        scope: &str,
        period: &str,
        open_only: bool,
        after: Option<&str>,
        limit: usize,
    ) -> Result<ReservationPage> {
        reservations_page(conn, scope, period, open_only, after, limit)
    }

    /// Totals per route and state for one allowance: the count, estimates and committed
    /// actuals of each (route, state) pair, ordered by route then state.
    pub fn route_totals_by_state(&self, scope: &str, period: &str) -> Result<Vec<RouteStateTotal>> {
        self.route_totals_by_state_on(&self.conn, scope, period)
    }

    /// [`Ledger::route_totals_by_state`] on the caller's connection (see [`Ledger::allowance_on`]).
    pub fn route_totals_by_state_on(
        &self,
        conn: &Connection,
        scope: &str,
        period: &str,
    ) -> Result<Vec<RouteStateTotal>> {
        route_totals_by_state_rows(conn, scope, period)
    }

    /// Hold `estimate_micro` against the allowance before a paid call, with no account and no
    /// currency. Idempotent on `request_id`: the same request id with the same scope, period,
    /// route, price version and estimate returns the stored reservation (`replayed`) and
    /// writes nothing; any difference is [`SpendError::RequestMismatch`].
    ///
    /// Checks, first failure wins and writes nothing: stored request id, allowance exists,
    /// no unacknowledged overrun, `estimate <= remaining` (so an estimate equal to the remaining
    /// amount fits).
    pub fn reserve(
        &mut self,
        scope: &str,
        request_id: &str,
        period: &str,
        route_id: &str,
        price_version: &str,
        estimate_micro: i64,
    ) -> Result<Outcome> {
        self.reserve_with(
            scope,
            request_id,
            period,
            route_id,
            price_version,
            estimate_micro,
            &ReserveOptions::default(),
        )
    }

    /// [`Ledger::reserve`] with an account key and a currency stored on the reservation. The
    /// account, currency, and optional normalized caller-budget key are part of the idempotency
    /// check: changing any of them is [`SpendError::RequestMismatch`]. Budget limits and the
    /// process budget are first-admission policy; a replay returns the stored reservation
    /// without checking them again or changing its budget tags.
    #[allow(clippy::too_many_arguments)]
    pub fn reserve_with(
        &mut self,
        scope: &str,
        request_id: &str,
        period: &str,
        route_id: &str,
        price_version: &str,
        estimate_micro: i64,
        options: &ReserveOptions,
    ) -> Result<Outcome> {
        self.run(|conn, ctx| {
            reserve_impl(
                conn,
                ctx,
                scope,
                request_id,
                period,
                route_id,
                price_version,
                estimate_micro,
                options,
            )
        })
    }

    /// [`Ledger::reserve_with`] inside the caller's transaction ([`Ledger::begin`]).
    #[allow(clippy::too_many_arguments)]
    pub fn reserve_in(
        &self,
        tx: &Transaction<'_>,
        scope: &str,
        request_id: &str,
        period: &str,
        route_id: &str,
        price_version: &str,
        estimate_micro: i64,
        options: &ReserveOptions,
    ) -> Result<Outcome> {
        self.run_in(tx, |conn, ctx| {
            reserve_impl(
                conn,
                ctx,
                scope,
                request_id,
                period,
                route_id,
                price_version,
                estimate_micro,
                options,
            )
        })
    }

    /// `reserved` -> `in_flight`, just before the wire request is written. Calling it again on
    /// an in-flight reservation changes nothing, or is [`SpendError::InvalidTransition`] in
    /// strict mode.
    pub fn mark_in_flight(&mut self, reservation_id: &str) -> Result<Outcome> {
        self.run(|conn, ctx| mark_in_flight_impl(conn, ctx, reservation_id))
    }

    /// [`Ledger::mark_in_flight`] inside the caller's transaction ([`Ledger::begin`]).
    pub fn mark_in_flight_in(&self, tx: &Transaction<'_>, reservation_id: &str) -> Result<Outcome> {
        self.run_in(tx, |conn, ctx| {
            mark_in_flight_impl(conn, ctx, reservation_id)
        })
    }

    /// Record the actual cost of a `reserved` or `in_flight` reservation: the estimate leaves
    /// `reserved` and the actual enters `committed`. An actual above the estimate takes the
    /// difference from the allowance and flags the reservation (`over_reservation`); if the
    /// allowance cannot cover it the actual is still recorded and `remaining` goes negative.
    /// Committing a committed reservation with the same actual returns it unchanged, or is
    /// [`SpendError::InvalidTransition`] in strict mode.
    pub fn commit(&mut self, reservation_id: &str, actual_micro: i64) -> Result<Outcome> {
        self.run(|conn, ctx| commit_impl(conn, ctx, reservation_id, actual_micro, None, None))
    }

    /// [`Ledger::commit`] recording a caller-chosen `reason` on the reservation. It replaces the
    /// reason the ledger would otherwise derive for an overrun.
    pub fn commit_with_reason(
        &mut self,
        reservation_id: &str,
        actual_micro: i64,
        reason: &str,
    ) -> Result<Outcome> {
        self.run(|conn, ctx| {
            commit_impl(conn, ctx, reservation_id, actual_micro, Some(reason), None)
        })
    }

    /// [`Ledger::commit`] keyed by an external request id (for example the provider's dispatch
    /// id). The id is stored with the commit and is unique per scope: repeating the same call
    /// returns the committed reservation with `replayed` and writes nothing. The same id on a
    /// different reservation, or the same reservation with a different actual, is
    /// [`SpendError::RequestMismatch`].
    pub fn commit_idempotent(
        &mut self,
        reservation_id: &str,
        actual_micro: i64,
        external_id: &str,
    ) -> Result<Outcome> {
        self.run(|conn, ctx| {
            commit_impl(
                conn,
                ctx,
                reservation_id,
                actual_micro,
                None,
                Some(external_id),
            )
        })
    }

    /// [`Ledger::commit`] inside the caller's transaction ([`Ledger::begin`]).
    pub fn commit_in(
        &self,
        tx: &Transaction<'_>,
        reservation_id: &str,
        actual_micro: i64,
    ) -> Result<Outcome> {
        self.run_in(tx, |conn, ctx| {
            commit_impl(conn, ctx, reservation_id, actual_micro, None, None)
        })
    }

    /// Commit a `reserved` or `in_flight` reservation at its ceiling (the estimate) because the
    /// call may have been billed and its real cost is not known. One atomic write: the row is
    /// `committed` at the estimate and flagged `billed_unknown` and `over_reservation`, so
    /// [`Ledger::reserve`] on its allowance refuses ([`SpendError::OverrunUnacknowledged`])
    /// until [`Ledger::acknowledge_overrun`]. `reason` defaults to `billed_unknown`. Repeating
    /// it on a billed-unknown reservation returns it unchanged (strict mode:
    /// [`SpendError::InvalidTransition`]); on any other settled reservation it is
    /// [`SpendError::InvalidTransition`].
    pub fn commit_unknown(
        &mut self,
        reservation_id: &str,
        reason: Option<&str>,
    ) -> Result<Outcome> {
        self.run(|conn, ctx| commit_unknown_impl(conn, ctx, reservation_id, reason))
    }

    /// [`Ledger::commit_unknown`] inside the caller's transaction ([`Ledger::begin`]).
    pub fn commit_unknown_in(
        &self,
        tx: &Transaction<'_>,
        reservation_id: &str,
        reason: Option<&str>,
    ) -> Result<Outcome> {
        self.run_in(tx, |conn, ctx| {
            commit_unknown_impl(conn, ctx, reservation_id, reason)
        })
    }

    /// Give a `reserved` or `in_flight` estimate back to the allowance (the call was not made,
    /// or provably cost nothing). Releasing a released reservation returns it unchanged, or is
    /// [`SpendError::InvalidTransition`] in strict mode.
    pub fn release(&mut self, reservation_id: &str, reason: Option<&str>) -> Result<Outcome> {
        self.run(|conn, ctx| release_impl(conn, ctx, reservation_id, reason))
    }

    /// [`Ledger::release`] inside the caller's transaction ([`Ledger::begin`]).
    pub fn release_in(
        &self,
        tx: &Transaction<'_>,
        reservation_id: &str,
        reason: Option<&str>,
    ) -> Result<Outcome> {
        self.run_in(tx, |conn, ctx| {
            release_impl(conn, ctx, reservation_id, reason)
        })
    }

    /// Settle stale leases of `scope` (every period, every account). A `reserved` reservation
    /// created strictly more than `older_than_secs` ago expires and returns its estimate. An
    /// `in_flight` one whose in-flight time is strictly older commits at its ceiling (the
    /// estimate), flagged `billed_unknown` and `over_reservation` with reason
    /// `lease_expired_in_flight`: it may have been sent, so it is never released. A reservation
    /// exactly that old is kept.
    pub fn expire_stale(&mut self, scope: &str, older_than_secs: i64) -> Result<ExpireReport> {
        self.run(|conn, ctx| expire_impl(conn, ctx, scope, None, older_than_secs))
    }

    /// [`Ledger::expire_stale`] inside the caller's transaction ([`Ledger::begin`]).
    pub fn expire_stale_in(
        &self,
        tx: &Transaction<'_>,
        scope: &str,
        older_than_secs: i64,
    ) -> Result<ExpireReport> {
        self.run_in(tx, |conn, ctx| {
            expire_impl(conn, ctx, scope, None, older_than_secs)
        })
    }

    /// As [`Ledger::expire_stale`], limited to reservations of `scope` made with this
    /// `account` ([`ReserveOptions::account`]). Reservations with another account, or none, are
    /// untouched.
    pub fn expire_stale_account(
        &mut self,
        scope: &str,
        account: &str,
        older_than_secs: i64,
    ) -> Result<ExpireReport> {
        non_empty("account", account)?;
        self.run(|conn, ctx| expire_impl(conn, ctx, scope, Some(account), older_than_secs))
    }

    /// [`Ledger::expire_stale_account`] inside the caller's transaction ([`Ledger::begin`]).
    pub fn expire_stale_account_in(
        &self,
        tx: &Transaction<'_>,
        scope: &str,
        account: &str,
        older_than_secs: i64,
    ) -> Result<ExpireReport> {
        non_empty("account", account)?;
        self.run_in(tx, |conn, ctx| {
            expire_impl(conn, ctx, scope, Some(account), older_than_secs)
        })
    }

    /// Record that a person saw a flagged reservation. Until every flagged reservation of an
    /// allowance has one, [`Ledger::reserve`] on that allowance refuses. Acknowledging twice
    /// keeps the first acknowledgement. A reservation that is not flagged is
    /// [`SpendError::BadInput`].
    pub fn acknowledge_overrun(&mut self, reservation_id: &str, note: Option<&str>) -> Result<()> {
        self.run(|conn, ctx| acknowledge_impl(conn, ctx, reservation_id, note))
    }

    /// [`Ledger::acknowledge_overrun`] inside the caller's transaction ([`Ledger::begin`]).
    pub fn acknowledge_overrun_in(
        &self,
        tx: &Transaction<'_>,
        reservation_id: &str,
        note: Option<&str>,
    ) -> Result<()> {
        self.run_in(tx, |conn, ctx| {
            acknowledge_impl(conn, ctx, reservation_id, note)
        })
    }

    // Connection-borrowing variants. Each takes a connection the caller has already put in a
    // transaction (for example `Connection::transaction` deref'd, or `BEGIN` run on
    // `ledger.connection()`) and runs the operation in a savepoint. A connection in autocommit
    // mode is refused with [`SpendError::NoActiveTransaction`] and nothing is written.

    /// [`Ledger::set_allowance`] on a connection with an open transaction (see above).
    pub fn set_allowance_on(
        &self,
        conn: &Connection,
        scope: &str,
        period: &str,
        limit_micro: i64,
        expected_revision: Option<i64>,
    ) -> Result<Allowance> {
        self.run_on(conn, |conn, ctx| {
            set_allowance_impl(conn, ctx, scope, period, limit_micro, expected_revision)
        })
    }

    /// [`Ledger::reserve_with`] on a connection with an open transaction (see above).
    #[allow(clippy::too_many_arguments)]
    pub fn reserve_on(
        &self,
        conn: &Connection,
        scope: &str,
        request_id: &str,
        period: &str,
        route_id: &str,
        price_version: &str,
        estimate_micro: i64,
        options: &ReserveOptions,
    ) -> Result<Outcome> {
        self.run_on(conn, |conn, ctx| {
            reserve_impl(
                conn,
                ctx,
                scope,
                request_id,
                period,
                route_id,
                price_version,
                estimate_micro,
                options,
            )
        })
    }

    /// [`Ledger::mark_in_flight`] on a connection with an open transaction (see above).
    pub fn mark_in_flight_on(&self, conn: &Connection, reservation_id: &str) -> Result<Outcome> {
        self.run_on(conn, |conn, ctx| {
            mark_in_flight_impl(conn, ctx, reservation_id)
        })
    }

    /// [`Ledger::commit`] on a connection with an open transaction (see above).
    pub fn commit_on(
        &self,
        conn: &Connection,
        reservation_id: &str,
        actual_micro: i64,
    ) -> Result<Outcome> {
        self.run_on(conn, |conn, ctx| {
            commit_impl(conn, ctx, reservation_id, actual_micro, None, None)
        })
    }

    /// [`Ledger::commit_unknown`] on a connection with an open transaction (see above).
    pub fn commit_unknown_on(
        &self,
        conn: &Connection,
        reservation_id: &str,
        reason: Option<&str>,
    ) -> Result<Outcome> {
        self.run_on(conn, |conn, ctx| {
            commit_unknown_impl(conn, ctx, reservation_id, reason)
        })
    }

    /// [`Ledger::release`] on a connection with an open transaction (see above).
    pub fn release_on(
        &self,
        conn: &Connection,
        reservation_id: &str,
        reason: Option<&str>,
    ) -> Result<Outcome> {
        self.run_on(conn, |conn, ctx| {
            release_impl(conn, ctx, reservation_id, reason)
        })
    }

    /// [`Ledger::expire_stale`] on a connection with an open transaction (see above).
    pub fn expire_stale_on(
        &self,
        conn: &Connection,
        scope: &str,
        older_than_secs: i64,
    ) -> Result<ExpireReport> {
        self.run_on(conn, |conn, ctx| {
            expire_impl(conn, ctx, scope, None, older_than_secs)
        })
    }

    /// [`Ledger::expire_stale_account`] on a connection with an open transaction (see above).
    pub fn expire_stale_account_on(
        &self,
        conn: &Connection,
        scope: &str,
        account: &str,
        older_than_secs: i64,
    ) -> Result<ExpireReport> {
        non_empty("account", account)?;
        self.run_on(conn, |conn, ctx| {
            expire_impl(conn, ctx, scope, Some(account), older_than_secs)
        })
    }

    /// [`Ledger::acknowledge_overrun`] on a connection with an open transaction (see above).
    pub fn acknowledge_overrun_on(
        &self,
        conn: &Connection,
        reservation_id: &str,
        note: Option<&str>,
    ) -> Result<()> {
        self.run_on(conn, |conn, ctx| {
            acknowledge_impl(conn, ctx, reservation_id, note)
        })
    }

    /// Accounting for one allowance, derived from its reservations.
    pub fn report(&self, scope: &str, period: &str) -> Result<Report> {
        self.report_on(&self.conn, scope, period)
    }

    /// [`Ledger::report`] on the caller's connection (see [`Ledger::allowance_on`]), so the
    /// figures include the caller's uncommitted rows.
    pub fn report_on(&self, conn: &Connection, scope: &str, period: &str) -> Result<Report> {
        let allowance = require_allowance(conn, scope, period)?;
        let sql = format!(
            "SELECT {RESERVATION_COLUMNS} FROM spend_reservations WHERE scope = ?1 AND period = ?2 \
             ORDER BY seq"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![scope, period], reservation_from_row)?;
        let all = rows.collect::<std::result::Result<Vec<_>, _>>()?;

        let mut report = Report {
            remaining_micro: allowance.remaining_micro(),
            allowance,
            count_reserved: 0,
            count_in_flight: 0,
            count_committed: 0,
            count_released: 0,
            count_expired: 0,
            over_reservations: 0,
            billed_unknown: 0,
            unacknowledged_overruns: unacknowledged_overruns(conn, scope, period)?,
            open_micro: 0,
            forecast_micro: 0,
            measured_micro: 0,
            variance_micro: 0,
            reconciles: false,
            routes: Vec::new(),
        };
        let mut committed_estimates: i64 = 0;
        let mut routes: std::collections::BTreeMap<String, RouteTotal> = Default::default();
        for res in &all {
            match res.state {
                ReservationState::Reserved => report.count_reserved += 1,
                ReservationState::InFlight => report.count_in_flight += 1,
                ReservationState::Committed => report.count_committed += 1,
                ReservationState::Released => report.count_released += 1,
                ReservationState::Expired => report.count_expired += 1,
            }
            if res.over_reservation {
                report.over_reservations += 1;
            }
            if res.billed_unknown {
                report.billed_unknown += 1;
            }
            if res.state.is_open() {
                report.open_micro = add(report.open_micro, res.estimate_micro)?;
            }
            let actual = res.committed_micro.unwrap_or(0);
            if res.state == ReservationState::Committed {
                report.measured_micro = add(report.measured_micro, actual)?;
                committed_estimates = add(committed_estimates, res.estimate_micro)?;
            }
            let route = routes
                .entry(res.route_id.clone())
                .or_insert_with(|| RouteTotal {
                    route_id: res.route_id.clone(),
                    count: 0,
                    estimated_micro: 0,
                    committed_micro: 0,
                });
            route.count += 1;
            route.estimated_micro = add(route.estimated_micro, res.estimate_micro)?;
            route.committed_micro = add(route.committed_micro, actual)?;
        }
        report.forecast_micro = add(committed_estimates, report.open_micro)?;
        report.variance_micro = report
            .measured_micro
            .checked_sub(committed_estimates)
            .ok_or(SpendError::Overflow)?;
        report.reconciles = report.open_micro == report.allowance.reserved_micro
            && report.measured_micro == report.allowance.committed_micro;
        report.routes = routes.into_values().collect();
        Ok(report)
    }
}

fn set_allowance_impl(
    conn: &Connection,
    ctx: &Ctx,
    scope: &str,
    period: &str,
    limit_micro: i64,
    expected_revision: Option<i64>,
) -> Result<Allowance> {
    non_empty("scope", scope)?;
    non_empty("period", period)?;
    if limit_micro < 0 {
        return Err(SpendError::BadInput("limit_micro is negative".into()));
    }
    match get_allowance(conn, scope, period)? {
        None => {
            if let Some(expected) = expected_revision {
                return Err(SpendError::RevisionConflict {
                    expected,
                    actual: 0,
                });
            }
            conn.execute(
                "INSERT INTO spend_allowances (scope, period, limit_micro, reserved_micro, \
                 committed_micro, revision, updated_at) VALUES (?1, ?2, ?3, 0, 0, 1, ?4)",
                params![scope, period, limit_micro, ctx.now],
            )?;
            audit::record(
                conn,
                ctx,
                "allowance",
                scope,
                period,
                (None, Some(limit_micro)),
                (None, Some(1)),
                None,
            )?;
            require_allowance(conn, scope, period)
        }
        Some(current) => {
            let expected = expected_revision.ok_or(SpendError::RevisionRequired)?;
            if expected != current.revision {
                return Err(SpendError::RevisionConflict {
                    expected,
                    actual: current.revision,
                });
            }
            let spent = spent_total(&current)?;
            if limit_micro < spent {
                return Err(SpendError::AllowanceBelowSpend {
                    limit_micro,
                    spent_micro: spent,
                });
            }
            let next = current
                .revision
                .checked_add(1)
                .ok_or(SpendError::Overflow)?;
            let changed = conn.execute(
                "UPDATE spend_allowances SET limit_micro = ?3, revision = ?4, updated_at = ?5 \
                 WHERE scope = ?1 AND period = ?2 AND revision = ?6",
                params![scope, period, limit_micro, next, ctx.now, current.revision],
            )?;
            if changed == 0 {
                return Err(conflict(conn, &current)?);
            }
            audit::record(
                conn,
                ctx,
                "allowance",
                scope,
                period,
                (Some(current.limit_micro), Some(limit_micro)),
                (Some(current.revision), Some(next)),
                None,
            )?;
            require_allowance(conn, scope, period)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn reserve_impl(
    conn: &Connection,
    ctx: &Ctx,
    scope: &str,
    request_id: &str,
    period: &str,
    route_id: &str,
    price_version: &str,
    estimate_micro: i64,
    options: &ReserveOptions,
) -> Result<Outcome> {
    let out = reserve_core(
        conn,
        ctx,
        scope,
        request_id,
        period,
        route_id,
        price_version,
        estimate_micro,
        options,
        options.carry_over,
    )?;
    if out.replayed {
        budget::check_replay_budget(conn, options, &out.reservation, request_id)?;
    } else {
        // After the cap: a budget can only refuse more. A refusal rolls the hold back with the
        // enclosing transaction or savepoint.
        budget::apply_budgets(conn, ctx, options, &out.reservation)?;
    }
    Ok(out)
}

/// One hold against one allowance: validation, request-id idempotency, the overrun gate, the
/// remaining check (with carry-over when asked) and the write. No budget handling.
#[allow(clippy::too_many_arguments)]
fn reserve_core(
    conn: &Connection,
    ctx: &Ctx,
    scope: &str,
    request_id: &str,
    period: &str,
    route_id: &str,
    price_version: &str,
    estimate_micro: i64,
    options: &ReserveOptions,
    carry_over: bool,
) -> Result<Outcome> {
    non_empty("scope", scope)?;
    non_empty("request_id", request_id)?;
    non_empty("period", period)?;
    non_empty("route_id", route_id)?;
    non_empty("price_version", price_version)?;
    options.validate()?;
    if estimate_micro < 0 {
        return Err(SpendError::BadInput("estimate_micro is negative".into()));
    }

    if let Some(stored) = find_by_request(conn, request_id)? {
        let same = stored.scope == scope
            && stored.period == period
            && stored.route_id == route_id
            && stored.price_version == price_version
            && stored.estimate_micro == estimate_micro
            && stored.account == options.account
            && stored.currency == options.currency;
        if !same {
            return Err(SpendError::RequestMismatch {
                request_id: request_id.to_owned(),
            });
        }
        return outcome(conn, stored, true);
    }

    let allowance = require_allowance(conn, scope, period)?;
    if ctx.gate {
        let unacked = unacknowledged_overruns(conn, scope, period)?;
        if unacked > 0 {
            return Err(SpendError::OverrunUnacknowledged {
                count: unacked,
                reservation_id: oldest_unacknowledged_overrun(conn, scope, period)?,
            });
        }
    }
    let mut remaining = allowance.remaining_micro();
    if carry_over {
        remaining = remaining
            .checked_sub(carried_open_micro(conn, scope, period)?)
            .ok_or(SpendError::Overflow)?;
    }
    if estimate_micro > remaining {
        return Err(SpendError::AllowanceExceeded {
            estimate_micro,
            remaining_micro: remaining,
        });
    }
    let reserved = allowance
        .reserved_micro
        .checked_add(estimate_micro)
        .ok_or(SpendError::Overflow)?;
    cas_totals(
        conn,
        &allowance,
        reserved,
        allowance.committed_micro,
        ctx.now,
    )?;
    conn.execute(
        "INSERT INTO spend_reservations (request_id, scope, period, route_id, price_version, \
         estimated_micro, state, committed_micro, over_reservation, created_at, account, \
         currency, billed_unknown) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'reserved', NULL, 0, ?7, ?8, ?9, 0)",
        params![
            request_id,
            scope,
            period,
            route_id,
            price_version,
            estimate_micro,
            ctx.now,
            options.account,
            options.currency
        ],
    )?;
    let stored = find_by_request(conn, request_id)?
        .ok_or_else(|| SpendError::NotFound(request_id.to_owned()))?;
    outcome(conn, stored, false)
}

fn mark_in_flight_impl(conn: &Connection, ctx: &Ctx, reservation_id: &str) -> Result<Outcome> {
    let res = require_reservation(conn, reservation_id)?;
    match res.state {
        ReservationState::InFlight if !ctx.strict => return outcome(conn, res, true),
        ReservationState::Reserved => {}
        other => {
            return Err(SpendError::InvalidTransition {
                from: other,
                to: ReservationState::InFlight,
            })
        }
    }
    conn.execute(
        "UPDATE spend_reservations SET state = 'in_flight', in_flight_at = ?2 \
         WHERE seq = ?1 AND state = 'reserved'",
        params![seq_of(reservation_id)?, ctx.now],
    )?;
    let res = require_reservation(conn, reservation_id)?;
    outcome(conn, res, false)
}

fn commit_impl(
    conn: &Connection,
    ctx: &Ctx,
    reservation_id: &str,
    actual_micro: i64,
    caller_reason: Option<&str>,
    external_id: Option<&str>,
) -> Result<Outcome> {
    if actual_micro < 0 {
        return Err(SpendError::BadInput("actual_micro is negative".into()));
    }
    if let Some(reason) = caller_reason {
        non_empty("reason", reason)?;
    }
    let res = require_reservation(conn, reservation_id)?;
    if let Some(ext) = external_id {
        non_empty("external_id", ext)?;
        let claimed: Option<i64> = conn
            .query_row(
                "SELECT seq FROM spend_reservations WHERE scope = ?1 AND commit_external_id = ?2",
                params![res.scope, ext],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(claim) = claimed {
            if claim == seq_of(reservation_id)?
                && res.state == ReservationState::Committed
                && res.committed_micro == Some(actual_micro)
            {
                return outcome(conn, res, true);
            }
            return Err(SpendError::RequestMismatch {
                request_id: ext.to_owned(),
            });
        }
    }
    match res.state {
        ReservationState::Committed
            if !ctx.strict
                && external_id.is_none()
                && res.committed_micro == Some(actual_micro) =>
        {
            return outcome(conn, res, true)
        }
        ReservationState::Reserved | ReservationState::InFlight => {}
        other => {
            return Err(SpendError::InvalidTransition {
                from: other,
                to: ReservationState::Committed,
            })
        }
    }
    let allowance = require_allowance(conn, &res.scope, &res.period)?;
    let reserved = allowance
        .reserved_micro
        .checked_sub(res.estimate_micro)
        .filter(|v| *v >= 0)
        .ok_or(SpendError::Overflow)?;
    let committed = allowance
        .committed_micro
        .checked_add(actual_micro)
        .ok_or(SpendError::Overflow)?;
    let after = cas_totals(conn, &allowance, reserved, committed, ctx.now)?;
    let over = actual_micro > res.estimate_micro;
    let derived = if !over {
        None
    } else if after.remaining_micro() >= 0 {
        Some("overrun taken from the allowance".to_owned())
    } else {
        Some(format!(
            "overrun exceeds the allowance; remaining {} after commit",
            after.remaining_micro()
        ))
    };
    let reason = caller_reason.map(str::to_owned).or(derived);
    conn.execute(
        "UPDATE spend_reservations SET state = 'committed', committed_micro = ?2, \
         over_reservation = ?3, settled_at = ?4, reason = ?5, commit_external_id = ?6 \
         WHERE seq = ?1 AND state IN ('reserved', 'in_flight')",
        params![
            seq_of(reservation_id)?,
            actual_micro,
            i64::from(over),
            ctx.now,
            reason,
            external_id
        ],
    )?;
    let res = require_reservation(conn, reservation_id)?;
    outcome(conn, res, false)
}

fn commit_unknown_impl(
    conn: &Connection,
    ctx: &Ctx,
    reservation_id: &str,
    reason: Option<&str>,
) -> Result<Outcome> {
    let res = require_reservation(conn, reservation_id)?;
    match res.state {
        ReservationState::Committed if !ctx.strict && res.billed_unknown => {
            return outcome(conn, res, true)
        }
        ReservationState::Reserved | ReservationState::InFlight => {}
        other => {
            return Err(SpendError::InvalidTransition {
                from: other,
                to: ReservationState::Committed,
            })
        }
    }
    commit_at_ceiling(conn, ctx.now, &res, reason.unwrap_or("billed_unknown"))?;
    let res = require_reservation(conn, reservation_id)?;
    outcome(conn, res, false)
}

fn release_impl(
    conn: &Connection,
    ctx: &Ctx,
    reservation_id: &str,
    reason: Option<&str>,
) -> Result<Outcome> {
    let res = require_reservation(conn, reservation_id)?;
    match res.state {
        ReservationState::Released if !ctx.strict => return outcome(conn, res, true),
        ReservationState::Reserved | ReservationState::InFlight => {}
        other => {
            return Err(SpendError::InvalidTransition {
                from: other,
                to: ReservationState::Released,
            })
        }
    }
    let allowance = require_allowance(conn, &res.scope, &res.period)?;
    let reserved = allowance
        .reserved_micro
        .checked_sub(res.estimate_micro)
        .filter(|v| *v >= 0)
        .ok_or(SpendError::Overflow)?;
    cas_totals(
        conn,
        &allowance,
        reserved,
        allowance.committed_micro,
        ctx.now,
    )?;
    conn.execute(
        "UPDATE spend_reservations SET state = 'released', settled_at = ?2, reason = ?3 \
         WHERE seq = ?1 AND state IN ('reserved', 'in_flight')",
        params![seq_of(reservation_id)?, ctx.now, reason],
    )?;
    let res = require_reservation(conn, reservation_id)?;
    outcome(conn, res, false)
}

fn expire_impl(
    conn: &Connection,
    ctx: &Ctx,
    scope: &str,
    account: Option<&str>,
    older_than_secs: i64,
) -> Result<ExpireReport> {
    non_empty("scope", scope)?;
    if older_than_secs < 0 {
        return Err(SpendError::BadInput("older_than_secs is negative".into()));
    }
    let cutoff = ctx
        .now
        .checked_sub(older_than_secs)
        .ok_or(SpendError::Overflow)?;
    let stale: Vec<Reservation> = {
        let sql = format!(
            "SELECT {RESERVATION_COLUMNS} FROM spend_reservations WHERE scope = ?1 AND \
             (?3 IS NULL OR account = ?3) AND \
             ((state = 'reserved' AND created_at < ?2) OR (state = 'in_flight' AND in_flight_at < ?2)) \
             ORDER BY seq"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![scope, cutoff, account], reservation_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut report = ExpireReport::default();
    for res in stale {
        let seq = seq_of(&res.reservation_id)?;
        if res.state == ReservationState::Reserved {
            let allowance = require_allowance(conn, &res.scope, &res.period)?;
            let reserved = allowance
                .reserved_micro
                .checked_sub(res.estimate_micro)
                .filter(|v| *v >= 0)
                .ok_or(SpendError::Overflow)?;
            cas_totals(
                conn,
                &allowance,
                reserved,
                allowance.committed_micro,
                ctx.now,
            )?;
            conn.execute(
                "UPDATE spend_reservations SET state = 'expired', settled_at = ?2, \
                 reason = 'stale_lease' WHERE seq = ?1 AND state = 'reserved'",
                params![seq, ctx.now],
            )?;
            report.released_micro = report
                .released_micro
                .checked_add(res.estimate_micro)
                .ok_or(SpendError::Overflow)?;
            report
                .expired
                .push(require_reservation(conn, &res.reservation_id)?);
        } else {
            commit_at_ceiling(conn, ctx.now, &res, "lease_expired_in_flight")?;
            report.ceiling_micro = report
                .ceiling_micro
                .checked_add(res.estimate_micro)
                .ok_or(SpendError::Overflow)?;
            report
                .committed_at_ceiling
                .push(require_reservation(conn, &res.reservation_id)?);
        }
    }
    Ok(report)
}

fn acknowledge_impl(
    conn: &Connection,
    ctx: &Ctx,
    reservation_id: &str,
    note: Option<&str>,
) -> Result<()> {
    let res = require_reservation(conn, reservation_id)?;
    if !res.over_reservation {
        return Err(SpendError::BadInput(format!(
            "{reservation_id} is not flagged as an overrun"
        )));
    }
    conn.execute(
        "INSERT OR IGNORE INTO spend_overrun_acks (seq, acknowledged_at, note) VALUES (?1, ?2, ?3)",
        params![seq_of(reservation_id)?, ctx.now, note],
    )?;
    Ok(())
}

/// Move an open reservation to `committed` at its estimate, flagged `billed_unknown` and
/// `over_reservation`: the estimate leaves `reserved` and enters `committed` in one
/// compare-and-swap, and the row changes in the same transaction.
fn commit_at_ceiling(conn: &Connection, now: i64, res: &Reservation, reason: &str) -> Result<()> {
    let allowance = require_allowance(conn, &res.scope, &res.period)?;
    let reserved = allowance
        .reserved_micro
        .checked_sub(res.estimate_micro)
        .filter(|v| *v >= 0)
        .ok_or(SpendError::Overflow)?;
    let committed = allowance
        .committed_micro
        .checked_add(res.estimate_micro)
        .ok_or(SpendError::Overflow)?;
    cas_totals(conn, &allowance, reserved, committed, now)?;
    conn.execute(
        "UPDATE spend_reservations SET state = 'committed', committed_micro = ?2, \
         over_reservation = 1, billed_unknown = 1, settled_at = ?3, reason = ?4 \
         WHERE seq = ?1 AND state IN ('reserved', 'in_flight')",
        params![
            seq_of(&res.reservation_id)?,
            res.estimate_micro,
            now,
            reason
        ],
    )?;
    Ok(())
}

/// Run `f` in a savepoint on the caller's transaction: release it on success, roll it back on
/// failure, so a failed ledger call leaves none of its partial writes in the caller's
/// transaction.
fn atomic<T>(conn: &Connection, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    conn.execute_batch("SAVEPOINT rightkit_spend_op")?;
    match f(conn) {
        Ok(value) => {
            conn.execute_batch("RELEASE rightkit_spend_op")?;
            Ok(value)
        }
        Err(err) => {
            // Best effort: the original error is the one to report.
            let _ = conn.execute_batch("ROLLBACK TO rightkit_spend_op; RELEASE rightkit_spend_op");
            Err(err)
        }
    }
}

fn system_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn non_empty(name: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(SpendError::BadInput(format!("{name} is empty")))
    } else {
        Ok(())
    }
}

fn add(a: i64, b: i64) -> Result<i64> {
    a.checked_add(b).ok_or(SpendError::Overflow)
}

fn spent_total(a: &Allowance) -> Result<i64> {
    add(a.reserved_micro, a.committed_micro)
}

/// Estimates of the open reservations of `scope` in earlier periods of the same width: the
/// amount a reservation with carry-over counts against `period`'s cap on top of its own totals.
fn carried_open_micro(conn: &Connection, scope: &str, period: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(estimated_micro), 0) FROM spend_reservations \
         WHERE scope = ?1 AND period < ?2 AND length(period) = length(?2) \
         AND state IN ('reserved', 'in_flight')",
        params![scope, period],
        |row| row.get(0),
    )?)
}

/// The stored limit as a usable cap. Fail closed: anything that is not a finite non-negative
/// number (a hand-edited `inf`, `nan` text, a negative) reads as 0, so the allowance refuses
/// every non-zero reservation instead of lifting the limit.
fn limit_from_raw(raw: rusqlite::types::Value) -> i64 {
    use rusqlite::types::Value;
    let from_float = |f: f64| {
        if f.is_finite() && f >= 0.0 {
            f.floor() as i64
        } else {
            0
        }
    };
    match raw {
        Value::Integer(n) if n >= 0 => n,
        Value::Real(f) => from_float(f),
        Value::Text(t) => t.trim().parse::<f64>().map_or(0, from_float),
        _ => 0,
    }
}

/// Convert a decimal amount of currency units (for example dollars) to micro-units. Rejects a
/// non-finite or negative amount ([`SpendError::BadInput`]) and one beyond `i64`
/// ([`SpendError::Overflow`]), so a `NaN` or infinite cap never reaches the ledger.
pub fn micro_from_units(units: f64) -> Result<i64> {
    if !units.is_finite() || units < 0.0 {
        return Err(SpendError::BadInput(
            "amount must be a finite, non-negative number".into(),
        ));
    }
    let micro = (units * 1_000_000.0).round();
    if micro >= 9.2e18 {
        return Err(SpendError::Overflow);
    }
    Ok(micro as i64)
}

fn allowance_from_row(row: &Row<'_>) -> rusqlite::Result<Allowance> {
    Ok(Allowance {
        scope: row.get(0)?,
        period: row.get(1)?,
        limit_micro: limit_from_raw(row.get(2)?),
        reserved_micro: row.get(3)?,
        committed_micro: row.get(4)?,
        revision: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn reservation_from_row(row: &Row<'_>) -> rusqlite::Result<Reservation> {
    let seq: i64 = row.get(0)?;
    let state_text: String = row.get(7)?;
    let state = ReservationState::parse(&state_text).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            7,
            rusqlite::types::Type::Text,
            format!("unknown reservation state {state_text:?}").into(),
        )
    })?;
    let over: i64 = row.get(9)?;
    let unknown: i64 = row.get(16)?;
    Ok(Reservation {
        reservation_id: format!("res-{seq:08}"),
        request_id: row.get(1)?,
        scope: row.get(2)?,
        period: row.get(3)?,
        route_id: row.get(4)?,
        price_version: row.get(5)?,
        estimate_micro: row.get(6)?,
        state,
        committed_micro: row.get(8)?,
        over_reservation: over != 0,
        billed_unknown: unknown != 0,
        account: row.get(14)?,
        currency: row.get(15)?,
        created_at: row.get(10)?,
        in_flight_at: row.get(11)?,
        settled_at: row.get(12)?,
        reason: row.get(13)?,
    })
}

fn get_allowance(conn: &Connection, scope: &str, period: &str) -> Result<Option<Allowance>> {
    let sql = format!(
        "SELECT {ALLOWANCE_COLUMNS} FROM spend_allowances WHERE scope = ?1 AND period = ?2"
    );
    Ok(conn
        .query_row(&sql, params![scope, period], allowance_from_row)
        .optional()?)
}

fn require_allowance(conn: &Connection, scope: &str, period: &str) -> Result<Allowance> {
    get_allowance(conn, scope, period)?.ok_or_else(|| SpendError::AllowanceMissing {
        scope: scope.to_owned(),
        period: period.to_owned(),
    })
}

/// The `res-NNNNNNNN` id as its number.
fn seq_of(reservation_id: &str) -> Result<i64> {
    reservation_id
        .strip_prefix("res-")
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|n| n.parse::<i64>().ok())
        .ok_or_else(|| SpendError::NotFound(reservation_id.to_owned()))
}

fn find_reservation(conn: &Connection, reservation_id: &str) -> Result<Option<Reservation>> {
    let Ok(seq) = seq_of(reservation_id) else {
        return Ok(None);
    };
    let sql = format!("SELECT {RESERVATION_COLUMNS} FROM spend_reservations WHERE seq = ?1");
    Ok(conn
        .query_row(&sql, [seq], reservation_from_row)
        .optional()?)
}

fn require_reservation(conn: &Connection, reservation_id: &str) -> Result<Reservation> {
    find_reservation(conn, reservation_id)?
        .ok_or_else(|| SpendError::NotFound(reservation_id.to_owned()))
}

fn find_by_request(conn: &Connection, request_id: &str) -> Result<Option<Reservation>> {
    let sql = format!("SELECT {RESERVATION_COLUMNS} FROM spend_reservations WHERE request_id = ?1");
    Ok(conn
        .query_row(&sql, [request_id], reservation_from_row)
        .optional()?)
}

fn unacknowledged_overruns(conn: &Connection, scope: &str, period: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM spend_reservations r WHERE r.scope = ?1 AND r.period = ?2 \
         AND r.over_reservation = 1 AND NOT EXISTS (SELECT 1 FROM spend_overrun_acks a WHERE a.seq = r.seq)",
        params![scope, period],
        |row| row.get(0),
    )?)
}

/// The id of the oldest overrun on the allowance that no acknowledgement covers.
fn oldest_unacknowledged_overrun(conn: &Connection, scope: &str, period: &str) -> Result<String> {
    let seq: i64 = conn.query_row(
        "SELECT r.seq FROM spend_reservations r WHERE r.scope = ?1 AND r.period = ?2 \
         AND r.over_reservation = 1 AND NOT EXISTS (SELECT 1 FROM spend_overrun_acks a WHERE a.seq = r.seq) \
         ORDER BY r.seq LIMIT 1",
        params![scope, period],
        |row| row.get(0),
    )?;
    Ok(format!("res-{seq:08}"))
}

fn reservations_page(
    conn: &Connection,
    scope: &str,
    period: &str,
    open_only: bool,
    after: Option<&str>,
    limit: usize,
) -> Result<ReservationPage> {
    non_empty("scope", scope)?;
    non_empty("period", period)?;
    let limit = limit.clamp(1, MAX_PAGE);
    let after_seq = match after {
        Some(cursor) => seq_of(cursor)
            .map_err(|_| SpendError::BadInput(format!("invalid page cursor {cursor:?}")))?,
        None => 0,
    };
    let state_filter = if open_only {
        "AND state IN ('reserved', 'in_flight')"
    } else {
        ""
    };
    let sql = format!(
        "SELECT {RESERVATION_COLUMNS} FROM spend_reservations \
         WHERE scope = ?1 AND period = ?2 AND seq > ?3 {state_filter} ORDER BY seq LIMIT ?4"
    );
    let fetch = i64::try_from(limit + 1).map_err(|_| SpendError::Overflow)?;
    let mut stmt = conn.prepare(&sql)?;
    let mut reservations = stmt
        .query_map(
            params![scope, period, after_seq, fetch],
            reservation_from_row,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let next = if reservations.len() > limit {
        reservations.truncate(limit);
        reservations.last().map(|r| r.reservation_id.clone())
    } else {
        None
    };
    Ok(ReservationPage { reservations, next })
}

fn route_totals_by_state_rows(
    conn: &Connection,
    scope: &str,
    period: &str,
) -> Result<Vec<RouteStateTotal>> {
    let mut stmt = conn.prepare(
        "SELECT route_id, state, COUNT(*), COALESCE(SUM(estimated_micro), 0), \
         COALESCE(SUM(committed_micro), 0) FROM spend_reservations \
         WHERE scope = ?1 AND period = ?2 GROUP BY route_id, state ORDER BY route_id, state",
    )?;
    let rows = stmt.query_map(params![scope, period], |row| {
        let state_text: String = row.get(1)?;
        let state = ReservationState::parse(&state_text).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                format!("unknown reservation state {state_text:?}").into(),
            )
        })?;
        Ok(RouteStateTotal {
            route_id: row.get(0)?,
            state,
            count: row.get(2)?,
            estimated_micro: row.get(3)?,
            committed_micro: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn outcome(conn: &Connection, reservation: Reservation, replayed: bool) -> Result<Outcome> {
    let allowance = require_allowance(conn, &reservation.scope, &reservation.period)?;
    Ok(Outcome {
        reservation,
        allowance,
        replayed,
    })
}

/// The error for a lost compare-and-swap: re-read the revision that won.
fn conflict(conn: &Connection, read: &Allowance) -> Result<SpendError> {
    let actual = get_allowance(conn, &read.scope, &read.period)?.map_or(0, |a| a.revision);
    Ok(SpendError::RevisionConflict {
        expected: read.revision,
        actual,
    })
}

/// The one compare-and-swap on an allowance's totals: write only if `revision` still equals the
/// value read in this transaction. Keeps `reserved + committed` within `i64`.
fn cas_totals(
    conn: &Connection,
    read: &Allowance,
    reserved_micro: i64,
    committed_micro: i64,
    now: i64,
) -> Result<Allowance> {
    add(reserved_micro, committed_micro)?;
    let next = read.revision.checked_add(1).ok_or(SpendError::Overflow)?;
    let changed = conn.execute(
        "UPDATE spend_allowances SET reserved_micro = ?3, committed_micro = ?4, revision = ?5, \
         updated_at = ?6 WHERE scope = ?1 AND period = ?2 AND revision = ?7",
        params![
            read.scope,
            read.period,
            reserved_micro,
            committed_micro,
            next,
            now,
            read.revision
        ],
    )?;
    if changed == 0 {
        return Err(conflict(conn, read)?);
    }
    require_allowance(conn, &read.scope, &read.period)
}
