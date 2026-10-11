mod common;

use std::cell::{Cell, RefCell};

use common::{pubkey, Claims, APP, DEVICE};

use rightkit_license::{
    evaluate_lifecycle, reconcile_clock, AntiRollbackClock, ClockDecision, ClockDecodeError,
    ClockError, ClockReading, ClockRefusal, ClockState, ClockStore, Lifecycle, LifecycleContext,
    MemoryClockStore, ProcessAnchor, StoredCredential, SystemClock, TrustedClock,
    CLOCK_ROLLBACK_TOLERANCE_SECS, CLOCK_WRITE_INTERVAL_SECS,
};

fn state(highest_seen: i64, trusted_process: &str, last_server_iat: i64) -> ClockState {
    ClockState {
        highest_seen,
        trusted_process: trusted_process.to_string(),
        last_server_iat,
    }
}

// ------------------------------------------------------------------ pure reconcile

#[test]
fn first_observation_trusts_the_larger_of_wall_and_monotonic() {
    assert_eq!(
        reconcile_clock(10_000, 10_050, "p", None),
        ClockDecision::Trusted {
            effective_now: 10_050,
            next: state(10_050, "p", 0),
        }
    );
}

#[test]
fn clock_moved_forward_advances_the_mark() {
    assert_eq!(
        reconcile_clock(20_000, 10_001, "p", Some(&state(10_000, "p", 9_000))),
        ClockDecision::Trusted {
            effective_now: 20_000,
            next: state(20_000, "p", 9_000),
        }
    );
    // Also across a restart, as long as wall time is strictly past the mark.
    assert_eq!(
        reconcile_clock(10_001, 10_001, "q", Some(&state(10_000, "p", 9_000))),
        ClockDecision::Trusted {
            effective_now: 10_001,
            next: state(10_001, "q", 9_000),
        }
    );
}

#[test]
fn clock_moved_back_beyond_tolerance_is_a_rollback_even_in_the_same_process() {
    assert_eq!(
        reconcile_clock(
            10_000 - CLOCK_ROLLBACK_TOLERANCE_SECS - 1,
            10_001,
            "p",
            Some(&state(10_000, "p", 9_000)),
        ),
        ClockDecision::Rollback {
            highest_seen: 10_000
        }
    );
}

#[test]
fn clock_moved_back_exactly_the_tolerance_is_not_yet_a_rollback_in_process() {
    let decision = reconcile_clock(
        10_000 - CLOCK_ROLLBACK_TOLERANCE_SECS,
        10_010,
        "p",
        Some(&state(10_000, "p", 9_000)),
    );
    assert_eq!(
        decision,
        ClockDecision::Trusted {
            effective_now: 10_010,
            next: state(10_010, "p", 9_000),
        }
    );
}

#[test]
fn repeated_small_rollback_keeps_advancing_on_monotonic_time() {
    let first = reconcile_clock(9_800, 10_010, "p", Some(&state(10_000, "p", 9_000)));
    assert_eq!(
        first,
        ClockDecision::Trusted {
            effective_now: 10_010,
            next: state(10_010, "p", 9_000),
        }
    );
    let second = reconcile_clock(9_800, 10_070, "p", Some(&state(10_010, "p", 9_000)));
    assert_eq!(
        second,
        ClockDecision::Trusted {
            effective_now: 10_070,
            next: state(10_070, "p", 9_000),
        }
    );
}

#[test]
fn small_rollback_episode_fails_closed_after_restart() {
    assert_eq!(
        reconcile_clock(9_900, 10_001, "q", Some(&state(10_000, "p", 9_000))),
        ClockDecision::Rollback {
            highest_seen: 10_000
        }
    );
}

#[test]
fn equal_time_restart_cannot_freeze_expiry() {
    assert_eq!(
        reconcile_clock(10_000, 10_000, "q", Some(&state(10_000, "p", 9_000))),
        ClockDecision::Rollback {
            highest_seen: 10_000
        }
    );
}

#[test]
fn corrected_wall_clock_clears_the_rollback_episode() {
    assert_eq!(
        reconcile_clock(10_020, 10_020, "q", Some(&state(10_010, "p", 9_000))),
        ClockDecision::Trusted {
            effective_now: 10_020,
            next: state(10_020, "q", 9_000),
        }
    );
}

