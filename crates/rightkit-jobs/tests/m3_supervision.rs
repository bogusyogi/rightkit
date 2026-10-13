//! M3 owner-gap coverage (engine supervision): restart policy wiring, snapshot reconciliation,
//! contract requirement, environment policy, deadlines, write stalls and shutdown bounds. Every case
//! launches the real scriptable `engine_fixture` child.

use rightkit_jobs::{
    CallOptions, EngineSupervisor, EventFrame, Lifecycle, ProgressDeadline, SupervisorConfig,
    Terminal, CONTRACT_VERSION, CONTRACT_VERSION_MISMATCH,
};
use rightkit_process::{OwnedCommand, RestartMode, RestartPolicy};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
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
fn consecutive_failures_without_ready_end_the_run_before_the_rolling_budget() {
    // Every boot is refused at the handshake (it never reports ready), so the consecutive cap
    // of 2 ends the run although the rolling budget (10) is far from spent.
    let (_d, cfg) = rig("reject_set");
    let policy = RestartPolicy::new(
        RestartMode::Automatic,
        10,
        Duration::from_secs(60),
        Duration::from_millis(5),
        Duration::from_millis(5),
    )
    .unwrap()
    .with_consecutive_failures(2, Duration::ZERO)
    .unwrap();
    let s = EngineSupervisor::new(cfg.with_restart(policy));
    s.set_snapshot("settings", "set_settings", json!({}))
        .unwrap();
    assert!(s.start().is_err());
    wait_until(&s, Lifecycle::Down);
    let st = s.status();
    assert!(
        matches!(st.terminal, Some(Terminal::Failed { .. })),
        "{st:?}"
    );
    assert_eq!(st.generation, 3, "first boot plus two restarts: {st:?}");
    assert!(st.restarts_in_window < 10);
}

#[test]
fn a_crash_after_ready_does_not_count_as_a_consecutive_failure() {
    // Each generation reaches ready and then crashes. With a cap of 1 and no cooldown a
    // consecutive count that ignored ready would stop at generation 2; it must keep restarting.
    let (_d, cfg) = rig("crash:20");
    let policy = RestartPolicy::new(
        RestartMode::Automatic,
        20,
        Duration::from_secs(60),
        Duration::from_millis(5),
        Duration::from_millis(5),
    )
    .unwrap()
    .with_consecutive_failures(1, Duration::ZERO)
    .unwrap();
    let s = EngineSupervisor::new(cfg.with_restart(policy));
    s.start().unwrap();
    wait_generation(&s, 4);
    assert!(s.status().terminal.is_none(), "{:?}", s.status());
    s.shutdown();
}

#[test]
fn a_snapshot_updated_while_a_generation_replays_reaches_it_before_ready() {
    // The first replay call takes 600 ms; the update lands inside it, after the replay was copied.
    let (_d, cfg) = rig("slow_set:600");
    let s = EngineSupervisor::new(cfg);
    s.set_snapshot("a", "set_a", json!({"v": 1})).unwrap();
    let ready = s.start_background();
    std::thread::sleep(Duration::from_millis(200));
    assert_ne!(
        s.lifecycle(),
        Lifecycle::Ready,
        "the boot must still be replaying"
    );
    s.set_snapshot("a", "set_a", json!({"v": 2})).unwrap();
    ready.ready().unwrap();
    assert_eq!(s.call("get_state", json!({})).unwrap()["set_a"]["v"], 2);
    s.shutdown();
}

#[test]
fn an_update_during_a_replay_is_not_replayed_twice_when_nothing_moved() {
    // Negative: with no update the replay is one call per snapshot, in registration order.
    let (_d, cfg) = rig("ok");
    let s = EngineSupervisor::new(cfg);
    s.set_snapshot("a", "set_a", json!({"v": 1})).unwrap();
    s.set_snapshot("b", "set_b", json!({"v": 2})).unwrap();
    s.start().unwrap();
    let state = s.call("get_state", json!({})).unwrap();
    assert_eq!(state["set_a"]["v"], 1);
    assert_eq!(state["set_b"]["v"], 2);
    s.shutdown();
}

#[test]
fn a_mandatory_contract_check_refuses_to_boot_without_a_version() {
    let (dir, cfg) = rig("ok");
    let s = EngineSupervisor::new(cfg.with_mandatory_contract_check());
    let err = s.start().unwrap_err();
    assert_eq!(err.code, "contract_check_required");
    assert!(!err.retryable);
    assert_eq!(s.lifecycle(), Lifecycle::Down);
    let background = s.start_background().ready().unwrap_err();
    assert_eq!(background.code, "contract_check_required");
    assert!(
        !dir.path().join("gen").exists(),
        "nothing may be spawned when the gate refuses"
    );
}

#[test]
fn a_mandatory_contract_check_with_the_current_version_boots_and_a_wrong_engine_is_refused() {
    let (_d, cfg) = rig("ok");
    let s = EngineSupervisor::new(
        cfg.with_mandatory_contract_check()
            .require_current_contract_version(),
    );
    s.start().unwrap();
    assert_eq!(
        s.status().hello.unwrap()["contract_version"],
        CONTRACT_VERSION
    );
    s.shutdown();

    let (_d, cfg) = rig("wrong_contract");
    let s = EngineSupervisor::new(
        cfg.with_mandatory_contract_check()
            .require_current_contract_version(),
    );
    assert_eq!(s.start().unwrap_err().code, CONTRACT_VERSION_MISMATCH);
}

