//! The daemon client lifecycle (PK06-4, PK06-6) against real hosts: an in-process host for the
//! attach side, and the compiled example `svc_resident` as a launched, owned host. The
//! deterministic cases (fake backend: stale replies, restart budget, claim contention) are unit
//! tests in `src/lifecycle.rs`.

use rightkit_child_process::{
    AdoptedIdentity, AdoptedSignal, OwnedCommand, RestartMode, RestartPolicy,
};
use rightkit_daemon::lifecycle::{
    DaemonLifecycle, Launcher, LifecycleConfig, LifecycleError, Provenance,
};
use rightkit_daemon::*;
use serde_json::{json, Value};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

struct Echo;

impl Handler for Echo {
    fn call(
        &self,
        _cx: &CallContext,
        method: &str,
        params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        match method {
            "echo" => Ok(params.clone()),
            _ => Err(ServiceError::new("unknown_method", method)),
        }
    }
}

fn setup() -> (tempfile::TempDir, Suite) {
    let d = tempfile::tempdir().unwrap();
    let s = Suite::at(d.path().join("suite")).unwrap();
    (d, s)
}

fn host_in_process(suite: &Suite, service: &str) -> Hosted {
    let allow = Allowlist::new().allow("shell", ["echo"]);
    let cfg = ServiceConfig::new(service, "resident", 5, "test", allow);
    host(suite, cfg, Arc::new(Echo)).expect("host the service in this process")
}

fn client(suite: &Suite, service: &str) -> ClientConfig {
    ClientConfig::new(service, "shell")
        .suite(suite.clone())
        .deadline(Some(Duration::from_secs(20)))
}

fn this_binary() -> HostExpectation {
    HostExpectation::executable(std::env::current_exe().unwrap())
}

fn example(name: &str) -> PathBuf {
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        })
}