#[test]
fn wall_clock_lagging_the_monotonic_offset_is_not_trusted_across_restart() {
    assert_eq!(
        reconcile_clock(10_100, 10_110, "q", Some(&state(10_000, "p", 9_000))),
        ClockDecision::Rollback {
            highest_seen: 10_000
        }
    );
}

#[test]
fn the_mark_never_moves_backwards_in_a_reconcile() {
    for wall in [9_700, 9_800, 9_999, 10_000, 10_500] {
        if let ClockDecision::Trusted { next, .. } =
            reconcile_clock(wall, 9_000, "p", Some(&state(10_000, "p", 1)))
        {
            assert!(next.highest_seen >= 10_000, "wall {wall}");
        }
    }
}

// ------------------------------------------------------------------ stored form

#[test]
fn stored_form_round_trips_and_matches_the_app_layout() {
    let original = state(12_345, "1234-99-abcd", 12_000);
    let raw = original.encode();
    assert_eq!(
        raw,
        r#"{"v":1,"highest_seen":12345,"trusted_process":"1234-99-abcd","last_server_iat":12000}"#
    );
    assert_eq!(ClockState::decode(&raw), Ok(original));
}

#[test]
fn legacy_scalar_form_decodes_as_a_legacy_process() {
    assert_eq!(ClockState::decode("10000"), Ok(state(10_000, "legacy", 0)));
    assert_eq!(
        ClockState::decode(r#"{"v":1,"highest_seen":5,"trusted_process":"x"}"#),
        Ok(state(5, "x", 0)),
        "last_server_iat defaults to 0"
    );
}

#[test]
fn bad_stored_forms_are_rejected() {
    assert_eq!(
        ClockState::decode(r#"{"v":2,"highest_seen":5,"trusted_process":"x"}"#),
        Err(ClockDecodeError::UnsupportedVersion(2))
    );
    assert_eq!(
        ClockState::decode("garbage"),
        Err(ClockDecodeError::Malformed)
    );
    assert_eq!(ClockState::decode(""), Err(ClockDecodeError::Malformed));
}

// ------------------------------------------------------------------ persisted clock

fn clock<'a>(
    store: &'a MemoryClockStore,
    process: &str,
    wall: i64,
) -> AntiRollbackClock<&'a MemoryClockStore> {
    // Explicit lifetime: `process` is a separate borrow, so elision would be ambiguous.
    AntiRollbackClock::new(store, ProcessAnchor::with_id(process, wall))
}

#[test]
fn reconcile_then_commit_persists_a_first_ledger() {
    let store = MemoryClockStore::new();
    let c = clock(&store, "p", 10_000);
    let reading = c.reconcile(10_000, false).unwrap();
    assert!(matches!(
        reading,
        ClockReading::Trusted {
            effective_now: 10_000,
            prior: None,
            ..
        }
    ));
    assert_eq!(store.load().unwrap(), None, "reconcile alone never writes");
    c.commit(&reading).unwrap();
    assert_eq!(store.load().unwrap(), Some(state(10_000, "p", 0)));
}

#[test]
fn restart_with_a_rolled_back_clock_is_a_rollback_and_commit_ignores_it() {
    let store = MemoryClockStore::new();
    let first = clock(&store, "a", 10_000);
    let reading = first.reconcile(10_000, false).unwrap();
    first.commit(&reading).unwrap();

    let second = clock(&store, "b", 4_000);
    let reading = second.reconcile(4_000, true).unwrap();
    assert_eq!(
        reading,
        ClockReading::Rollback {
            highest_seen: 10_000
        }
    );
    second.commit(&reading).unwrap();
    assert_eq!(store.load().unwrap(), Some(state(10_000, "a", 0)));
}

#[test]
fn deleting_the_ledger_of_an_initialised_install_is_detected() {
    let store = MemoryClockStore::new();
    let c = clock(&store, "a", 10_000);
    let reading = c.reconcile(10_000, false).unwrap();
    c.commit(&reading).unwrap();
    store.clear();
    assert_eq!(c.reconcile(10_000, true), Err(ClockError::Missing));
    // Not initialised: allowed to start fresh.
    assert!(c.reconcile(10_000, false).is_ok());
}

struct CountingStore {
    inner: RefCell<Option<ClockState>>,
    saves: Cell<u32>,
}

impl CountingStore {
    fn new() -> Self {
        Self {
            inner: RefCell::new(None),
            saves: Cell::new(0),
        }
    }
}

