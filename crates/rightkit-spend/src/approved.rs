//! Approved budgets: a spend ceiling the owner approved, sealed so a later edit of the ledger
//! file cannot lift it, with an append-only audit and idempotent spend reports.
//!
//! An [`ApprovedBudget`] is one allowance (`scope`, `period`) plus the facts an owner approves:
//! an id, a currency and a limit. It is created unapproved and issues nothing. The app shows
//! the owner the budget and the value from [`ApprovedBudget::approval_bind`], asks for approval
//! in its own approvals flow bound to that value, and then calls [`Ledger::approve_budget`]
//! with the same value. A budget that changed in between is refused
//! ([`SpendError::ApprovalChanged`]) and nothing is approved.
//!
//! The approval is sealed: an HMAC-SHA256 ([`rightkit_crypto::hmac_sha256`]) over a
//! length-prefixed encoding of the budget id, scope, period, currency, limit, approval time and
//! approver, under a [`SealKey`] that never lives in the ledger database. Every reservation
//! through [`Ledger::reserve_approved`] re-verifies the seal in constant time first, and
//! refuses a budget whose seal fails ([`SpendError::SealInvalid`]) or whose allowance limit was
//! raised above the approved limit. The key is held by the app: from the OS credential store
//! with [`SealKey::from_keychain`] (feature `keychain`, [`rightkit_credentials`]), or any other
//! secure source through [`SealKey::from_bytes`]. A key that is lost orphans the seals (the
//! budgets then read as sealed-invalid and the owner re-approves); the ledger never stores,
//! logs or formats it.
//!
//! The approver is the principal the app names (a user or device id); it is part of the seal and
//! of the audit row, so a later edit of it breaks verification. The ledger cannot tell who the
//! owner is: the app proves the approval happened by presenting the bind value.
//!
//! The audit (`spend_approved_audit`) and the report log (`spend_approved_reports`) are
//! append-only: database triggers refuse any update or delete, and an approved budget row never
//! changes again. A spend report carries a caller-chosen report id and is recorded once: the
//! same id with the same budget, reservation and actual replays, any difference is
//! [`SpendError::ReportConflict`].

use rightkit_crypto::{ct_eq, hex_encode, hmac_sha256, sha256};
use zeroize::Zeroizing;

use super::*;

const SEAL_DOMAIN: &[u8] = b"rightkit.spend.approved-budget.seal.v1";
const BIND_DOMAIN: &[u8] = b"rightkit.spend.approved-budget.bind.v1";
const MIN_KEY_LEN: usize = 16;

/// The key that seals approved budgets. Zeroed on drop; its `Debug` shows nothing.
pub struct SealKey(Zeroizing<Vec<u8>>);

impl SealKey {
    /// A key from raw bytes (at least 16). The caller got them from a secure source.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < MIN_KEY_LEN {
            return Err(SpendError::SealKeyUnavailable(format!(
                "the seal key must be at least {MIN_KEY_LEN} bytes"
            )));
        }
        Ok(Self(Zeroizing::new(bytes.to_vec())))
    }

    /// The seal key held in the OS credential store under `service` (the app's frozen
    /// namespace) and `account`, created on first use with 32 random bytes. A store that is
    /// guarded or not durable is refused: a key held only in memory would orphan every seal.
    #[cfg(feature = "keychain")]
    pub fn from_keychain(service: &str, account: &str) -> Result<Self> {
        let unavailable = |why: rightkit_credentials::Error| {
            SpendError::SealKeyUnavailable(format!(
                "the seal key is not available in the OS credential store: {why}"
            ))
        };
        let namespace =
            rightkit_credentials::Namespace::new(service.to_owned()).map_err(unavailable)?;
        let store = rightkit_credentials::Store::open(rightkit_credentials::Config::new(namespace));
        if !store.is_durable() {
            return Err(SpendError::SealKeyUnavailable(
                "the credential store is guarded or not durable".into(),
            ));
        }
        let (secret, _created) = store.get_or_create_key(account, 32).map_err(unavailable)?;
        Self::from_bytes(secret.expose_bytes())
    }
}

impl std::fmt::Debug for SealKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SealKey(<redacted>)")
    }
}

