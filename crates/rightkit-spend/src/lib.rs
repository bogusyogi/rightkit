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

mod scope;
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
    #[error("{count} overrun(s) await acknowledgement before this allowance reserves again")]
    OverrunUnacknowledged { count: i64 },
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
}

impl ReserveOptions {
    /// Options with an account key.
    pub fn with_account(account: impl Into<String>) -> Self {
        Self {
            account: Some(account.into()),
            currency: None,
        }
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

const MIGRATIONS: &[Migration] = &[
    Migration::sql(1, "spend-ledger", SCHEMA_V1),
    Migration::sql(2, "spend-account-currency-unknown", SCHEMA_V2),
];

const ALLOWANCE_COLUMNS: &str =
    "scope, period, limit_micro, reserved_micro, committed_micro, revision, updated_at";

const RESERVATION_COLUMNS: &str = "seq, request_id, scope, period, route_id, price_version, \
    estimated_micro, state, committed_micro, over_reservation, created_at, in_flight_at, \
    settled_at, reason, account, currency, billed_unknown";

/// The clock reading and mode one ledger call runs under.
struct Ctx {
    now: i64,
    strict: bool,
}

/// The ledger. It owns one connection; open a second connection for the app's own queries, or
/// use [`Ledger::begin`] and the `_in` methods to write the app's rows in the same transaction.
pub struct Ledger {
    conn: Connection,
    clock: Box<dyn Fn() -> i64 + Send>,
    strict: bool,
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
        })
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
    /// options are part of the idempotency check: a repeat with a different account or
    /// currency is [`SpendError::RequestMismatch`].
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
        self.run(|conn, ctx| commit_impl(conn, ctx, reservation_id, actual_micro))
    }

    /// [`Ledger::commit`] inside the caller's transaction ([`Ledger::begin`]).
    pub fn commit_in(
        &self,
        tx: &Transaction<'_>,
        reservation_id: &str,
        actual_micro: i64,
    ) -> Result<Outcome> {
        self.run_in(tx, |conn, ctx| {
            commit_impl(conn, ctx, reservation_id, actual_micro)
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
            commit_impl(conn, ctx, reservation_id, actual_micro)
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
        let allowance = require_allowance(&self.conn, scope, period)?;
        let sql = format!(
            "SELECT {RESERVATION_COLUMNS} FROM spend_reservations WHERE scope = ?1 AND period = ?2 \
             ORDER BY seq"
        );
        let mut stmt = self.conn.prepare(&sql)?;
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
            unacknowledged_overruns: unacknowledged_overruns(&self.conn, scope, period)?,
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
    let unacked = unacknowledged_overruns(conn, scope, period)?;
    if unacked > 0 {
        return Err(SpendError::OverrunUnacknowledged { count: unacked });
    }
    let remaining = allowance.remaining_micro();
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
) -> Result<Outcome> {
    if actual_micro < 0 {
        return Err(SpendError::BadInput("actual_micro is negative".into()));
    }
    let res = require_reservation(conn, reservation_id)?;
    match res.state {
        ReservationState::Committed if !ctx.strict && res.committed_micro == Some(actual_micro) => {
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
    let reason = if !over {
        None
    } else if after.remaining_micro() >= 0 {
        Some("overrun taken from the allowance".to_owned())
    } else {
        Some(format!(
            "overrun exceeds the allowance; remaining {} after commit",
            after.remaining_micro()
        ))
    };
    conn.execute(
        "UPDATE spend_reservations SET state = 'committed', committed_micro = ?2, \
         over_reservation = ?3, settled_at = ?4, reason = ?5 \
         WHERE seq = ?1 AND state IN ('reserved', 'in_flight')",
        params![
            seq_of(reservation_id)?,
            actual_micro,
            i64::from(over),
            ctx.now,
            reason
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

fn allowance_from_row(row: &Row<'_>) -> rusqlite::Result<Allowance> {
    Ok(Allowance {
        scope: row.get(0)?,
        period: row.get(1)?,
        limit_micro: row.get(2)?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    const SCOPE: &str = "orthic/genright";
    const MONTH: &str = "2026-10";

    struct Fixture {
        ledger: Ledger,
        clock: Arc<AtomicI64>,
    }

    fn fixture() -> Fixture {
        let clock = Arc::new(AtomicI64::new(1_000_000));
        let c = Arc::clone(&clock);
        let conn = Connection::open_in_memory().unwrap();
        let ledger = Ledger::attach_with_clock(conn, move || c.load(Ordering::SeqCst)).unwrap();
        Fixture { ledger, clock }
    }

    fn funded(limit: i64) -> Fixture {
        let mut f = fixture();
        f.ledger.set_allowance(SCOPE, MONTH, limit, None).unwrap();
        f
    }

    fn reserve(f: &mut Fixture, request: &str, estimate: i64) -> Result<Outcome> {
        f.ledger
            .reserve(SCOPE, request, MONTH, "fal-ai/flux", "v1", estimate)
    }

    #[test]
    fn attach_is_repeatable_and_leaves_user_version_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA user_version = 7; CREATE TABLE app_table (x INTEGER);")
                .unwrap();
            let mut ledger = Ledger::attach(conn).unwrap();
            ledger.set_allowance(SCOPE, MONTH, 100, None).unwrap();
        }
        let conn = Connection::open(&path).unwrap();
        let ledger = Ledger::attach(conn).unwrap();
        assert_eq!(
            ledger.allowance(SCOPE, MONTH).unwrap().unwrap().limit_micro,
            100
        );
        let uv: i64 = ledger
            .connection()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(uv, 7);
    }

    #[test]
    fn set_allowance_creates_then_needs_the_revision() {
        let mut f = fixture();
        let a = f.ledger.set_allowance(SCOPE, MONTH, 1_000, None).unwrap();
        assert_eq!(
            (
                a.revision,
                a.limit_micro,
                a.reserved_micro,
                a.committed_micro
            ),
            (1, 1_000, 0, 0)
        );
        assert!(matches!(
            f.ledger.set_allowance(SCOPE, MONTH, 2_000, None),
            Err(SpendError::RevisionRequired)
        ));
        assert!(matches!(
            f.ledger.set_allowance(SCOPE, MONTH, 2_000, Some(5)),
            Err(SpendError::RevisionConflict {
                expected: 5,
                actual: 1
            })
        ));
        let b = f
            .ledger
            .set_allowance(SCOPE, MONTH, 2_000, Some(1))
            .unwrap();
        assert_eq!((b.revision, b.limit_micro), (2, 2_000));
        // A stale writer loses the swap.
        assert!(matches!(
            f.ledger.set_allowance(SCOPE, MONTH, 3_000, Some(1)),
            Err(SpendError::RevisionConflict {
                expected: 1,
                actual: 2
            })
        ));
        assert!(matches!(
            f.ledger.set_allowance(SCOPE, "2026-11", 10, Some(1)),
            Err(SpendError::RevisionConflict {
                expected: 1,
                actual: 0
            })
        ));
        assert!(matches!(
            f.ledger.set_allowance(SCOPE, "2026-12", -1, None),
            Err(SpendError::BadInput(_))
        ));
        assert!(matches!(
            f.ledger.set_allowance("", "p", 1, None),
            Err(SpendError::BadInput(_))
        ));
    }

    #[test]
    fn limit_cannot_drop_below_spend() {
        let mut f = funded(1_000);
        reserve(&mut f, "r1", 600).unwrap();
        assert!(matches!(
            f.ledger.set_allowance(SCOPE, MONTH, 599, Some(2)),
            Err(SpendError::AllowanceBelowSpend {
                limit_micro: 599,
                spent_micro: 600
            })
        ));
        let a = f.ledger.set_allowance(SCOPE, MONTH, 600, Some(2)).unwrap();
        assert_eq!(a.remaining_micro(), 0);
    }

    #[test]
    fn reserve_fits_exactly_and_refuses_beyond() {
        let mut f = funded(1_000);
        let a = reserve(&mut f, "r1", 400).unwrap();
        assert_eq!(a.reservation.reservation_id, "res-00000001");
        assert_eq!(a.reservation.state, ReservationState::Reserved);
        assert_eq!(a.allowance.reserved_micro, 400);
        assert!(!a.replayed);
        let b = reserve(&mut f, "r2", 600).unwrap();
        assert_eq!(
            b.allowance.remaining_micro(),
            0,
            "an estimate equal to the remaining fits"
        );
        assert!(matches!(
            reserve(&mut f, "r3", 1),
            Err(SpendError::AllowanceExceeded {
                estimate_micro: 1,
                remaining_micro: 0
            })
        ));
        // The refused reserve wrote nothing.
        assert_eq!(
            f.ledger
                .allowance(SCOPE, MONTH)
                .unwrap()
                .unwrap()
                .reserved_micro,
            1_000
        );
        assert!(f.ledger.reservation("res-00000003").unwrap().is_none());
    }

    #[test]
    fn reserve_needs_an_allowance_and_valid_input() {
        let mut f = fixture();
        assert!(matches!(
            reserve(&mut f, "r1", 1),
            Err(SpendError::AllowanceMissing { .. })
        ));
        let mut g = funded(10);
        assert!(matches!(
            reserve(&mut g, "r1", -1),
            Err(SpendError::BadInput(_))
        ));
        assert!(matches!(
            reserve(&mut g, " ", 1),
            Err(SpendError::BadInput(_))
        ));
    }

    #[test]
    fn reserve_is_idempotent_on_request_id() {
        let mut f = funded(1_000);
        let first = reserve(&mut f, "req-1", 300).unwrap();
        f.clock.fetch_add(60, Ordering::SeqCst);
        let again = reserve(&mut f, "req-1", 300).unwrap();
        assert!(again.replayed);
        assert_eq!(again.reservation, first.reservation);
        assert_eq!(
            again.allowance.reserved_micro, 300,
            "nothing reserved twice"
        );
        assert_eq!(
            again.allowance.revision, first.allowance.revision,
            "nothing written"
        );
        for bad in [
            f.ledger
                .reserve(SCOPE, "req-1", MONTH, "fal-ai/flux", "v1", 301),
            f.ledger
                .reserve(SCOPE, "req-1", MONTH, "other-route", "v1", 300),
            f.ledger
                .reserve(SCOPE, "req-1", MONTH, "fal-ai/flux", "v2", 300),
            f.ledger
                .reserve(SCOPE, "req-1", "2026-11", "fal-ai/flux", "v1", 300),
            f.ledger
                .reserve("other", "req-1", MONTH, "fal-ai/flux", "v1", 300),
        ] {
            assert!(matches!(bad, Err(SpendError::RequestMismatch { .. })));
        }
        // A replay still succeeds when the allowance has no room left.
        reserve(&mut f, "req-2", 700).unwrap();
        assert!(reserve(&mut f, "req-1", 300).unwrap().replayed);
    }

    #[test]
    fn commit_below_estimate_returns_the_unused_part() {
        let mut f = funded(1_000);
        let r = reserve(&mut f, "r1", 400).unwrap().reservation;
        let out = f.ledger.commit(&r.reservation_id, 150).unwrap();
        assert_eq!(out.reservation.state, ReservationState::Committed);
        assert_eq!(out.reservation.committed_micro, Some(150));
        assert!(!out.reservation.over_reservation);
        assert_eq!(
            (out.allowance.reserved_micro, out.allowance.committed_micro),
            (0, 150)
        );
        assert_eq!(out.allowance.remaining_micro(), 850);
        // Same actual again is a no-op; a different one is a transition error.
        let again = f.ledger.commit(&r.reservation_id, 150).unwrap();
        assert!(again.replayed);
        assert!(matches!(
            f.ledger.commit(&r.reservation_id, 151),
            Err(SpendError::InvalidTransition {
                from: ReservationState::Committed,
                ..
            })
        ));
        assert!(matches!(
            f.ledger.release(&r.reservation_id, None),
            Err(SpendError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn overrun_within_the_allowance_is_flagged_and_blocks_reserve_until_acknowledged() {
        let mut f = funded(1_000);
        let r = reserve(&mut f, "r1", 400).unwrap().reservation;
        let out = f.ledger.commit(&r.reservation_id, 500).unwrap();
        assert!(out.reservation.over_reservation);
        assert_eq!(
            out.reservation.reason.as_deref(),
            Some("overrun taken from the allowance")
        );
        assert_eq!(out.allowance.remaining_micro(), 500);
        assert!(matches!(
            reserve(&mut f, "r2", 1),
            Err(SpendError::OverrunUnacknowledged { count: 1 })
        ));
        // Acknowledging a reservation that was not flagged is refused.
        let ok = {
            f.ledger
                .acknowledge_overrun(&r.reservation_id, Some("seen"))
                .unwrap();
            f.ledger
                .acknowledge_overrun(&r.reservation_id, None)
                .unwrap();
            reserve(&mut f, "r2", 1)
        };
        assert!(ok.is_ok());
        let clean = ok.unwrap().reservation;
        assert!(matches!(
            f.ledger.acknowledge_overrun(&clean.reservation_id, None),
            Err(SpendError::BadInput(_))
        ));
    }

    #[test]
    fn overrun_beyond_the_allowance_goes_negative_and_refuses_later_reserves() {
        let mut f = funded(1_000);
        let r = reserve(&mut f, "r1", 1_000).unwrap().reservation;
        let out = f.ledger.commit(&r.reservation_id, 1_300).unwrap();
        assert!(out.reservation.over_reservation);
        assert_eq!(out.allowance.remaining_micro(), -300);
        assert!(out.reservation.reason.unwrap().contains("-300"));
        f.ledger
            .acknowledge_overrun(&r.reservation_id, None)
            .unwrap();
        assert!(matches!(
            reserve(&mut f, "r2", 0),
            Err(SpendError::AllowanceExceeded {
                remaining_micro: -300,
                ..
            })
        ));
        // Raising the limit restores room.
        let a = f
            .ledger
            .set_allowance(SCOPE, MONTH, 2_000, Some(out.allowance.revision))
            .unwrap();
        assert_eq!(a.remaining_micro(), 700);
        assert!(reserve(&mut f, "r3", 700).is_ok());
    }

    #[test]
    fn release_returns_the_estimate_and_is_idempotent() {
        let mut f = funded(1_000);
        let r = reserve(&mut f, "r1", 400).unwrap().reservation;
        let out = f
            .ledger
            .release(&r.reservation_id, Some("call skipped"))
            .unwrap();
        assert_eq!(out.reservation.state, ReservationState::Released);
        assert_eq!(out.reservation.reason.as_deref(), Some("call skipped"));
        assert_eq!(out.allowance.reserved_micro, 0);
        assert!(f.ledger.release(&r.reservation_id, None).unwrap().replayed);
        assert!(matches!(
            f.ledger.commit(&r.reservation_id, 1),
            Err(SpendError::InvalidTransition {
                from: ReservationState::Released,
                ..
            })
        ));
        assert!(matches!(
            f.ledger.release("res-00000099", None),
            Err(SpendError::NotFound(_))
        ));
        assert!(matches!(
            f.ledger.release("nonsense", None),
            Err(SpendError::NotFound(_))
        ));
    }

    #[test]
    fn in_flight_can_commit_or_release_and_only_from_reserved() {
        let mut f = funded(1_000);
        let a = reserve(&mut f, "a", 100).unwrap().reservation;
        let marked = f.ledger.mark_in_flight(&a.reservation_id).unwrap();
        assert_eq!(marked.reservation.state, ReservationState::InFlight);
        assert_eq!(marked.reservation.in_flight_at, Some(1_000_000));
        assert!(f.ledger.mark_in_flight(&a.reservation_id).unwrap().replayed);
        assert_eq!(
            f.ledger
                .commit(&a.reservation_id, 90)
                .unwrap()
                .reservation
                .state,
            ReservationState::Committed
        );
        assert!(matches!(
            f.ledger.mark_in_flight(&a.reservation_id),
            Err(SpendError::InvalidTransition {
                from: ReservationState::Committed,
                ..
            })
        ));
        let b = reserve(&mut f, "b", 100).unwrap().reservation;
        f.ledger.mark_in_flight(&b.reservation_id).unwrap();
        assert_eq!(
            f.ledger
                .release(&b.reservation_id, None)
                .unwrap()
                .allowance
                .reserved_micro,
            0
        );
    }

    #[test]
    fn expire_releases_stale_reserved_and_commits_stale_in_flight_at_ceiling() {
        let mut f = funded(10_000);
        let stale = reserve(&mut f, "stale", 100).unwrap().reservation;
        let sent = reserve(&mut f, "sent", 200).unwrap().reservation;
        f.ledger.mark_in_flight(&sent.reservation_id).unwrap();
        f.clock.fetch_add(100, Ordering::SeqCst);
        let fresh = reserve(&mut f, "fresh", 300).unwrap().reservation;
        f.clock.fetch_add(100, Ordering::SeqCst);

        // Exactly 100 s old is kept; the 200 s old ones go.
        let report = f.ledger.expire_stale(SCOPE, 100).unwrap();
        assert_eq!(report.expired.len(), 1);
        assert_eq!(report.expired[0].reservation_id, stale.reservation_id);
        assert_eq!(report.expired[0].state, ReservationState::Expired);
        assert_eq!(report.released_micro, 100);
        assert_eq!(report.committed_at_ceiling.len(), 1);
        let c = &report.committed_at_ceiling[0];
        assert_eq!(c.reservation_id, sent.reservation_id);
        assert_eq!(c.state, ReservationState::Committed);
        assert_eq!(c.committed_micro, Some(200));
        assert!(c.over_reservation);
        assert_eq!(c.reason.as_deref(), Some("lease_expired_in_flight"));
        assert_eq!(report.ceiling_micro, 200);

        let a = f.ledger.allowance(SCOPE, MONTH).unwrap().unwrap();
        assert_eq!((a.reserved_micro, a.committed_micro), (300, 200));
        assert_eq!(
            f.ledger
                .reservation(&fresh.reservation_id)
                .unwrap()
                .unwrap()
                .state,
            ReservationState::Reserved
        );
        // The ceiling commit is a flagged overrun: reserve waits for an acknowledgement.
        assert!(matches!(
            reserve(&mut f, "next", 1),
            Err(SpendError::OverrunUnacknowledged { count: 1 })
        ));
        // Nothing left to expire.
        assert_eq!(
            f.ledger.expire_stale(SCOPE, 100).unwrap(),
            ExpireReport::default()
        );
        assert!(matches!(
            f.ledger.expire_stale(SCOPE, -1),
            Err(SpendError::BadInput(_))
        ));
    }

    #[test]
    fn periods_and_scopes_are_independent_allowances() {
        let mut f = funded(1_000);
        f.ledger
            .set_allowance(SCOPE, "2026-10-17", 100, None)
            .unwrap();
        f.ledger
            .set_allowance(SCOPE, "device:dev_abc", 50, None)
            .unwrap();
        f.ledger.set_allowance("other", MONTH, 5, None).unwrap();
        f.ledger.reserve(SCOPE, "m", MONTH, "r", "v1", 60).unwrap();
        f.ledger
            .reserve(SCOPE, "d", "2026-10-17", "r", "v1", 60)
            .unwrap();
        assert!(matches!(
            f.ledger
                .reserve(SCOPE, "dev", "device:dev_abc", "r", "v1", 60),
            Err(SpendError::AllowanceExceeded {
                remaining_micro: 50,
                ..
            })
        ));
        assert!(matches!(
            f.ledger.reserve(SCOPE, "d2", "2026-10-17", "r", "v1", 41),
            Err(SpendError::AllowanceExceeded {
                remaining_micro: 40,
                ..
            })
        ));
        assert_eq!(
            f.ledger
                .allowance(SCOPE, MONTH)
                .unwrap()
                .unwrap()
                .reserved_micro,
            60
        );
        assert_eq!(
            f.ledger
                .allowance("other", MONTH)
                .unwrap()
                .unwrap()
                .reserved_micro,
            0
        );
    }

    #[test]
    fn reservation_ids_are_never_reused() {
        let mut f = funded(1_000);
        let a = reserve(&mut f, "a", 1).unwrap().reservation;
        f.ledger.release(&a.reservation_id, None).unwrap();
        let b = reserve(&mut f, "b", 1).unwrap().reservation;
        assert_eq!(
            (a.reservation_id.as_str(), b.reservation_id.as_str()),
            ("res-00000001", "res-00000002")
        );
    }

    #[test]
    fn checked_arithmetic_refuses_overflow() {
        let mut f = fixture();
        f.ledger
            .set_allowance(SCOPE, MONTH, i64::MAX, None)
            .unwrap();
        let a = reserve(&mut f, "a", i64::MAX - 1).unwrap().reservation;
        let out = f.ledger.commit(&a.reservation_id, i64::MAX - 1).unwrap();
        assert_eq!(out.allowance.committed_micro, i64::MAX - 1);
        let b = f
            .ledger
            .reserve(SCOPE, "b", MONTH, "r", "v1", 1)
            .unwrap()
            .reservation;
        // Committing 2 more would push committed + reserved past i64::MAX.
        assert!(matches!(
            f.ledger.commit(&b.reservation_id, 2),
            Err(SpendError::Overflow)
        ));
        // The failed commit left the reservation open and the totals unchanged.
        let a2 = f.ledger.allowance(SCOPE, MONTH).unwrap().unwrap();
        assert_eq!((a2.reserved_micro, a2.committed_micro), (1, i64::MAX - 1));
    }

    #[test]
    fn report_reconciles_and_groups_by_route() {
        let mut f = funded(10_000);
        let a = f
            .ledger
            .reserve(SCOPE, "a", MONTH, "route-a", "v1", 100)
            .unwrap()
            .reservation;
        let b = f
            .ledger
            .reserve(SCOPE, "b", MONTH, "route-b", "v1", 200)
            .unwrap()
            .reservation;
        let c = f
            .ledger
            .reserve(SCOPE, "c", MONTH, "route-a", "v1", 300)
            .unwrap()
            .reservation;
        let d = f
            .ledger
            .reserve(SCOPE, "d", MONTH, "route-b", "v1", 50)
            .unwrap()
            .reservation;
        f.ledger.commit(&a.reservation_id, 80).unwrap();
        f.ledger.commit(&b.reservation_id, 260).unwrap();
        f.ledger.release(&d.reservation_id, None).unwrap();
        f.ledger.mark_in_flight(&c.reservation_id).unwrap();

        let r = f.ledger.report(SCOPE, MONTH).unwrap();
        assert_eq!(
            (
                r.count_reserved,
                r.count_in_flight,
                r.count_committed,
                r.count_released,
                r.count_expired
            ),
            (0, 1, 2, 1, 0)
        );
        assert_eq!(r.over_reservations, 1);
        assert_eq!(r.unacknowledged_overruns, 1);
        assert_eq!(r.open_micro, 300);
        assert_eq!(r.measured_micro, 340);
        assert_eq!(
            r.forecast_micro,
            300 + 300,
            "committed estimates 100+200 plus open 300"
        );
        assert_eq!(r.variance_micro, 340 - 300);
        assert_eq!(r.remaining_micro, 10_000 - 300 - 340);
        assert!(r.reconciles);
        assert_eq!(
            r.routes,
            vec![
                RouteTotal {
                    route_id: "route-a".into(),
                    count: 2,
                    estimated_micro: 400,
                    committed_micro: 80
                },
                RouteTotal {
                    route_id: "route-b".into(),
                    count: 2,
                    estimated_micro: 250,
                    committed_micro: 260
                },
            ]
        );
        assert!(matches!(
            f.ledger.report(SCOPE, "2030-01"),
            Err(SpendError::AllowanceMissing { .. })
        ));
    }

    #[test]
    fn state_survives_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.db");
        let id = {
            let mut ledger = Ledger::attach(Connection::open(&path).unwrap()).unwrap();
            ledger.set_allowance(SCOPE, MONTH, 1_000, None).unwrap();
            let r = ledger
                .reserve(SCOPE, "req", MONTH, "r", "v1", 250)
                .unwrap()
                .reservation;
            ledger.mark_in_flight(&r.reservation_id).unwrap();
            r.reservation_id
        };
        let mut ledger = Ledger::attach(Connection::open(&path).unwrap()).unwrap();
        let r = ledger.reservation(&id).unwrap().unwrap();
        assert_eq!(r.state, ReservationState::InFlight);
        // The retry after a crash finds the same reservation.
        assert!(
            ledger
                .reserve(SCOPE, "req", MONTH, "r", "v1", 250)
                .unwrap()
                .replayed
        );
        let out = ledger.commit(&id, 240).unwrap();
        assert_eq!(out.allowance.remaining_micro(), 760);
    }

    fn reserve_with(
        f: &mut Fixture,
        request: &str,
        estimate: i64,
        options: &ReserveOptions,
    ) -> Result<Outcome> {
        f.ledger.reserve_with(
            SCOPE,
            request,
            MONTH,
            "fal-ai/flux",
            "v1",
            estimate,
            options,
        )
    }

    fn sidecar(f: &Fixture) {
        f.ledger
            .connection()
            .execute_batch("CREATE TABLE sidecar (note TEXT NOT NULL)")
            .unwrap();
    }

    fn sidecar_rows(f: &Fixture) -> i64 {
        f.ledger
            .connection()
            .query_row("SELECT COUNT(*) FROM sidecar", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn commit_unknown_commits_at_the_ceiling_in_one_swap_and_blocks_until_acknowledged() {
        let mut f = funded(1_000);
        let r = reserve(&mut f, "r1", 400).unwrap();
        f.ledger
            .mark_in_flight(&r.reservation.reservation_id)
            .unwrap();
        let before = f.ledger.allowance(SCOPE, MONTH).unwrap().unwrap();
        let out = f
            .ledger
            .commit_unknown(&r.reservation.reservation_id, Some("timeout after send"))
            .unwrap();
        let res = &out.reservation;
        assert_eq!(res.state, ReservationState::Committed);
        assert_eq!(res.committed_micro, Some(400));
        assert!(res.billed_unknown);
        assert!(res.over_reservation);
        assert_eq!(res.reason.as_deref(), Some("timeout after send"));
        assert_eq!(
            (out.allowance.reserved_micro, out.allowance.committed_micro),
            (0, 400)
        );
        assert_eq!(
            out.allowance.revision,
            before.revision + 1,
            "one compare-and-swap for the whole commit"
        );
        assert!(matches!(
            reserve(&mut f, "r2", 1),
            Err(SpendError::OverrunUnacknowledged { count: 1 })
        ));
        let report = f.ledger.report(SCOPE, MONTH).unwrap();
        assert_eq!(report.billed_unknown, 1);
        assert_eq!(report.unacknowledged_overruns, 1);
        assert!(report.reconciles);
        f.ledger
            .acknowledge_overrun(&res.reservation_id, Some("checked the provider bill"))
            .unwrap();
        assert!(reserve(&mut f, "r2", 1).is_ok());
    }

    #[test]
    fn commit_unknown_works_from_reserved_defaults_its_reason_and_repeats_as_a_replay() {
        let mut f = funded(1_000);
        let r = reserve(&mut f, "r1", 250).unwrap().reservation;
        let out = f.ledger.commit_unknown(&r.reservation_id, None).unwrap();
        assert_eq!(out.reservation.reason.as_deref(), Some("billed_unknown"));
        assert_eq!(out.reservation.in_flight_at, None);
        let again = f.ledger.commit_unknown(&r.reservation_id, None).unwrap();
        assert!(again.replayed);
        assert_eq!(again.allowance, out.allowance, "nothing written twice");
        f.ledger
            .acknowledge_overrun(&r.reservation_id, None)
            .unwrap();
        // The same ceiling as a plain commit is also a replay; another actual is refused.
        assert!(f.ledger.commit(&r.reservation_id, 250).unwrap().replayed);
        assert!(matches!(
            f.ledger.commit(&r.reservation_id, 100),
            Err(SpendError::InvalidTransition { .. })
        ));
        // A reservation settled any other way cannot become billed-unknown.
        let ok = reserve(&mut f, "r2", 10).unwrap().reservation;
        f.ledger.release(&ok.reservation_id, None).unwrap();
        assert!(matches!(
            f.ledger.commit_unknown(&ok.reservation_id, None),
            Err(SpendError::InvalidTransition {
                from: ReservationState::Released,
                ..
            })
        ));
        let done = reserve(&mut f, "r3", 10).unwrap().reservation;
        f.ledger.commit(&done.reservation_id, 5).unwrap();
        assert!(matches!(
            f.ledger.commit_unknown(&done.reservation_id, None),
            Err(SpendError::InvalidTransition {
                from: ReservationState::Committed,
                ..
            })
        ));
        assert!(matches!(
            f.ledger.commit_unknown("res-00000099", None),
            Err(SpendError::NotFound(_))
        ));
    }

    #[test]
    fn an_expired_in_flight_lease_is_billed_unknown() {
        let mut f = funded(1_000);
        let r = reserve(&mut f, "r1", 100).unwrap().reservation;
        f.ledger.mark_in_flight(&r.reservation_id).unwrap();
        f.clock.fetch_add(500, Ordering::SeqCst);
        let report = f.ledger.expire_stale(SCOPE, 100).unwrap();
        assert!(report.committed_at_ceiling[0].billed_unknown);
        assert_eq!(f.ledger.report(SCOPE, MONTH).unwrap().billed_unknown, 1);
    }

    #[test]
    fn account_and_currency_are_stored_and_part_of_the_idempotency_check() {
        let mut f = funded(1_000);
        let opts = ReserveOptions::with_account("acct-a").currency("USD");
        let first = reserve_with(&mut f, "req", 100, &opts).unwrap();
        assert_eq!(first.reservation.account.as_deref(), Some("acct-a"));
        assert_eq!(first.reservation.currency.as_deref(), Some("USD"));
        assert!(reserve_with(&mut f, "req", 100, &opts).unwrap().replayed);
        for bad in [
            reserve(&mut f, "req", 100),
            reserve_with(
                &mut f,
                "req",
                100,
                &ReserveOptions::with_account("acct-b").currency("USD"),
            ),
            reserve_with(
                &mut f,
                "req",
                100,
                &ReserveOptions::with_account("acct-a").currency("EUR"),
            ),
            reserve_with(&mut f, "req", 100, &ReserveOptions::with_account("acct-a")),
        ] {
            assert!(matches!(bad, Err(SpendError::RequestMismatch { .. })));
        }
        let plain = reserve(&mut f, "plain", 10).unwrap().reservation;
        assert_eq!((plain.account, plain.currency), (None, None));
        for bad in [
            ReserveOptions::with_account(" "),
            ReserveOptions::default().currency(""),
        ] {
            assert!(matches!(
                reserve_with(&mut f, "bad", 1, &bad),
                Err(SpendError::BadInput(_))
            ));
        }
    }

    #[test]
    fn currency_is_fixed_at_reserve_time_through_settlement() {
        let mut f = funded(1_000);
        let r = reserve_with(
            &mut f,
            "r1",
            100,
            &ReserveOptions::default().currency("USD"),
        )
        .unwrap()
        .reservation;
        f.ledger.mark_in_flight(&r.reservation_id).unwrap();
        let out = f.ledger.commit(&r.reservation_id, 90).unwrap();
        assert_eq!(out.reservation.currency.as_deref(), Some("USD"));
        // The stored value is what a later read returns, whatever the caller uses next.
        let next = reserve_with(
            &mut f,
            "r2",
            100,
            &ReserveOptions::default().currency("EUR"),
        )
        .unwrap()
        .reservation;
        assert_eq!(
            f.ledger
                .reservation(&r.reservation_id)
                .unwrap()
                .unwrap()
                .currency
                .as_deref(),
            Some("USD")
        );
        assert_eq!(next.currency.as_deref(), Some("EUR"));
    }

    #[test]
    fn expiry_can_be_limited_to_one_account() {
        let mut f = funded(10_000);
        let acct_a = ReserveOptions::with_account("acct-a");
        let a1 = reserve_with(&mut f, "a1", 100, &acct_a)
            .unwrap()
            .reservation;
        let a2 = reserve_with(&mut f, "a2", 200, &acct_a)
            .unwrap()
            .reservation;
        f.ledger.mark_in_flight(&a2.reservation_id).unwrap();
        let b = reserve_with(&mut f, "b", 300, &ReserveOptions::with_account("acct-b"))
            .unwrap()
            .reservation;
        let none = reserve(&mut f, "none", 400).unwrap().reservation;
        f.clock.fetch_add(100, Ordering::SeqCst);

        let report = f.ledger.expire_stale_account(SCOPE, "acct-a", 50).unwrap();
        assert_eq!(report.expired.len(), 1);
        assert_eq!(report.expired[0].reservation_id, a1.reservation_id);
        assert_eq!(report.released_micro, 100);
        assert_eq!(report.committed_at_ceiling.len(), 1);
        assert_eq!(
            report.committed_at_ceiling[0].reservation_id,
            a2.reservation_id
        );
        assert_eq!(report.ceiling_micro, 200);
        let a = f.ledger.allowance(SCOPE, MONTH).unwrap().unwrap();
        assert_eq!((a.reserved_micro, a.committed_micro), (700, 200));
        for id in [&b.reservation_id, &none.reservation_id] {
            assert_eq!(
                f.ledger.reservation(id).unwrap().unwrap().state,
                ReservationState::Reserved,
                "another account, or none, is untouched"
            );
        }
        assert_eq!(
            f.ledger.expire_stale_account(SCOPE, "acct-a", 50).unwrap(),
            ExpireReport::default()
        );
        assert_eq!(
            f.ledger.expire_stale_account(SCOPE, "nobody", 50).unwrap(),
            ExpireReport::default()
        );
        assert!(matches!(
            f.ledger.expire_stale_account(SCOPE, " ", 50),
            Err(SpendError::BadInput(_))
        ));
        // Scope-wide expiry still takes every account.
        assert_eq!(f.ledger.expire_stale(SCOPE, 50).unwrap().expired.len(), 2);
    }

    #[test]
    fn strict_mode_turns_replays_into_invalid_transitions() {
        let mut f = funded(1_000);
        assert!(!f.ledger.is_strict());
        f.ledger.set_strict(true);
        assert!(f.ledger.is_strict());

        let a = reserve(&mut f, "a", 100).unwrap().reservation;
        assert!(
            reserve(&mut f, "a", 100).unwrap().replayed,
            "the request id stays idempotent"
        );
        f.ledger.mark_in_flight(&a.reservation_id).unwrap();
        assert!(matches!(
            f.ledger.mark_in_flight(&a.reservation_id),
            Err(SpendError::InvalidTransition {
                from: ReservationState::InFlight,
                to: ReservationState::InFlight
            })
        ));
        f.ledger.commit(&a.reservation_id, 90).unwrap();
        assert!(matches!(
            f.ledger.commit(&a.reservation_id, 90),
            Err(SpendError::InvalidTransition {
                from: ReservationState::Committed,
                to: ReservationState::Committed
            })
        ));

        let b = reserve(&mut f, "b", 100).unwrap().reservation;
        f.ledger.release(&b.reservation_id, None).unwrap();
        assert!(matches!(
            f.ledger.release(&b.reservation_id, None),
            Err(SpendError::InvalidTransition {
                from: ReservationState::Released,
                to: ReservationState::Released
            })
        ));

        let c = reserve(&mut f, "c", 100).unwrap().reservation;
        f.ledger.commit_unknown(&c.reservation_id, None).unwrap();
        assert!(matches!(
            f.ledger.commit_unknown(&c.reservation_id, None),
            Err(SpendError::InvalidTransition {
                from: ReservationState::Committed,
                ..
            })
        ));

        // The refused repeats changed nothing.
        let allowance = f.ledger.allowance(SCOPE, MONTH).unwrap().unwrap();
        assert_eq!(
            (allowance.reserved_micro, allowance.committed_micro),
            (0, 190)
        );
        assert!(f.ledger.report(SCOPE, MONTH).unwrap().reconciles);

        // The `_in` variants follow the same mode.
        f.ledger
            .acknowledge_overrun(&c.reservation_id, None)
            .unwrap();
        let d = reserve(&mut f, "d", 10).unwrap().reservation;
        let tx = f.ledger.begin().unwrap();
        f.ledger.release_in(&tx, &d.reservation_id, None).unwrap();
        assert!(matches!(
            f.ledger.release_in(&tx, &d.reservation_id, None),
            Err(SpendError::InvalidTransition { .. })
        ));
        tx.commit().unwrap();
        assert_eq!(
            f.ledger
                .reservation(&d.reservation_id)
                .unwrap()
                .unwrap()
                .state,
            ReservationState::Released
        );

        f.ledger.set_strict(false);
        assert!(f.ledger.release(&d.reservation_id, None).unwrap().replayed);
        let lenient = Ledger::attach(Connection::open_in_memory().unwrap())
            .unwrap()
            .with_strict(true);
        assert!(lenient.is_strict());
    }

    #[test]
    fn every_in_variant_commits_with_the_callers_transaction() {
        let mut f = fixture();
        sidecar(&f);
        let tx = f.ledger.begin().unwrap();
        f.ledger
            .set_allowance_in(&tx, SCOPE, MONTH, 10_000, None)
            .unwrap();
        let ledger = &f.ledger;
        let opts = ReserveOptions::default();
        let acct_x = ReserveOptions::with_account("x");
        let res = |request: &str, estimate: i64, options: &ReserveOptions| {
            ledger
                .reserve_in(&tx, SCOPE, request, MONTH, "r", "v1", estimate, options)
                .unwrap()
                .reservation
                .reservation_id
        };
        let a = res("a", 100, &opts);
        let b = res("b", 200, &opts);
        let c = res("c", 300, &acct_x);
        let d = res("d", 50, &opts);
        let e = res("e", 40, &opts);

        ledger.mark_in_flight_in(&tx, &a).unwrap();
        ledger.commit_in(&tx, &a, 90).unwrap();
        ledger.commit_unknown_in(&tx, &b, None).unwrap();
        ledger.release_in(&tx, &e, Some("skip")).unwrap();
        tx.execute("INSERT INTO sidecar (note) VALUES ('inside')", [])
            .unwrap();
        f.clock.fetch_add(100, Ordering::SeqCst);
        let by_account = ledger.expire_stale_account_in(&tx, SCOPE, "x", 50).unwrap();
        assert_eq!(by_account.expired.len(), 1);
        assert_eq!(by_account.expired[0].reservation_id, c);
        let by_scope = ledger.expire_stale_in(&tx, SCOPE, 50).unwrap();
        assert_eq!(by_scope.expired.len(), 1);
        assert_eq!(by_scope.expired[0].reservation_id, d);
        ledger
            .acknowledge_overrun_in(&tx, &b, Some("seen"))
            .unwrap();
        tx.commit().unwrap();

        assert_eq!(sidecar_rows(&f), 1);
        let a = f.ledger.allowance(SCOPE, MONTH).unwrap().unwrap();
        assert_eq!((a.reserved_micro, a.committed_micro), (0, 290));
        let report = f.ledger.report(SCOPE, MONTH).unwrap();
        assert!(report.reconciles);
        assert_eq!(report.unacknowledged_overruns, 0);
        assert!(reserve(&mut f, "next", 1).is_ok());
    }

    #[test]
    fn dropping_the_callers_transaction_rolls_back_ledger_and_sidecar_together() {
        let f = funded(1_000);
        sidecar(&f);
        let before = f.ledger.allowance(SCOPE, MONTH).unwrap().unwrap();
        {
            let tx = f.ledger.begin().unwrap();
            let held = f
                .ledger
                .reserve_in(
                    &tx,
                    SCOPE,
                    "r1",
                    MONTH,
                    "r",
                    "v1",
                    400,
                    &ReserveOptions::default(),
                )
                .unwrap();
            tx.execute(
                "INSERT INTO sidecar (note) VALUES (?1)",
                [&held.reservation.reservation_id],
            )
            .unwrap();
            let id = held.reservation.reservation_id;
            f.ledger.mark_in_flight_in(&tx, &id).unwrap();
            f.ledger.commit_in(&tx, &id, 300).unwrap();
            assert_eq!(
                f.ledger
                    .allowance(SCOPE, MONTH)
                    .unwrap()
                    .unwrap()
                    .committed_micro,
                300,
                "visible inside the transaction"
            );
            drop(tx);
        }
        assert_eq!(f.ledger.allowance(SCOPE, MONTH).unwrap().unwrap(), before);
        assert!(f.ledger.reservation("res-00000001").unwrap().is_none());
        assert_eq!(sidecar_rows(&f), 0);

        // The same work committed lands everywhere, and the ids restart from the rolled-back
        // sequence because the AUTOINCREMENT bump rolled back too.
        let tx = f.ledger.begin().unwrap();
        let held = f
            .ledger
            .reserve_in(
                &tx,
                SCOPE,
                "r1",
                MONTH,
                "r",
                "v1",
                400,
                &ReserveOptions::default(),
            )
            .unwrap();
        tx.execute(
            "INSERT INTO sidecar (note) VALUES (?1)",
            [&held.reservation.reservation_id],
        )
        .unwrap();
        tx.commit().unwrap();
        assert_eq!(sidecar_rows(&f), 1);
        assert_eq!(
            f.ledger
                .allowance(SCOPE, MONTH)
                .unwrap()
                .unwrap()
                .reserved_micro,
            400
        );
    }

    #[test]
    fn a_failed_in_call_undoes_its_partial_writes_and_keeps_the_transaction_usable() {
        let mut f = funded(1_000);
        let id = reserve(&mut f, "r1", 400)
            .unwrap()
            .reservation
            .reservation_id;
        let before = f.ledger.allowance(SCOPE, MONTH).unwrap().unwrap();
        f.ledger
            .connection()
            .execute_batch(
                "CREATE TRIGGER block_settle BEFORE UPDATE ON spend_reservations \
                 WHEN NEW.state = 'committed' BEGIN SELECT RAISE(ABORT, 'blocked'); END;",
            )
            .unwrap();
        let tx = f.ledger.begin().unwrap();
        // The allowance swap runs first and the row update then aborts: the swap must not stay.
        assert!(matches!(
            f.ledger.commit_in(&tx, &id, 300),
            Err(SpendError::Sqlite(_))
        ));
        assert!(matches!(
            f.ledger.commit_unknown_in(&tx, &id, None),
            Err(SpendError::Sqlite(_))
        ));
        assert_eq!(
            f.ledger.allowance(SCOPE, MONTH).unwrap().unwrap(),
            before,
            "the failed calls left no partial write in the caller's transaction"
        );
        // A refused reserve inside the transaction also writes nothing and the tx carries on.
        assert!(matches!(
            f.ledger.reserve_in(
                &tx,
                SCOPE,
                "big",
                MONTH,
                "r",
                "v1",
                10_000,
                &ReserveOptions::default()
            ),
            Err(SpendError::AllowanceExceeded { .. })
        ));
        tx.execute_batch("DROP TRIGGER block_settle").unwrap();
        let out = f.ledger.commit_in(&tx, &id, 300).unwrap();
        assert_eq!(out.allowance.committed_micro, 300);
        tx.commit().unwrap();
        let after = f.ledger.allowance(SCOPE, MONTH).unwrap().unwrap();
        assert_eq!((after.reserved_micro, after.committed_micro), (0, 300));
        assert!(f.ledger.report(SCOPE, MONTH).unwrap().reconciles);
    }

    #[test]
    fn begin_refuses_a_second_open_transaction() {
        let f = funded(10);
        let tx = f.ledger.begin().unwrap();
        assert!(f.ledger.begin().is_err());
        drop(tx);
        assert!(f.ledger.begin().is_ok());
    }

    #[test]
    fn a_version_1_ledger_upgrades_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute_batch(
                "CREATE TABLE rightkit_spend_meta (\"key\" TEXT PRIMARY KEY, \"value\" TEXT NOT NULL);
                 INSERT INTO rightkit_spend_meta (\"key\", \"value\") VALUES ('schema', '1');
                 INSERT INTO spend_allowances VALUES ('s', 'p', 1000, 0, 300, 4, 1);
                 INSERT INTO spend_reservations (request_id, scope, period, route_id, price_version,
                     estimated_micro, state, committed_micro, over_reservation, created_at,
                     in_flight_at, settled_at, reason)
                 VALUES ('old-ceiling', 's', 'p', 'r', 'v1', 200, 'committed', 200, 1, 10, 11, 99,
                     'lease_expired_in_flight'),
                        ('old-plain', 's', 'p', 'r', 'v1', 100, 'committed', 100, 0, 10, 11, 12, NULL);
                 INSERT INTO spend_overrun_acks (seq, acknowledged_at, note) VALUES (1, 100, NULL);",
            )
            .unwrap();
        }
        let mut ledger = Ledger::attach(Connection::open(&path).unwrap()).unwrap();
        let ceiling = ledger.reservation("res-00000001").unwrap().unwrap();
        assert!(ceiling.billed_unknown);
        assert_eq!((ceiling.account, ceiling.currency), (None, None));
        let plain = ledger.reservation("res-00000002").unwrap().unwrap();
        assert!(!plain.billed_unknown);
        let out = ledger
            .reserve_with(
                "s",
                "new",
                "p",
                "r",
                "v1",
                50,
                &ReserveOptions::with_account("acct").currency("USD"),
            )
            .unwrap();
        assert_eq!(out.reservation.account.as_deref(), Some("acct"));
    }

    #[test]
    fn v2_backfill_marks_only_ceiling_commits_not_reason_text() {
        // Schema v1 rows, then the exact v2 statements (the migration body) run over them.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute_batch(
            "INSERT INTO spend_allowances (scope, period, limit_micro, reserved_micro, \
                 committed_micro, revision, updated_at) VALUES ('s', 'p', 1000, 0, 200, 1, 0);
             INSERT INTO spend_reservations (request_id, scope, period, route_id, price_version,
                 estimated_micro, state, committed_micro, over_reservation, created_at, settled_at,
                 reason) VALUES
                 ('released-with-reason', 's', 'p', 'r', 'v1', 100, 'released', NULL, 0, 10, 11,
                     'lease_expired_in_flight'),
                 ('ceiling', 's', 'p', 'r', 'v1', 200, 'committed', 200, 1, 10, 11,
                     'lease_expired_in_flight'),
                 ('not-at-ceiling', 's', 'p', 'r', 'v1', 200, 'committed', 100, 0, 10, 11,
                     'lease_expired_in_flight');",
        )
        .unwrap();
        conn.execute_batch(SCHEMA_V2).unwrap();
        let mut stmt = conn
            .prepare("SELECT request_id, billed_unknown FROM spend_reservations ORDER BY seq")
            .unwrap();
        let flags: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(
            flags,
            vec![
                ("released-with-reason".to_owned(), 0),
                ("ceiling".to_owned(), 1),
                ("not-at-ceiling".to_owned(), 0),
            ]
        );
    }

    #[test]
    fn connection_variants_need_an_open_transaction_and_keep_savepoint_isolation() {
        let f = funded(1_000);
        let opts = ReserveOptions::default();
        let conn = f.ledger.connection();
        // Autocommit: refused, nothing written.
        assert!(matches!(
            f.ledger
                .reserve_on(conn, SCOPE, "r1", MONTH, "fal-ai/flux", "v1", 100, &opts),
            Err(SpendError::NoActiveTransaction)
        ));
        assert!(f.ledger.reservation("res-00000001").unwrap().is_none());

        // Inside the caller's transaction: works; a failed call leaves no partial write and
        // the transaction stays open and usable.
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let out = f
            .ledger
            .reserve_on(conn, SCOPE, "r1", MONTH, "fal-ai/flux", "v1", 100, &opts)
            .unwrap();
        assert_eq!(out.reservation.state, ReservationState::Reserved);
        assert!(matches!(
            f.ledger
                .reserve_on(conn, SCOPE, "r2", MONTH, "fal-ai/flux", "v1", 5_000, &opts),
            Err(SpendError::AllowanceExceeded { .. })
        ));
        assert!(!conn.is_autocommit());
        assert!(f.ledger.reservation("res-00000002").unwrap().is_none());
        f.ledger
            .commit_on(conn, &out.reservation.reservation_id, 90)
            .unwrap();
        conn.execute_batch("COMMIT").unwrap();

        let report = f.ledger.report(SCOPE, MONTH).unwrap();
        assert_eq!(report.count_committed, 1);
        assert_eq!(report.measured_micro, 90);
        assert!(report.reconciles);

        // A rolled-back transaction takes the ledger writes with it.
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        f.ledger
            .reserve_on(conn, SCOPE, "r3", MONTH, "fal-ai/flux", "v1", 50, &opts)
            .unwrap();
        conn.execute_batch("ROLLBACK").unwrap();
        assert!(f.ledger.reservation("res-00000003").unwrap().is_none());
        assert_eq!(
            f.ledger
                .allowance(SCOPE, MONTH)
                .unwrap()
                .unwrap()
                .reserved_micro,
            0
        );
    }

    #[test]
    fn reservation_and_report_fields_are_read_through_accessors() {
        let mut f = funded(1_000);
        let opts = ReserveOptions::with_account("acct").currency("USD");
        let r = reserve_with(&mut f, "req", 100, &opts).unwrap().reservation;
        assert_eq!(r.account(), Some("acct"));
        assert_eq!(r.currency(), Some("USD"));
        assert!(!r.billed_unknown());
        f.ledger.commit_unknown(&r.reservation_id, None).unwrap();
        let stored = f.ledger.reservation(&r.reservation_id).unwrap().unwrap();
        assert!(stored.billed_unknown());
        assert_eq!(f.ledger.report(SCOPE, MONTH).unwrap().billed_unknown(), 1);
    }
}
