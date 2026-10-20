//! Public-API single request replay semantics: identity is caller key, ceilings are admission policy.
use rightkit_spend::{rusqlite::Connection, Budget, BudgetKind, Ledger, Outcome, ReserveOptions, SpendError};

const SCOPE: &str = "replay/single";
const PERIOD: &str = "2026-10";
fn opts(key: Option<&str>, limit: i64) -> ReserveOptions {
    let options = ReserveOptions::with_account("account").currency("USD");
    match key { Some(key) => options.caller_budget(Budget::new(key, limit).unwrap()), None => options }
}
fn reserve(ledger: &mut Ledger, request: &str, options: &ReserveOptions) -> Result<Outcome, SpendError> {
    ledger.reserve_with(SCOPE, request, PERIOD, "route", "price-v1", 20, options)
}
fn funded(conn: Connection) -> Ledger {
    let mut ledger = Ledger::attach(conn).unwrap();
    ledger.set_allowance(SCOPE, PERIOD, 1_000, None).unwrap();
    ledger
}
// total_changes catches any row, tag, audit or allowance write, including writes later rolled back.
fn changes(ledger: &Ledger) -> u64 { ledger.connection().total_changes() }
fn counts(ledger: &Ledger) -> (i64, i64) {
    ledger.connection().query_row(
        "SELECT (SELECT count(*) FROM spend_reservations), (SELECT count(*) FROM spend_budget_tags)",
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap()
}

#[test]
fn same_normalized_caller_key_replays_current_settlement_at_zero_limit_without_writes() {
    for committed in [false, true] {
        let mut ledger = funded(Connection::open_in_memory().unwrap());
        ledger.set_process_budget(Some(Budget::new("old-process", 100).unwrap()));
        let original = reserve(&mut ledger, "same", &opts(Some(" caller-A "), 100)).unwrap();
        let settled = if committed {
            ledger.commit(&original.reservation.reservation_id, 13).unwrap()
        } else {
            ledger.release(&original.reservation.reservation_id, Some("cancelled")).unwrap()
        };
        let allowance = ledger.allowance(SCOPE, PERIOD).unwrap();
        let caller = ledger.budget_spend(BudgetKind::Caller, "caller-A").unwrap();
        let process = ledger.budget_spend(BudgetKind::Process, "old-process").unwrap();
        let row_counts = counts(&ledger);
        for process_budget in [Some(Budget::new("different-process", 0).unwrap()), None] {
            ledger.set_process_budget(process_budget);
            let before = changes(&ledger);
            let replay = reserve(&mut ledger, "same", &opts(Some("caller-A"), 0)).unwrap();
            assert!(replay.replayed);
            assert_eq!(replay.reservation, settled.reservation);
            assert_eq!(replay.reservation.reservation_id, original.reservation.reservation_id);
            assert_eq!(ledger.allowance(SCOPE, PERIOD).unwrap(), allowance);
            assert_eq!(ledger.budget_spend(BudgetKind::Caller, "caller-A").unwrap(), caller);
            assert_eq!(ledger.budget_spend(BudgetKind::Process, "old-process").unwrap(), process);
            assert_eq!(ledger.budget_spend(BudgetKind::Process, "different-process").unwrap().counted_micro(), 0);
            assert_eq!(counts(&ledger), row_counts);
            assert_eq!(changes(&ledger), before);
        }
        assert!(matches!(reserve(&mut ledger, "new", &opts(Some("caller-A"), 0)), Err(SpendError::BudgetExceeded { .. })));
        // Admission may stage SQL writes before rollback; only persisted state must be unchanged.
        assert_eq!(counts(&ledger), row_counts);
        assert_eq!(ledger.allowance(SCOPE, PERIOD).unwrap(), allowance);
        assert_eq!(ledger.budget_spend(BudgetKind::Caller, "caller-A").unwrap(), caller);
        assert_eq!(ledger.budget_spend(BudgetKind::Process, "old-process").unwrap(), process);
        assert_eq!(ledger.budget_spend(BudgetKind::Process, "different-process").unwrap().counted_micro(), 0);
    }
}

#[test]
fn changed_removed_or_added_caller_key_is_mismatch_before_zero_budget_admission() {
    for (initial, retry) in [(Some("A"), Some("B")), (Some("A"), None), (None, Some("B"))] {
        let mut ledger = funded(Connection::open_in_memory().unwrap());
        let original = reserve(&mut ledger, "identity", &opts(initial, 100)).unwrap();
        ledger.set_process_budget(Some(Budget::new("blocked-process", 0).unwrap()));
        let allowance = ledger.allowance(SCOPE, PERIOD).unwrap();
        let row_counts = counts(&ledger);
        let before = changes(&ledger);
        assert!(matches!(reserve(&mut ledger, "identity", &opts(retry, 0)), Err(SpendError::RequestMismatch { request_id }) if request_id == "identity"));
        assert_eq!(ledger.reservation(&original.reservation.reservation_id).unwrap().unwrap(), original.reservation);
        assert_eq!(ledger.allowance(SCOPE, PERIOD).unwrap(), allowance);
        assert_eq!(counts(&ledger), row_counts);
        assert_eq!(ledger.budget_spend(BudgetKind::Caller, "B").unwrap().counted_micro(), 0);
        assert_eq!(changes(&ledger), before);
    }
}

#[test]
fn schema_three_reopen_preserves_caller_identity_and_zero_limit_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spend.sqlite");
    let settled = {
        let mut ledger = funded(Connection::open(&path).unwrap());
        let first = reserve(&mut ledger, "durable", &opts(Some("A"), 100)).unwrap();
        ledger.commit(&first.reservation.reservation_id, 11).unwrap().reservation
    };
    let mut ledger = Ledger::attach(Connection::open(&path).unwrap()).unwrap();
    let schema: String = ledger.connection().query_row("SELECT value FROM rightkit_spend_meta WHERE key='schema'", [], |row| row.get(0)).unwrap();
    assert_eq!(schema, "3");
    let row_counts = counts(&ledger);
    let before = changes(&ledger);
    let replay = reserve(&mut ledger, "durable", &opts(Some("A"), 0)).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.reservation, settled);
    assert!(matches!(reserve(&mut ledger, "durable", &opts(Some("B"), 0)), Err(SpendError::RequestMismatch { .. })));
    assert_eq!(counts(&ledger), row_counts);
    assert_eq!(changes(&ledger), before);
}

