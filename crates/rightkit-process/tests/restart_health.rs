use std::time::Duration;

use rightkit_process::{
    HealthOutcome, HealthPolicy, HealthProbe, HealthTracker, RestartMode, RestartOutcome,
    RestartPolicy, RestartTracker,
};

fn automatic_policy(max_restarts: u32) -> RestartPolicy {
    RestartPolicy::new(
        RestartMode::Automatic,
        max_restarts,
        Duration::from_secs(10),
        Duration::from_millis(100),
        Duration::from_millis(250),
    )
    .unwrap()
}

#[test]
fn automatic_restart_uses_bounded_backoff_then_exhausts_the_window() {
    let mut tracker = RestartTracker::new(automatic_policy(3));

    let generation = tracker.started(Duration::ZERO);
    let first = tracker.exited(generation, Duration::from_secs(1));
    let RestartOutcome::Restart(first) = first else {
        panic!("first crash should receive a restart permit");
    };
    assert_eq!(first.delay(), Duration::from_millis(100));
    assert!(tracker.permit_valid(&first, Duration::from_millis(1_100)));

    let generation = tracker.started(Duration::from_millis(1_100));
    let second = tracker.exited(generation, Duration::from_secs(2));
    let RestartOutcome::Restart(second) = second else {
        panic!("second crash should receive a restart permit");
    };
    assert_eq!(second.delay(), Duration::from_millis(200));

    let generation = tracker.started(Duration::from_millis(2_200));
    let third = tracker.exited(generation, Duration::from_secs(3));
    let RestartOutcome::Restart(third) = third else {
        panic!("third crash should receive a restart permit");
    };
    assert_eq!(third.delay(), Duration::from_millis(250));

    let generation = tracker.started(Duration::from_millis(3_250));
    assert_eq!(
        tracker.exited(generation, Duration::from_secs(4)),
        RestartOutcome::Exhausted
    );

    let generation = tracker.started(Duration::from_secs(12));
    assert!(matches!(
        tracker.exited(generation, Duration::from_secs(12)),
        RestartOutcome::Restart(_)
    ));
}

#[test]
fn shutdown_invalidates_a_pending_restart_permit() {
    let mut tracker = RestartTracker::new(automatic_policy(1));
    let generation = tracker.started(Duration::ZERO);
    let RestartOutcome::Restart(permit) = tracker.exited(generation, Duration::ZERO) else {
        panic!("crash should receive a restart permit");
    };

    tracker.shutdown();

    assert!(!tracker.permit_valid(&permit, Duration::from_secs(1)));
    assert_eq!(
        tracker.request_restart(Duration::from_secs(1)),
        RestartOutcome::Stopped
    );
}

#[test]
fn a_new_restart_permit_invalidates_the_previous_permit() {
    let mut tracker = RestartTracker::new(automatic_policy(3));
    let generation = tracker.started(Duration::ZERO);
    let RestartOutcome::Restart(first) = tracker.exited(generation, Duration::ZERO) else {
        panic!("crash should receive a restart permit");
    };
    let RestartOutcome::Restart(second) = tracker.request_restart(Duration::from_millis(10)) else {
        panic!("new request should replace the pending restart permit");
    };

    assert!(!tracker.permit_valid(&first, Duration::from_secs(1)));
    assert!(tracker.permit_valid(&second, Duration::from_secs(1)));
}

#[test]
fn stale_generation_exit_is_ignored() {
    let mut tracker = RestartTracker::new(automatic_policy(2));
    let old = tracker.started(Duration::ZERO);
    let current = tracker.started(Duration::from_secs(1));

    assert_eq!(
        tracker.exited(old, Duration::from_secs(2)),
        RestartOutcome::IgnoredStale
    );
    assert!(matches!(
        tracker.exited(current, Duration::from_secs(2)),
        RestartOutcome::Restart(_)
    ));
}

#[test]
fn on_demand_and_disabled_modes_never_restart_automatically() {
    let mut on_demand = RestartTracker::new(
        RestartPolicy::new(
            RestartMode::OnDemand,
            2,
            Duration::from_secs(10),
            Duration::ZERO,
            Duration::ZERO,
        )
        .unwrap(),
    );
    let generation = on_demand.started(Duration::ZERO);
    assert_eq!(
        on_demand.exited(generation, Duration::ZERO),
        RestartOutcome::AwaitingDemand
    );
    assert!(matches!(
        on_demand.request_restart(Duration::ZERO),
        RestartOutcome::Restart(_)
    ));

    let mut disabled = RestartTracker::new(
        RestartPolicy::new(
            RestartMode::Disabled,
            2,
            Duration::from_secs(10),
            Duration::ZERO,
            Duration::ZERO,
        )
        .unwrap(),
    );
    let generation = disabled.started(Duration::ZERO);
    assert_eq!(
        disabled.exited(generation, Duration::ZERO),
        RestartOutcome::Stopped
    );
    assert_eq!(
        disabled.request_restart(Duration::ZERO),
        RestartOutcome::Stopped
    );
}