fn env_rig(scrub: bool) -> (tempfile::TempDir, SupervisorConfig) {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_path_buf();
    let cfg = SupervisorConfig::new("fixture", move || {
        let mut c = OwnedCommand::new(fixture());
        if scrub {
            c.env_allowlist_with_defaults(["RK_M3_JOBS_KEEP"]);
        }
        c.command_mut().env("FX_DIR", &state).env("FX_PLAN", "ok");
        Ok(c)
    })
    .with_hello_timeout(Duration::from_secs(10))
    .with_shutdown_grace(Duration::from_millis(200));
    (dir, cfg)
}

#[test]
fn a_scrubbed_engine_sees_only_its_allowlist() {
    std::env::set_var("RK_M3_JOBS_KEEP", "yes");
    std::env::set_var("RK_M3_JOBS_SECRET", "no");
    let (_d, cfg) = env_rig(true);
    let s = EngineSupervisor::new(cfg.require_scrubbed_env());
    s.start().unwrap();
    let vars = s.call("env", json!({})).unwrap()["vars"].clone();
    assert_eq!(vars["RK_M3_JOBS_KEEP"], "yes");
    assert!(vars.get("RK_M3_JOBS_SECRET").is_none(), "{vars}");
    assert_eq!(vars["FX_PLAN"], "ok", "explicit variables are still passed");
    s.shutdown();
}

#[test]
fn an_unscrubbed_command_is_refused_when_scrubbing_is_required_and_inherits_otherwise() {
    std::env::set_var("RK_M3_JOBS_SECRET", "no");
    let (dir, cfg) = env_rig(false);
    let s = EngineSupervisor::new(cfg.require_scrubbed_env());
    let err = s.start().unwrap_err();
    assert_eq!(err.code, "env_not_scrubbed");
    assert!(matches!(s.status().terminal, Some(Terminal::Failed { .. })));
    assert!(!dir.path().join("gen").exists(), "nothing was spawned");
    // Without the requirement the default is unchanged: the environment is inherited.
    let (_d, cfg) = env_rig(false);
    let s = EngineSupervisor::new(cfg);
    s.start().unwrap();
    let vars = s.call("env", json!({})).unwrap()["vars"].clone();
    assert_eq!(vars["RK_M3_JOBS_SECRET"], "no");
    s.shutdown();
}

fn keep_alive() -> CallOptions {
    CallOptions {
        keep_alive_on_timeout: true,
        ..CallOptions::default()
    }
}

#[test]
fn progress_of_the_call_extends_an_inactivity_deadline_that_an_absolute_one_would_not() {
    let (_d, cfg) = rig("ok");
    let s = EngineSupervisor::new(cfg);
    s.start().unwrap();
    // 5 events 200 ms apart: a second of work, never more than 200 ms of silence.
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    let progress = s
        .call_with_progress_deadline(
            "slow_progress",
            json!({"n": 5, "ms": 200}),
            CallOptions {
                on_frame: Some(Box::new(move |_: &EventFrame<'_>| {
                    counter.fetch_add(1, Ordering::SeqCst);
                })),
                ..keep_alive()
            },
            ProgressDeadline::new(Duration::from_millis(600)),
        )
        .unwrap();
    assert!(progress["id"].is_u64());
    assert_eq!(seen.load(Ordering::SeqCst), 5);
    // Negative: the default is still one absolute deadline, which the same call misses.
    let err = s
        .call_with(
            "slow_progress",
            json!({"n": 5, "ms": 200}),
            CallOptions {
                timeout: Some(Duration::from_millis(600)),
                ..keep_alive()
            },
        )
        .unwrap_err();
    assert_eq!(err.code, "timeout");
    s.shutdown();
}

#[test]
fn silence_past_the_idle_bound_times_out_and_max_total_still_caps_a_chatty_call() {
    let (_d, cfg) = rig("ok");
    let s = EngineSupervisor::new(cfg);
    s.start().unwrap();
    let started = Instant::now();
    let err = s
        .call_with_progress_deadline(
            "sleep",
            json!({"ms": 3000}),
            keep_alive(),
            ProgressDeadline::new(Duration::from_millis(300)),
        )
        .unwrap_err();
    assert_eq!(err.code, "timeout");
    assert!(started.elapsed() < Duration::from_millis(2000));
    // The engine was kept alive (the sleep still runs); wait it out for the next call.
    std::thread::sleep(Duration::from_millis(3000));
    let started = Instant::now();
    let err = s
        .call_with_progress_deadline(
            "slow_progress",
            json!({"n": 50, "ms": 100}),
            keep_alive(),
            ProgressDeadline::new(Duration::from_millis(500))
                .with_max_total(Duration::from_millis(700)),
        )
        .unwrap_err();
    assert_eq!(err.code, "timeout");
    assert!(
        started.elapsed() < Duration::from_millis(2000),
        "{:?}",
        started.elapsed()
    );
    s.shutdown();
}

#[test]
fn events_that_do_not_belong_to_the_call_are_not_progress() {
    let (_d, cfg) = rig("ok");
    let s = EngineSupervisor::new(cfg);
    s.start().unwrap();
    // 8 rounds of unsolicited and other-request events, 100 ms apart; this call's own id never appears.
    let started = Instant::now();
    let err = s
        .call_with_progress_deadline(
            "chatter",
            json!({"n": 8, "ms": 100}),
            keep_alive(),
            ProgressDeadline::new(Duration::from_millis(300)),
        )
        .unwrap_err();
    assert_eq!(err.code, "timeout");
    assert!(
        started.elapsed() < Duration::from_millis(750),
        "unrelated chatter must not extend the deadline: {:?}",
        started.elapsed()
    );
    s.shutdown();
}
