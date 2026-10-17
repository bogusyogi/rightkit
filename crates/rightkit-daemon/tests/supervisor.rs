//! The supervisor loop without a child process: a launch closure that always fails drives the
//! restart tracker through its budget, which exercises the poll, backoff, give-up and stop paths.
//! (A real child is the app's integration test: it needs the app's engine binary.)

use rightkit_child_process::{HealthPolicy, RestartMode, RestartPolicy};
use rightkit_daemon::supervisor::{
    supervise, SupervisorConfig, SupervisorEvent, SupervisorEventFn, SupervisorState,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn recorder() -> (SupervisorEventFn, Arc<Mutex<Vec<SupervisorEvent>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let f: SupervisorEventFn = Arc::new(move |e| sink.lock().unwrap().push(e.clone()));
    (f, seen)
}

fn config(mode: RestartMode, max_restarts: u32) -> SupervisorConfig {
    let restart = RestartPolicy::new(
        mode,
        max_restarts,
        Duration::from_secs(60),
        Duration::from_millis(20),
        Duration::from_millis(40),
    )
    .unwrap();
    let health = HealthPolicy::new(Duration::from_secs(5), 3).unwrap();
    SupervisorConfig::new(restart, health).poll_interval(Duration::from_millis(10))
}

#[test]
fn a_launch_that_always_fails_is_retried_within_the_budget_then_given_up() {
    let (events, seen) = recorder();
    let attempts = Arc::new(Mutex::new(0u32));
    let counter = attempts.clone();
    let handle = supervise(
        config(RestartMode::Automatic, 2),
        move || {
            *counter.lock().unwrap() += 1;
            Err(std::io::Error::other("no such engine"))
        },
        |_| true,
        Some(events),
    )
    .unwrap();
    wait_for("the budget to be spent", || {
        handle.status().state == SupervisorState::Exhausted
    });
    // The first attempt plus two restarts, then nothing more.
    assert_eq!(*attempts.lock().unwrap(), 3);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(*attempts.lock().unwrap(), 3);
    assert_eq!(handle.status().launches, 0);
    assert_eq!(handle.status().pid, None);
    {
        let seen = seen.lock().unwrap();
        let failed = seen
            .iter()
            .filter(|e| matches!(e, SupervisorEvent::LaunchFailed { .. }))
            .count();
        let scheduled = seen
            .iter()
            .filter(|e| matches!(e, SupervisorEvent::RestartScheduled { .. }))
            .count();
        assert_eq!((failed, scheduled), (3, 2));
        assert_eq!(seen.last(), Some(&SupervisorEvent::Exhausted));
    }
    // An explicit start after exhaustion forgets the budget and tries again.
    handle.restart_now();
    wait_for("a fourth attempt", || *attempts.lock().unwrap() > 3);
    handle.stop();
}

#[test]
fn a_policy_that_never_restarts_stops_after_the_failed_launch() {
    let (events, seen) = recorder();
    let handle = supervise(
        config(RestartMode::Disabled, 0),
        || Err(std::io::Error::other("no such engine")),
        |_| true,
        Some(events),
    )
    .unwrap();
    wait_for("the supervisor to stop", || {
        handle.status().state == SupervisorState::Stopped
    });
    handle.stop();
    let seen = seen.lock().unwrap();
    assert!(seen.contains(&SupervisorEvent::Stopped));
    assert!(!seen
        .iter()
        .any(|e| matches!(e, SupervisorEvent::RestartScheduled { .. })));
}

#[test]
fn an_on_demand_policy_waits_for_restart_now() {
    let (events, _seen) = recorder();
    let attempts = Arc::new(Mutex::new(0u32));
    let counter = attempts.clone();
    let handle = supervise(
        config(RestartMode::OnDemand, 5),
        move || {
            *counter.lock().unwrap() += 1;
            Err(std::io::Error::other("no such engine"))
        },
        |_| true,
        Some(events),
    )
    .unwrap();
    wait_for("the demand wait", || {
        handle.status().state == SupervisorState::AwaitingDemand
    });
    assert_eq!(*attempts.lock().unwrap(), 1);
    handle.restart_now();
    wait_for("the demanded attempt", || *attempts.lock().unwrap() >= 2);
    handle.stop();
}