fn resident_launcher(suite: &Suite, service: &str, extra: Option<&'static str>) -> Launcher {
    let exe = example("svc_resident");
    let root = suite.root().to_path_buf();
    let service = service.to_string();
    Box::new(move || {
        let mut cmd = OwnedCommand::new(&exe);
        cmd.command_mut()
            .arg(&root)
            .args([service.as_str(), "daemon", "3"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(flag) = extra {
            cmd.command_mut().arg(flag);
        }
        cmd.spawn()
    })
}

fn resident_config(grace: Duration) -> LifecycleConfig {
    let policy = RestartPolicy::new(
        RestartMode::Automatic,
        3,
        Duration::from_secs(60),
        Duration::from_millis(10),
        Duration::from_millis(20),
    )
    .unwrap();
    LifecycleConfig::new()
        .startup_timeout(Duration::from_secs(60))
        .poll_interval(Duration::from_millis(20))
        .stop_grace(grace)
        .restart_policy(policy)
        .graceful_stop(|link| link.call("shutdown", json!({})).map(|_| ()))
}

#[test]
fn two_shells_attach_and_neither_may_stop_the_shared_host() {
    let (_d, suite) = setup();
    let hosted = host_in_process(&suite, "resident");
    let config = || LifecycleConfig::new();
    let a = DaemonLifecycle::attach_or_launch_local(
        client(&suite, "resident"),
        this_binary(),
        None,
        config(),
    )
    .expect("shell A attaches");
    let b = DaemonLifecycle::attach_or_launch_local(
        client(&suite, "resident"),
        this_binary(),
        None,
        config(),
    )
    .expect("shell B attaches");
    assert_eq!(a.provenance(), Some(Provenance::Attached));
    assert_eq!(b.provenance(), Some(Provenance::Attached));
    assert_eq!(a.host(), b.host());
    assert_eq!(a.host().map(|h| h.pid), Some(std::process::id()));

    let session_a = a.acquire_session("shell-a").unwrap();
    let session_b = b.acquire_session("shell-b").unwrap();
    assert_eq!(
        session_a.call("echo", json!({"from": "a"})).unwrap(),
        json!({"from": "a"})
    );
    assert_eq!(
        session_b.call("echo", json!({"from": "b"})).unwrap(),
        json!({"from": "b"})
    );

    // Attaching never implies the right to stop: nothing is sent, nothing is signalled.
    let refused = a
        .shutdown_owned()
        .err()
        .expect("an attached host is not stoppable");
    assert!(matches!(refused, LifecycleError::NotOwned), "{refused}");

    // One client detaching leaves the shared service running for the other.
    let report = a.release_session(session_a);
    assert_eq!(report.provenance, Provenance::Attached);
    assert_eq!(report.remaining_sessions, 0);
    assert!(
        hosted.status().connections >= 1,
        "B's connection is still open"
    );
    assert_eq!(
        session_b.call("echo", json!({"still": "up"})).unwrap(),
        json!({"still": "up"})
    );
    drop(hosted);
}

#[test]
fn a_host_with_the_wrong_executable_is_refused_before_anything_is_attached() {
    let (d, suite) = setup();
    let hosted = host_in_process(&suite, "resident");
    let other = d.path().join("not-the-host");
    std::fs::write(&other, b"x").unwrap();
    let error = DaemonLifecycle::attach_or_launch_local(
        client(&suite, "resident"),
        HostExpectation::executable(&other),
        None,
        LifecycleConfig::new(),
    )
    .err()
    .expect("a host that is not the expected program is not attached");
    match error {
        LifecycleError::HostIdentity(e) => {
            assert_eq!(e.reason, HostIdentityReason::ExecutableMismatch);
            assert_eq!(e.found_pid, Some(std::process::id()));
        }
        other => panic!("expected a host identity refusal, got {other:?}"),
    }
    // The host never saw an application byte from the refused client.
    assert_eq!(hosted.status().connections, 0);
}

#[test]
fn a_record_whose_pid_was_recycled_is_not_attached() {
    let (_d, suite) = setup();
    let hosted = host_in_process(&suite, "resident");
    // The endpoint is live and the pid is alive, but the record names another process birth time:
    // that is what a recycled pid looks like.
    let mut record = read_record(&suite, "resident").expect("the host published a record");
    record.process_started_ms = 1;
    write_atomic(
        &record_path(&suite, "resident"),
        &serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();
    let error = DaemonLifecycle::attach_or_launch_local(
        client(&suite, "resident"),
        this_binary(),
        None,
        LifecycleConfig::new(),
    )
    .err()
    .expect("a recycled pid is not a host");
    assert!(matches!(error, LifecycleError::NoHost), "{error}");
    assert!(error.is_retryable());
    assert_eq!(hosted.status().connections, 0);
}

#[test]
fn when_the_owner_exits_sessions_go_stale_and_a_replacement_is_a_new_generation() {
    let (_d, suite) = setup();
    let first = host_in_process(&suite, "resident");
    let life = DaemonLifecycle::attach_or_launch_local(
        client(&suite, "resident"),
        this_binary(),
        None,
        LifecycleConfig::new(),
    )
    .unwrap();
    let old = life.acquire_session("shell").unwrap();
    let old_generation = old.host().clone();
    assert!(old.call("echo", json!({})).is_ok());

    // The owner exits.
    drop(first);
    let stale = old.call("echo", json!({})).err().expect("the host is gone");
    assert!(
        matches!(stale, LifecycleError::StaleGeneration { .. }),
        "{stale}"
    );
    let none = life
        .acquire_session("shell")
        .err()
        .expect("no host, no launcher");
    assert!(matches!(none, LifecycleError::NoHost), "{none}");
    assert_eq!(life.provenance(), None);

    // A replacement starts (a later publication time makes it a different generation).
    std::thread::sleep(Duration::from_millis(30));
    let second = host_in_process(&suite, "resident");
    let new = life.acquire_session("shell").unwrap();
    assert_ne!(new.host(), &old_generation);
    assert!(new.epoch() > old.epoch());
    assert_eq!(new.call("echo", json!({"n": 2})).unwrap(), json!({"n": 2}));
    // The session of the old generation stays stale even though a host answers again.
    assert!(matches!(
        old.call("echo", json!({})),
        Err(LifecycleError::StaleGeneration { .. })
    ));
    drop(second);
}

#[test]
fn an_owned_host_stops_gracefully_and_an_attached_shell_cannot_stop_it() {
    let (_d, suite) = setup();
    let exe = example("svc_resident");
    let owner = DaemonLifecycle::attach_or_launch_local(
        client(&suite, "resident"),
        HostExpectation::executable(&exe),
        Some(resident_launcher(&suite, "resident", None)),
        resident_config(Duration::from_secs(20)),
    )
    .expect("the owner launches the host");
    assert_eq!(owner.provenance(), Some(Provenance::Owned));
    let host_pid = owner.host().expect("bound").pid;

    // A second shell attaches to the host the first one launched.
    let guest = DaemonLifecycle::attach_or_launch_local(
        client(&suite, "resident"),
        HostExpectation::executable(&exe),
        None,
        resident_config(Duration::from_secs(20)),
    )
    .expect("the guest attaches");
    assert_eq!(guest.provenance(), Some(Provenance::Attached));
    assert_eq!(guest.host().map(|h| h.pid), Some(host_pid));
    let guest_session = guest.acquire_session("guest").unwrap();
    let owner_session = owner.acquire_session("owner").unwrap();
    assert_eq!(owner_session.provenance(), Provenance::Owned);
    assert_eq!(guest_session.provenance(), Provenance::Attached);
    assert_eq!(
        guest_session.call("echo", json!({"x": 1})).unwrap(),
        json!({"x": 1})
    );

    let refused = guest
        .shutdown_owned()
        .err()
        .expect("a guest may not stop the host");
    assert!(matches!(refused, LifecycleError::NotOwned), "{refused}");
    assert!(
        guest_session.call("echo", json!({})).is_ok(),
        "the host is still up"
    );
    // Releasing the guest's session closes its connection and nothing else.
    let released = guest.release_session(guest_session);
    assert_eq!(released.provenance, Provenance::Attached);
    assert!(owner_session.call("echo", json!({})).is_ok());

    // The owner stops it: the host honours the polite request, so nothing is killed.
    let report = owner.shutdown_owned().expect("the owner stops its host");
    assert!(report.graceful_requested);
    assert!(!report.killed, "{report:?}");
    assert!(AdoptedIdentity::capture(NonZeroU32::new(host_pid).unwrap())
        .map(|found| found.is_none())
        .unwrap_or(true));
    assert!(matches!(
        owner_session.call("echo", json!({})),
        Err(LifecycleError::Released) | Err(LifecycleError::StaleGeneration { .. })
    ));
}

#[test]
fn a_host_that_ignores_the_polite_request_is_killed_after_the_grace() {
    let (_d, suite) = setup();
    let exe = example("svc_resident");
    let owner = DaemonLifecycle::attach_or_launch_local(
        client(&suite, "resident"),
        HostExpectation::executable(&exe),
        Some(resident_launcher(
            &suite,
            "resident",
            Some("ignore-shutdown"),
        )),
        resident_config(Duration::from_millis(500)),
    )
    .expect("the owner launches the host");
    let host_pid = owner.host().expect("bound").pid;
    let report = owner.shutdown_owned().expect("the owner stops its host");
    assert!(
        report.graceful_requested,
        "the polite request was sent first"
    );
    assert!(
        report.graceful_acknowledged,
        "this host answers it and does not leave"
    );
    assert!(report.killed, "{report:?}");
    assert!(AdoptedIdentity::capture(NonZeroU32::new(host_pid).unwrap())
        .map(|found| found.is_none())
        .unwrap_or(true));
}

#[test]
fn an_owned_host_that_dies_is_replaced_by_the_owner_and_stays_owned() {
    let (_d, suite) = setup();
    let exe = example("svc_resident");
    let owner = DaemonLifecycle::attach_or_launch_local(
        client(&suite, "resident"),
        HostExpectation::executable(&exe),
        Some(resident_launcher(&suite, "resident", None)),
        resident_config(Duration::from_secs(20)),
    )
    .expect("the owner launches the host");
    let first_pid = owner.host().expect("bound").pid;
    let old = owner.acquire_session("owner").unwrap();
    assert!(old.call("echo", json!({})).is_ok());

    // The host dies out from under the owner (a crash), by identity, not by name.
    let victim = AdoptedIdentity::capture(NonZeroU32::new(first_pid).unwrap())
        .unwrap()
        .expect("the host is running");
    victim.signal(AdoptedSignal::Force).unwrap();
    assert!(victim.wait_confirmed_gone(Duration::from_secs(10)).unwrap());

    let stale = old
        .call("echo", json!({}))
        .err()
        .expect("the generation is gone");
    assert!(
        matches!(stale, LifecycleError::StaleGeneration { .. }),
        "{stale}"
    );

    // The next session starts a replacement under the restart budget, and owns it.
    let new = owner
        .acquire_session("owner")
        .expect("a replacement starts");
    assert_eq!(new.provenance(), Provenance::Owned);
    assert_ne!(new.host().pid, first_pid);
    assert_eq!(new.call("echo", json!({"n": 3})).unwrap(), json!({"n": 3}));
    assert!(matches!(
        old.call("echo", json!({})),
        Err(LifecycleError::StaleGeneration { .. })
    ));
    owner
        .shutdown_owned()
        .expect("the owner stops the replacement");
}
