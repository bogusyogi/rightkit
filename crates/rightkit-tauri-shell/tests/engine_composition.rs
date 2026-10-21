//! PK06-3: the engine host composition (presence and idle exit, ordered shutdown, snapshot replay
//! and revocation) against the real NDJSON fixture child.
#![cfg(feature = "engine")]
use rightkit_child_process::OwnedCommand;
use rightkit_tauri_shell::engine::{
    EngineHost, EnginePolicy, EngineSpec, PresencePolicy, RestartMode, RestartPolicy, SnapshotSlot,
    ERROR_REVOCATION_PENDING, ERROR_SNAPSHOT_NOT_DECLARED,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn spec() -> EngineSpec {
    let exe = std::env::current_exe().unwrap();
    let fixture = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(if cfg!(windows) {
            "shell_engine_fixture.exe"
        } else {
            "shell_engine_fixture"
        });
    assert!(
        fixture.is_file(),
        "build fixture with cargo build --features engine --example shell_engine_fixture"
    );
    let mut spec = EngineSpec::new(move || OwnedCommand::new(&fixture));
    spec.hello_timeout = Duration::from_secs(5);
    spec.shutdown_grace = Duration::from_millis(500);
    spec.call_timeout = Some(Duration::from_secs(3));
    spec
}

fn base_policy() -> EnginePolicy {
    EnginePolicy::default()
        .allow_methods(["echo", "slow", "slower", "crash", "seen", "pid"])
        .restart_policy(
            RestartPolicy::new(
                RestartMode::Automatic,
                3,
                Duration::from_secs(60),
                Duration::from_millis(5),
                Duration::from_millis(10),
            )
            .unwrap(),
        )
}

fn call(host: &EngineHost, method: &str) -> Result<Value, rightkit_job_queue::JobError> {
    host.call_tagged(method, Value::Null, None, Arc::new(|_| {}))
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn state(host: &EngineHost) -> String {
    host.lifecycle()["state"].as_str().unwrap_or("").to_owned()
}

fn pid(host: &EngineHost) -> u64 {
    call(host, "pid").unwrap()["pid"].as_u64().unwrap()
}

/// What the engine process has been sent apart from the probes themselves.
fn seen(host: &EngineHost) -> Vec<String> {
    call(host, "seen")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap().to_owned())
        .filter(|m| m != "seen" && m != "pid")
        .collect()
}

fn presence_host(idle: Duration) -> Arc<EngineHost> {
    let host = Arc::new(EngineHost::with_policy(
        spec(),
        base_policy().presence(PresencePolicy::idle_exit(idle)),
    ));
    assert!(host.start_idle_monitor());
    assert!(!host.start_idle_monitor(), "one monitor per host");
    host
}

#[test]
fn an_engine_nobody_used_is_never_idled_out() {
    let host = presence_host(Duration::from_millis(100));
    host.start().unwrap();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(state(&host), "ready");
    // Starting again is idempotent: the same process answers.
    let first = host.hello().unwrap()["pid"].clone();
    host.start().unwrap();
    assert_eq!(host.hello().unwrap()["pid"], first);
    host.shutdown();
}

#[test]
fn a_used_engine_idles_out_and_the_next_call_starts_a_new_generation() {
    let host = presence_host(Duration::from_millis(150));
    host.start().unwrap();
    let first_pid = pid(&host);
    let first_generation = host.generation();
    wait_for("idle exit", || state(&host) == "idle");
    assert_eq!(host.lifecycle()["hello"], Value::Null);
    // The call that finds it idle starts it again.
    let second_pid = pid(&host);
    assert_ne!(first_pid, second_pid, "a new engine process");
    assert!(host.generation() > first_generation);
    assert_eq!(state(&host), "ready");
    host.shutdown();
    assert_eq!(state(&host), "stopped");
}

#[test]
fn a_hold_keeps_the_engine_and_releasing_it_lets_it_idle() {
    let host = presence_host(Duration::from_millis(120));
    host.start().unwrap();
    let hold = host.hold("main-window").unwrap();
    assert_eq!(hold.label(), "main-window");
    assert_eq!(host.holders(), 1);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(state(&host), "ready", "a held engine does not idle");
    drop(hold);
    assert_eq!(host.holders(), 0);
    wait_for("idle exit after release", || state(&host) == "idle");
    // A hold on an idle engine starts it.
    let again = host.hold("second").unwrap();
    assert_eq!(state(&host), "ready");
    drop(again);
    host.shutdown();
}

#[test]
fn a_call_in_flight_keeps_the_engine_even_when_it_outlasts_idle_exit() {
    let host = presence_host(Duration::from_millis(100));
    host.start().unwrap();
    let worker = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || call(&host, "slower"))
    };
    wait_for("the call to start", || host.in_flight() == 1);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(state(&host), "ready", "idle exit waited for the call");
    worker.join().unwrap().unwrap();
    assert_eq!(host.in_flight(), 0);
    wait_for("idle exit after the call", || state(&host) == "idle");
    host.shutdown();
}