/// An approved budget row. The money lives in the allowance of `(scope, period)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovedBudget {
    pub id: String,
    pub scope: String,
    pub period: String,
    /// Upper-case three-letter code.
    pub currency: String,
    pub limit_micro: i64,
    pub created_at: i64,
    /// Unix seconds; `None` until the owner's approval is recorded.
    pub approved_at: Option<i64>,
    /// The principal the app recorded as the approver.
    pub approved_by: Option<String>,
}

impl ApprovedBudget {
    pub fn is_approved(&self) -> bool {
        self.approved_at.is_some()
    }

    /// The value the owner's approval request is bound to; see [`approval_bind`].
    pub fn approval_bind(&self) -> String {
        approval_bind(
            &self.id,
            &self.scope,
            &self.period,
            &self.currency,
            self.limit_micro,
        )
    }
}

/// What the owner is shown and approves, as one value: the hex SHA-256 of the length-prefixed
/// id, scope, period, currency and limit. [`Ledger::approve_budget`] recomputes it from the
/// stored budget and refuses when it differs from the value the owner approved.
pub fn approval_bind(
    id: &str,
    scope: &str,
    period: &str,
    currency: &str,
    limit_micro: i64,
) -> String {
    let limit = limit_micro.to_string();
    hex_encode(&sha256(&encode(
        BIND_DOMAIN,
        &[
            id.as_bytes(),
            scope.as_bytes(),
            period.as_bytes(),
            currency.as_bytes(),
            limit.as_bytes(),
        ],
    )))
}

fn encode(domain: &[u8], fields: &[&[u8]]) -> Vec<u8> {
    let mut out = domain.to_vec();
    for field in fields {
        out.extend_from_slice(&(field.len() as u64).to_be_bytes());
        out.extend_from_slice(field);
    }
    out
}

fn seal_message(budget: &ApprovedBudget, approved_at: i64, approved_by: &str) -> Vec<u8> {
    let limit = budget.limit_micro.to_string();
    let at = approved_at.to_string();
    encode(
        SEAL_DOMAIN,
        &[
            budget.id.as_bytes(),
            budget.scope.as_bytes(),
            budget.period.as_bytes(),
            budget.currency.as_bytes(),
            limit.as_bytes(),
            at.as_bytes(),
            approved_by.as_bytes(),
        ],
    )
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// One row of the approved-budget audit trail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovedAudit {
    pub seq: i64,
    /// Unix seconds.
    pub at: i64,
    /// `create`, `approve` or `report`.
    pub action: String,
    pub budget_id: String,
    /// The approver for `approve`, otherwise the ledger's actor ([`Ledger::with_actor`]).
    pub actor: String,
    /// Plain `key=value` text naming the facts of the action (never the key or the seal).
    pub detail: String,
}

/// A recorded spend report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportOutcome {
    pub report_id: String,
    /// True when the report id was already recorded with the same facts; nothing was written.
    pub replayed: bool,
    /// The reservation after the report (committed at the reported actual).
    pub outcome: Outcome,
}

fn valid_id(what: &str, id: &str) -> Result<()> {
    if !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_graphic()) {
        Ok(())
    } else {
        Err(SpendError::BadInput(format!(
            "{what} is 1 to 128 visible characters without spaces"
        )))
    }
}

fn valid_principal(approver: &str) -> Result<()> {
    if approver.trim().is_empty()
        || approver.chars().count() > 128
        || approver.chars().any(char::is_control)
    {
        return Err(SpendError::BadInput(
            "approver is 1 to 128 characters with no control characters".into(),
        ));
    }
    Ok(())
}

const BUDGET_COLUMNS: &str =
    "id, scope, period, currency, limit_micro, created_at, approved_at, approved_by";

fn budget_from_row(row: &Row<'_>) -> rusqlite::Result<ApprovedBudget> {
    Ok(ApprovedBudget {
        id: row.get(0)?,
        scope: row.get(1)?,
        period: row.get(2)?,
        currency: row.get(3)?,
        limit_micro: row.get(4)?,
        created_at: row.get(5)?,
        approved_at: row.get(6)?,
        approved_by: row.get(7)?,
    })
}

fn find_budget(conn: &Connection, id: &str) -> Result<Option<ApprovedBudget>> {
    Ok(conn
        .query_row(
            &format!("SELECT {BUDGET_COLUMNS} FROM spend_approved_budgets WHERE id = ?1"),
            [id],
            budget_from_row,
        )
        .optional()?)
}

