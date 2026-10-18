//! The supervisor loop without a child process: a launch closure that always fails drives the
//! restart tracker through its budget, which exercises the poll, backoff, give-up and stop paths.
//! (A real child is the app's integration test: it needs the app's engine binary.)

use rightkit_child_process::{HealthPolicy, OwnedCommand, RestartMode, RestartPolicy};
use rightkit_daemon::supervisor::{
    supervise, supervise_adopted, supervise_with_lock_state, LockState, SupervisorConfig,
    SupervisorEvent, SupervisorEventFn, SupervisorState,
};
use rightkit_daemon::PrivateChild;
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
fn an_unknown_lock_state_restarts_locked_and_only_an_answered_unlock_restarts_unlocked() {
    // The app's lock-state input reports Unknown (an unlock the child never answered):
    // every launch it drives must start locked.
    let flags = Arc::new(Mutex::new(Vec::new()));
    let seen = flags.clone();
    let handle = supervise_with_lock_state(
        config(RestartMode::OnDemand, 5),
        move |locked| {
            seen.lock().unwrap().push(locked);
            Err(std::io::Error::other("no such engine"))
        },
        |_| true,
        Some(Arc::new(|| LockState::Unknown)),
        None,
    )
    .unwrap();
    wait_for("the first launch", || !flags.lock().unwrap().is_empty());
    wait_for("the demand wait", || {
        handle.status().state == SupervisorState::AwaitingDemand
    });
    handle.restart_now();
    wait_for("the second launch", || flags.lock().unwrap().len() == 2);
    wait_for("the demand wait again", || {
        handle.status().state == SupervisorState::AwaitingDemand
    });
    // A positively answered unlock is the one input that restarts unlocked.
    handle.restart_now_with_lock(LockState::Unlocked);
    wait_for("the third launch", || flags.lock().unwrap().len() == 3);
    assert_eq!(*flags.lock().unwrap(), [true, true, false]);
    handle.stop();
}

/// The child process the adoption test supervises: this test binary re-run with the
/// helper env set. Without it (the ordinary suite run) it returns at once.
#[test]
fn adopted_helper_process() {
    if std::env::var_os("RK_SUPERVISOR_HELPER").is_some() {
        std::thread::sleep(Duration::from_secs(60));
    }
}

#[test]
fn an_adopted_child_survives_the_transfer_and_stop_returns_it_once_without_a_restart() {
    let mut command = OwnedCommand::new(std::env::current_exe().unwrap());
    command
        .command_mut()
        .args(["--exact", "adopted_helper_process", "--nocapture"])
        .env("RK_SUPERVISOR_HELPER", "1");
    let child = PrivateChild::spawn(command).unwrap();
    let pid = child.child().id();
    let launches = Arc::new(Mutex::new(0u32));
    let counter = launches.clone();
    let handle = supervise_adopted(
        config(RestartMode::Automatic, 3),
        child,
        move |_locked| {
            *counter.lock().unwrap() += 1;
            Err(std::io::Error::other("must not be relaunched"))
        },
        |_| true,
        None,
        None,
    )
    .unwrap();
    // Adopted as launch 1, already ready, still the same process.
    wait_for("the adopted child to show as ready", || {
        let status = handle.status();
        status.state == SupervisorState::Ready && status.pid == Some(pid) && status.launches == 1
    });
    let mut returned = handle
        .stop_returning_child()
        .expect("the running child is handed back");
    assert_eq!(returned.child().id(), pid);
    assert!(
        returned.child_mut().try_wait().unwrap().is_none(),
        "stop_returning_child must not stop the child"
    );
    // No restart follows: the supervisor thread has ended.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(*launches.lock().unwrap(), 0);
    assert!(returned.child_mut().try_wait().unwrap().is_none());
    drop(returned);
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