impl ClockStore for CountingStore {
    type Error = std::convert::Infallible;
    fn load(&self) -> Result<Option<ClockState>, Self::Error> {
        Ok(self.inner.borrow().clone())
    }
    fn save(&self, value: &ClockState) -> Result<(), Self::Error> {
        self.saves.set(self.saves.get() + 1);
        *self.inner.borrow_mut() = Some(value.clone());
        Ok(())
    }
}

#[test]
fn commit_is_throttled_to_the_write_interval() {
    let store = CountingStore::new();
    let c = AntiRollbackClock::new(&store, ProcessAnchor::with_id("p", 10_000));
    let r = c.reconcile(10_000, false).unwrap();
    c.commit(&r).unwrap();
    assert_eq!(store.saves.get(), 1);

    let r = c
        .reconcile(10_000 + CLOCK_WRITE_INTERVAL_SECS - 1, true)
        .unwrap();
    c.commit(&r).unwrap();
    assert_eq!(store.saves.get(), 1, "inside the interval: no write");

    let r = c
        .reconcile(10_000 + CLOCK_WRITE_INTERVAL_SECS, true)
        .unwrap();
    c.commit(&r).unwrap();
    assert_eq!(store.saves.get(), 2, "interval elapsed: write");

    // A different process identity always writes.
    let other = AntiRollbackClock::new(&store, ProcessAnchor::with_id("q", 20_000));
    let r = other.reconcile(20_000, true).unwrap();
    other.commit(&r).unwrap();
    assert_eq!(store.saves.get(), 3);
}

// ------------------------------------------------------------------ server anchors

#[test]
fn newer_server_anchor_advances_the_mark() {
    let store = MemoryClockStore::new();
    store.save(&state(10_000, "a", 9_000)).unwrap();
    let c = clock(&store, "b", 10_050);
    let result = c.reanchor(10_040, 10_050, true).unwrap();
    assert_eq!(result.state, state(10_040, "b", 10_040));
    assert!(!result.lowered, "10_040 is above the previous mark 10_000");
    assert_eq!(store.load().unwrap(), Some(result.state));
}

#[test]
fn anchor_older_than_the_high_water_mark_repairs_a_clock_set_too_far_ahead() {
    // The mark was inflated (clock once set a year ahead), then the user fixed the clock
    // and is now locked out. A *newer* server anchor lowers the mark to real time.
    let store = MemoryClockStore::new();
    store.save(&state(900_000, "a", 9_000)).unwrap();
    let c = clock(&store, "b", 10_050);
    assert!(matches!(
        c.reconcile(10_050, true).unwrap(),
        ClockReading::Rollback { .. }
    ));
    let result = c.reanchor(10_040, 10_050, true).unwrap();
    assert!(result.lowered);
    assert_eq!(result.state, state(10_040, "b", 10_040));
    assert_eq!(store.load().unwrap(), Some(result.state));
    // Now the (correct) wall clock is believed again.
    assert!(matches!(
        c.reconcile(10_060, true).unwrap(),
        ClockReading::Trusted { .. }
    ));
}

#[test]
fn replayed_or_older_server_anchor_is_refused_and_leaves_the_ledger_alone() {
    let store = MemoryClockStore::new();
    store.save(&state(10_000, "a", 9_000)).unwrap();
    let c = clock(&store, "b", 10_050);
    for replayed in [9_000, 8_999, 0] {
        assert_eq!(
            c.reanchor(replayed, 10_050, true),
            Err(ClockError::ReplayedAnchor {
                last_server_iat: 9_000,
                got: replayed
            })
        );
    }
    assert_eq!(store.load().unwrap(), Some(state(10_000, "a", 9_000)));
}

#[test]
fn first_anchor_seeds_the_ledger_from_the_later_of_wall_and_server_time() {
    let store = MemoryClockStore::new();
    let c = clock(&store, "a", 10_000);
    let result = c.reanchor(9_500, 10_000, false).unwrap();
    assert_eq!(result.state, state(10_000, "a", 9_500));
    assert!(!result.lowered);

    let store = MemoryClockStore::new();
    let c = clock(&store, "a", 10_000);
    let result = c.reanchor(12_000, 10_000, false).unwrap();
    assert_eq!(result.state, state(12_000, "a", 12_000));
}

#[test]
fn first_anchor_on_an_initialised_install_with_no_ledger_fails_closed() {
    let store = MemoryClockStore::new();
    let c = clock(&store, "a", 10_000);
    assert_eq!(c.reanchor(12_000, 10_000, true), Err(ClockError::Missing));
    assert_eq!(store.load().unwrap(), None);
}

