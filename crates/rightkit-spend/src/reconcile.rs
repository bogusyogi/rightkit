//! Provider-reported cost, invoice reconciliation and actual-cost adjustment.
//!
//! Four amounts are kept apart for a reservation: the estimate (reserved), the committed actual
//! (what the caps count once settled), the provider-reported cost (what the provider's response
//! stated) and the invoice amount (from the provider's invoice line). The last two are stored
//! beside the ledger and move no cap. Only [`Ledger::adjust_actual`], or
//! [`Ledger::reconcile_invoice`] with `adjust_committed`, changes a committed actual, and then
//! the allowance's committed total moves with it in the same compare-and-swap, with an
//! adjustment row as the trail.

use super::*;

/// What a provider's response stated for one reservation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderReported {
    pub reservation_id: String,
    pub reported_micro: i64,
    /// Where the figure came from (a response header, an API field, ...).
    pub source: String,
    pub reported_at: i64,
}

/// One line of a provider invoice, matched to a reservation by id or by request id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvoiceLine {
    pub reservation_id: Option<String>,
    pub request_id: Option<String>,
    pub invoice_micro: i64,
}

impl InvoiceLine {
    /// A line for the reservation `res-NNNNNNNN`.
    pub fn by_reservation(reservation_id: impl Into<String>, invoice_micro: i64) -> Self {
        Self {
            reservation_id: Some(reservation_id.into()),
            request_id: None,
            invoice_micro,
        }
    }

    /// A line for the reservation made with this request id.
    pub fn by_request(request_id: impl Into<String>, invoice_micro: i64) -> Self {
        Self {
            reservation_id: None,
            request_id: Some(request_id.into()),
            invoice_micro,
        }
    }
}

/// An invoice to record against the ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvoiceInput {
    /// Matched against the route id of a reservation: the route is the provider, or starts with
    /// `<provider>/`.
    pub provider: String,
    /// Inclusive unix-seconds range over reservation `created_at`.
    pub period_from: i64,
    pub period_to: i64,
    pub total_micro: i64,
    /// The idempotency key. Without one it is generated from provider and period.
    pub invoice_ref: Option<String>,
    pub lines: Vec<InvoiceLine>,
    /// Also set each matched committed reservation's actual to its invoice line, through
    /// [`Ledger::adjust_actual`]. Off by default: the invoice is then recorded beside the ledger
    /// and moves no cap.
    pub adjust_committed: bool,
}

impl InvoiceInput {
    /// An invoice total for a provider and period, with no lines and no adjustment.
    pub fn new(
        provider: impl Into<String>,
        period_from: i64,
        period_to: i64,
        total_micro: i64,
    ) -> Self {
        Self {
            provider: provider.into(),
            period_from,
            period_to,
            total_micro,
            invoice_ref: None,
            lines: Vec::new(),
            adjust_committed: false,
        }
    }
}

/// A recorded invoice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invoice {
    pub invoice_ref: String,
    pub ref_generated: bool,
    pub provider: String,
    pub period_from: i64,
    pub period_to: i64,
    pub total_micro: i64,
    pub recorded_at: i64,
}

/// An invoice line that matched a reservation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchedLine {
    pub reservation_id: String,
    pub invoice_micro: i64,
    /// The committed actual before this call (0 if the reservation is not committed).
    pub settled_micro: i64,
    /// `invoice - settled`.
    pub difference_micro: i64,
    /// The committed actual was changed to the invoice amount.
    pub adjusted: bool,
}

/// An invoice line that matched nothing, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnmatchedLine {
    /// Position in `InvoiceInput::lines`.
    pub index: usize,
    pub reason: String,
}

/// What [`Ledger::reconcile_invoice`] recorded and found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvoiceReport {
    pub invoice: Invoice,
    /// The invoice reference was already stored with the same provider, period and total.
    pub already_recorded: bool,
    /// Committed actuals of the provider's reservations in the period, after any adjustment.
    pub settled_micro: i64,
    /// `invoice total - settled`.
    pub total_minus_settled_micro: i64,
    pub lines_total_micro: i64,
    /// `lines total - invoice total`, when there are lines.
    pub lines_minus_total_micro: Option<i64>,
    pub matched: Vec<MatchedLine>,
    pub unmatched_lines: Vec<UnmatchedLine>,
    /// Reservations of the provider and period with spend and no invoice line, reported when
    /// lines were given. Never guessed into a line.
    pub unmatched_reservations: Vec<String>,
}

