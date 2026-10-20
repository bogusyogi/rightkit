use rightkit_spend::rusqlite::Connection;
use rightkit_spend::{Budget, BudgetKind, Cap, GroupOutcome, Ledger, ReserveOptions, SpendError};

fn funded() -> Ledger {
    let mut ledger = Ledger::attach(Connection::open_in_memory().unwrap()).unwrap();
    ledger.set_allowance("account", "day", 1000, None).unwrap();
    ledger
        .set_allowance("account", "month", 2000, None)
        .unwrap();
    ledger
}
fn caps() -> Vec<Cap> {
    vec![
        Cap::allowance("account", "day"),
        Cap::allowance("account", "month"),
    ]
}
fn opts(key: &str, limit: i64) -> ReserveOptions {
    ReserveOptions::default().caller_budget(Budget::new(key, limit).unwrap())
}
fn reserve(
    l: &mut Ledger,
    caps: &[Cap],
    options: &ReserveOptions,
) -> rightkit_spend::Result<GroupOutcome> {
    l.reserve_multi("request", caps, "route", "price-v1", 100, options)
}
fn changes(l: &Ledger) -> i64 {
    l.connection()
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap()
}
fn counts(l: &Ledger) -> (i64, i64, i64, i64) {
    let c = l.connection();
    let count = |table: &str| {
        c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    (
        count("spend_groups"),
        count("spend_group_legs"),
        count("spend_reservations"),
        count("spend_budget_tags"),
    )
}
fn same_legs(left: &GroupOutcome, right: &GroupOutcome) {
    assert_eq!(left.request_id, right.request_id);
    assert_eq!(left.legs.len(), right.legs.len());
    for (a, b) in left.legs.iter().zip(&right.legs) {
        assert_eq!(a.reservation, b.reservation);
        assert_eq!(a.allowance, b.allowance);
        assert!(b.replayed);
    }
    assert!(right.replayed);
}

#[test]
fn settled_group_replays_same_normalized_caller_once_even_in_strict_mode() {
    let mut l = funded();
    let held = reserve(&mut l, &caps(), &opts(" job ", 500)).unwrap();
    assert_eq!(held.legs.len(), 2);
    assert_eq!(counts(&l), (1, 2, 2, 1));
    assert_eq!(
        l.budget_spend(BudgetKind::Caller, "job")
            .unwrap()
            .counted_micro(),
        100
    );
    l.mark_in_flight_group("request").unwrap();
    let settled = l.commit_group("request", 60).unwrap();
    l.set_strict(true);
    let before = changes(&l);
    let replay = reserve(&mut l, &caps(), &opts("job", 0)).unwrap();
    same_legs(&settled, &replay);
    assert_eq!(changes(&l), before);
    assert_eq!(counts(&l), (1, 2, 2, 1));
    let spend = l.budget_spend(BudgetKind::Caller, "job").unwrap();
    assert_eq!((spend.reserved_micro, spend.committed_micro), (0, 60));
    for period in ["day", "month"] {
        let a = l.allowance("account", period).unwrap().unwrap();
        assert_eq!((a.reserved_micro, a.committed_micro), (0, 60));
    }
}

#[test]
fn caller_identity_change_add_or_remove_mismatches_without_writes_before_zero_budget() {
    for original in [Some("job"), None] {
        let mut l = funded();
        let original_opts = original.map(|key| opts(key, 500)).unwrap_or_default();
        reserve(&mut l, &caps(), &original_opts).unwrap();
        let alternatives = if original.is_some() {
            vec![opts("alternate", 0), ReserveOptions::default()]
        } else {
            vec![opts("alternate", 0)]
        };
        for changed in alternatives {
            let before = changes(&l);
            let rows = counts(&l);
            assert!(matches!(
                reserve(&mut l, &caps(), &changed),
                Err(SpendError::RequestMismatch { .. })
            ));
            assert_eq!(changes(&l), before);
            assert_eq!(counts(&l), rows);
            assert_eq!(
                l.budget_spend(BudgetKind::Caller, "alternate")
                    .unwrap()
                    .counted_micro(),
                0
            );
        }
    }
}

#[test]
fn reordered_caps_use_original_first_tagged_leg_and_changed_fingerprint_refuses() {
    let mut l = funded();
    let original = reserve(&mut l, &caps(), &opts("job", 500)).unwrap();
    let first = original.legs[0].reservation.reservation_id.clone();
    let tag_seq: i64 = l
        .connection()
        .query_row(
            "SELECT seq FROM spend_budget_tags WHERE kind='caller' AND key='job'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(first, format!("res-{tag_seq:08}"));
    let mut reversed = caps();
    reversed.reverse();
    let before = changes(&l);
    same_legs(
        &original,
        &reserve(&mut l, &reversed, &opts("job", 0)).unwrap(),
    );
    assert_eq!(changes(&l), before);
    let changed = vec![
        Cap::allowance("account", "day").carry_over(true),
        Cap::allowance("account", "month"),
    ];
    assert!(matches!(
        reserve(&mut l, &changed, &opts("job", 0)),
        Err(SpendError::RequestMismatch { .. })
    ));
    assert_eq!(changes(&l), before);
    assert_eq!(counts(&l), (1, 2, 2, 1));
}

#[test]
fn changed_process_policy_is_not_request_identity() {
    let mut l = funded();
    l.set_process_budget(Some(Budget::new("process-a", 500).unwrap()));
    let original = reserve(&mut l, &caps(), &opts("job", 500)).unwrap();
    l.set_process_budget(Some(Budget::new("process-b", 0).unwrap()));
    let before = changes(&l);
    same_legs(
        &original,
        &reserve(&mut l, &caps(), &opts("job", 0)).unwrap(),
    );
    assert_eq!(changes(&l), before);
    assert_eq!(counts(&l), (1, 2, 2, 2));
    assert_eq!(
        l.budget_spend(BudgetKind::Process, "process-a")
            .unwrap()
            .counted_micro(),
        100
    );
    assert_eq!(
        l.budget_spend(BudgetKind::Process, "process-b")
            .unwrap()
            .counted_micro(),
        0
    );
}

#[test]
fn caller_transaction_replay_and_mismatch_leave_transaction_usable() {
    let mut l = funded();
    let original = reserve(&mut l, &caps(), &opts("job", 500)).unwrap();
    let tx = l.begin().unwrap();
    let before = changes(&l);
    let replay = l
        .reserve_multi_in(
            &tx,
            "request",
            &caps(),
            "route",
            "price-v1",
            100,
            &opts("job", 0),
        )
        .unwrap();
    same_legs(&original, &replay);
    assert!(matches!(
        l.reserve_multi_in(
            &tx,
            "request",
            &caps(),
            "route",
            "price-v1",
            100,
            &opts("other", 0)
        ),
        Err(SpendError::RequestMismatch { .. })
    ));
    assert_eq!(changes(&l), before);
    tx.execute_batch(
        "CREATE TABLE caller_work (value INTEGER); INSERT INTO caller_work VALUES (1)",
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(
        l.connection()
            .query_row("SELECT value FROM caller_work", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn missing_group_legs_keep_not_found_policy_without_writes() {
    let mut l = funded();
    reserve(&mut l, &caps(), &opts("job", 500)).unwrap();
    l.connection()
        .execute(
            "DELETE FROM spend_group_legs WHERE request_id='request'",
            [],
        )
        .unwrap();
    let before = changes(&l);
    assert!(matches!(
        reserve(&mut l, &caps(), &opts("job", 0)),
        Err(SpendError::NotFound(_))
    ));
    assert_eq!(changes(&l), before);
}

#[test]
fn caller_tag_storage_failure_remains_sqlite_error_without_writes() {
    let mut l = funded();
    reserve(&mut l, &caps(), &opts("job", 500)).unwrap();
    l.connection()
        .execute_batch("DROP TABLE spend_budget_tags")
        .unwrap();
    let before = changes(&l);
    assert!(matches!(
        reserve(&mut l, &caps(), &opts("job", 0)),
        Err(SpendError::Sqlite(_))
    ));
    assert_eq!(changes(&l), before);
}

#[test]
fn missing_original_leg_zero_is_not_an_untagged_replay() {
    let mut l = funded();
    reserve(&mut l, &caps(), &ReserveOptions::default()).unwrap();
    l.connection()
        .execute(
            "DELETE FROM spend_group_legs WHERE request_id='request' AND leg=0",
            [],
        )
        .unwrap();
    let before = changes(&l);
    assert!(matches!(
        reserve(&mut l, &caps(), &ReserveOptions::default()),
        Err(SpendError::NotFound(_))
    ));
    assert_eq!(changes(&l), before);
    assert_eq!(counts(&l), (1, 1, 2, 0));
}