// ------------------------------------------------------------------ TrustedClock hook

#[test]
fn system_clock_is_the_wall_clock() {
    assert_eq!(SystemClock.trusted_now(123, false), Ok(123));
    assert_eq!(SystemClock.trusted_now(123, true), Ok(123));
}

#[test]
fn anti_rollback_clock_serves_the_hook_and_refuses_a_rolled_back_clock() {
    let store = MemoryClockStore::new();
    let first = clock(&store, "a", 10_000);
    let hook: &dyn TrustedClock = &first;
    assert_eq!(hook.trusted_now(10_000, false), Ok(10_000));
    assert_eq!(store.load().unwrap(), Some(state(10_000, "a", 0)));

    let restarted = clock(&store, "b", 3_000);
    let hook: &dyn TrustedClock = &restarted;
    assert_eq!(
        hook.trusted_now(3_000, true),
        Err(ClockRefusal::Rollback {
            highest_seen: 10_000
        })
    );
    // Moving forward still works after the restart.
    assert_eq!(hook.trusted_now(10_500, true), Ok(10_500));
}

#[test]
fn hook_reports_a_missing_ledger() {
    let store = MemoryClockStore::new();
    let c = clock(&store, "a", 10_000);
    assert_eq!(c.trusted_now(10_000, true), Err(ClockRefusal::Missing));
}

#[test]
fn process_anchors_have_distinct_ids_and_a_non_decreasing_monotonic_offset() {
    let a = ProcessAnchor::new(5_000);
    let b = ProcessAnchor::new(5_000);
    assert_ne!(a.id(), b.id());
    assert!(a.monotonic_now() >= 5_000);
}

// ------------------------------------------------------------------ audit findings (2026-10-11)

fn lifecycle_at(c: &AntiRollbackClock<&MemoryClockStore>, token: &str, wall: i64) -> Lifecycle {
    let key = pubkey();
    let ctx = LifecycleContext::new(&key, APP, DEVICE);
    evaluate_lifecycle(
        &ctx,
        c,
        StoredCredential::Signed {
            token,
            clock_initialized: true,
        },
        wall,
    )
    .unwrap()
    .lifecycle
}

/// P1 (throttling forgets trusted time): the observation at 1050 is not persisted (less
/// than the write interval past the stored 1000), and a later wall reading of 1020 must
/// not bring a token that expired at 1040 back to life.
#[test]
fn throttled_observation_is_kept_in_memory_and_cannot_be_undone_by_a_lower_wall_reading() {
    let store = MemoryClockStore::new();
    store.save(&state(1_000, "p", 0)).unwrap();
    // Monotonic reference lags (system sleep): it starts well below the mark.
    let c = clock(&store, "p", 900);
    let token = Claims::term(900, 1_040).token();

    assert_eq!(lifecycle_at(&c, &token, 1_050), Lifecycle::Expired);
    assert_eq!(
        store.load().unwrap(),
        Some(state(1_000, "p", 0)),
        "the 1050 observation is inside the write interval, so it is not persisted"
    );
    assert_eq!(
        lifecycle_at(&c, &token, 1_020),
        Lifecycle::Expired,
        "a lower wall reading must not undo an observed expiry"
    );
    assert_eq!(c.trusted_now(1_020, true), Ok(1_050));
}

/// P1 (stale commits): A prepares 1000 from 900, B commits 1100, A commits. The mark must
/// stay 1100, and a restart at wall 1001 must not trust the earlier time.
#[test]
fn a_stale_commit_cannot_move_the_mark_backwards() {
    let store = MemoryClockStore::new();
    store.save(&state(900, "p", 0)).unwrap();
    let a = clock(&store, "a", 1_000);
    let b = clock(&store, "b", 1_100);
    let reading_a = a.reconcile(1_000, true).unwrap();
    let reading_b = b.reconcile(1_100, true).unwrap();
    b.commit(&reading_b).unwrap();
    assert_eq!(store.load().unwrap(), Some(state(1_100, "b", 0)));
    a.commit(&reading_a).unwrap();
    assert_eq!(
        store.load().unwrap(),
        Some(state(1_100, "b", 0)),
        "A's older commit must not overwrite B's newer mark"
    );

    let restarted = clock(&store, "c", 1_001);
    assert_eq!(
        restarted.reconcile(1_001, true).unwrap(),
        ClockReading::Rollback {
            highest_seen: 1_100
        }
    );
    assert_eq!(
        restarted.trusted_now(1_001, true),
        Err(ClockRefusal::Rollback {
            highest_seen: 1_100
        })
    );
}

