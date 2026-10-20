// rightkit-spend 0.2.0 consumed by exact registry pin, outside the workspace.
//
// Proves the families GenRight's ledger uses (genright-gen ledger.rs:61 imports, :385-393 and
// :911-913 reserve_on holds inside the caller's transaction, :1541 record_provider_reported,
// :1665 reconcile_invoice, :1842-1844 and :1920-1926 caller budget): an in-memory ledger with a
// fixed clock; an allowance; reserve_on with a caller budget inside BEGIN IMMEDIATE; commit_on;
// record_provider_reported; reconcile_invoice matching the committed line by reservation id; a
// reservation refused by the caller budget; the allowance report after the settle.
// Does not prove: reserve_multi, device allowances, the *_in variants, overrun acknowledgement,
// stale-lease expiry, budget_spend/check_budget, or any file-backed database.
// State: in-memory SQLite only. No network, no files, no real user data.
use rightkit_spend::rusqlite::Connection;
use rightkit_spend::{
    micro_from_units, Budget, InvoiceInput, InvoiceLine, Ledger, ReservationState, ReserveOptions,
    Scope, SpendError,
};

// Unix seconds. 1_792_454_400 is 2026-10-20T00:00:00Z; NOW is inside that day. The clock is
// fixed so created_at, and therefore the invoice period match, do not depend on the run time.
const NOW: i64 = 1_792_458_000;
const DAY_FROM: i64 = 1_792_454_400;
const DAY_TO: i64 = 1_792_540_799;
const PERIOD: &str = "2026-10-20";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let scope = Scope::new("genright", "spend", "local")?;
    let key = scope.key();
    let mut ledger = Ledger::attach_with_clock(Connection::open_in_memory()?, || NOW)?;
    ledger.set_allowance(&key, PERIOD, micro_from_units(10.0)?, None)?;

    // Hold path as the GenRight hook takes it: reserve_on on the ledger's own connection,
    // inside a transaction the caller opened, against a caller budget of 2.00 USD.
    let budget = Budget::new("demo-project", micro_from_units(2.0)?)?;
    let opts = ReserveOptions::default().currency("USD").caller_budget(budget);
    let hold = {
        let conn = ledger.connection();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = ledger.reserve_on(
            conn,
            &key,
            "job-1:1",
            PERIOD,
            "demo/model-x",
            "price-2026-10",
            micro_from_units(1.0)?,
            &opts,
        )?;
        conn.execute_batch("COMMIT")?;
        outcome
    };
    assert!(!hold.replayed, "first hold is not a replay");
    assert_eq!(hold.reservation.state, ReservationState::Reserved);
    assert_eq!(hold.reservation.estimate_micro, 1_000_000);
    let rid = hold.reservation.reservation_id.clone();

    // Settle below the estimate through commit_on: the difference returns to the allowance.
    {
        let conn = ledger.connection();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let settled = ledger.commit_on(conn, &rid, micro_from_units(0.75)?)?;
        conn.execute_batch("COMMIT")?;
        assert_eq!(settled.reservation.state, ReservationState::Committed);
        assert_eq!(settled.reservation.committed_micro, Some(750_000));
        assert!(!settled.reservation.over_reservation, "actual below estimate is no overrun");
    }

    // Provider figure and invoice reconciliation, as GenRight's ledger.rs:1541 and :1665 use them.
    let reported = ledger.record_provider_reported(&rid, 750_000, "fixture response header")?;
    assert_eq!(reported.reported_micro, 750_000);

    let mut invoice = InvoiceInput::new("demo", DAY_FROM, DAY_TO, 750_000);
    invoice.invoice_ref = Some("inv-demo-2026-10-20".to_owned());
    invoice.lines.push(InvoiceLine::by_reservation(rid.clone(), 750_000));
    let reconciled = ledger.reconcile_invoice(&invoice)?;
    assert!(!reconciled.already_recorded, "invoice is new");
    assert_eq!(reconciled.invoice.invoice_ref, "inv-demo-2026-10-20");
    assert_eq!(reconciled.matched.len(), 1, "the committed line matches its reservation");
    assert!(reconciled.unmatched_lines.is_empty(), "no unmatched invoice lines");

    // Caller budget refusal: 0.75 committed plus 1.50 requested is over the 2.00 budget.
    let refused = ledger.reserve_with(
        &key,
        "job-2:1",
        PERIOD,
        "demo/model-x",
        "price-2026-10",
        micro_from_units(1.5)?,
        &opts,
    );
    assert!(
        matches!(refused, Err(SpendError::BudgetExceeded { .. })),
        "caller budget must refuse the second hold"
    );

    // Allowance after the settle: 10.00 limit, nothing held, 0.75 committed, one committed row.
    let report = ledger.report(&key, PERIOD)?;
    assert_eq!(report.allowance.limit_micro, 10_000_000);
    assert_eq!(report.count_reserved, 0);
    assert_eq!(report.count_committed, 1);
    assert_eq!(report.unacknowledged_overruns, 0);
    assert_eq!(report.remaining_micro, 9_250_000);

    println!("OK rightkit-spend 0.2.0 (registry consumer)");
    Ok(())
}