#[test]
fn health_threshold_and_startup_timeout_are_deterministic() {
    let policy = HealthPolicy::new(Duration::from_secs(5), 3).unwrap();
    let mut tracker = HealthTracker::new(policy, Duration::ZERO);

    assert_eq!(
        tracker.outcome(Duration::from_secs(4)),
        HealthOutcome::Starting
    );
    assert_eq!(
        tracker.observe(Duration::from_secs(1), HealthProbe::Failure),
        HealthOutcome::Starting
    );
    assert_eq!(
        tracker.observe(Duration::from_secs(2), HealthProbe::Failure),
        HealthOutcome::Starting
    );
    assert_eq!(
        tracker.observe(Duration::from_secs(3), HealthProbe::Failure),
        HealthOutcome::Unhealthy {
            consecutive_failures: 3
        }
    );
    assert_eq!(
        tracker.observe(Duration::from_secs(4), HealthProbe::Success),
        HealthOutcome::Healthy
    );
    assert_eq!(
        tracker.observe(Duration::from_secs(5), HealthProbe::Failure),
        HealthOutcome::Healthy
    );

    let never_healthy = HealthTracker::new(policy, Duration::ZERO);
    assert_eq!(
        never_healthy.outcome(Duration::from_secs(5)),
        HealthOutcome::StartupTimedOut
    );
}

fn crash_once(tracker: &mut RestartTracker, at: Duration) -> RestartOutcome {
    let generation = tracker.started(at);
    tracker.exited(generation, at)
}

#[test]
fn consecutive_cap_applies_the_cooldown_then_starts_a_new_count() {
    let policy = RestartPolicy::new(
        RestartMode::Automatic,
        100,
        Duration::from_secs(1_000),
        Duration::from_millis(10),
        Duration::from_millis(40),
    )
    .unwrap()
    .with_consecutive_failures(2, Duration::from_secs(30))
    .unwrap();
    let mut tracker = RestartTracker::new(policy);
    let delays: Vec<_> = (1..=5)
        .map(|i| match crash_once(&mut tracker, Duration::from_secs(i)) {
            RestartOutcome::Restart(p) => p.delay(),
            other => panic!("expected a restart, got {other:?}"),
        })
        .collect();
    assert_eq!(delays[0], Duration::from_millis(10));
    assert_eq!(delays[1], Duration::from_millis(20));
    // Two restarts in a row without a ready: the third waits out the cooldown.
    assert_eq!(delays[2], Duration::from_secs(30));
    // A new consecutive count started (the rolling window still sets the backoff); the cap then
    // applies again after two more.
    assert_eq!(delays[3], Duration::from_millis(40));
    assert_eq!(delays[4], Duration::from_secs(30));
}

#[test]
fn consecutive_cap_with_zero_cooldown_ends_the_run_and_ready_prevents_it() {
    let policy = RestartPolicy::new(
        RestartMode::Automatic,
        100,
        Duration::from_secs(1_000),
        Duration::from_millis(10),
        Duration::from_millis(40),
    )
    .unwrap()
    .with_consecutive_failures(2, Duration::ZERO)
    .unwrap();
    // Failing without ever reaching ready: the third crash is refused.
    let mut tracker = RestartTracker::new(policy);
    assert!(matches!(
        crash_once(&mut tracker, Duration::from_secs(1)),
        RestartOutcome::Restart(_)
    ));
    assert!(matches!(
        crash_once(&mut tracker, Duration::from_secs(2)),
        RestartOutcome::Restart(_)
    ));
    assert_eq!(
        crash_once(&mut tracker, Duration::from_secs(3)),
        RestartOutcome::Exhausted
    );
    // Reaching ready between crashes resets the count: the same crash count is never refused.
    let mut tracker = RestartTracker::new(policy);
    for i in 1..=6 {
        let generation = tracker.started(Duration::from_secs(i));
        tracker.ready(generation);
        assert!(matches!(
            tracker.exited(generation, Duration::from_secs(i)),
            RestartOutcome::Restart(_)
        ));
        assert_eq!(tracker.consecutive_failures(), 1);
    }
}

#[test]
fn ready_of_a_stale_generation_changes_nothing_and_budget_reset_is_opt_in() {
    let base = automatic_policy(2);
    let mut tracker = RestartTracker::new(base);
    let old = tracker.started(Duration::ZERO);
    assert!(matches!(
        tracker.exited(old, Duration::from_secs(1)),
        RestartOutcome::Restart(_)
    ));
    let _current = tracker.started(Duration::from_secs(2));
    tracker.ready(old);
    assert_eq!(tracker.restarts_in_window(Duration::from_secs(2)), 1);
    // Default: ready keeps the rolling budget.
    let g = tracker.started(Duration::from_secs(3));
    tracker.ready(g);
    assert_eq!(tracker.restarts_in_window(Duration::from_secs(3)), 1);
    // Opt-in: ready forgets it.
    let mut tracker = RestartTracker::new(base.with_reset_budget_on_ready());
    let g = tracker.started(Duration::ZERO);
    assert!(matches!(
        tracker.exited(g, Duration::from_secs(1)),
        RestartOutcome::Restart(_)
    ));
    let g = tracker.started(Duration::from_secs(2));
    tracker.ready(g);
    assert_eq!(tracker.restarts_in_window(Duration::from_secs(2)), 0);
}

#[test]
fn a_zero_cap_is_rejected() {
    assert!(automatic_policy(1)
        .with_consecutive_failures(0, Duration::ZERO)
        .is_err());
}