#[test]
fn ordered_shutdown_drains_a_call_in_flight_and_refuses_new_use() {
    let host = Arc::new(EngineHost::with_policy(
        spec(),
        base_policy().shutdown_drain(Duration::from_secs(3)),
    ));
    host.start().unwrap();
    let worker = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || call(&host, "slow"))
    };
    wait_for("the call to start", || host.in_flight() == 1);
    let report = host.shutdown_ordered();
    assert!(report.first && report.drained);
    assert_eq!(report.abandoned_calls, 0);
    assert!(report.engine.is_some());
    worker
        .join()
        .unwrap()
        .expect("the call finished before the engine stopped");
    assert_eq!(call(&host, "echo").unwrap_err().code, "shutting_down");
    assert_eq!(host.hold("late").unwrap_err().code, "shutting_down");
    let again = host.shutdown_ordered();
    assert!(!again.first);
    assert_eq!(state(&host), "stopped");
}

#[test]
fn a_call_that_outlasts_the_drain_is_abandoned_and_reported() {
    let host = Arc::new(EngineHost::with_policy(
        spec(),
        base_policy().shutdown_drain(Duration::from_millis(60)),
    ));
    host.start().unwrap();
    let events = Arc::new(AtomicUsize::new(0));
    let worker = {
        let host = Arc::clone(&host);
        let events = Arc::clone(&events);
        std::thread::spawn(move || {
            host.call_tagged(
                "slower",
                Value::Null,
                None,
                Arc::new(move |_| {
                    events.fetch_add(1, Ordering::SeqCst);
                }),
            )
        })
    };
    wait_for("the call to start", || host.in_flight() == 1);
    let report = host.shutdown_ordered();
    assert!(!report.drained);
    assert_eq!(report.abandoned_calls, 1);
    // The engine stop ends the call; it is an error, never a late success.
    assert!(worker.join().unwrap().is_err());
    assert_eq!(events.load(Ordering::SeqCst), 0);
}

fn snapshot_host() -> Arc<EngineHost> {
    Arc::new(EngineHost::with_policy(
        spec(),
        base_policy()
            .snapshot(SnapshotSlot::new("init", "set_init").required())
            .snapshot(SnapshotSlot::new("settings", "set_settings"))
            .snapshot(SnapshotSlot::new("auth", "set_auth")),
    ))
}

#[test]
fn snapshots_register_in_declared_order_and_replay_to_the_next_generation() {
    let host = snapshot_host();
    let events = host.subscribe_lifecycle().unwrap();
    assert_eq!(
        host.set_snapshot("unknown", json!(1)).unwrap_err().code,
        ERROR_SNAPSHOT_NOT_DECLARED
    );
    // `settings` arrives before the required `init`: held back, not sent out of order.
    host.set_snapshot("settings", json!({"a": 1})).unwrap();
    host.start().unwrap();
    assert!(seen(&host).is_empty(), "nothing before `init` has a value");
    host.set_snapshot("init", json!({"project": "demo"}))
        .unwrap();
    assert_eq!(seen(&host), ["set_init", "set_settings"]);
    host.set_snapshot("auth", json!({"user": "x"})).unwrap();
    host.set_snapshot("settings", json!({"a": 2})).unwrap();
    assert_eq!(
        seen(&host),
        ["set_init", "set_settings", "set_auth", "set_settings"]
    );
    // The engine dies; its replacement boots on the latest values, in registration order, once each.
    assert!(call(&host, "crash").is_err());
    loop {
        let event = events.recv_timeout(Duration::from_secs(10)).unwrap();
        if event.state == rightkit_job_queue::Lifecycle::Ready && event.generation > 1 {
            break;
        }
    }
    assert_eq!(seen(&host), ["set_init", "set_settings", "set_auth"]);
    host.shutdown();
}

