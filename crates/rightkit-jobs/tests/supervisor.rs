//! Supervisor end-to-end coverage. Every case launches the real scriptable
//! `engine_fixture` child and checks observable frames, lifecycle, or files.

use rightkit_jobs::{CallOptions, EngineSupervisor, Lifecycle, SupervisorConfig};
use rightkit_process::{OwnedCommand, RestartMode, RestartPolicy};
use serde_json::json;
use std::path::PathBuf;
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
    let s = EngineSupervisor::new(cfg.with_hello_timeout(Duration::from_millis(250)));
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
