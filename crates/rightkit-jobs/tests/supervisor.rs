//! Supervisor end-to-end coverage. Every case launches the real scriptable
//! `engine_fixture` child and checks observable frames, lifecycle, or files.

use rightkit_jobs::{
    CallOptions, EngineSupervisor, ErrorOrigin, EventFrame, InitialFailure, Lifecycle,
    ProtocolError, SupervisorConfig, Terminal, CONTRACT_VERSION, CONTRACT_VERSION_MISMATCH,
};
use rightkit_process::{OwnedCommand, RestartMode, RestartPolicy};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn fixture() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let examples = exe.parent().unwrap().parent().unwrap().join("examples");
    let exact = if cfg!(windows) {
        "engine_fixture.exe"
    } else {
        "engine_fixture"
    };
    // `--all-targets` also builds a libtest harness named `engine_fixture-<hash>`; the real
    // example is the exact name, so prefer it and only fall back to a hashed binary.
    if examples.join(exact).is_file() {
        return examples.join(exact);
    }
    std::fs::read_dir(&examples)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let binary =
                cfg!(windows) && name.ends_with(".exe") || !cfg!(windows) && !name.contains('.');
            name.starts_with("engine_fixture-") && binary && entry.path().is_file()
        })
        .map(|entry| entry.path())
        .unwrap_or_else(|| examples.join(exact))
}

fn rig(plan: &str) -> (tempfile::TempDir, SupervisorConfig) {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_path_buf();
    let plan = plan.to_string();
    let cfg = SupervisorConfig::new("fixture", move || {
        let mut c = OwnedCommand::new(fixture());
        c.command_mut().env("FX_DIR", &state).env("FX_PLAN", &plan);
        Ok(c)
    })
    .with_hello_timeout(Duration::from_secs(10))
    .with_ready_wait(Duration::from_secs(3))
    .with_shutdown_grace(Duration::from_millis(200));
    (dir, cfg)
}

fn wait_until(s: &EngineSupervisor, state: Lifecycle) {
    let until = Instant::now() + Duration::from_secs(4);
    while s.lifecycle() != state && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        s.lifecycle(),
        state,
        "status: {:?}",
        s.status().last_exit_reason
    );
}

fn wait_generation(s: &EngineSupervisor, generation: u64) {
    let until = Instant::now() + Duration::from_secs(4);
    while s.status().generation < generation && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        s.status().generation >= generation,
        "status: {:?}",
        s.status()
    );
}

#[test]
fn healthy_start_snapshot_events_and_shutdown() {
    let (_d, mut cfg) = rig("ok");
    cfg.rpc_timeout = Duration::from_secs(1);
    let s = EngineSupervisor::new(cfg);
    let events = s.subscribe_events();
    s.set_snapshot("settings", "set_settings", json!({"theme":"dark"}))
        .unwrap();
    s.start().unwrap();
    assert_eq!(
        s.call("get_state", json!({})).unwrap()["set_settings"]["theme"],
        "dark"
    );
    assert_eq!(s.call("notify", json!({"n": 2})).unwrap(), json!({}));
    assert_eq!(
        events.recv_timeout(Duration::from_secs(1)).unwrap()["event"]["i"],
        0
    );
    s.shutdown();
    assert_eq!(s.lifecycle(), Lifecycle::Down);
}

#[test]
fn crash_restarts_and_replays_snapshot_with_redacted_stderr() {
    let (_d, cfg) = rig("crash_on_call,ok");
    let s = EngineSupervisor::new(
        cfg.with_restart(
            RestartPolicy::new(
                RestartMode::Automatic,
                2,
                Duration::from_secs(10),
                Duration::from_millis(10),
                Duration::from_millis(20),
            )
            .unwrap(),
        )
        .with_stderr_redaction(|line| Some(line.replace("PRIVATE", "[redacted]"))),
    );
    let lifecycle = s.subscribe_lifecycle();
    s.set_snapshot("settings", "set_settings", json!({"v": 7}))
        .unwrap();
    s.start().unwrap();
    assert!(s.call("work", json!({})).is_err());
    wait_generation(&s, 2);
    wait_until(&s, Lifecycle::Ready);
    assert_eq!(s.call("gen", json!({})).unwrap()["gen"], 2);
    assert_eq!(
        s.call("get_state", json!({})).unwrap()["set_settings"]["v"],
        7
    );
    assert!(s.status().generation >= 2);
    assert!(lifecycle.try_iter().any(|event| event
        .stderr_tail
        .as_deref()
        .unwrap_or_default()
        .contains("[redacted]")));
    s.shutdown();
}