#[test]
fn a_slot_the_app_did_not_declare_cannot_reach_the_engine() {
    let host = snapshot_host();
    host.start().unwrap();
    assert_eq!(
        host.revoke_snapshot("entitlements", json!({}))
            .unwrap_err()
            .code,
        ERROR_SNAPSHOT_NOT_DECLARED
    );
    assert!(seen(&host).is_empty());
    host.shutdown();
}

#[test]
fn new_calls_are_refused_while_a_revocation_applies_and_work_resumes_after() {
    // The auth slot's method is slow, so the revocation is observably in progress.
    let host = Arc::new(EngineHost::with_policy(
        spec(),
        base_policy().snapshot(SnapshotSlot::new("auth", "slower")),
    ));
    host.start().unwrap();
    let first_pid = pid(&host);
    let revoker = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || host.revoke_snapshot("auth", json!({"signed_in": false})))
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match call(&host, "echo") {
            Err(e) if e.code == ERROR_REVOCATION_PENDING => break,
            _ => {
                assert!(Instant::now() < deadline, "never saw revocation_pending");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    revoker.join().unwrap().unwrap();
    // Acknowledged, so the same engine carries on.
    assert_eq!(pid(&host), first_pid);
    assert!(call(&host, "echo").is_ok());
    host.shutdown();
}

#[test]
fn a_restart_replaces_the_engine_and_ends_the_calls_of_the_old_one() {
    let host = Arc::new(EngineHost::with_policy(spec(), base_policy()));
    host.start().unwrap();
    let before = pid(&host);
    let generation = host.generation();
    let events = Arc::new(AtomicUsize::new(0));
    let worker = {
        let host = Arc::clone(&host);
        let events = Arc::clone(&events);
        std::thread::spawn(move || {
            host.call_tagged(
                "slower",
                Value::Null,
                None,
                Arc::new(move |_| {
                    events.fetch_add(1, Ordering::SeqCst);
                }),
            )
        })
    };
    wait_for("the call to start", || host.in_flight() == 1);
    host.restart().unwrap();
    // The old call ends in an error: its reply is never delivered as a success.
    assert!(worker.join().unwrap().is_err());
    assert_eq!(events.load(Ordering::SeqCst), 0);
    assert!(host.generation() > generation);
    assert_ne!(pid(&host), before);
    host.shutdown();
}

// README-EXAMPLE-BEGIN
fn readme_example(spec: EngineSpec) -> Result<(), rightkit_job_queue::JobError> {
    let policy = EnginePolicy::default()
        .allow_methods(["echo", "seen"])
        .snapshot(SnapshotSlot::new("init", "set_init").required())
        .snapshot(SnapshotSlot::new("settings", "set_settings"))
        .presence(PresencePolicy::idle_exit(Duration::from_secs(300)))
        .shutdown_drain(Duration::from_secs(2));
    let host = Arc::new(EngineHost::with_policy(spec, policy));
    host.start_idle_monitor();
    host.set_snapshot("init", json!({"project": "demo"}))?;
    let hold = host.hold("main-window")?;
    host.call_tagged("echo", json!({"hello": 1}), None, Arc::new(|_| {}))?;
    drop(hold);
    let report = host.shutdown_ordered();
    assert!(report.first && report.drained);
    Ok(())
}
// README-EXAMPLE-END

#[test]
fn the_readme_example_runs_and_is_the_readme_example() {
    readme_example(spec()).unwrap();
    // The README shows exactly these lines (a packaged crate has the README; so does a checkout).
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/engine_composition.rs"
    ))
    .unwrap();
    let begin = source.find("// README-EXAMPLE-BEGIN").unwrap();
    let end = source.find("// README-EXAMPLE-END").unwrap();
    let readme =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).unwrap();
    for line in source[begin..end].lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        assert!(
            readme.contains(line),
            "README is missing the example line {line:?}"
        );
    }
}
