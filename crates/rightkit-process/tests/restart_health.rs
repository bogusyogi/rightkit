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