#[test]
fn restart_budget_exhausts_until_explicit_start_resets_it() {
    let (_d, cfg) = rig("crash:30");
    let policy = RestartPolicy::new(
        RestartMode::Automatic,
        1,
        Duration::from_secs(10),
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .unwrap();
    let s = EngineSupervisor::new(cfg.with_restart(policy));
    assert!(s.start().is_ok());
    wait_until(&s, Lifecycle::Down);
    assert!(s.call("ping", json!({})).is_err());
    assert!(s.start().is_ok());
    wait_until(&s, Lifecycle::Down);
    s.shutdown();
}

#[test]
fn rpc_timeout_kills_wedged_child_and_recovers() {
    let (_d, cfg) = rig("ok");
    let s = EngineSupervisor::new(cfg.with_rpc_timeout(Duration::from_millis(80)));
    s.start().unwrap();
    let e = s
        .call_with(
            "hang",
            json!({}),
            CallOptions {
                timeout: Some(Duration::from_millis(80)),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(e.code, "timeout");
    // The kill and restart land after the timeout returns; until then the old
    // generation can still read Ready.
    wait_generation(&s, 2);
    wait_until(&s, Lifecycle::Ready);
    assert!(s.status().generation >= 2);
    s.shutdown();
}

#[test]
fn health_ping_wedge_recovers() {
    let (_d, cfg) = rig("wedge:30,ok");
    let s = EngineSupervisor::new(
        cfg.with_restart(
            RestartPolicy::new(
                RestartMode::Automatic,
                2,
                Duration::from_secs(10),
                Duration::from_millis(1),
                Duration::from_millis(1),
            )
            .unwrap(),
        )
        .with_health(rightkit_jobs::HealthCheck {
            method: "ping".into(),
            interval: Duration::from_millis(20),
            timeout: Duration::from_millis(20),
            unhealthy_threshold: 1,
        }),
    );
    s.start().unwrap();
    wait_generation(&s, 2);
    wait_until(&s, Lifecycle::Ready);
    s.shutdown();
}

#[test]
fn hello_timeout_leaves_down_and_oversized_response_keeps_child() {
    let (_d, cfg) = rig("no_hello");
    let s = EngineSupervisor::new(
        cfg.with_hello_timeout(Duration::from_millis(250))
            .with_initial_failure(InitialFailure::Terminal),
    );
    let e = s.start().unwrap_err();
    assert_eq!(e.code, "hello_timeout");
    assert_eq!(s.lifecycle(), Lifecycle::Down);

    let (_d, cfg) = rig("ok");
    let s = EngineSupervisor::new(cfg.with_frame_limit(64 * 1024));
    s.start().unwrap();
    let e = s.call("big", json!({})).unwrap_err();
    assert_eq!(e.code, "frame_too_large");
    assert_eq!(s.call("echo", json!("still alive")).unwrap(), "still alive");
    s.shutdown();
}

#[test]
fn on_demand_crash_restarts_on_next_call() {
    let (_d, cfg) = rig("crash:30,ok");
    let s = EngineSupervisor::new(
        cfg.with_restart(
            RestartPolicy::new(
                RestartMode::OnDemand,
                2,
                Duration::from_secs(10),
                Duration::from_millis(1),
                Duration::from_millis(1),
            )
            .unwrap(),
        ),
    );
    s.start().unwrap();
    wait_until(&s, Lifecycle::Down);
    assert_eq!(s.call("ping", json!({})).unwrap()["pong"], true);
    s.shutdown();
}

#[test]
fn lifecycle_subscription_observes_boot_and_ready() {
    let (_d, cfg) = rig("ok");
    let s = EngineSupervisor::new(cfg);
    let rx = s.subscribe_lifecycle();
    s.start().unwrap();
    let mut seen = Vec::new();
    while let Ok(event) = rx.try_recv() {
        seen.push(event.state);
    }
    assert!(
        seen.contains(&Lifecycle::Booting) && seen.contains(&Lifecycle::Ready),
        "{seen:?}"
    );
    s.shutdown();
}

#[test]
fn frames_carry_the_request_id_and_merge_with_on_event() {
    let (_d, cfg) = rig("ok");
    let s = EngineSupervisor::new(cfg);
    s.start().unwrap();
    let frames = Arc::new(Mutex::new(Vec::new()));
    let payloads = Arc::new(AtomicUsize::new(0));
    let (f, p) = (frames.clone(), payloads.clone());
    let reply = s
        .call_with(
            "progress",
            json!({"n": 3}),
            CallOptions {
                on_event: Some(Box::new(move |_| {
                    p.fetch_add(1, Ordering::SeqCst);
                })),
                on_frame: Some(Box::new(move |fr: &EventFrame<'_>| {
                    f.lock().unwrap().push((fr.id, fr.event["i"].as_u64()));
                })),
                ..Default::default()
            },
        )
        .unwrap();
    // The engine answers with the request id it read; every event must carry it.
    let id = reply["id"].as_u64().unwrap();
    let expected: Vec<(u64, Option<u64>)> = (0..3u64).map(|i| (id, Some(i))).collect();
    assert_eq!(*frames.lock().unwrap(), expected);
    assert_eq!(payloads.load(Ordering::SeqCst), 3);
    s.shutdown();
}

#[test]
fn contract_version_gate_refuses_a_mismatched_engine() {
    let (_d, cfg) = rig("ok");
    let s = EngineSupervisor::new(cfg.require_contract_version(CONTRACT_VERSION + 1));
    let e = s.start().unwrap_err();
    assert_eq!(e.code, CONTRACT_VERSION_MISMATCH);
    assert!(!e.retryable);
    assert_eq!(s.lifecycle(), Lifecycle::Down);
    assert!(matches!(
        s.status().terminal,
        Some(Terminal::Failed { ref reason }) if reason.contains(CONTRACT_VERSION_MISMATCH)
    ));
    assert!(s
        .status()
        .last_exit_reason
        .unwrap_or_default()
        .contains(CONTRACT_VERSION_MISMATCH));

    // The fixture advertises contract_version 1: the integer and its text both match.
    let (_d, cfg) = rig("ok");
    let s = EngineSupervisor::new(cfg.require_contract_version(CONTRACT_VERSION));
    s.start().unwrap();
    assert_eq!(s.call("ping", json!({})).unwrap()["pong"], true);
    s.shutdown();
    let (_d, cfg) = rig("ok");
    let s = EngineSupervisor::new(cfg.require_contract_version("1"));
    s.start().unwrap();
    s.shutdown();
}

#[test]
fn start_background_returns_at_once_and_readiness_reports_the_boot() {
    let (_d, cfg) = rig("slow_hello:300");
    let s = EngineSupervisor::new(cfg);
    let began = Instant::now();
    let readiness = s.start_background();
    assert!(
        began.elapsed() < Duration::from_millis(150),
        "start_background blocked for {:?}",
        began.elapsed()
    );
    assert_eq!(s.lifecycle(), Lifecycle::Booting);
    assert!(readiness.try_ready().is_none());
    assert!(readiness.wait_timeout(Duration::from_millis(1)).is_none());
    readiness.ready().unwrap();
    assert_eq!(s.lifecycle(), Lifecycle::Ready);
    assert_eq!(readiness.try_ready(), Some(Ok(())));
    assert_eq!(s.call("ping", json!({})).unwrap()["pong"], true);
    // Already Ready: the handle resolves at once.
    assert_eq!(s.start_background().try_ready(), Some(Ok(())));
    s.shutdown();
}

#[test]
fn start_background_boot_failure_is_reported_and_terminal() {
    let (_d, cfg) = rig("no_hello");
    let s = EngineSupervisor::new(cfg.with_hello_timeout(Duration::from_millis(250)));
    let e = s.start_background().ready().unwrap_err();
    assert_eq!(e.code, "hello_timeout");
    assert_eq!(s.lifecycle(), Lifecycle::Down);
    match s.status().terminal {
        Some(Terminal::Failed { reason }) => assert!(reason.contains("hello"), "{reason}"),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn restarts_count_and_a_spent_budget_is_terminal_failed() {
    let (_d, cfg) = rig("crash:30");
    let policy = RestartPolicy::new(
        RestartMode::Automatic,
        1,
        Duration::from_secs(10),
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .unwrap();
    let s = EngineSupervisor::new(cfg.with_restart(policy));
    assert_eq!(s.status().restarts, 0);
    s.start().unwrap();
    wait_until(&s, Lifecycle::Down);
    let status = s.status();
    assert_eq!(status.restarts, 1);
    assert!(matches!(
        status.terminal,
        Some(Terminal::Failed { ref reason }) if reason.contains("budget")
    ));
    // An explicit start grants a fresh budget; restarts stays lifetime.
    s.start().unwrap();
    wait_until(&s, Lifecycle::Down);
    assert_eq!(s.status().restarts, 2);
    s.shutdown();
    assert_eq!(s.status().terminal, Some(Terminal::Stopped));
}

#[test]
fn shutdown_is_terminal_stopped_and_start_clears_it() {
    let (_d, cfg) = rig("ok");
    let s = EngineSupervisor::new(cfg);
    assert_eq!(s.status().terminal, None, "never started");
    s.start().unwrap();
    assert_eq!(s.status().terminal, None);
    s.shutdown();
    assert_eq!(s.lifecycle(), Lifecycle::Down);
    assert_eq!(s.status().terminal, Some(Terminal::Stopped));
    assert_eq!(s.status().restarts, 0);
    s.start().unwrap();
    assert_eq!(s.status().terminal, None);
    s.shutdown();
}

#[test]
fn on_demand_down_is_recoverable_not_terminal() {
    let (_d, cfg) = rig("crash:30,ok");
    let s = EngineSupervisor::new(
        cfg.with_restart(
            RestartPolicy::new(
                RestartMode::OnDemand,
                2,
                Duration::from_secs(10),
                Duration::from_millis(1),
                Duration::from_millis(1),
            )
            .unwrap(),
        ),
    );
    s.start().unwrap();
    wait_until(&s, Lifecycle::Down);
    assert_eq!(s.status().terminal, None);
    assert_eq!(s.call("ping", json!({})).unwrap()["pong"], true);
    assert_eq!(s.status().restarts, 1);
    s.shutdown();
}

fn restart_policy(max_restarts: u32) -> RestartPolicy {
    RestartPolicy::new(
        RestartMode::Automatic,
        max_restarts,
        Duration::from_secs(10),
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .unwrap()
}

#[test]
fn failed_first_boot_is_retried_within_the_budget_by_default() {
    let (_d, cfg) = rig("no_hello,ok");
    let s = EngineSupervisor::new(
        cfg.with_hello_timeout(Duration::from_millis(250))
            .with_restart(restart_policy(2)),
    );
    // The first generation never says hello; the retry boots and start() returns Ok.
    s.start().unwrap();
    assert_eq!(s.lifecycle(), Lifecycle::Ready);
    assert_eq!(s.call("ping", json!({})).unwrap()["pong"], true);
    let status = s.status();
    assert_eq!((status.generation, status.restarts), (2, 1));
    assert_eq!(status.terminal, None);
    s.shutdown();
}

#[test]
fn terminal_initial_failure_gives_up_at_once() {
    let (_d, cfg) = rig("no_hello,ok");
    let s = EngineSupervisor::new(
        cfg.with_hello_timeout(Duration::from_millis(250))
            .with_restart(restart_policy(2))
            .with_initial_failure(InitialFailure::Terminal),
    );
    assert_eq!(s.start().unwrap_err().code, "hello_timeout");
    let status = s.status();
    assert_eq!((status.generation, status.restarts), (1, 0));
    assert!(matches!(status.terminal, Some(Terminal::Failed { .. })));
}

#[test]
fn contract_mismatch_on_a_replacement_is_terminal_and_not_retried() {
    let (_d, cfg) = rig("crash:30,wrong_contract");
    let s = EngineSupervisor::new(
        cfg.require_contract_version(CONTRACT_VERSION)
            .with_restart(restart_policy(5)),
    );
    s.start().unwrap();
    wait_until(&s, Lifecycle::Down);
    // Give a budgeted retry time to show up; there must be none.
    std::thread::sleep(Duration::from_millis(200));
    let status = s.status();
    assert_eq!(status.generation, 2, "no third boot after a mismatch");
    assert_eq!(status.restarts, 1);
    assert!(matches!(
        status.terminal,
        Some(Terminal::Failed { ref reason }) if reason.contains(CONTRACT_VERSION_MISMATCH)
    ));
    s.shutdown();
}

#[test]
fn protocol_error_sink_sees_malformed_lines_and_calls_still_answer() {
    let (_d, cfg) = rig("ok");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let s = EngineSupervisor::new(cfg.with_protocol_error_sink(move |e: &ProtocolError| {
        sink.lock().unwrap().push(e.line.clone())
    }));
    s.start().unwrap();
    assert_eq!(s.call("junk", json!({})).unwrap()["after"], true);
    assert_eq!(*seen.lock().unwrap(), vec!["not json {".to_string()]);
    s.shutdown();
}

#[test]
fn errors_say_where_they_arose_and_bare_engine_errors_keep_a_message() {
    let (_d, unused) = rig("ok");
    let never_started = EngineSupervisor::new(unused);
    let down = never_started.call("ping", json!({})).unwrap_err();
    assert_eq!(
        (down.code.as_str(), down.origin),
        ("down", ErrorOrigin::Supervisor)
    );

    let (_d, cfg) = rig("ok");
    let s = EngineSupervisor::new(cfg);
    s.start().unwrap();
    let unknown = s.call("nope", json!({})).unwrap_err();
    assert_eq!(unknown.origin, ErrorOrigin::Engine);
    let bare = s.call("bare_error", json!({})).unwrap_err();
    assert_eq!(bare.code, "engine_error");
    assert_eq!(bare.message, "engine reported failure without detail");
    assert_eq!(bare.origin, ErrorOrigin::Engine);
    s.shutdown();

    let (_d, cfg) = rig("ok");
    let s = EngineSupervisor::new(cfg.with_fallback_error_code("host_fallback"));
    s.start().unwrap();
    assert_eq!(
        s.call("bare_error", json!({})).unwrap_err().code,
        "host_fallback"
    );
    s.shutdown();
}