fn provider_matches(route_id: &str, provider: &str) -> bool {
    route_id == provider
        || route_id
            .strip_prefix(provider)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn record_provider_reported_impl(
    conn: &Connection,
    ctx: &Ctx,
    reservation_id: &str,
    reported_micro: i64,
    source: &str,
) -> Result<ProviderReported> {
    if reported_micro < 0 {
        return Err(SpendError::BadInput("reported_micro is negative".into()));
    }
    let source = source.trim();
    if source.is_empty() {
        return Err(SpendError::BadInput(
            "provider-reported source must name where the figure came from".into(),
        ));
    }
    require_reservation(conn, reservation_id)?;
    conn.execute(
        "INSERT INTO spend_provider_reported (seq, reported_micro, source, reported_at) \
         VALUES (?1, ?2, ?3, ?4) ON CONFLICT (seq) DO UPDATE SET \
         reported_micro = excluded.reported_micro, source = excluded.source, \
         reported_at = excluded.reported_at",
        params![seq_of(reservation_id)?, reported_micro, source, ctx.now],
    )?;
    Ok(ProviderReported {
        reservation_id: reservation_id.to_owned(),
        reported_micro,
        source: source.to_owned(),
        reported_at: ctx.now,
    })
}

fn provider_reported_of(
    conn: &Connection,
    reservation_id: &str,
) -> Result<Option<ProviderReported>> {
    let Ok(seq) = seq_of(reservation_id) else {
        return Ok(None);
    };
    Ok(conn
        .query_row(
            "SELECT reported_micro, source, reported_at FROM spend_provider_reported \
             WHERE seq = ?1",
            [seq],
            |row| {
                Ok(ProviderReported {
                    reservation_id: reservation_id.to_owned(),
                    reported_micro: row.get(0)?,
                    source: row.get(1)?,
                    reported_at: row.get(2)?,
                })
            },
        )
        .optional()?)
}

fn adjust_actual_impl(
    conn: &Connection,
    ctx: &Ctx,
    reservation_id: &str,
    actual_micro: i64,
    source: &str,
    note: Option<&str>,
) -> Result<Outcome> {
    if actual_micro < 0 {
        return Err(SpendError::BadInput("actual_micro is negative".into()));
    }
    non_empty("source", source)?;
    let res = require_reservation(conn, reservation_id)?;
    if res.state != ReservationState::Committed {
        return Err(SpendError::InvalidTransition {
            from: res.state,
            to: ReservationState::Committed,
        });
    }
    let old = res.committed_micro.unwrap_or(0);
    if old == actual_micro && !res.billed_unknown {
        return outcome(conn, res, true);
    }
    let allowance = require_allowance(conn, &res.scope, &res.period)?;
    let committed = allowance
        .committed_micro
        .checked_sub(old)
        .filter(|v| *v >= 0)
        .ok_or(SpendError::Overflow)?;
    let committed = add(committed, actual_micro)?;
    cas_totals(
        conn,
        &allowance,
        allowance.reserved_micro,
        committed,
        ctx.now,
    )?;
    // The real cost is now known: the overrun flag follows the new actual, and an unknown
    // ceiling commit is no longer unknown.
    let over = actual_micro > res.estimate_micro;
    let seq = seq_of(reservation_id)?;
    conn.execute(
        "UPDATE spend_reservations SET committed_micro = ?2, over_reservation = ?3, \
         billed_unknown = 0, reason = ?4 WHERE seq = ?1 AND state = 'committed'",
        params![
            seq,
            actual_micro,
            i64::from(over),
            format!("adjusted: {source}")
        ],
    )?;
    if over && old != actual_micro {
        // A larger overrun is a new fact: a person looks at it again.
        conn.execute("DELETE FROM spend_overrun_acks WHERE seq = ?1", [seq])?;
    }
    conn.execute(
        "INSERT INTO spend_adjustments (seq, at, old_micro, new_micro, source, note) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![seq, ctx.now, old, actual_micro, source, note],
    )?;
    let res = require_reservation(conn, reservation_id)?;
    outcome(conn, res, false)
}

fn invoice_of(conn: &Connection, invoice_ref: &str) -> Result<Option<Invoice>> {
    Ok(conn
        .query_row(
            "SELECT invoice_ref, ref_generated, provider, period_from, period_to, total_micro, \
             recorded_at FROM spend_invoices WHERE invoice_ref = ?1",
            [invoice_ref],
            |row| {
                let generated: i64 = row.get(1)?;
                Ok(Invoice {
                    invoice_ref: row.get(0)?,
                    ref_generated: generated != 0,
                    provider: row.get(2)?,
                    period_from: row.get(3)?,
                    period_to: row.get(4)?,
                    total_micro: row.get(5)?,
                    recorded_at: row.get(6)?,
                })
            },
        )
        .optional()?)
}

/// Committed actuals of the provider's reservations created in `[from, to]`.
fn settled_in(conn: &Connection, provider: &str, from: i64, to: i64) -> Result<i64> {
    let sql = format!(
        "SELECT {RESERVATION_COLUMNS} FROM spend_reservations \
         WHERE created_at >= ?1 AND created_at <= ?2 ORDER BY seq"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![from, to], reservation_from_row)?;
    let mut total: i64 = 0;
    for res in rows {
        let res = res?;
        if res.state == ReservationState::Committed && provider_matches(&res.route_id, provider) {
            total = add(total, res.committed_micro.unwrap_or(0))?;
        }
    }
    Ok(total)
}

fn reconcile_invoice_impl(
    conn: &Connection,
    ctx: &Ctx,
    input: &InvoiceInput,
) -> Result<InvoiceReport> {
    let provider = input.provider.trim();
    non_empty("provider", provider)?;
    if input.period_from > input.period_to {
        return Err(SpendError::BadInput(
            "period_from is after period_to".into(),
        ));
    }
    if input.total_micro < 0 {
        return Err(SpendError::BadInput("total_micro is negative".into()));
    }
    let mut lines_total: i64 = 0;
    for (i, line) in input.lines.iter().enumerate() {
        if line.reservation_id.is_some() == line.request_id.is_some() {
            return Err(SpendError::BadInput(format!(
                "lines[{i}] needs exactly one of reservation_id or request_id"
            )));
        }
        if line.invoice_micro < 0 {
            return Err(SpendError::BadInput(format!(
                "lines[{i}] amount is negative"
            )));
        }
        lines_total = add(lines_total, line.invoice_micro)?;
    }
    let (invoice_ref, generated) = match &input.invoice_ref {
        None => (
            format!("auto:{provider}:{}:{}", input.period_from, input.period_to),
            true,
        ),
        Some(r) => {
            non_empty("invoice_ref", r)?;
            (r.trim().to_owned(), false)
        }
    };

    let already_recorded = match invoice_of(conn, &invoice_ref)? {
        Some(stored) => {
            if stored.provider != provider
                || stored.period_from != input.period_from
                || stored.period_to != input.period_to
                || stored.total_micro != input.total_micro
            {
                return Err(SpendError::InvoiceConflict { invoice_ref });
            }
            true
        }
        None => {
            conn.execute(
                "INSERT INTO spend_invoices (invoice_ref, ref_generated, provider, period_from, \
                 period_to, total_micro, recorded_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    invoice_ref,
                    i64::from(generated),
                    provider,
                    input.period_from,
                    input.period_to,
                    input.total_micro,
                    ctx.now
                ],
            )?;
            false
        }
    };

    let mut matched = Vec::new();
    let mut unmatched_lines = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (index, line) in input.lines.iter().enumerate() {
        let mut unmatched = |reason: &str| {
            unmatched_lines.push(UnmatchedLine {
                index,
                reason: reason.to_owned(),
            })
        };
        let found = match (&line.reservation_id, &line.request_id) {
            (Some(id), _) => find_reservation(conn, id)?,
            (_, Some(request)) => find_by_request(conn, request)?,
            _ => None,
        };
        let Some(res) = found.filter(|r| {
            provider_matches(&r.route_id, provider)
                && r.created_at >= input.period_from
                && r.created_at <= input.period_to
        }) else {
            unmatched("no_reservation_for_provider_and_period");
            continue;
        };
        if seen.contains(&res.reservation_id) {
            unmatched("duplicate_line_for_reservation");
            continue;
        }
        let seq = seq_of(&res.reservation_id)?;
        let prior: Option<(String, i64)> = conn
            .query_row(
                "SELECT invoice_ref, invoice_micro FROM spend_invoice_lines WHERE seq = ?1",
                [seq],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((prior_ref, prior_micro)) = prior {
            if prior_ref != invoice_ref || prior_micro != line.invoice_micro {
                unmatched("reservation_already_reconciled_by_another_invoice_or_amount");
                continue;
            }
        }
        conn.execute(
            "INSERT INTO spend_invoice_lines (seq, invoice_ref, invoice_micro, recorded_at) \
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT (seq) DO UPDATE SET \
             invoice_ref = excluded.invoice_ref, invoice_micro = excluded.invoice_micro, \
             recorded_at = excluded.recorded_at",
            params![seq, invoice_ref, line.invoice_micro, ctx.now],
        )?;
        seen.push(res.reservation_id.clone());
        let settled = res.committed_micro.unwrap_or(0);
        let mut adjusted = false;
        if input.adjust_committed
            && res.state == ReservationState::Committed
            && (res.committed_micro != Some(line.invoice_micro) || res.billed_unknown)
        {
            adjust_actual_impl(
                conn,
                ctx,
                &res.reservation_id,
                line.invoice_micro,
                &format!("invoice:{invoice_ref}"),
                None,
            )?;
            adjusted = true;
        }
        matched.push(MatchedLine {
            reservation_id: res.reservation_id,
            invoice_micro: line.invoice_micro,
            settled_micro: settled,
            difference_micro: line
                .invoice_micro
                .checked_sub(settled)
                .ok_or(SpendError::Overflow)?,
            adjusted,
        });
    }

    let mut unmatched_reservations = Vec::new();
    if !input.lines.is_empty() {
        let sql = format!(
            "SELECT {RESERVATION_COLUMNS} FROM spend_reservations \
             WHERE created_at >= ?1 AND created_at <= ?2 \
             AND seq NOT IN (SELECT seq FROM spend_invoice_lines) ORDER BY seq"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![input.period_from, input.period_to],
            reservation_from_row,
        )?;
        for res in rows {
            let res = res?;
            let spend = res.committed_micro.unwrap_or(res.estimate_micro);
            let counts = matches!(
                res.state,
                ReservationState::Reserved
                    | ReservationState::InFlight
                    | ReservationState::Committed
            );
            if counts && spend > 0 && provider_matches(&res.route_id, provider) {
                unmatched_reservations.push(res.reservation_id);
            }
        }
    }

    let settled = settled_in(conn, provider, input.period_from, input.period_to)?;
    let invoice =
        invoice_of(conn, &invoice_ref)?.ok_or_else(|| SpendError::NotFound(invoice_ref.clone()))?;
    Ok(InvoiceReport {
        invoice,
        already_recorded,
        settled_micro: settled,
        total_minus_settled_micro: input
            .total_micro
            .checked_sub(settled)
            .ok_or(SpendError::Overflow)?,
        lines_total_micro: lines_total,
        lines_minus_total_micro: if input.lines.is_empty() {
            None
        } else {
            Some(
                lines_total
                    .checked_sub(input.total_micro)
                    .ok_or(SpendError::Overflow)?,
            )
        },
        matched,
        unmatched_lines,
        unmatched_reservations,
    })
}

impl Ledger {
    /// Record what the provider's response stated for a reservation. It is stored beside the
    /// ledger: the estimate, the committed actual, the caps and every total are unchanged. A
    /// second call replaces the figure. To make it the actual, call [`Ledger::adjust_actual`].
    pub fn record_provider_reported(
        &mut self,
        reservation_id: &str,
        reported_micro: i64,
        source: &str,
    ) -> Result<ProviderReported> {
        self.run(|conn, ctx| {
            record_provider_reported_impl(conn, ctx, reservation_id, reported_micro, source)
        })
    }

    /// [`Ledger::record_provider_reported`] inside the caller's transaction.
    pub fn record_provider_reported_in(
        &self,
        tx: &Transaction<'_>,
        reservation_id: &str,
        reported_micro: i64,
        source: &str,
    ) -> Result<ProviderReported> {
        self.run_in(tx, |conn, ctx| {
            record_provider_reported_impl(conn, ctx, reservation_id, reported_micro, source)
        })
    }

    /// [`Ledger::record_provider_reported`] on a connection with an open transaction.
    pub fn record_provider_reported_on(
        &self,
        conn: &Connection,
        reservation_id: &str,
        reported_micro: i64,
        source: &str,
    ) -> Result<ProviderReported> {
        self.run_on(conn, |conn, ctx| {
            record_provider_reported_impl(conn, ctx, reservation_id, reported_micro, source)
        })
    }

    /// The provider-reported figure of a reservation, if one was recorded.
    pub fn provider_reported(&self, reservation_id: &str) -> Result<Option<ProviderReported>> {
        provider_reported_of(&self.conn, reservation_id)
    }

    /// Correct the actual of a `committed` reservation (for example to the provider-reported
    /// cost or an invoice line). The allowance's committed total moves by the difference in the
    /// same compare-and-swap, even past the limit (the money is already spent; `remaining` goes
    /// negative). The overrun flag follows the new actual, a `billed_unknown` ceiling commit
    /// stops being unknown, and an adjustment row records old and new. A reservation that is not
    /// committed is [`SpendError::InvalidTransition`]; the same actual again is a replay.
    pub fn adjust_actual(
        &mut self,
        reservation_id: &str,
        actual_micro: i64,
        source: &str,
        note: Option<&str>,
    ) -> Result<Outcome> {
        self.run(|conn, ctx| {
            adjust_actual_impl(conn, ctx, reservation_id, actual_micro, source, note)
        })
    }

    /// [`Ledger::adjust_actual`] inside the caller's transaction.
    pub fn adjust_actual_in(
        &self,
        tx: &Transaction<'_>,
        reservation_id: &str,
        actual_micro: i64,
        source: &str,
        note: Option<&str>,
    ) -> Result<Outcome> {
        self.run_in(tx, |conn, ctx| {
            adjust_actual_impl(conn, ctx, reservation_id, actual_micro, source, note)
        })
    }

    /// [`Ledger::adjust_actual`] on a connection with an open transaction.
    pub fn adjust_actual_on(
        &self,
        conn: &Connection,
        reservation_id: &str,
        actual_micro: i64,
        source: &str,
        note: Option<&str>,
    ) -> Result<Outcome> {
        self.run_on(conn, |conn, ctx| {
            adjust_actual_impl(conn, ctx, reservation_id, actual_micro, source, note)
        })
    }

    /// Record a provider invoice against the ledger.
    ///
    /// The invoice reference is the idempotency key: the same reference with the same provider,
    /// period and total is recorded once (`already_recorded`; lines apply idempotently), and any
    /// difference is [`SpendError::InvoiceConflict`]. Lines match a reservation of that provider
    /// created in the period by id or request id; unmatched lines and reservations are reported,
    /// never guessed, and a reservation already reconciled by another invoice or amount is not
    /// overwritten. Unless `adjust_committed` is set nothing a cap counts changes.
    pub fn reconcile_invoice(&mut self, input: &InvoiceInput) -> Result<InvoiceReport> {
        self.run(|conn, ctx| reconcile_invoice_impl(conn, ctx, input))
    }

    /// [`Ledger::reconcile_invoice`] inside the caller's transaction.
    pub fn reconcile_invoice_in(
        &self,
        tx: &Transaction<'_>,
        input: &InvoiceInput,
    ) -> Result<InvoiceReport> {
        self.run_in(tx, |conn, ctx| reconcile_invoice_impl(conn, ctx, input))
    }

    /// [`Ledger::reconcile_invoice`] on a connection with an open transaction.
    pub fn reconcile_invoice_on(
        &self,
        conn: &Connection,
        input: &InvoiceInput,
    ) -> Result<InvoiceReport> {
        self.run_on(conn, |conn, ctx| reconcile_invoice_impl(conn, ctx, input))
    }

    /// A recorded invoice by reference.
    pub fn invoice(&self, invoice_ref: &str) -> Result<Option<Invoice>> {
        invoice_of(&self.conn, invoice_ref)
    }
}