fn require_budget(conn: &Connection, id: &str) -> Result<ApprovedBudget> {
    find_budget(conn, id)?.ok_or_else(|| SpendError::BudgetNotFound {
        budget_id: id.to_owned(),
    })
}

fn audit(
    conn: &Connection,
    at: i64,
    action: &str,
    budget_id: &str,
    actor: &str,
    detail: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO spend_approved_audit (at, action, budget_id, actor, detail) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![at, action, budget_id, actor, detail],
    )?;
    Ok(())
}

/// True when the stored seal of an approved budget verifies under `key`.
fn seal_holds(conn: &Connection, key: &SealKey, budget: &ApprovedBudget) -> Result<bool> {
    let (Some(at), Some(by)) = (budget.approved_at, budget.approved_by.as_deref()) else {
        return Ok(false);
    };
    let stored: Option<String> = conn
        .query_row(
            "SELECT seal FROM spend_approved_budgets WHERE id = ?1",
            [&budget.id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let Some(tag) = stored.as_deref().and_then(unhex) else {
        return Ok(false);
    };
    Ok(ct_eq(
        &hmac_sha256(&key.0, &seal_message(budget, at, by)),
        &tag,
    ))
}

/// The budget when it is approved, its seal verifies and its allowance has not been raised
/// above the approved limit.
fn require_sealed(conn: &Connection, key: &SealKey, id: &str) -> Result<ApprovedBudget> {
    let budget = require_budget(conn, id)?;
    if !budget.is_approved() {
        return Err(SpendError::NotApproved {
            budget_id: id.to_owned(),
        });
    }
    if !seal_holds(conn, key, &budget)? {
        return Err(SpendError::SealInvalid {
            budget_id: id.to_owned(),
        });
    }
    let allowance = require_allowance(conn, &budget.scope, &budget.period)?;
    if allowance.limit_micro > budget.limit_micro {
        return Err(SpendError::SealInvalid {
            budget_id: id.to_owned(),
        });
    }
    Ok(budget)
}

fn create_impl(
    conn: &Connection,
    ctx: &Ctx,
    id: &str,
    scope: &str,
    period: &str,
    currency: &str,
    limit_micro: i64,
) -> Result<ApprovedBudget> {
    valid_id("budget id", id)?;
    if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Err(SpendError::BadInput(
            "a currency is a 3-letter code such as USD".into(),
        ));
    }
    let currency = currency.to_ascii_uppercase();
    if limit_micro <= 0 {
        return Err(SpendError::BadInput(
            "an approved budget limit is above zero".into(),
        ));
    }
    non_empty("scope", scope)?;
    non_empty("period", period)?;
    if let Some(existing) = find_budget(conn, id)? {
        if existing.scope == scope
            && existing.period == period
            && existing.currency == currency
            && existing.limit_micro == limit_micro
        {
            return Ok(existing);
        }
        return Err(SpendError::ApprovedBudgetConflict {
            budget_id: id.to_owned(),
        });
    }
    set_allowance_impl(conn, ctx, scope, period, limit_micro, None)?;
    conn.execute(
        "INSERT INTO spend_approved_budgets (id, scope, period, currency, limit_micro, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, scope, period, currency, limit_micro, ctx.now],
    )?;
    audit(
        conn,
        ctx.now,
        "create",
        id,
        &ctx.actor,
        &format!("scope={scope} period={period} currency={currency} limit_micro={limit_micro}"),
    )?;
    require_budget(conn, id)
}

fn approve_impl(
    conn: &Connection,
    ctx: &Ctx,
    key: &SealKey,
    id: &str,
    approver: &str,
    expected_bind: &str,
) -> Result<ApprovedBudget> {
    valid_principal(approver)?;
    let budget = require_budget(conn, id)?;
    if budget.is_approved() {
        return Ok(budget);
    }
    let allowance = require_allowance(conn, &budget.scope, &budget.period)?;
    if allowance.limit_micro != budget.limit_micro
        || !ct_eq(
            budget.approval_bind().as_bytes(),
            expected_bind.trim().as_bytes(),
        )
    {
        return Err(SpendError::ApprovalChanged {
            budget_id: id.to_owned(),
        });
    }
    let at = ctx.now;
    let seal = hex_encode(&hmac_sha256(&key.0, &seal_message(&budget, at, approver)));
    let changed = conn.execute(
        "UPDATE spend_approved_budgets SET approved_at = ?2, approved_by = ?3, seal = ?4 \
         WHERE id = ?1 AND approved_at IS NULL",
        params![id, at, approver, seal],
    )?;
    if changed == 1 {
        audit(
            conn,
            at,
            "approve",
            id,
            approver,
            &format!("limit_micro={}", budget.limit_micro),
        )?;
    }
    require_budget(conn, id)
}

fn report_impl(
    conn: &Connection,
    ctx: &Ctx,
    key: &SealKey,
    budget_id: &str,
    report_id: &str,
    reservation_id: &str,
    actual_micro: i64,
) -> Result<ReportOutcome> {
    valid_id("report id", report_id)?;
    if actual_micro < 0 {
        return Err(SpendError::BadInput("actual_micro is negative".into()));
    }
    let budget = require_sealed(conn, key, budget_id)?;
    let reservation = require_reservation(conn, reservation_id)?;
    if reservation.scope != budget.scope || reservation.period != budget.period {
        return Err(SpendError::BadInput(format!(
            "reservation {reservation_id} is not held against budget {budget_id}"
        )));
    }
    let prior: Option<(String, String, i64)> = conn
        .query_row(
            "SELECT budget_id, reservation_id, actual_micro FROM spend_approved_reports \
             WHERE report_id = ?1",
            [report_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((prior_budget, prior_reservation, prior_actual)) = prior {
        if prior_budget != budget_id
            || prior_reservation != reservation_id
            || prior_actual != actual_micro
        {
            return Err(SpendError::ReportConflict {
                report_id: report_id.to_owned(),
            });
        }
        return Ok(ReportOutcome {
            report_id: report_id.to_owned(),
            replayed: true,
            outcome: Outcome {
                reservation: require_reservation(conn, reservation_id)?,
                allowance: require_allowance(conn, &budget.scope, &budget.period)?,
                replayed: true,
            },
        });
    }
    let outcome = commit_impl(conn, ctx, reservation_id, actual_micro, None, None)?;
    conn.execute(
        "INSERT INTO spend_approved_reports (report_id, budget_id, reservation_id, actual_micro, reported_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![report_id, budget_id, reservation_id, actual_micro, ctx.now],
    )?;
    audit(
        conn,
        ctx.now,
        "report",
        budget_id,
        &ctx.actor,
        &format!(
            "report_id={report_id} reservation={reservation_id} actual_micro={actual_micro} over_reservation={}",
            outcome.reservation.over_reservation
        ),
    )?;
    Ok(ReportOutcome {
        report_id: report_id.to_owned(),
        replayed: false,
        outcome,
    })
}

impl Ledger {
    /// Create an unapproved budget: a fresh allowance for `(scope, period)` at `limit_micro`
    /// (above zero) and the budget row for `id`. An existing allowance for the pair is
    /// refused ([`SpendError::RevisionRequired`]): an approved budget owns its allowance.
    /// Repeating the call with the same facts returns the stored budget; other facts under the
    /// same id are [`SpendError::ApprovedBudgetConflict`]. Nothing can be reserved against it
    /// until [`Ledger::approve_budget`].
    pub fn create_approved_budget(
        &mut self,
        id: &str,
        scope: &str,
        period: &str,
        currency: &str,
        limit_micro: i64,
    ) -> Result<ApprovedBudget> {
        self.run(|conn, ctx| create_impl(conn, ctx, id, scope, period, currency, limit_micro))
    }

    /// [`Ledger::create_approved_budget`] inside the caller's transaction ([`Ledger::begin`]).
    pub fn create_approved_budget_in(
        &self,
        tx: &Transaction<'_>,
        id: &str,
        scope: &str,
        period: &str,
        currency: &str,
        limit_micro: i64,
    ) -> Result<ApprovedBudget> {
        self.run_in(tx, |conn, ctx| {
            create_impl(conn, ctx, id, scope, period, currency, limit_micro)
        })
    }

    /// Record the owner's approval and seal it under `key`. `expected_bind` is the value the
    /// owner approved ([`ApprovedBudget::approval_bind`]); a budget whose facts (or allowance
    /// limit) differ now is [`SpendError::ApprovalChanged`] and nothing is approved.
    /// `approver` is the principal the app records (it is sealed and audited). Approving an
    /// approved budget returns it and writes nothing.
    pub fn approve_budget(
        &mut self,
        key: &SealKey,
        id: &str,
        approver: &str,
        expected_bind: &str,
    ) -> Result<ApprovedBudget> {
        self.run(|conn, ctx| approve_impl(conn, ctx, key, id, approver, expected_bind))
    }

    /// [`Ledger::approve_budget`] inside the caller's transaction ([`Ledger::begin`]).
    pub fn approve_budget_in(
        &self,
        tx: &Transaction<'_>,
        key: &SealKey,
        id: &str,
        approver: &str,
        expected_bind: &str,
    ) -> Result<ApprovedBudget> {
        self.run_in(tx, |conn, ctx| {
            approve_impl(conn, ctx, key, id, approver, expected_bind)
        })
    }

    /// One approved budget by id, if any.
    pub fn approved_budget(&self, id: &str) -> Result<Option<ApprovedBudget>> {
        find_budget(&self.conn, id)
    }

    /// Every approved budget of `scope`, oldest first.
    pub fn approved_budgets(&self, scope: &str) -> Result<Vec<ApprovedBudget>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {BUDGET_COLUMNS} FROM spend_approved_budgets WHERE scope = ?1 \
             ORDER BY created_at, id"
        ))?;
        let rows = stmt.query_map([scope], budget_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Whether `id` is approved and its stored seal verifies under `key` (constant-time compare).
    /// False for an unapproved budget, a seal that does not verify, and a lost or changed key.
    pub fn verify_budget_seal(&self, key: &SealKey, id: &str) -> Result<bool> {
        let budget = require_budget(&self.conn, id)?;
        seal_holds(&self.conn, key, &budget)
    }

    /// [`Ledger::reserve_with`] against an approved budget: re-verifies the seal first and
    /// reserves on the budget's `(scope, period)` in the budget's currency (a different
    /// `options.currency` is `BadInput`). An unapproved budget is [`SpendError::NotApproved`],
    /// a failed seal, or an allowance limit above the approved limit, is
    /// [`SpendError::SealInvalid`]; nothing is reserved either way.
    #[allow(clippy::too_many_arguments)]
    pub fn reserve_approved(
        &mut self,
        key: &SealKey,
        budget_id: &str,
        request_id: &str,
        route_id: &str,
        price_version: &str,
        estimate_micro: i64,
        options: &ReserveOptions,
    ) -> Result<Outcome> {
        self.run(|conn, ctx| {
            let budget = require_sealed(conn, key, budget_id)?;
            let mut options = options.clone();
            if let Some(c) = &options.currency {
                if !c.eq_ignore_ascii_case(&budget.currency) {
                    return Err(SpendError::BadInput(format!(
                        "budget {budget_id} is in {}, not {c}",
                        budget.currency
                    )));
                }
            }
            options.currency = Some(budget.currency.clone());
            reserve_impl(
                conn,
                ctx,
                &budget.scope,
                request_id,
                &budget.period,
                route_id,
                price_version,
                estimate_micro,
                &options,
            )
        })
    }

    /// Record the actual spend of `reservation_id` against an approved budget, once per
    /// `report_id`: the reservation is committed at `actual_micro` (an actual over the estimate
    /// is recorded and flagged, never refused: the money has gone) and the report is logged.
    /// The same id with the same budget, reservation and actual replays and writes nothing; any
    /// difference is [`SpendError::ReportConflict`]. The seal is verified first.
    pub fn report_approved_spend(
        &mut self,
        key: &SealKey,
        budget_id: &str,
        report_id: &str,
        reservation_id: &str,
        actual_micro: i64,
    ) -> Result<ReportOutcome> {
        self.run(|conn, ctx| {
            report_impl(
                conn,
                ctx,
                key,
                budget_id,
                report_id,
                reservation_id,
                actual_micro,
            )
        })
    }

    /// The newest `limit` approved-budget audit rows, newest first, optionally only those of
    /// one budget.
    pub fn approved_audit(
        &self,
        budget_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ApprovedAudit>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, at, action, budget_id, actor, detail FROM spend_approved_audit \
             WHERE (?1 IS NULL OR budget_id = ?1) ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![budget_id, limit], |row| {
            Ok(ApprovedAudit {
                seq: row.get(0)?,
                at: row.get(1)?,
                action: row.get(2)?,
                budget_id: row.get(3)?,
                actor: row.get(4)?,
                detail: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}