/// P1 (stale commits vs reanchor): a commit prepared before a server anchor landed must
/// not overwrite the newer `last_server_iat`, which would reopen the replay window.
#[test]
fn a_stale_commit_cannot_undo_a_newer_server_anchor() {
    let store = MemoryClockStore::new();
    store.save(&state(9_000, "p", 9_000)).unwrap();
    let reader = clock(&store, "a", 10_000);
    let reading = reader.reconcile(10_000, true).unwrap();

    let refresher = clock(&store, "b", 10_050);
    refresher.reanchor(10_040, 10_050, true).unwrap();
    assert_eq!(store.load().unwrap(), Some(state(10_040, "b", 10_040)));

    reader.commit(&reading).unwrap();
    let stored = store.load().unwrap().unwrap();
    assert_eq!(
        stored.last_server_iat, 10_040,
        "anchor must survive a stale commit"
    );
    assert_eq!(
        refresher.reanchor(10_030, 10_060, true),
        Err(ClockError::ReplayedAnchor {
            last_server_iat: 10_040,
            got: 10_030
        })
    );
}

/// P2 (clock repair leaves a poisoned process anchor): the process started with the wall
/// clock far ahead; after the user fixes the clock and a fresh server anchor lands, the
/// old monotonic reference must not pull trusted time back up.
#[test]
fn reanchor_repairs_the_process_time_reference() {
    let store = MemoryClockStore::new();
    let c = clock(&store, "p", 900_000);
    assert_eq!(c.trusted_now(900_000, false), Ok(900_000));
    assert!(matches!(
        c.reconcile(10_000, true).unwrap(),
        ClockReading::Rollback { .. }
    ));
    let result = c.reanchor(10_040, 10_050, true).unwrap();
    assert!(result.lowered);
    assert_eq!(c.trusted_now(10_060, true), Ok(10_060));
    let token = Claims::term(10_000, 10_100).token();
    assert_eq!(lifecycle_at(&c, &token, 10_070), Lifecycle::Active);
}

#[test]
fn a_reanchor_that_is_refused_does_not_repair_the_process_reference() {
    // Only an accepted (fresh, newer) server anchor may re-base the monotonic reference.
    let store = MemoryClockStore::new();
    store.save(&state(900_000, "p", 9_000)).unwrap();
    let c = clock(&store, "p", 900_000);
    assert_eq!(
        c.reanchor(8_000, 10_050, true),
        Err(ClockError::ReplayedAnchor {
            last_server_iat: 9_000,
            got: 8_000
        })
    );
    assert!(c.monotonic_now() >= 900_000);
    assert!(matches!(
        c.reconcile(10_060, true).unwrap(),
        ClockReading::Rollback {
            highest_seen: 900_000
        }
    ));
    assert_eq!(store.load().unwrap(), Some(state(900_000, "p", 9_000)));
}

#[test]
fn reanchor_never_lowers_trusted_time_below_a_prior_server_attestation() {
    let store = MemoryClockStore::new();
    let c = clock(&store, "p", 10_000);
    c.reanchor(10_040, 10_000, false).unwrap();
    // An older attestation is refused, so the mark stays at or above 10_040.
    assert!(c.reanchor(10_030, 10_100, true).is_err());
    let stored = store.load().unwrap().unwrap();
    assert!(stored.highest_seen >= stored.last_server_iat);
    assert_eq!(stored.last_server_iat, 10_040);
    // A newer one may lower the mark, but never below itself.
    let lowered = c.reanchor(10_041, 10_100, true).unwrap();
    assert!(lowered.state.highest_seen >= 10_041);
}

/// Store whose first compare-and-save is preceded by a competing writer.
struct RacingStore {
    inner: MemoryClockStore,
    race: std::sync::Mutex<Option<ClockState>>,
}

impl RacingStore {
    fn new(initial: ClockState, race: ClockState) -> Self {
        let inner = MemoryClockStore::new();
        inner.save(&initial).unwrap();
        Self {
            inner,
            race: std::sync::Mutex::new(Some(race)),
        }
    }
}