#[test]
fn caller_transaction_reserve_on_observes_same_replay_and_mismatch_rules() {
    let mut ledger = funded(Connection::open_in_memory().unwrap());
    let first = reserve(&mut ledger, "transaction", &opts(Some("A"), 100)).unwrap();
    let settled = ledger.commit(&first.reservation.reservation_id, 12).unwrap().reservation;
    ledger.set_process_budget(Some(Budget::new("new-process", 0).unwrap()));
    let row_counts = counts(&ledger);
    let before = changes(&ledger);
    ledger.connection().execute_batch("BEGIN IMMEDIATE").unwrap();
    ledger.connection().execute_batch("CREATE TEMP TABLE caller_work (value INTEGER); INSERT INTO caller_work VALUES (7)").unwrap();
    let after_caller_write = changes(&ledger);
    let replay = ledger.reserve_on(ledger.connection(), SCOPE, "transaction", PERIOD, "route", "price-v1", 20, &opts(Some("A"), 0)).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.reservation, settled);
    assert!(matches!(ledger.reserve_on(ledger.connection(), SCOPE, "transaction", PERIOD, "route", "price-v1", 20, &opts(Some("B"), 0)), Err(SpendError::RequestMismatch { .. })));
    assert_eq!(changes(&ledger), after_caller_write);
    let caller_value: i64 = ledger.connection().query_row("SELECT value FROM caller_work", [], |row| row.get(0)).unwrap();
    assert_eq!(caller_value, 7);
    ledger.connection().execute_batch("COMMIT").unwrap();
    assert_eq!(counts(&ledger), row_counts);
    assert_eq!(changes(&ledger), before + 1);
}