impl ClockStore for RacingStore {
    type Error = std::convert::Infallible;
    fn load(&self) -> Result<Option<ClockState>, Self::Error> {
        self.inner.load()
    }
    fn save(&self, value: &ClockState) -> Result<(), Self::Error> {
        self.inner.save(value)
    }
    fn compare_and_save(
        &self,
        expected: Option<&ClockState>,
        new: &ClockState,
    ) -> Result<bool, Self::Error> {
        if let Some(competing) = self.race.lock().unwrap().take() {
            self.inner.save(&competing)?;
        }
        self.inner.compare_and_save(expected, new)
    }
}

#[test]
fn a_competing_write_between_load_and_save_is_not_overwritten_by_a_commit() {
    let store = RacingStore::new(state(900, "p", 0), state(1_100, "x", 0));
    let c = AntiRollbackClock::new(&store, ProcessAnchor::with_id("a", 1_000));
    let reading = c.reconcile(1_000, true).unwrap();
    c.commit(&reading).unwrap();
    assert_eq!(store.load().unwrap(), Some(state(1_100, "x", 0)));
}

#[test]
fn a_competing_newer_anchor_between_load_and_save_wins_over_a_reanchor() {
    let store = RacingStore::new(state(10_000, "a", 9_000), state(11_000, "x", 20_000));
    let c = AntiRollbackClock::new(&store, ProcessAnchor::with_id("b", 10_050));
    assert_eq!(
        c.reanchor(10_040, 10_050, true),
        Err(ClockError::ReplayedAnchor {
            last_server_iat: 20_000,
            got: 10_040
        })
    );
    assert_eq!(store.load().unwrap(), Some(state(11_000, "x", 20_000)));
    // And the refused reanchor did not repair this clock's reference.
    assert!(c.monotonic_now() < 10_100);
}

/// A store that always reports the ledger changed.
struct AlwaysContended(MemoryClockStore);
impl ClockStore for AlwaysContended {
    type Error = std::convert::Infallible;
    fn load(&self) -> Result<Option<ClockState>, Self::Error> {
        self.0.load()
    }
    fn save(&self, value: &ClockState) -> Result<(), Self::Error> {
        self.0.save(value)
    }
    fn compare_and_save(
        &self,
        _: Option<&ClockState>,
        _: &ClockState,
    ) -> Result<bool, Self::Error> {
        Ok(false)
    }
}

#[test]
fn endless_contention_fails_with_contended_and_changes_nothing() {
    let store = AlwaysContended(MemoryClockStore::new());
    store.save(&state(10_000, "a", 9_000)).unwrap();
    let c = AntiRollbackClock::new(&store, ProcessAnchor::with_id("b", 20_000));
    let reading = c.reconcile(20_000, true).unwrap();
    assert_eq!(c.commit(&reading), Err(ClockError::Contended));
    assert_eq!(c.reanchor(12_000, 20_000, true), Err(ClockError::Contended));
    assert_eq!(store.load().unwrap(), Some(state(10_000, "a", 9_000)));
    assert!(c.monotonic_now() >= 20_000, "reference untouched");
}

#[test]
fn concurrent_clocks_on_one_ledger_never_move_the_stored_mark_backwards() {
    let store = MemoryClockStore::new();
    store.save(&state(1_000, "seed", 0)).unwrap();
    let max_ok = std::sync::atomic::AtomicI64::new(0);
    std::thread::scope(|scope| {
        for t in 0..8_i64 {
            let store = &store;
            let max_ok = &max_ok;
            scope.spawn(move || {
                let c = AntiRollbackClock::new(
                    store,
                    ProcessAnchor::with_id(format!("t{t}"), 1_000 + t),
                );
                let mut last_seen = 0;
                for i in 0..200_i64 {
                    // Each step is a full write interval ahead, so every advance persists.
                    let wall = 1_100 + (i * 8 + t) * (CLOCK_WRITE_INTERVAL_SECS + 1);
                    if let Ok(now) = c.trusted_now(wall, true) {
                        max_ok.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                    }
                    let seen = store.load().unwrap().unwrap().highest_seen;
                    assert!(
                        seen >= last_seen,
                        "mark moved backwards: {seen} < {last_seen}"
                    );
                    last_seen = seen;
                }
            });
        }
    });
    let stored = store.load().unwrap().unwrap();
    assert!(
        stored.highest_seen
            >= max_ok.load(std::sync::atomic::Ordering::SeqCst) - CLOCK_WRITE_INTERVAL_SECS
    );
}
